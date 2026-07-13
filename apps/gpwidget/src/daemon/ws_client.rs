use std::time::Duration;

use anyhow::Context;
use futures_util::{SinkExt, StreamExt};
use gpapi::{
  service::{event::WsEvent, request::WsRequest},
  utils::{crypto::Crypto, endpoint::ws_endpoint, lock_file::gpservice_lock_info},
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

async fn run(api_key: Vec<u8>, notice_tx: mpsc::Sender<WsNotice>, mut req_rx: mpsc::Receiver<WsRequest>) {
  let crypto = Crypto::new(api_key);

  // gpservice launches this daemon concurrently with binding its WS server,
  // so the lock file may not exist (or be complete) yet — wait for it
  // instead of declaring the service gone at birth.
  let deadline = tokio::time::Instant::now() + Duration::from_secs(STARTUP_GRACE_SECS);
  while !service_alive().await {
    if tokio::time::Instant::now() >= deadline {
      warn!("gpservice did not come up within {}s", STARTUP_GRACE_SECS);
      let _ = notice_tx.send(WsNotice::Gone).await;
      return;
    }

    tokio::time::sleep(Duration::from_millis(500)).await;
  }

  loop {
    match pump(&crypto, &notice_tx, &mut req_rx).await {
      Ok(()) => info!("WS connection closed by gpservice"),
      Err(err) => warn!("WS connection error: {}", err),
    }

    if notice_tx.send(WsNotice::Lost).await.is_err() {
      return;
    }

    let mut reconnected = false;
    for delay_ms in RECONNECT_DELAYS_MS {
      tokio::time::sleep(Duration::from_millis(delay_ms)).await;

      if !service_alive().await {
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
) -> anyhow::Result<()> {
  let url = ws_endpoint().await.context("Failed to discover gpservice endpoint")?;
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

async fn service_alive() -> bool {
  let Ok(lock_info) = gpservice_lock_info().await else {
    return false;
  };

  let url = format!("http://127.0.0.1:{}/health", lock_info.port);
  let client = reqwest::Client::builder()
    .timeout(Duration::from_secs(2))
    .build()
    .expect("reqwest client");

  match client.get(&url).send().await {
    Ok(resp) => resp.status().is_success(),
    Err(_) => false,
  }
}
