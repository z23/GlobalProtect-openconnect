//! Tunnel interface discovery and traffic statistics.
//!
//! Nothing in the gpservice protocol names the tun device (the kernel
//! assigns it), so it is discovered by diffing the interface set captured
//! at Connecting time against the set once Connected, with a tun-name
//! fallback. Counters come from /sys/class/net.

use std::collections::HashSet;
use std::time::Duration;

use log::info;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::proto::ConnStats;

pub struct StatsTracker {
  baseline: HashSet<String>,
  task: Option<JoinHandle<()>>,
}

impl StatsTracker {
  pub fn new() -> Self {
    Self {
      baseline: HashSet::new(),
      task: None,
    }
  }

  /// Capture the pre-tunnel interface set.
  pub fn on_connecting(&mut self) {
    self.stop();
    self.baseline = interface_names();
  }

  /// Start the ticker; `since` is the tunnel establishment epoch.
  pub fn on_connected(&mut self, since: u64, interval: Duration, updates: mpsc::Sender<ConnStats>) {
    self.stop();

    let baseline = self.baseline.clone();
    self.task = Some(tokio::spawn(ticker(baseline, since, interval, updates)));
  }

  pub fn stop(&mut self) {
    if let Some(task) = self.task.take() {
      task.abort();
    }
  }
}

async fn ticker(baseline: HashSet<String>, since: u64, interval: Duration, updates: mpsc::Sender<ConnStats>) {
  let mut ifname: Option<String> = None;
  let mut ipv4: Option<String> = None;
  let mut prev: Option<(u64, u64, tokio::time::Instant)> = None;
  let mut ticks: u32 = 0;

  loop {
    tokio::time::sleep(interval).await;
    ticks += 1;

    // (Re-)discover if unknown or the device vanished (openconnect
    // reconnects can recreate it under a new name).
    let vanished = ifname
      .as_deref()
      .is_some_and(|name| !std::path::Path::new(&format!("/sys/class/net/{}", name)).exists());

    if ifname.is_none() || vanished {
      prev = None;
      (ifname, ipv4) = discover_tun(&baseline);

      if let Some(name) = &ifname {
        info!("Tunnel interface: {} ({})", name, ipv4.as_deref().unwrap_or("no IPv4"));
      }
    } else if ticks % 5 == 0 {
      // Periodic address refresh; cheap and catches renegotiation.
      if let Some(name) = &ifname {
        ipv4 = interface_ipv4(name);
      }
    }

    let mut stats = ConnStats {
      since,
      ifname: ifname.clone(),
      ipv4: ipv4.clone(),
      ..Default::default()
    };

    if let Some(name) = &ifname {
      if let (Some(rx), Some(tx)) = (read_counter(name, "rx_bytes"), read_counter(name, "tx_bytes")) {
        let now = tokio::time::Instant::now();

        if let Some((prev_rx, prev_tx, prev_at)) = prev {
          let elapsed = now.duration_since(prev_at).as_secs_f64().max(0.001);
          stats.rx_rate = ((rx.saturating_sub(prev_rx)) as f64 / elapsed) as u64;
          stats.tx_rate = ((tx.saturating_sub(prev_tx)) as f64 / elapsed) as u64;
        }

        stats.rx_bytes = rx;
        stats.tx_bytes = tx;
        prev = Some((rx, tx, now));
      }
    }

    if updates.send(stats).await.is_err() {
      return;
    }
  }
}

/// Returns (ifname, ipv4). Preference order: a new tun-like interface since
/// the baseline, then any up tun-named interface with an address.
fn discover_tun(baseline: &HashSet<String>) -> (Option<String>, Option<String>) {
  let interfaces = netdev::get_interfaces();

  let pick = interfaces
    .iter()
    .filter(|iface| is_tun_like(&iface.name))
    .filter(|iface| !iface.ipv4.is_empty())
    .find(|iface| !baseline.contains(&iface.name))
    .or_else(|| {
      interfaces
        .iter()
        .filter(|iface| is_tun_like(&iface.name))
        .find(|iface| !iface.ipv4.is_empty())
    });

  match pick {
    Some(iface) => (
      Some(iface.name.clone()),
      iface.ipv4.first().map(|net| net.addr().to_string()),
    ),
    None => (None, None),
  }
}

fn is_tun_like(name: &str) -> bool {
  name.starts_with("tun") || name.starts_with("gpd") || name.starts_with("vpn")
}

fn interface_names() -> HashSet<String> {
  netdev::get_interfaces().into_iter().map(|iface| iface.name).collect()
}

fn interface_ipv4(name: &str) -> Option<String> {
  netdev::get_interfaces()
    .into_iter()
    .find(|iface| iface.name == name)
    .and_then(|iface| iface.ipv4.first().map(|net| net.addr().to_string()))
}

fn read_counter(ifname: &str, counter: &str) -> Option<u64> {
  std::fs::read_to_string(format!("/sys/class/net/{}/statistics/{}", ifname, counter))
    .ok()?
    .trim()
    .parse()
    .ok()
}
