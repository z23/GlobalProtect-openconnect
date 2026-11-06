//! Bringing the VPN service stack up from a dead start:
//! spawn `gpclient launch-gui` and wait for the daemon socket to appear.

use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, bail};
use log::info;

use crate::client::SocketClient;

const STARTUP_TIMEOUT_SECS: u64 = 30;

/// Launch the service stack (gpclient launch-gui → pkexec gpservice →
/// gpwidget daemon) and wait for the daemon socket.
pub async fn start_stack() -> anyhow::Result<SocketClient> {
  let gpclient = common::binary_paths::gpclient();

  info!("Starting VPN service stack via {}", gpclient.display());

  tokio::process::Command::new(&gpclient)
    .arg("launch-gui")
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::null())
    .process_group(0)
    .spawn()
    .context(format!("Failed to spawn {} launch-gui", gpclient.display()))?;

  let deadline = tokio::time::Instant::now() + Duration::from_secs(STARTUP_TIMEOUT_SECS);

  loop {
    tokio::time::sleep(Duration::from_millis(500)).await;

    if let Some(client) = SocketClient::try_open().await? {
      return Ok(client);
    }

    if tokio::time::Instant::now() >= deadline {
      bail!(
        "VPN service did not come up within {}s — polkit authorization may have been denied",
        STARTUP_TIMEOUT_SECS
      );
    }
  }
}
