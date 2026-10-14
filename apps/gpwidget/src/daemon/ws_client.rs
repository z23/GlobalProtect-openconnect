use std::io::ErrorKind;
use std::time::Duration;

use anyhow::{Context, bail};
use futures_util::{SinkExt, StreamExt};
use gpapi::{
  service::{
    event::WsEvent,
    request::WsRequest,
    transport::{
      ClientMessage, ClientPrelude, HIP_PROTOCOL_VERSION, NoiseInitiator, NoiseTransport, ServerMessage,
      ServerPrelude, ServiceResult, SessionCredential,
    },
    vpn_env::VpnEnv,
  },
  utils::lock_file::{LockFileError, LockInfo, gpservice_lock_info},
};
use log::{debug, info, warn};
use tokio::{net::TcpStream, sync::mpsc};
use tokio_tungstenite::{
  MaybeTlsStream, WebSocketStream, connect_async,
  tungstenite::{Message, protocol::CloseFrame},
};
use uuid::Uuid;

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// What the WS task reports up to the daemon core.
pub enum WsNotice {
  Event(WsEvent),
  /// gpservice refused a request; carries its reason.
  Rejected(String),
  /// Connection dropped; reconnection attempts are underway.
  Lost,
  /// gpservice is confirmed gone (or unreachable past the backoff budget).
  /// The daemon should shut down: the credential is per-launch, so a *new*
  /// gpservice can never be rejoined by this process.
  Gone,
}

pub struct WsClient {
  pub notices: mpsc::Receiver<WsNotice>,
  pub requests: mpsc::Sender<WsRequest>,
}

const RECONNECT_DELAYS_MS: [u64; 6] = [500, 1000, 2000, 4000, 5000, 5000];

pub fn spawn(credential: SessionCredential) -> WsClient {
  let (notice_tx, notice_rx) = mpsc::channel(64);
  let (req_tx, req_rx) = mpsc::channel::<WsRequest>(16);

  tokio::spawn(run(credential, notice_tx, req_rx));

  WsClient {
    notices: notice_rx,
    requests: req_tx,
  }
}

const STARTUP_GRACE_SECS: u64 = 15;
const CONNECT_TIMEOUT: Duration = Duration::from_millis(400);
// Abandoning a handshake on a not-yet-activated credential makes gpservice
// discard it, so allow the service's own 5s handshake budget.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);

/// gpservice will never accept this credential again (revoked, superseded,
/// or a restarted service), so reconnecting is pointless.
#[derive(Debug)]
struct Revoked(String);

impl std::fmt::Display for Revoked {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    write!(f, "gpservice ended the session: {}", self.0)
  }
}

impl std::error::Error for Revoked {}

fn is_revoked(err: &anyhow::Error) -> bool {
  err.downcast_ref::<Revoked>().is_some()
}

/// Port discovered for this process. A later gpservice issues a new
/// credential, so this process never looks the port up again after the
/// first success.
struct Endpoint {
  port: Option<u16>,
  announced_probe: bool,
}

/// An authenticated, attached session ready to relay messages.
struct Session {
  socket: Socket,
  noise: NoiseTransport,
  revision: u64,
  snapshot: VpnEnv,
}

async fn run(credential: SessionCredential, notice_tx: mpsc::Sender<WsNotice>, mut req_rx: mpsc::Receiver<WsRequest>) {
  let mut endpoint = Endpoint {
    port: None,
    announced_probe: false,
  };

  // The credential must be activated within gpservice's activation window,
  // and the lock file can lag the launch, so retry briefly instead of
  // declaring the service gone at birth.
  let deadline = tokio::time::Instant::now() + Duration::from_secs(STARTUP_GRACE_SECS);
  let mut session = loop {
    match open(&credential, &mut endpoint, None).await {
      Ok(session) => break session,
      Err(err) if is_revoked(&err) => {
        warn!("{:#}", err);
        let _ = notice_tx.send(WsNotice::Gone).await;
        return;
      }
      Err(err) => {
        if tokio::time::Instant::now() >= deadline {
          warn!("gpservice did not come up within {}s: {:#}", STARTUP_GRACE_SECS, err);
          let _ = notice_tx.send(WsNotice::Gone).await;
          return;
        }
        debug!("gpservice is not reachable yet: {:#}", err);
      }
    }

    tokio::time::sleep(Duration::from_millis(500)).await;
  };

  loop {
    let mut revision = session.revision;

    match pump(session, &mut revision, &notice_tx, &mut req_rx).await {
      Ok(()) => info!("WS connection closed by gpservice"),
      Err(err) if is_revoked(&err) => {
        warn!("{:#}", err);
        let _ = notice_tx.send(WsNotice::Gone).await;
        return;
      }
      Err(err) => warn!("WS connection error: {:#}", err),
    }

    if notice_tx.send(WsNotice::Lost).await.is_err() {
      return;
    }

    let mut reconnected = None;
    for delay_ms in RECONNECT_DELAYS_MS {
      tokio::time::sleep(Duration::from_millis(delay_ms)).await;

      if let Some(port) = endpoint.port
        && !health_ok(port).await
      {
        info!("gpservice is gone, stopping reconnection attempts");
        break;
      }

      match open(&credential, &mut endpoint, Some(revision)).await {
        Ok(session) => {
          reconnected = Some(session);
          break;
        }
        Err(err) if is_revoked(&err) => {
          warn!("{:#}", err);
          break;
        }
        Err(err) => info!("Reconnect attempt failed: {:#}", err),
      }
    }

    let Some(next) = reconnected else {
      let _ = notice_tx.send(WsNotice::Gone).await;
      return;
    };
    session = next;
  }
}

/// One WS session: relay events out and requests in until the connection
/// dies. `revision` tracks the last event seen, for the next attach.
async fn pump(
  session: Session,
  revision: &mut u64,
  notice_tx: &mpsc::Sender<WsNotice>,
  req_rx: &mut mpsc::Receiver<WsRequest>,
) -> anyhow::Result<()> {
  let Session {
    socket,
    mut noise,
    snapshot,
    ..
  } = session;

  if notice_tx.send(WsNotice::Event(WsEvent::VpnEnv(snapshot))).await.is_err() {
    return Ok(());
  }

  let (mut sink, mut source) = socket.split();

  loop {
    tokio::select! {
      frame = source.next() => {
        let Some(frame) = frame else {
          return Ok(());
        };

        match frame? {
          Message::Binary(payload) => {
            let message: ServerMessage = noise
              .decrypt(&payload)
              .context("Failed to authenticate a gpservice message")?;

            let notice = match message {
              ServerMessage::Event { revision: next, event } => {
                if next != revision.wrapping_add(1) {
                  warn!("Missed gpservice events ({} -> {}), requesting a snapshot", revision, next);
                  let resync = noise.encrypt(&ClientMessage::Resync { last_event_revision: *revision })?;
                  sink.send(Message::Binary(resync.into())).await?;
                }
                *revision = next;
                WsNotice::Event(event)
              }
              ServerMessage::Snapshot { current_revision, snapshot } => {
                *revision = current_revision;
                WsNotice::Event(WsEvent::VpnState(snapshot.vpn_state))
              }
              ServerMessage::Reply { result: ServiceResult::Rejected(rejection), .. } => {
                warn!("gpservice rejected a request ({:?}): {}", rejection.code(), rejection.message());
                WsNotice::Rejected(rejection.message().to_owned())
              }
              ServerMessage::Reply { .. } => continue,
              ServerMessage::Attached { .. } => bail!("gpservice attached an already attached session"),
            };

            if notice_tx.send(notice).await.is_err() {
              return Ok(());
            }
          }
          // gpservice pings on idle and closes without a pong; answer
          // explicitly instead of relying on split-stream auto-pong flushing.
          Message::Ping(payload) => sink.send(Message::Pong(payload)).await?,
          Message::Close(frame) => return closed(frame),
          _ => {}
        }
      }

      req = req_rx.recv() => {
        let Some(request) = req else {
          return Ok(());
        };

        let message = ClientMessage::Request { id: Uuid::new_v4(), request };
        let payload = noise.encrypt(&message).context("Failed to encrypt WS request")?;
        sink.send(Message::Binary(payload.into())).await?;
      }
    }
  }
}

/// gpservice's application close codes that mean the credential is dead:
/// VersionMismatch, Unauthorized, SessionReplaced, ServiceRestarted.
const FINAL_CLOSE_CODES: [u16; 4] = [4001, 4002, 4003, 4011];

fn closed(frame: Option<CloseFrame>) -> anyhow::Result<()> {
  match frame {
    Some(frame) if FINAL_CLOSE_CODES.contains(&u16::from(frame.code)) => Err(Revoked(frame.reason.to_string()).into()),
    Some(frame) => bail!("gpservice closed the connection: {}", frame.reason),
    None => Ok(()),
  }
}

/// Dial and authenticate, discovering the port on first use.
async fn open(
  credential: &SessionCredential,
  endpoint: &mut Endpoint,
  last_revision: Option<u64>,
) -> anyhow::Result<Session> {
  if let Some(port) = endpoint.port {
    return establish(port, credential, last_revision).await;
  }

  match classify_lock(gpservice_lock_info().await) {
    LockView::Port(port) => {
      let session = establish(port, credential, last_revision).await?;
      endpoint.port = Some(port);
      Ok(session)
    }
    LockView::Missing => bail!("gpservice has not published its port yet"),
    LockView::Unreadable => {
      if !endpoint.announced_probe {
        info!("gpservice lock file is not readable by this user; scanning localhost listeners");
        endpoint.announced_probe = true;
      }

      let table = tokio::fs::read_to_string("/proc/net/tcp")
        .await
        .context("Failed to read /proc/net/tcp")?;
      let (port, session) = probe(&localhost_root_listeners(&table), credential, last_revision).await?;
      endpoint.port = Some(port);
      Ok(session)
    }
  }
}

/// Only gpservice holds the credential's secret, so the first candidate that
/// completes the handshake is gpservice. Any other listener just fails it.
async fn probe(
  ports: &[u16],
  credential: &SessionCredential,
  last_revision: Option<u64>,
) -> anyhow::Result<(u16, Session)> {
  for &port in ports {
    match establish(port, credential, last_revision).await {
      Ok(session) => return Ok((port, session)),
      Err(err) => debug!("Listener {} is not this session's gpservice: {:#}", port, err),
    }
  }

  bail!("No localhost listener accepted the gpservice handshake")
}

async fn establish(port: u16, credential: &SessionCredential, last_revision: Option<u64>) -> anyhow::Result<Session> {
  let url = format!("ws://127.0.0.1:{port}/ws");
  let (mut socket, _) = tokio::time::timeout(CONNECT_TIMEOUT, connect_async(&url))
    .await
    .context("Timed out connecting to gpservice WS")?
    .context("Failed to connect to gpservice WS")?;

  let (noise, revision, snapshot) = tokio::time::timeout(
    HANDSHAKE_TIMEOUT,
    handshake(&mut socket, credential, last_revision),
  )
  .await
  .context("Timed out authenticating with gpservice")??;

  info!("Connected to gpservice at {}", url);

  Ok(Session {
    socket,
    noise,
    revision,
    snapshot,
  })
}

/// Prelude, Noise NNpsk0 with the credential secret, then attach.
async fn handshake(
  socket: &mut Socket,
  credential: &SessionCredential,
  last_revision: Option<u64>,
) -> anyhow::Result<(NoiseTransport, u64, VpnEnv)> {
  let prelude = ClientPrelude {
    product_version: credential.product_version().to_owned(),
    hip_protocol_version: HIP_PROTOCOL_VERSION,
    service_instance_id: credential.service_instance_id(),
    session_id: credential.session_id(),
  };
  socket.send(Message::Binary(prelude.encode()?.into())).await?;

  if let ServerPrelude::Rejected(rejection) = ServerPrelude::decode(&recv_binary(socket).await?)? {
    return Err(Revoked(format!("{rejection:?}")).into());
  }

  let mut initiator = NoiseInitiator::new(
    credential.product_version(),
    credential.service_instance_id(),
    credential.session_id(),
    credential.secret(),
  )?;
  socket.send(Message::Binary(initiator.write_first()?.into())).await?;
  let mut noise = initiator.read_response(&recv_binary(socket).await?)?;

  let attach = noise.encrypt(&ClientMessage::Attach {
    last_event_revision: last_revision,
  })?;
  socket.send(Message::Binary(attach.into())).await?;

  match noise.decrypt(&recv_binary(socket).await?)? {
    ServerMessage::Attached {
      current_revision,
      snapshot,
      ..
    } => Ok((noise, current_revision, snapshot)),
    _ => bail!("gpservice did not attach the session"),
  }
}

async fn recv_binary(socket: &mut Socket) -> anyhow::Result<Vec<u8>> {
  loop {
    match socket.next().await.context("gpservice closed the connection")?? {
      Message::Binary(payload) => return Ok(payload.to_vec()),
      Message::Ping(payload) => socket.send(Message::Pong(payload)).await?,
      Message::Close(frame) => {
        closed(frame)?;
        bail!("gpservice closed the connection during the handshake");
      }
      _ => bail!("Unexpected frame during the gpservice handshake"),
    }
  }
}

/// Where the lock file left us. `Missing` means gpservice has not published a
/// port yet. `Unreadable` means it has, but the desktop user cannot see it:
/// pkexec keeps the caller's umask, so a `0027` umask creates a `0640`
/// root-owned `/var/run/gpservice.lock`.
#[derive(Debug, PartialEq, Eq)]
enum LockView {
  Port(u16),
  Missing,
  Unreadable,
}

fn classify_lock(result: Result<LockInfo, LockFileError>) -> LockView {
  match result {
    Ok(info) => match u16::try_from(info.port) {
      Ok(port) => LockView::Port(port),
      Err(_) => LockView::Missing,
    },
    Err(LockFileError::IoError(err)) if err.kind() == ErrorKind::NotFound => LockView::Missing,
    Err(LockFileError::IoError(err)) if err.kind() == ErrorKind::PermissionDenied => LockView::Unreadable,
    Err(_) => LockView::Missing,
  }
}

async fn health_ok(port: u16) -> bool {
  let url = format!("http://127.0.0.1:{port}/health");
  let client = reqwest::Client::builder()
    .timeout(Duration::from_secs(2))
    .build()
    .expect("reqwest client");

  match client.get(&url).send().await {
    Ok(resp) => resp.status().is_success(),
    Err(_) => false,
  }
}

/// `127.0.0.1` TCP listeners owned by uid 0, from a `/proc/net/tcp` snapshot.
///
/// `hidepid=2` hides this table. The lock file remains the fast path when
/// the desktop user can read it.
fn localhost_root_listeners(table: &str) -> Vec<u16> {
  let mut ports = Vec::new();

  for line in table.lines().skip(1) {
    let cols: Vec<&str> = line.split_whitespace().collect();
    if cols.len() < 8 || cols[3] != "0A" || cols[7] != "0" {
      continue;
    }

    let Some((ip, port_hex)) = cols[1].split_once(':') else {
      continue;
    };

    // 127.0.0.1 in the kernel's little-endian hex form.
    if ip != "0100007F" {
      continue;
    }

    if let Ok(port) = u16::from_str_radix(port_hex, 16) {
      ports.push(port);
    }
  }

  ports
}

#[cfg(test)]
mod tests {
  use gpapi::{
    os_profile::HostIdentity,
    service::{
      request::DisconnectRequest,
      transport::{HandshakeRejection, NoiseResponder, ServiceErrorCode},
      vpn_env::HostInfo,
      vpn_state::VpnState,
    },
  };
  use tokio::net::TcpListener;

  use super::*;

  const VERSION: &str = "test-version";

  fn credential() -> SessionCredential {
    SessionCredential::generate(Uuid::new_v4(), VERSION).unwrap()
  }

  fn snapshot() -> VpnEnv {
    VpnEnv {
      vpn_state: VpnState::Disconnected,
      vpnc_script: None,
      auth_executable: "/usr/bin/gpauth".into(),
      host_info: HostInfo {
        host_identity: HostIdentity::collect(),
      },
    }
  }

  async fn recv(ws: &mut WebSocketStream<TcpStream>) -> Vec<u8> {
    match ws.next().await.unwrap().unwrap() {
      Message::Binary(payload) => payload.to_vec(),
      frame => panic!("unexpected frame: {frame:?}"),
    }
  }

  /// gpservice's side of the handshake, built from the same gpapi types.
  async fn accept(
    listener: &TcpListener,
    credential: &SessionCredential,
  ) -> (WebSocketStream<TcpStream>, NoiseTransport, Option<u64>) {
    let (stream, _) = listener.accept().await.unwrap();
    let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();

    let prelude = ClientPrelude::decode(&recv(&mut ws).await).unwrap();
    assert_eq!(prelude.product_version, VERSION);
    assert_eq!(prelude.hip_protocol_version, HIP_PROTOCOL_VERSION);
    assert_eq!(prelude.service_instance_id, credential.service_instance_id());
    assert_eq!(prelude.session_id, credential.session_id());
    ws.send(Message::Binary(ServerPrelude::Ready.encode().unwrap().into()))
      .await
      .unwrap();

    let mut responder = NoiseResponder::new(
      VERSION,
      credential.service_instance_id(),
      credential.session_id(),
      credential.secret(),
    )
    .unwrap();
    responder.read_first(&recv(&mut ws).await).unwrap();
    let (response, mut noise) = responder.write_response().unwrap();
    ws.send(Message::Binary(response.into())).await.unwrap();

    let ClientMessage::Attach { last_event_revision } = noise.decrypt(&recv(&mut ws).await).unwrap() else {
      panic!("expected attach");
    };
    let attached = noise
      .encrypt(&ServerMessage::Attached {
        connection_id: Uuid::new_v4(),
        current_revision: 7,
        snapshot: snapshot(),
      })
      .unwrap();
    ws.send(Message::Binary(attached.into())).await.unwrap();

    (ws, noise, last_event_revision)
  }

  #[test]
  fn tcp_table_keeps_root_loopback_listeners_only() {
    let table = "\
  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode
   0: 0100007F:1F90 00000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 1 1 00000000 100 0 0 10 0
   1: 00000000:0050 00000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 2 1 00000000 100 0 0 10 0
   2: 0100007F:01BB 00000000:0000 01 00000000:00000000 00:00000000 00000000     0        0 3 1 00000000 100 0 0 10 0
   3: 0100007F:0400 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 4 1 00000000 100 0 0 10 0
";

    assert_eq!(localhost_root_listeners(table), vec![0x1F90]);
  }

  #[test]
  fn missing_lock_is_not_a_probe_and_denied_lock_is() {
    let missing = LockFileError::IoError(std::io::Error::new(ErrorKind::NotFound, "absent"));
    let denied = LockFileError::IoError(std::io::Error::new(ErrorKind::PermissionDenied, "umask"));

    assert_eq!(classify_lock(Err(missing)), LockView::Missing);
    assert_eq!(classify_lock(Err(denied)), LockView::Unreadable);
    assert_eq!(classify_lock(Ok(LockInfo { pid: 1, port: 4433 })), LockView::Port(4433));
  }

  #[tokio::test]
  async fn session_relays_snapshot_events_and_requests() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let credential = credential();
    let server_credential = SessionCredential::decode_frame(&credential.encode_frame().unwrap()).unwrap();

    let server = tokio::spawn(async move {
      let (mut ws, mut noise, last_event_revision) = accept(&listener, &server_credential).await;
      assert_eq!(last_event_revision, Some(3));

      let event = noise
        .encrypt(&ServerMessage::Event {
          revision: 8,
          event: WsEvent::VpnState(VpnState::Disconnecting),
        })
        .unwrap();
      ws.send(Message::Binary(event.into())).await.unwrap();

      let ClientMessage::Request { id, request } = noise.decrypt(&recv(&mut ws).await).unwrap() else {
        panic!("expected a request");
      };
      assert!(matches!(request, WsRequest::Disconnect(_)));
      let reply = noise
        .encrypt(&ServerMessage::Reply {
          id,
          result: ServiceResult::rejected(ServiceErrorCode::Busy, "busy"),
        })
        .unwrap();
      ws.send(Message::Binary(reply.into())).await.unwrap();
      ws
    });

    let session = establish(port, &credential, Some(3)).await.unwrap();
    assert_eq!(session.revision, 7);

    let (notice_tx, mut notices) = mpsc::channel(8);
    let (req_tx, mut req_rx) = mpsc::channel(8);
    let pump = tokio::spawn(async move {
      let mut revision = session.revision;
      let result = pump(session, &mut revision, &notice_tx, &mut req_rx).await;
      (result, revision)
    });

    assert!(matches!(notices.recv().await, Some(WsNotice::Event(WsEvent::VpnEnv(_)))));
    assert!(matches!(
      notices.recv().await,
      Some(WsNotice::Event(WsEvent::VpnState(VpnState::Disconnecting)))
    ));

    req_tx.send(WsRequest::Disconnect(DisconnectRequest)).await.unwrap();
    assert!(matches!(notices.recv().await, Some(WsNotice::Rejected(message)) if message == "busy"));

    drop(server.await.unwrap());
    let (_, revision) = pump.await.unwrap();
    assert_eq!(revision, 8);
  }

  #[tokio::test]
  async fn rejected_handshake_revokes_the_session() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    tokio::spawn(async move {
      let (stream, _) = listener.accept().await.unwrap();
      let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
      let _ = recv(&mut ws).await;
      let rejected = ServerPrelude::Rejected(HandshakeRejection::ServiceRestarted {
        service_instance_id: Uuid::new_v4(),
      });
      ws.send(Message::Binary(rejected.encode().unwrap().into())).await.unwrap();
    });

    let err = establish(port, &credential(), None).await.err().unwrap();
    assert!(is_revoked(&err));
  }

  #[tokio::test]
  async fn probe_skips_listeners_that_cannot_authenticate() {
    let decoy = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let real = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let decoy_port = decoy.local_addr().unwrap().port();
    let real_port = real.local_addr().unwrap().port();
    let credential = credential();
    let server_credential = SessionCredential::decode_frame(&credential.encode_frame().unwrap()).unwrap();

    // A WS listener holding a different secret: the Noise handshake fails.
    tokio::spawn(async move {
      let (stream, _) = decoy.accept().await.unwrap();
      let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
      let prelude = ClientPrelude::decode(&recv(&mut ws).await).unwrap();
      ws.send(Message::Binary(ServerPrelude::Ready.encode().unwrap().into()))
        .await
        .unwrap();
      let mut responder = NoiseResponder::new(
        VERSION,
        prelude.service_instance_id,
        prelude.session_id,
        &[7; 32],
      )
      .unwrap();
      let _ = responder.read_first(&recv(&mut ws).await);
      let _ = ws.close(None).await;
    });

    let server = tokio::spawn(async move { accept(&real, &server_credential).await.0 });

    let (port, _session) = probe(&[decoy_port, real_port], &credential, None).await.unwrap();
    assert_eq!(port, real_port);
    drop(server.await.unwrap());
  }

  #[test]
  fn final_close_codes_end_the_session() {
    use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;

    let frame = |code: u16| {
      Some(CloseFrame {
        code: CloseCode::from(code),
        reason: "reason".into(),
      })
    };

    assert!(is_revoked(&closed(frame(4002)).unwrap_err()));
    assert!(is_revoked(&closed(frame(4011)).unwrap_err()));
    assert!(!is_revoked(&closed(frame(4012)).unwrap_err()));
    assert!(closed(None).is_ok());
  }
}
