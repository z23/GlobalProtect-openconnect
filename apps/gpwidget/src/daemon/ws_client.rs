use std::io::ErrorKind;
use std::time::Duration;

use anyhow::Context;
use futures_util::{SinkExt, StreamExt};
use gpapi::{
  service::{event::WsEvent, request::WsRequest},
  utils::{
    crypto::Crypto,
    lock_file::{LockFileError, LockInfo, gpservice_lock_info},
  },
};
use log::{info, warn};
use tokio::sync::mpsc;
use tokio_tungstenite::{connect_async, tungstenite::Message};

/// What the WS task reports up to the daemon core.
pub enum WsNotice {
  Event(WsEvent),
  /// Connection dropped; reconnection attempts are underway.
  Lost,
  /// gpservice is confirmed gone (or unreachable past the backoff budget).
  /// The daemon should shut down: the api-key is per-launch, so a *new*
  /// gpservice can never be rejoined by this process.
  Gone,
}

pub struct WsClient {
  pub notices: mpsc::Receiver<WsNotice>,
  pub requests: mpsc::Sender<WsRequest>,
}

const RECONNECT_DELAYS_MS: [u64; 6] = [500, 1000, 2000, 4000, 5000, 5000];

pub fn spawn(api_key: Vec<u8>) -> WsClient {
  let (notice_tx, notice_rx) = mpsc::channel(64);
  let (req_tx, req_rx) = mpsc::channel::<WsRequest>(16);

  tokio::spawn(run(api_key, notice_tx, req_rx));

  WsClient {
    notices: notice_rx,
    requests: req_tx,
  }
}

const STARTUP_GRACE_SECS: u64 = 15;

/// Port discovered for this process. A later gpservice has a new api-key, so
/// this process never looks the port up again after the first success.
struct Endpoint {
  port: Option<u16>,
  announced_probe: bool,
}

async fn run(api_key: Vec<u8>, notice_tx: mpsc::Sender<WsNotice>, mut req_rx: mpsc::Receiver<WsRequest>) {
  let crypto = Crypto::new(api_key);
  let mut endpoint = Endpoint {
    port: None,
    announced_probe: false,
  };

  // gpservice launches this daemon concurrently with binding its WS server,
  // so the lock file may not exist yet — wait for it instead of declaring
  // the service gone at birth.
  let deadline = tokio::time::Instant::now() + Duration::from_secs(STARTUP_GRACE_SECS);
  while !service_alive(&mut endpoint).await {
    if tokio::time::Instant::now() >= deadline {
      warn!("gpservice did not come up within {}s", STARTUP_GRACE_SECS);
      let _ = notice_tx.send(WsNotice::Gone).await;
      return;
    }

    tokio::time::sleep(Duration::from_millis(500)).await;
  }

  loop {
    match pump(&crypto, &notice_tx, &mut req_rx, &mut endpoint).await {
      Ok(()) => info!("WS connection closed by gpservice"),
      Err(err) => warn!("WS connection error: {}", err),
    }

    if notice_tx.send(WsNotice::Lost).await.is_err() {
      return;
    }

    let mut reconnected = false;
    for delay_ms in RECONNECT_DELAYS_MS {
      tokio::time::sleep(Duration::from_millis(delay_ms)).await;

      if !service_alive(&mut endpoint).await {
        info!("gpservice is gone, stopping reconnection attempts");
        break;
      }

      // Service is alive; the drop was transient. Next pump() will redial.
      reconnected = true;
      break;
    }

    if !reconnected {
      let _ = notice_tx.send(WsNotice::Gone).await;
      return;
    }
  }
}

/// One WS session: dial, then relay events out and requests in until the
/// connection dies.
async fn pump(
  crypto: &Crypto,
  notice_tx: &mpsc::Sender<WsNotice>,
  req_rx: &mut mpsc::Receiver<WsRequest>,
  endpoint: &mut Endpoint,
) -> anyhow::Result<()> {
  let port = discover_port(endpoint)
    .await
    .context("Failed to discover gpservice endpoint")?;
  let url = format!("ws://127.0.0.1:{port}/ws");
  let (stream, _) = connect_async(&url).await.context("Failed to connect to gpservice WS")?;

  info!("Connected to gpservice at {}", url);

  let (mut sink, mut source) = stream.split();

  loop {
    tokio::select! {
      msg = source.next() => {
        let Some(msg) = msg else {
          return Ok(());
        };

        match msg? {
          Message::Binary(payload) => {
            let event: WsEvent = crypto
              .decrypt(payload.to_vec())
              .context("Failed to decrypt WS event (api-key mismatch?)")?;

            if notice_tx.send(WsNotice::Event(event)).await.is_err() {
              return Ok(());
            }
          }
          // gpservice sends an initial Ping and waits for any reply frame
          // before registering the client; answer explicitly instead of
          // relying on split-stream auto-pong flushing.
          Message::Ping(payload) => sink.send(Message::Pong(payload)).await?,
          Message::Close(_) => return Ok(()),
          _ => {}
        }
      }

      req = req_rx.recv() => {
        let Some(req) = req else {
          return Ok(());
        };

        let payload = crypto.encrypt(&req).context("Failed to encrypt WS request")?;
        sink.send(Message::Binary(payload.into())).await?;
      }
    }
  }
}

async fn service_alive(endpoint: &mut Endpoint) -> bool {
  let Some(port) = discover_port(endpoint).await else {
    return false;
  };

  health_ok(port).await
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

async fn discover_port(endpoint: &mut Endpoint) -> Option<u16> {
  if let Some(port) = endpoint.port {
    return Some(port);
  }

  match classify_lock(gpservice_lock_info().await) {
    LockView::Port(port) => {
      endpoint.port = Some(port);
      Some(port)
    }
    LockView::Missing => None,
    LockView::Unreadable => {
      if !endpoint.announced_probe {
        info!("gpservice lock file is not readable by this user; scanning localhost listeners");
        endpoint.announced_probe = true;
      }

      let table = tokio::fs::read_to_string("/proc/net/tcp").await.ok()?;
      let ports = localhost_root_listeners(&table);
      let port = first_gpservice_port(&ports).await?;
      endpoint.port = Some(port);
      Some(port)
    }
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

/// gpservice's first frame is `Ping("Hi")`, sent before it reads. The probe
/// must not write the api-key, or any other frame, to a candidate.
async fn first_gpservice_port(ports: &[u16]) -> Option<u16> {
  let checks = futures_util::future::join_all(ports.iter().copied().map(looks_like_gpservice)).await;

  ports
    .iter()
    .copied()
    .zip(checks)
    .find(|(_, matched)| *matched)
    .map(|(port, _)| port)
}

async fn looks_like_gpservice(port: u16) -> bool {
  let url = format!("ws://127.0.0.1:{port}/ws");
  let timeout = Duration::from_millis(400);

  let Ok(Ok((mut stream, _))) = tokio::time::timeout(timeout, connect_async(&url)).await else {
    return false;
  };

  match tokio::time::timeout(timeout, stream.next()).await {
    Ok(Some(Ok(Message::Ping(payload)))) => payload.as_ref() == b"Hi",
    _ => false,
  }
}

#[cfg(test)]
mod tests {
  use super::*;

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
  async fn probe_picks_ping_hi_and_sends_no_frame() {
    use std::sync::{
      Arc,
      atomic::{AtomicBool, Ordering},
    };

    use tokio::net::TcpListener;

    let ping = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let decoy = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ping_port = ping.local_addr().unwrap().port();
    let decoy_port = decoy.local_addr().unwrap().port();
    let wrote = Arc::new(AtomicBool::new(false));
    let wrote_probe = Arc::clone(&wrote);

    tokio::spawn(async move {
      let (stream, _) = ping.accept().await.unwrap();
      let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
      ws.send(Message::Ping(b"Hi".to_vec().into())).await.unwrap();

      let next = tokio::time::timeout(Duration::from_millis(500), ws.next()).await;
      if let Ok(Some(Ok(Message::Binary(_)))) = next {
        wrote_probe.store(true, Ordering::SeqCst);
      }
    });

    tokio::spawn(async move {
      let (stream, _) = decoy.accept().await.unwrap();
      let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
      let _ = ws.send(Message::Text("nope".into())).await;
      let _ = tokio::time::timeout(Duration::from_millis(500), ws.next()).await;
    });

    let found = first_gpservice_port(&[decoy_port, ping_port]).await;

    assert_eq!(found, Some(ping_port));
    assert!(!wrote.load(Ordering::SeqCst));
  }
}
