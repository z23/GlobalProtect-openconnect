//! The NDJSON protocol spoken over `$XDG_RUNTIME_DIR/gpwidget.sock`.
//!
//! This module is the single source of truth for the wire format consumed by
//! the waybar module, the GTK popup and the DMS plugin (which mirrors these
//! shapes in QML/JS). One JSON object per line, both directions.
//!
//! Security: status only — cookies, the api-key and SAML data never cross
//! this socket.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u32 = 1;

pub fn socket_path() -> PathBuf {
  let runtime_dir = std::env::var_os("XDG_RUNTIME_DIR")
    .map(PathBuf::from)
    .unwrap_or_else(|| PathBuf::from(format!("/run/user/{}", uzers::get_current_uid())));

  runtime_dir.join("gpwidget.sock")
}

/// Widget-level state; `stack-down` is deliberately absent — clients
/// synthesize it from socket absence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WidgetState {
  NeedsSetup,
  Disconnected,
  Authenticating,
  Connecting,
  Connected,
  Disconnecting,
  Error,
}

impl WidgetState {
  pub fn as_str(&self) -> &'static str {
    match self {
      WidgetState::NeedsSetup => "needs-setup",
      WidgetState::Disconnected => "disconnected",
      WidgetState::Authenticating => "authenticating",
      WidgetState::Connecting => "connecting",
      WidgetState::Connected => "connected",
      WidgetState::Disconnecting => "disconnecting",
      WidgetState::Error => "error",
    }
  }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GatewayInfo {
  pub name: String,
  pub address: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnStats {
  /// Unix epoch seconds when the tunnel came up; clients tick uptime locally.
  pub since: u64,
  pub ifname: Option<String>,
  pub ipv4: Option<String>,
  pub rx_bytes: u64,
  pub tx_bytes: u64,
  /// Bytes per second over the last stats interval.
  pub rx_rate: u64,
  pub tx_rate: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSummary {
  /// Unix epoch seconds when the session expires.
  pub expires_at: Option<u64>,
  pub expires_in_human: Option<String>,
  /// Total session lifetime, for remaining-percentage displays.
  pub lifetime_secs: Option<u32>,
  pub warn_prior_secs: Option<u32>,
  pub allow_extend: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusMsg {
  pub state: Option<WidgetState>,
  pub portal: Option<String>,
  pub gateway: Option<GatewayInfo>,
  #[serde(default)]
  pub gateways: Vec<GatewayInfo>,
  pub conn: Option<ConnStats>,
  pub session: Option<SessionSummary>,
  pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ToastLevel {
  Info,
  Warn,
  Error,
}

/// Daemon → client messages.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum ServerMsg {
  Hello {
    version: String,
    protocol: u32,
  },
  Status(StatusMsg),
  Toast {
    level: ToastLevel,
    title: String,
    message: String,
  },
  AuthPrompt {
    kind: String,
    message: String,
  },
  Ack {
    id: u64,
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
  },
  Bye,
}

/// Client → daemon commands.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum ClientMsg {
  GetStatus {
    id: Option<u64>,
  },
  Connect {
    id: Option<u64>,
    portal: Option<String>,
    gateway: Option<String>,
  },
  Disconnect {
    id: Option<u64>,
  },
  Toggle {
    id: Option<u64>,
  },
  SubmitOtp {
    id: Option<u64>,
    otp: String,
  },
  OpenPopup {
    id: Option<u64>,
  },
  Quit {
    id: Option<u64>,
  },
}

impl ClientMsg {
  pub fn id(&self) -> Option<u64> {
    match self {
      ClientMsg::GetStatus { id }
      | ClientMsg::Connect { id, .. }
      | ClientMsg::Disconnect { id }
      | ClientMsg::Toggle { id }
      | ClientMsg::SubmitOtp { id, .. }
      | ClientMsg::OpenPopup { id }
      | ClientMsg::Quit { id } => *id,
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn server_msg_wire_format_is_stable() {
    let msg = ServerMsg::Status(StatusMsg {
      state: Some(WidgetState::Connected),
      portal: Some("portal.example.com".into()),
      gateway: Some(GatewayInfo {
        name: "GW".into(),
        address: "gw.example.com".into(),
      }),
      gateways: vec![],
      conn: Some(ConnStats {
        since: 100,
        ifname: Some("tun0".into()),
        ipv4: Some("10.0.0.2".into()),
        rx_bytes: 1,
        tx_bytes: 2,
        rx_rate: 3,
        tx_rate: 4,
      }),
      session: Some(SessionSummary {
        expires_at: Some(200),
        expires_in_human: Some("12h".into()),
        lifetime_secs: Some(43_200),
        warn_prior_secs: Some(1800),
        allow_extend: true,
      }),
      error: None,
    });

    let value = serde_json::to_value(&msg).unwrap();

    assert_eq!(value["type"], "status");
    assert_eq!(value["state"], "connected");
    assert_eq!(value["conn"]["rxBytes"], 1);
    assert_eq!(value["session"]["expiresAt"], 200);
    assert_eq!(value["gateway"]["name"], "GW");
  }

  #[test]
  fn client_msg_roundtrip() {
    let line = r#"{"type":"connect","id":7,"gateway":"GW"}"#;
    let msg: ClientMsg = serde_json::from_str(line).unwrap();

    match &msg {
      ClientMsg::Connect { id, gateway, portal } => {
        assert_eq!(*id, Some(7));
        assert_eq!(gateway.as_deref(), Some("GW"));
        assert!(portal.is_none());
      }
      other => panic!("unexpected message: {other:?}"),
    }

    assert_eq!(msg.id(), Some(7));
  }

  #[test]
  fn auth_prompt_uses_kebab_tag() {
    let msg = ServerMsg::AuthPrompt {
      kind: "otp".into(),
      message: "Enter OTP".into(),
    };

    assert_eq!(serde_json::to_value(&msg).unwrap()["type"], "auth-prompt");
  }
}
