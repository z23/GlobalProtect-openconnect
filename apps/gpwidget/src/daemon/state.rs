use gpapi::{
  gateway::Gateway,
  service::{vpn_env::VpnEnv, vpn_state::VpnState},
  session::SessionInfo,
};

use crate::{
  config::Config,
  proto::{ConnStats, GatewayInfo, SessionSummary, StatusMsg, WidgetState},
  ux::unix_now,
};

/// The daemon's view of the world, from which every socket snapshot is built.
pub struct Model {
  pub config: Config,
  pub vpn_env: Option<VpnEnv>,
  pub vpn_state: VpnState,
  /// Set while the connect flow is running gpauth / gateway login, i.e.
  /// before gpservice has been told anything.
  pub authenticating: bool,
  pub last_error: Option<String>,
  /// Gateways from the most recent connect; kept after disconnect so
  /// pickers have data before the next connect.
  pub known_gateways: Vec<GatewayInfo>,
  pub current_gateway: Option<GatewayInfo>,
  pub portal: Option<String>,
  pub session: Option<SessionSummary>,
  pub conn: Option<ConnStats>,
}

impl Model {
  pub fn new(config: Config) -> Self {
    let portal = config.portal.clone();

    Self {
      config,
      vpn_env: None,
      vpn_state: VpnState::Disconnected,
      authenticating: false,
      last_error: None,
      known_gateways: Vec::new(),
      current_gateway: None,
      portal,
      session: None,
      conn: None,
    }
  }

  pub fn widget_state(&self) -> WidgetState {
    match &self.vpn_state {
      VpnState::Connecting(_) => WidgetState::Connecting,
      VpnState::Connected(_) => WidgetState::Connected,
      VpnState::Disconnecting => WidgetState::Disconnecting,
      VpnState::Disconnected => {
        if self.authenticating {
          WidgetState::Authenticating
        } else if self.last_error.is_some() {
          WidgetState::Error
        } else if self.portal.is_none() {
          WidgetState::NeedsSetup
        } else {
          WidgetState::Disconnected
        }
      }
    }
  }

  pub fn apply_vpn_state(&mut self, vpn_state: VpnState) {
    // `portal` and `gateways` are private on upstream `ConnectInfo`. The
    // serialized state carries them; `session_info()` is already public.
    let wire = serde_json::to_value(&vpn_state).ok();

    match &vpn_state {
      VpnState::Connecting(_) => {
        if let Some(view) = wire
          .as_ref()
          .and_then(|value| value.get("connecting"))
          .and_then(connect_view)
        {
          self.portal = Some(view.portal);
          self.current_gateway = Some(gateway_info(&view.gateway));
          self.known_gateways = view.gateways.iter().map(gateway_info).collect();
        }
        self.session = None;
      }
      VpnState::Connected(connected) => {
        if let Some(view) = wire
          .as_ref()
          .and_then(|value| value.get("connected"))
          .and_then(|value| value.get("info"))
          .and_then(connect_view)
        {
          self.portal = Some(view.portal);
          self.current_gateway = Some(gateway_info(&view.gateway));
          self.known_gateways = view.gateways.iter().map(gateway_info).collect();
        }
        self.session = connected.session_info().map(session_summary);

        // Preserve the original connect time across repeated Connected
        // events (e.g. session extension re-broadcasts).
        if self.conn.is_none() {
          self.conn = Some(ConnStats {
            since: unix_now(),
            ..Default::default()
          });
        }
      }
      VpnState::Disconnected => {
        self.conn = None;
        self.session = None;
        self.current_gateway = None;
      }
      VpnState::Disconnecting => {}
    }

    self.vpn_state = vpn_state;
  }

  pub fn snapshot(&self) -> StatusMsg {
    let state = self.widget_state();

    StatusMsg {
      state: Some(state),
      portal: self.portal.clone(),
      gateway: self.current_gateway.clone(),
      gateways: self.known_gateways.clone(),
      conn: match state {
        WidgetState::Connected => self.conn.clone(),
        _ => None,
      },
      session: self.session.clone(),
      error: self.last_error.clone(),
    }
  }
}

struct ConnectView {
  portal: String,
  gateway: Gateway,
  gateways: Vec<Gateway>,
}

fn connect_view(value: &serde_json::Value) -> Option<ConnectView> {
  let portal = value.get("portal")?.as_str()?.to_string();
  let gateway = serde_json::from_value(value.get("gateway")?.clone()).ok()?;
  let gateways = serde_json::from_value(value.get("gateways")?.clone()).ok()?;

  Some(ConnectView {
    portal,
    gateway,
    gateways,
  })
}

fn gateway_info(gateway: &Gateway) -> GatewayInfo {
  GatewayInfo {
    name: gateway.name().to_string(),
    address: gateway.server().to_string(),
  }
}

fn session_summary(session: &SessionInfo) -> SessionSummary {
  SessionSummary {
    expires_at: session.user_expires.map(u64::from),
    expires_in_human: session.expires_in_human.clone(),
    lifetime_secs: session.lifetime_secs,
    warn_prior_secs: session.lifetime_warning.as_ref().map(|warning| warning.prior_secs),
    allow_extend: session.allow_extend_session,
  }
}

#[cfg(test)]
mod tests {
  use gpapi::service::vpn_state::{ConnectInfo, ConnectedInfo};

  use super::*;

  fn test_gateway() -> Gateway {
    Gateway::new("GW".to_string(), "gw.example.com".to_string())
  }

  fn connect_info() -> ConnectInfo {
    ConnectInfo::new("portal.example.com".to_string(), test_gateway(), vec![test_gateway()])
  }

  #[test]
  fn no_portal_means_needs_setup() {
    let model = Model::new(Config::default());
    assert_eq!(model.widget_state(), WidgetState::NeedsSetup);
  }

  #[test]
  fn connected_state_builds_full_snapshot() {
    let mut model = Model::new(Config::default());

    let session = SessionInfo {
      user_expires: Some(1_000_000),
      allow_extend_session: true,
      ..Default::default()
    };
    model.apply_vpn_state(VpnState::Connected(Box::new(ConnectedInfo::new(
      connect_info(),
      Some(session),
    ))));

    let snapshot = model.snapshot();

    assert_eq!(snapshot.state, Some(WidgetState::Connected));
    assert_eq!(snapshot.portal.as_deref(), Some("portal.example.com"));
    assert_eq!(snapshot.gateway.as_ref().unwrap().name, "GW");
    assert_eq!(snapshot.gateways.len(), 1);
    assert_eq!(snapshot.session.as_ref().unwrap().expires_at, Some(1_000_000));
    assert!(snapshot.conn.is_some());
  }

  #[test]
  fn disconnect_clears_connection_but_keeps_gateways() {
    let mut model = Model::new(Config::default());

    model.apply_vpn_state(VpnState::Connected(Box::new(ConnectedInfo::new(connect_info(), None))));
    model.apply_vpn_state(VpnState::Disconnected);

    let snapshot = model.snapshot();

    assert_eq!(snapshot.state, Some(WidgetState::Disconnected));
    assert!(snapshot.conn.is_none());
    assert!(snapshot.gateway.is_none());
    assert_eq!(snapshot.gateways.len(), 1, "picker data survives disconnect");
  }

  #[test]
  fn error_is_sticky_until_cleared() {
    let mut model = Model::new(Config::default());
    model.portal = Some("portal.example.com".to_string());
    model.last_error = Some("boom".to_string());

    assert_eq!(model.widget_state(), WidgetState::Error);

    model.last_error = None;
    assert_eq!(model.widget_state(), WidgetState::Disconnected);
  }
}
