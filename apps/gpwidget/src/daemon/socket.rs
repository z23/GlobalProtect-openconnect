use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, bail};
use log::{info, warn};
use tokio::{
  io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
  net::{UnixListener, UnixStream},
  sync::{broadcast, mpsc, watch},
};

use crate::proto::ClientMsg;

/// A parsed client command paired with a way to answer just that client.
pub struct Command {
  pub msg: ClientMsg,
  pub reply: mpsc::Sender<Arc<str>>,
}

pub struct SocketServer {
  pub commands: mpsc::Receiver<Command>,
  path: PathBuf,
}

impl SocketServer {
  /// Bind the daemon socket, replacing a stale one; refuse to displace a
  /// live daemon.
  pub async fn bind(
    path: &Path,
    hello_line: Arc<str>,
    broadcast_tx: broadcast::Sender<Arc<str>>,
    snapshot_rx: watch::Receiver<Arc<str>>,
  ) -> anyhow::Result<Self> {
    if path.exists() {
      match UnixStream::connect(path).await {
        Ok(_) => bail!("Another gpwidget daemon is already serving {}", path.display()),
        Err(_) => {
          info!("Removing stale socket {}", path.display());
          std::fs::remove_file(path)?;
        }
      }
    }

    if let Some(dir) = path.parent() {
      std::fs::create_dir_all(dir)?;
    }

    let listener = UnixListener::bind(path).context(format!("Failed to bind {}", path.display()))?;
    std::fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(0o600))?;

    info!("Listening on {}", path.display());

    let (cmd_tx, cmd_rx) = mpsc::channel(32);

    tokio::spawn(accept_loop(listener, hello_line, broadcast_tx, snapshot_rx, cmd_tx));

    Ok(Self {
      commands: cmd_rx,
      path: path.to_path_buf(),
    })
  }

  pub fn cleanup(&self) {
    if let Err(err) = std::fs::remove_file(&self.path) {
      if err.kind() != std::io::ErrorKind::NotFound {
        warn!("Failed to remove socket: {}", err);
      }
    }
  }
}

async fn accept_loop(
  listener: UnixListener,
  hello_line: Arc<str>,
  broadcast_tx: broadcast::Sender<Arc<str>>,
  snapshot_rx: watch::Receiver<Arc<str>>,
  cmd_tx: mpsc::Sender<Command>,
) {
  loop {
    match listener.accept().await {
      Ok((stream, _)) => {
        tokio::spawn(serve_client(
          stream,
          hello_line.clone(),
          broadcast_tx.subscribe(),
          snapshot_rx.clone(),
          cmd_tx.clone(),
        ));
      }
      Err(err) => {
        warn!("Socket accept failed: {}", err);
        return;
      }
    }
  }
}

async fn serve_client(
  stream: UnixStream,
  hello_line: Arc<str>,
  mut broadcast_rx: broadcast::Receiver<Arc<str>>,
  snapshot_rx: watch::Receiver<Arc<str>>,
  cmd_tx: mpsc::Sender<Command>,
) {
  let (read_half, mut write_half) = stream.into_split();
  let mut lines = BufReader::new(read_half).lines();

  // Private queue for acks and direct replies to this client.
  let (reply_tx, mut reply_rx) = mpsc::channel::<Arc<str>>(16);

  let latest_snapshot = snapshot_rx.borrow().clone();
  let greeting = format!("{}\n{}\n", hello_line, latest_snapshot);
  if write_half.write_all(greeting.as_bytes()).await.is_err() {
    return;
  }

  loop {
    tokio::select! {
      line = broadcast_rx.recv() => {
        let line = match line {
          Ok(line) => line,
          Err(broadcast::error::RecvError::Lagged(_)) => {
            // Slow reader (e.g. a busy QML client): resync with the
            // latest snapshot instead of replaying the backlog.
            snapshot_rx.borrow().clone()
          }
          Err(broadcast::error::RecvError::Closed) => return,
        };

        if write_line(&mut write_half, &line).await.is_err() {
          return;
        }
      }

      reply = reply_rx.recv() => {
        let Some(line) = reply else { return };

        if write_line(&mut write_half, &line).await.is_err() {
          return;
        }
      }

      line = lines.next_line() => {
        let line = match line {
          Ok(Some(line)) => line,
          Ok(None) | Err(_) => return,
        };

        let trimmed = line.trim();
        if trimmed.is_empty() {
          continue;
        }

        match serde_json::from_str::<ClientMsg>(trimmed) {
          Ok(msg) => {
            let command = Command { msg, reply: reply_tx.clone() };
            if cmd_tx.send(command).await.is_err() {
              return;
            }
          }
          Err(err) => {
            warn!("Ignoring malformed client message: {}", err);
          }
        }
      }
    }
  }
}

async fn write_line(write_half: &mut tokio::net::unix::OwnedWriteHalf, line: &str) -> std::io::Result<()> {
  write_half.write_all(line.as_bytes()).await?;
  write_half.write_all(b"\n").await
}

#[cfg(test)]
mod tests {
  use super::*;

  async fn start_server(dir: &Path) -> (SocketServer, broadcast::Sender<Arc<str>>, watch::Sender<Arc<str>>) {
    let (broadcast_tx, _) = broadcast::channel::<Arc<str>>(8);
    let (snapshot_tx, snapshot_rx) = watch::channel::<Arc<str>>(r#"{"type":"status"}"#.into());

    let server = SocketServer::bind(
      &dir.join("test.sock"),
      r#"{"type":"hello","version":"test","protocol":1}"#.into(),
      broadcast_tx.clone(),
      snapshot_rx,
    )
    .await
    .unwrap();

    (server, broadcast_tx, snapshot_tx)
  }

  async fn connect(dir: &Path) -> (tokio::io::Lines<BufReader<tokio::net::unix::OwnedReadHalf>>, tokio::net::unix::OwnedWriteHalf) {
    let stream = UnixStream::connect(dir.join("test.sock")).await.unwrap();
    let (read_half, write_half) = stream.into_split();

    (BufReader::new(read_half).lines(), write_half)
  }

  #[tokio::test]
  async fn greets_with_hello_and_snapshot_then_routes_commands() {
    let dir = tempfile::tempdir().unwrap();
    let (mut server, _broadcast_tx, _snapshot_tx) = start_server(dir.path()).await;

    let (mut lines, mut write_half) = connect(dir.path()).await;

    assert!(lines.next_line().await.unwrap().unwrap().contains("hello"));
    assert!(lines.next_line().await.unwrap().unwrap().contains("status"));

    write_half.write_all(b"{\"type\":\"get-status\",\"id\":3}\n").await.unwrap();

    let command = server.commands.recv().await.unwrap();
    assert!(matches!(command.msg, ClientMsg::GetStatus { id: Some(3) }));

    command.reply.send(r#"{"type":"ack","id":3,"ok":true}"#.into()).await.unwrap();
    assert!(lines.next_line().await.unwrap().unwrap().contains("ack"));
  }

  #[tokio::test]
  async fn broadcast_reaches_all_clients() {
    let dir = tempfile::tempdir().unwrap();
    let (_server, broadcast_tx, _snapshot_tx) = start_server(dir.path()).await;

    let (mut lines_a, _wa) = connect(dir.path()).await;
    let (mut lines_b, _wb) = connect(dir.path()).await;

    for lines in [&mut lines_a, &mut lines_b] {
      lines.next_line().await.unwrap();
      lines.next_line().await.unwrap();
    }

    broadcast_tx.send(r#"{"type":"status","state":"connected"}"#.into()).unwrap();

    assert!(lines_a.next_line().await.unwrap().unwrap().contains("connected"));
    assert!(lines_b.next_line().await.unwrap().unwrap().contains("connected"));
  }

  #[tokio::test]
  async fn replaces_stale_socket_but_refuses_live_one() {
    let dir = tempfile::tempdir().unwrap();

    // Stale file: nothing listening.
    std::fs::write(dir.path().join("test.sock"), b"").unwrap();
    let (_server, broadcast_tx, snapshot_tx) = start_server(dir.path()).await;

    // A second bind must refuse while the first is alive.
    let (second_broadcast, _) = broadcast::channel::<Arc<str>>(8);
    let (_second_snapshot_tx, second_snapshot_rx) = watch::channel::<Arc<str>>("{}".into());
    let result = SocketServer::bind(
      &dir.path().join("test.sock"),
      "{}".into(),
      second_broadcast,
      second_snapshot_rx,
    )
    .await;

    assert!(result.is_err());
    drop((broadcast_tx, snapshot_tx));
  }
}
