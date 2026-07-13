//! Socket-client side used by the CLI subcommands (and the popup).

use anyhow::{Context, bail};
use tokio::{
  io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines},
  net::{
    UnixStream,
    unix::{OwnedReadHalf, OwnedWriteHalf},
  },
};

use crate::proto::{self, ClientMsg, ServerMsg, StatusMsg};

pub struct SocketClient {
  lines: Lines<BufReader<OwnedReadHalf>>,
  writer: OwnedWriteHalf,
}

impl SocketClient {
  /// None when the daemon socket doesn't exist or nothing is listening —
  /// the "stack down" state clients render themselves.
  pub async fn try_open() -> anyhow::Result<Option<Self>> {
    let path = proto::socket_path();

    match UnixStream::connect(&path).await {
      Ok(stream) => {
        let (read_half, writer) = stream.into_split();

        Ok(Some(Self {
          lines: BufReader::new(read_half).lines(),
          writer,
        }))
      }
      Err(err)
        if matches!(
          err.kind(),
          std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
        ) =>
      {
        Ok(None)
      }
      Err(err) => Err(err).context(format!("Failed to connect to {}", path.display())),
    }
  }

  pub async fn send(&mut self, msg: &ClientMsg) -> anyhow::Result<()> {
    let line = serde_json::to_string(msg)?;
    self.writer.write_all(line.as_bytes()).await?;
    self.writer.write_all(b"\n").await?;

    Ok(())
  }

  pub async fn next_msg(&mut self) -> anyhow::Result<Option<ServerMsg>> {
    loop {
      let Some(line) = self.lines.next_line().await? else {
        return Ok(None);
      };

      let trimmed = line.trim();
      if trimmed.is_empty() {
        continue;
      }

      return Ok(Some(serde_json::from_str(trimmed)?));
    }
  }

  /// Wait for the ack matching `id`, surfacing daemon-reported errors.
  pub async fn wait_ack(&mut self, id: u64) -> anyhow::Result<()> {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);

    loop {
      let msg = tokio::time::timeout_at(deadline, self.next_msg())
        .await
        .context("Timed out waiting for the daemon's reply")??;

      match msg {
        Some(ServerMsg::Ack { id: ack_id, ok, error }) if ack_id == id => {
          if ok {
            return Ok(());
          }

          bail!(error.unwrap_or_else(|| "daemon reported failure".to_string()));
        }
        Some(_) => continue,
        None => bail!("Daemon closed the connection before replying"),
      }
    }
  }

  /// Read messages until the first status snapshot arrives.
  pub async fn next_status(&mut self) -> anyhow::Result<StatusMsg> {
    loop {
      match self.next_msg().await? {
        Some(ServerMsg::Status(status)) => return Ok(status),
        Some(_) => continue,
        None => bail!("Daemon closed the connection"),
      }
    }
  }
}

/// A status snapshot representing "the stack is down" — what clients render
/// when there is no daemon socket. `state` is None on the wire; consumers map
/// it to their own stack-down presentation.
pub fn stack_down_status() -> StatusMsg {
  StatusMsg::default()
}

pub async fn status(waybar: bool, follow: bool) -> anyhow::Result<()> {
  if waybar {
    return crate::waybar::run(follow).await;
  }

  let Some(mut client) = SocketClient::try_open().await? else {
    if follow {
      bail!("The VPN service stack is not running");
    }

    let mut value = serde_json::to_value(stack_down_status())?;
    value["state"] = "stack-down".into();
    println!("{}", value);
    return Ok(());
  };

  if follow {
    while let Some(msg) = client.next_msg().await? {
      if let ServerMsg::Status(status) = msg {
        println!("{}", serde_json::to_string(&status)?);
      }
    }

    return Ok(());
  }

  let status = client.next_status().await?;
  println!("{}", serde_json::to_string(&status)?);

  Ok(())
}

pub async fn connect(gateway: Option<String>, portal: Option<String>) -> anyhow::Result<()> {
  let mut client = match SocketClient::try_open().await? {
    Some(client) => client,
    None => crate::launch::start_stack().await?,
  };

  client
    .send(&ClientMsg::Connect {
      id: Some(1),
      portal,
      gateway,
    })
    .await?;
  client.wait_ack(1).await?;

  println!("Connect initiated");

  Ok(())
}

pub async fn disconnect() -> anyhow::Result<()> {
  let Some(mut client) = SocketClient::try_open().await? else {
    println!("The VPN service stack is not running; nothing to disconnect");
    return Ok(());
  };

  client.send(&ClientMsg::Disconnect { id: Some(1) }).await?;
  client.wait_ack(1).await?;

  println!("Disconnect initiated");

  Ok(())
}

pub async fn toggle() -> anyhow::Result<()> {
  let mut client = match SocketClient::try_open().await? {
    Some(client) => client,
    None => crate::launch::start_stack().await?,
  };

  client.send(&ClientMsg::Toggle { id: Some(1) }).await?;
  client.wait_ack(1).await?;

  Ok(())
}

pub async fn quit() -> anyhow::Result<()> {
  let Some(mut client) = SocketClient::try_open().await? else {
    println!("The VPN service stack is not running");
    return Ok(());
  };

  client.send(&ClientMsg::Quit { id: Some(1) }).await?;
  client.wait_ack(1).await?;

  println!("VPN service stack shutting down");

  Ok(())
}
