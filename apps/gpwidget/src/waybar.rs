//! Status snapshot → waybar custom-module JSON line translation.
//!
//! Runs in waybar's continuous mode (no `interval`): each stdout line is one
//! update. Socket absence renders as the synthesized `stack-down` state and
//! the socket is re-tried every 2 seconds.

use std::io::Write;
use std::time::Duration;

use serde::Serialize;

use crate::{
  client::SocketClient,
  config::Config,
  proto::{ServerMsg, StatusMsg, WidgetState},
  ux,
};

#[derive(Debug, Serialize, PartialEq)]
struct WaybarLine {
  text: String,
  alt: String,
  tooltip: String,
  class: Vec<String>,
  percentage: u8,
}

pub async fn run(_follow: bool) -> anyhow::Result<()> {
  let config = Config::load().unwrap_or_default();
  let mut last_line: Option<String> = None;

  loop {
    match SocketClient::try_open().await {
      Ok(Some(mut client)) => {
        let mut status: Option<StatusMsg> = None;
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
          tokio::select! {
            msg = client.next_msg() => {
              match msg {
                Ok(Some(ServerMsg::Status(new_status))) => {
                  status = Some(new_status);
                  emit(&mut last_line, status.as_ref(), &config);
                }
                Ok(Some(_)) => {}
                Ok(None) | Err(_) => break,
              }
            }

            // Local uptime tick: refresh the tooltip while connected.
            _ = tick.tick() => {
              if status.as_ref().is_some_and(|s| s.state == Some(WidgetState::Connected)) {
                emit(&mut last_line, status.as_ref(), &config);
              }
            }
          }
        }
      }
      Ok(None) | Err(_) => {
        emit(&mut last_line, None, &config);
        tokio::time::sleep(Duration::from_secs(2)).await;
      }
    }
  }
}

fn emit(last_line: &mut Option<String>, status: Option<&StatusMsg>, config: &Config) {
  let line = build_line(status, config.waybar.show_gateway, ux::unix_now());

  let Ok(json) = serde_json::to_string(&line) else {
    return;
  };

  if last_line.as_deref() == Some(json.as_str()) {
    return;
  }

  let mut stdout = std::io::stdout().lock();
  // Waybar reads a line per update; a failed write means waybar is gone.
  if writeln!(stdout, "{}", json).and_then(|_| stdout.flush()).is_err() {
    std::process::exit(0);
  }

  *last_line = Some(json);
}

fn build_line(status: Option<&StatusMsg>, show_gateway: bool, now: u64) -> WaybarLine {
  let state = status.and_then(|s| s.state);
  let style = ux::state_style(state);

  let mut classes = vec!["vpn".to_string(), style.class.to_string()];
  let mut text = style.glyph.to_string();
  let mut tooltip_lines = vec![format!("GlobalProtect — {}", style.label)];
  let mut percentage = 0u8;

  match state {
    None => {
      tooltip_lines.push("VPN service not running".to_string());
      tooltip_lines.push("Left-click: details · Right-click: connect".to_string());
    }

    Some(WidgetState::NeedsSetup) => {
      tooltip_lines.push(format!("Set the portal in {}", Config::path().display()));
    }

    Some(WidgetState::Disconnected) => {
      if let Some(portal) = status.and_then(|s| s.portal.as_deref()) {
        tooltip_lines.push(format!("Portal: {}", portal));
      }
    }

    Some(WidgetState::Authenticating) => {
      tooltip_lines.push("Complete the login in the browser window".to_string());
    }

    Some(WidgetState::Connecting) | Some(WidgetState::Disconnecting) => {
      if let Some(gateway) = status.and_then(|s| s.gateway.as_ref()) {
        tooltip_lines.push(format!("Gateway: {}", gateway.name));
      }
    }

    Some(WidgetState::Connected) => {
      let status = status.expect("state implies status");

      if let Some(gateway) = &status.gateway {
        if show_gateway && !gateway.name.is_empty() {
          text = format!("{} {}", style.glyph, gateway.name);
        }
        tooltip_lines.push(format!("Gateway: {} ({})", gateway.name, gateway.address));
      }

      if let Some(conn) = &status.conn {
        if let Some(ipv4) = &conn.ipv4 {
          tooltip_lines.push(format!(
            "IP: {} ({})",
            ipv4,
            conn.ifname.as_deref().unwrap_or("unknown")
          ));
        }

        tooltip_lines.push(format!("Uptime: {}", ux::format_duration_short(now.saturating_sub(conn.since))));

        if conn.rx_bytes > 0 || conn.tx_bytes > 0 {
          tooltip_lines.push(format!(
            "Traffic: ↓ {} · ↑ {}",
            ux::format_bytes(conn.rx_bytes),
            ux::format_bytes(conn.tx_bytes)
          ));
        }
      }

      if let Some(session) = &status.session {
        if let Some(expires_at) = session.expires_at {
          tooltip_lines.push(format!("Session: {}", ux::format_expiry(expires_at, now)));

          let warn_at = u64::from(session.warn_prior_secs.unwrap_or(1800));
          if expires_at.saturating_sub(now) < warn_at {
            classes.push("expiring".to_string());
          }
        }

        percentage = ux::session_percentage(session.expires_at, session.lifetime_secs, now);
      }
    }

    Some(WidgetState::Error) => {
      if let Some(error) = status.and_then(|s| s.error.as_deref()) {
        tooltip_lines.push(error.to_string());
      }
    }
  }

  WaybarLine {
    text,
    alt: state.map(|s| s.as_str().to_string()).unwrap_or_else(|| "stack-down".to_string()),
    tooltip: tooltip_lines.join("\n"),
    class: classes,
    percentage,
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::proto::{ConnStats, GatewayInfo, SessionSummary};

  fn connected_status() -> StatusMsg {
    StatusMsg {
      state: Some(WidgetState::Connected),
      portal: Some("portal.example.com".into()),
      gateway: Some(GatewayInfo {
        name: "AU-Perth".into(),
        address: "gw.example.com".into(),
      }),
      gateways: vec![],
      conn: Some(ConnStats {
        since: 1_000,
        ifname: Some("tun0".into()),
        ipv4: Some("10.8.1.23".into()),
        rx_bytes: 1_320_702_444,
        tx_bytes: 91_750_400,
        rx_rate: 0,
        tx_rate: 0,
      }),
      session: Some(SessionSummary {
        expires_at: Some(44_200),
        expires_in_human: None,
        lifetime_secs: Some(43_200),
        warn_prior_secs: Some(1_800),
        allow_extend: true,
      }),
      error: None,
    }
  }

  #[test]
  fn stack_down_line() {
    let line = build_line(None, true, 0);

    assert_eq!(line.alt, "stack-down");
    assert_eq!(line.class, vec!["vpn", "stack-down"]);
    assert_eq!(line.percentage, 0);
    assert!(line.tooltip.contains("VPN service not running"));
  }

  #[test]
  fn connected_line_carries_gateway_stats_and_percentage() {
    let now = 12_280u64; // uptime 3h 8m, session remaining 31920s of 43200 = 73%
    let line = build_line(Some(&connected_status()), true, now);

    assert_eq!(line.alt, "connected");
    assert_eq!(line.text, "\u{f099d} AU-Perth");
    assert_eq!(line.class, vec!["vpn", "connected"]);
    assert_eq!(line.percentage, 73);
    assert!(line.tooltip.contains("Gateway: AU-Perth (gw.example.com)"));
    assert!(line.tooltip.contains("IP: 10.8.1.23 (tun0)"));
    assert!(line.tooltip.contains("Uptime: 3h 8m"));
    assert!(line.tooltip.contains("Traffic: ↓ 1.23 GiB · ↑ 87.5 MiB"));
  }

  #[test]
  fn expiring_session_adds_class() {
    let status = connected_status();
    let now = 43_000u64; // 1200s remaining < 1800 warn threshold
    let line = build_line(Some(&status), true, now);

    assert!(line.class.contains(&"expiring".to_string()));
  }

  #[test]
  fn gateway_name_is_omitted_when_configured_off() {
    let line = build_line(Some(&connected_status()), false, 2_000);

    assert_eq!(line.text, "\u{f099d}");
  }

  #[test]
  fn error_line_shows_message_in_tooltip() {
    let status = StatusMsg {
      state: Some(WidgetState::Error),
      error: Some("portal unreachable".into()),
      ..Default::default()
    };

    let line = build_line(Some(&status), true, 0);

    assert_eq!(line.class, vec!["vpn", "error"]);
    assert!(line.tooltip.contains("portal unreachable"));
  }
}
