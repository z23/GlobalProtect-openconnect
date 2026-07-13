mod connect_flow;
mod notify;
mod socket;
mod state;
mod stats;
mod ws_client;

use std::sync::Arc;
use std::time::Duration;

use gpapi::service::{
  event::WsEvent,
  request::{ConnectRequest, DisconnectRequest, WsRequest},
  vpn_state::VpnState,
};
use log::{info, warn};
use tokio::{
  signal::unix::{SignalKind, signal},
  sync::{broadcast, mpsc, oneshot, watch},
};
use tokio_util::sync::CancellationToken;

use crate::{
  cli::VERSION,
  config::Config,
  proto::{self, ClientMsg, ConnStats, ServerMsg, ToastLevel},
};
use connect_flow::{FlowCtx, FlowMsg};
use notify::Notifier;
use socket::{Command, SocketServer};
use state::Model;
use stats::StatsTracker;
use ws_client::{WsClient, WsNotice};

const CONNECT_WATCHDOG_SECS: u64 = 15;
const QUIT_DISCONNECT_TIMEOUT_SECS: u64 = 10;

pub async fn run(api_key: Vec<u8>, config: Config) -> anyhow::Result<i32> {
  // Must happen before the first TLS use in this process; the temp file
  // backing OPENSSL_CONF has to outlive every TLS call.
  let openssl_conf = if config.advanced.fix_openssl {
    info!("Applying the OpenSSL legacy-renegotiation fix");
    Some(gpapi::utils::openssl::fix_openssl_env()?)
  } else {
    None
  };

  let notifier = Notifier::new(config.notifications);
  let model = Model::new(config);

  let (broadcast_tx, _) = broadcast::channel::<Arc<str>>(64);
  let initial_snapshot: Arc<str> = serialize(&ServerMsg::Status(model.snapshot()))?.into();
  let (snapshot_tx, snapshot_rx) = watch::channel(initial_snapshot);

  let hello_line: Arc<str> = serialize(&ServerMsg::Hello {
    version: VERSION.to_string(),
    protocol: proto::PROTOCOL_VERSION,
  })?
  .into();

  let socket_server = SocketServer::bind(&proto::socket_path(), hello_line, broadcast_tx.clone(), snapshot_rx).await?;

  let ws = ws_client::spawn(api_key);
  let (flow_tx, flow_rx) = mpsc::channel(16);
  let (timer_tx, timer_rx) = mpsc::channel(8);
  let (stats_tx, stats_rx) = mpsc::channel(8);

  let mut daemon = Daemon {
    model,
    ws,
    socket_server,
    broadcast_tx,
    snapshot_tx,
    flow_tx,
    flow_rx,
    timer_tx,
    timer_rx,
    stats_tx,
    stats_rx,
    stats: StatsTracker::new(),
    notifier,
    connect_cancel: None,
    pending_otp: None,
    cached_request: None,
    disconnect_requested: false,
    quitting: false,
    watchdog_generation: 0,
    expiry_generation: 0,
  };

  let exit_code = daemon.run().await;

  daemon.publish_msg(&ServerMsg::Bye);
  daemon.socket_server.cleanup();
  drop(openssl_conf);

  Ok(exit_code)
}

enum TimerMsg {
  ConnectWatchdog { generation: u64 },
  ExpiryWarning { generation: u64 },
  QuitTimeout,
}

struct Daemon {
  model: Model,
  ws: WsClient,
  socket_server: SocketServer,
  broadcast_tx: broadcast::Sender<Arc<str>>,
  snapshot_tx: watch::Sender<Arc<str>>,
  flow_tx: mpsc::Sender<FlowMsg>,
  flow_rx: mpsc::Receiver<FlowMsg>,
  timer_tx: mpsc::Sender<TimerMsg>,
  timer_rx: mpsc::Receiver<TimerMsg>,
  stats_tx: mpsc::Sender<ConnStats>,
  stats_rx: mpsc::Receiver<ConnStats>,
  stats: StatsTracker,
  notifier: Notifier,
  /// Cancellation handle of the running connect flow, if any.
  connect_cancel: Option<CancellationToken>,
  pending_otp: Option<oneshot::Sender<String>>,
  /// Last successfully-submitted connect request; replayed on
  /// ResumeConnection without re-running auth.
  cached_request: Option<Box<ConnectRequest>>,
  /// True when the upcoming Disconnected state was asked for (disconnect
  /// command or quit), as opposed to an unexpected drop.
  disconnect_requested: bool,
  quitting: bool,
  watchdog_generation: u64,
  expiry_generation: u64,
}

impl Daemon {
  async fn run(&mut self) -> i32 {
    let mut sigterm = match signal(SignalKind::terminate()) {
      Ok(signal) => signal,
      Err(err) => {
        warn!("Failed to install SIGTERM handler: {}", err);
        return 1;
      }
    };
    let mut sigint = match signal(SignalKind::interrupt()) {
      Ok(signal) => signal,
      Err(err) => {
        warn!("Failed to install SIGINT handler: {}", err);
        return 1;
      }
    };

    loop {
      tokio::select! {
        notice = self.ws.notices.recv() => {
          match notice {
            None => {
              warn!("WS task ended unexpectedly");
              return 1;
            }
            Some(WsNotice::Event(event)) => {
              if self.handle_ws_event(event) {
                return 0;
              }
            }
            Some(WsNotice::Lost) => {
              warn!("Lost connection to gpservice, attempting to reconnect");
            }
            Some(WsNotice::Gone) => {
              warn!("gpservice is gone, shutting down");
              self.toast(ToastLevel::Error, "GlobalProtect", "VPN service terminated unexpectedly");
              self.notifier.critical("GlobalProtect", "VPN service terminated unexpectedly");
              return 0;
            }
          }
        }

        command = self.socket_server.commands.recv() => {
          let Some(command) = command else {
            warn!("Socket server ended unexpectedly");
            return 1;
          };

          if self.handle_command(command).await {
            return 0;
          }
        }

        flow_msg = self.flow_rx.recv() => {
          if let Some(flow_msg) = flow_msg {
            self.handle_flow_msg(flow_msg);
          }
        }

        timer_msg = self.timer_rx.recv() => {
          match timer_msg {
            Some(TimerMsg::ConnectWatchdog { generation }) => self.handle_watchdog(generation),
            Some(TimerMsg::ExpiryWarning { generation }) => self.handle_expiry_warning(generation),
            Some(TimerMsg::QuitTimeout) => {
              if self.quitting {
                warn!("Disconnect timed out during quit, exiting anyway");
                return 0;
              }
            }
            None => {}
          }
        }

        stats = self.stats_rx.recv() => {
          if let Some(stats) = stats {
            if matches!(self.model.vpn_state, VpnState::Connected(_)) {
              self.model.conn = Some(stats);

              // Skip broadcast churn when no widget is listening; snapshots
              // built on demand still carry the fresh numbers.
              if self.broadcast_tx.receiver_count() > 0 {
                self.publish_snapshot();
              }
            }
          }
        }

        _ = sigterm.recv() => {
          info!("Received SIGTERM, shutting down");
          return 0;
        }

        _ = sigint.recv() => {
          info!("Received SIGINT, shutting down");
          return 0;
        }
      }
    }
  }

  /// Returns true when the daemon should shut down.
  fn handle_ws_event(&mut self, event: WsEvent) -> bool {
    match event {
      WsEvent::VpnEnv(env) => {
        info!(
          "Received VpnEnv: auth_executable={}, vpnc_script={:?}",
          env.auth_executable, env.vpnc_script
        );

        self.model.apply_vpn_state(env.vpn_state.clone());
        self.model.vpn_env = Some(env);
        self.publish_snapshot();

        if self.model.config.auto_connect && matches!(self.model.vpn_state, VpnState::Disconnected) {
          info!("auto-connect is enabled, starting the connect flow");
          if let Err(err) = self.start_connect_flow(None, None) {
            warn!("Auto-connect failed to start: {}", err);
          }
        }
      }
      WsEvent::VpnState(vpn_state) => {
        match &vpn_state {
          VpnState::Connecting(_) | VpnState::Connected(_) => {
            self.model.authenticating = false;
            self.model.last_error = None;
            self.watchdog_generation += 1;
          }
          VpnState::Disconnected => {
            if self.quitting {
              info!("Disconnected during quit, exiting");
              return true;
            }

            if matches!(self.model.vpn_state, VpnState::Connected(_) | VpnState::Disconnecting) {
              if self.disconnect_requested {
                self.notifier.low("GlobalProtect disconnected", "");
              } else if matches!(self.model.vpn_state, VpnState::Connected(_)) {
                self.toast(ToastLevel::Error, "GlobalProtect", "VPN connection lost");
                self.notifier.critical("GlobalProtect", "VPN connection lost");
              }
            }
            self.disconnect_requested = false;
          }
          VpnState::Disconnecting => {}
        }

        let was_connected = matches!(self.model.vpn_state, VpnState::Connected(_));
        self.model.apply_vpn_state(vpn_state);

        match &self.model.vpn_state {
          VpnState::Connecting(_) => self.stats.on_connecting(),
          VpnState::Connected(_) if !was_connected => {
            let since = self.model.conn.as_ref().map(|conn| conn.since).unwrap_or_else(crate::ux::unix_now);
            let interval = Duration::from_secs(self.model.config.stats_interval_secs.max(1));
            self.stats.on_connected(since, interval, self.stats_tx.clone());

            let gateway = self
              .model
              .current_gateway
              .as_ref()
              .map(|gateway| gateway.name.clone())
              .unwrap_or_default();
            self.notifier.info("GlobalProtect connected", &gateway);

            self.on_session_established();
          }
          VpnState::Disconnected => {
            self.stats.stop();
            self.expiry_generation += 1;
          }
          _ => {}
        }

        self.publish_snapshot();
      }
      WsEvent::ActiveGui => {
        info!("Received ActiveGui event, opening the popup");
        open_popup();
      }
      WsEvent::ResumeConnection => {
        info!("Received ResumeConnection event");
        self.resume_connection();
      }
    }

    false
  }

  /// Returns true when the daemon should shut down.
  async fn handle_command(&mut self, command: Command) -> bool {
    let id = command.msg.id();

    match command.msg {
      ClientMsg::GetStatus { .. } => {
        let snapshot = self.snapshot_tx.borrow().clone();
        let _ = command.reply.send(snapshot).await;
        self.ack(&command.reply, id, Ok(())).await;
      }

      ClientMsg::Connect { portal, gateway, .. } => {
        let result = self.start_connect_flow(portal, gateway);
        self.ack(&command.reply, id, result.map_err(|err| format!("{:#}", err))).await;
      }

      ClientMsg::Disconnect { .. } => {
        self.disconnect();
        self.ack(&command.reply, id, Ok(())).await;
      }

      ClientMsg::Toggle { .. } => {
        let result = if self.connect_cancel.is_some()
          || matches!(self.model.vpn_state, VpnState::Connecting(_) | VpnState::Connected(_))
        {
          self.disconnect();
          Ok(())
        } else {
          self.start_connect_flow(None, None)
        };

        self.ack(&command.reply, id, result.map_err(|err| format!("{:#}", err))).await;
      }

      ClientMsg::SubmitOtp { otp, .. } => {
        let result = match self.pending_otp.take() {
          Some(respond) => respond.send(otp).map_err(|_| "the OTP prompt expired".to_string()),
          None => Err("no pending OTP prompt".to_string()),
        };

        self.ack(&command.reply, id, result).await;
      }

      ClientMsg::OpenPopup { .. } => {
        open_popup();
        self.ack(&command.reply, id, Ok(())).await;
      }

      ClientMsg::Quit { .. } => {
        info!("Quit requested via socket");
        self.ack(&command.reply, id, Ok(())).await;

        if let Some(cancel) = self.connect_cancel.take() {
          cancel.cancel();
        }

        if matches!(self.model.vpn_state, VpnState::Connecting(_) | VpnState::Connected(_)) {
          self.quitting = true;
          self.disconnect_requested = true;
          self.send_ws(WsRequest::Disconnect(DisconnectRequest)).await;

          let timer_tx = self.timer_tx.clone();
          tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(QUIT_DISCONNECT_TIMEOUT_SECS)).await;
            let _ = timer_tx.send(TimerMsg::QuitTimeout).await;
          });
        } else {
          return true;
        }
      }
    }

    false
  }

  fn handle_flow_msg(&mut self, flow_msg: FlowMsg) {
    match flow_msg {
      FlowMsg::OtpPrompt { message, respond } => {
        info!("OTP required for gateway login");
        self.pending_otp = Some(respond);
        self.publish_msg(&ServerMsg::AuthPrompt {
          kind: "otp".to_string(),
          message,
        });
      }

      FlowMsg::Submitted { request } => {
        self.connect_cancel = None;
        self.cached_request = Some(request);

        // gpservice silently ignores Connect when not Disconnected; make
        // sure a stuck request surfaces instead of spinning forever.
        self.watchdog_generation += 1;
        let generation = self.watchdog_generation;
        let timer_tx = self.timer_tx.clone();
        tokio::spawn(async move {
          tokio::time::sleep(Duration::from_secs(CONNECT_WATCHDOG_SECS)).await;
          let _ = timer_tx.send(TimerMsg::ConnectWatchdog { generation }).await;
        });
      }

      FlowMsg::Failed { error, cancelled } => {
        self.connect_cancel = None;
        self.pending_otp = None;
        self.model.authenticating = false;

        if !cancelled {
          self.model.last_error = Some(error.clone());
          self.toast(ToastLevel::Error, "GlobalProtect connection failed", &error);
          self.notifier.critical("GlobalProtect connection failed", &error);
        }

        self.publish_snapshot();
      }
    }
  }

  /// Schedule the session-expiry warning and surface any admin message.
  fn on_session_established(&mut self) {
    self.expiry_generation += 1;

    let VpnState::Connected(connected) = &self.model.vpn_state else {
      return;
    };
    let Some(session) = connected.session_info() else {
      return;
    };

    if let Some(message) = session.admin_logout_message.as_deref() {
      self.toast(ToastLevel::Warn, "GlobalProtect", message);
      self.notifier.critical("GlobalProtect", message);
    }

    let Some(expires_at) = session.user_expires.map(u64::from) else {
      return;
    };

    let warn_prior = u64::from(session.lifetime_warning.as_ref().map(|warning| warning.prior_secs).unwrap_or(600));
    let now = crate::ux::unix_now();
    let delay = expires_at.saturating_sub(warn_prior).saturating_sub(now);

    let generation = self.expiry_generation;
    let timer_tx = self.timer_tx.clone();
    tokio::spawn(async move {
      tokio::time::sleep(Duration::from_secs(delay)).await;
      let _ = timer_tx.send(TimerMsg::ExpiryWarning { generation }).await;
    });
  }

  fn handle_expiry_warning(&mut self, generation: u64) {
    if generation != self.expiry_generation {
      return;
    }

    let VpnState::Connected(connected) = &self.model.vpn_state else {
      return;
    };

    let message = connected
      .session_info()
      .and_then(|session| session.lifetime_warning.as_ref())
      .map(|warning| warning.message.clone())
      .unwrap_or_else(|| "The VPN session is about to expire".to_string());

    info!("Session expiry warning: {}", message);
    self.toast(ToastLevel::Warn, "GlobalProtect", &message);
    self.notifier.info("GlobalProtect", &message);
  }

  fn handle_watchdog(&mut self, generation: u64) {
    if generation != self.watchdog_generation {
      return;
    }

    if matches!(self.model.vpn_state, VpnState::Disconnected) && self.connect_cancel.is_none() {
      warn!("gpservice did not react to the connect request");
      self.model.authenticating = false;
      self.model.last_error = Some("The VPN service did not react to the connect request".to_string());
      self.publish_snapshot();
    }
  }

  fn start_connect_flow(&mut self, portal_override: Option<String>, gateway_override: Option<String>) -> anyhow::Result<()> {
    if self.connect_cancel.is_some() {
      anyhow::bail!("a connection attempt is already in progress");
    }

    if !matches!(self.model.vpn_state, VpnState::Disconnected) {
      anyhow::bail!("already connected or connecting");
    }

    let Some(vpn_env) = self.model.vpn_env.clone() else {
      anyhow::bail!("the VPN service environment is not ready yet");
    };

    // Config is re-read on every attempt so edits apply without restarting.
    let mut config = Config::load().unwrap_or_else(|err| {
      warn!("Failed to reload config ({}), using the current one", err);
      self.model.config.clone()
    });

    if let Some(portal) = portal_override {
      let portal = portal.trim().to_string();
      if !portal.is_empty() && config.portal.as_deref() != Some(portal.as_str()) {
        config.portal = Some(portal);
        if let Err(err) = config.save() {
          warn!("Failed to persist the portal to the config: {}", err);
        }
      }
    }

    let Some(portal) = config.portal.clone() else {
      anyhow::bail!("no portal configured — set one in {}", Config::path().display());
    };

    self.model.config = config.clone();
    self.model.portal = Some(portal.clone());
    self.model.authenticating = true;
    self.model.last_error = None;
    self.publish_snapshot();

    let cancel = CancellationToken::new();
    self.connect_cancel = Some(cancel.clone());

    connect_flow::spawn(FlowCtx {
      portal,
      gateway_override,
      config,
      vpn_env,
      ws_tx: self.ws.requests.clone(),
      flow_tx: self.flow_tx.clone(),
      cancel,
    });

    Ok(())
  }

  fn disconnect(&mut self) {
    if let Some(cancel) = self.connect_cancel.take() {
      info!("Cancelling the running connect flow");
      cancel.cancel();
      return;
    }

    if matches!(self.model.vpn_state, VpnState::Connecting(_) | VpnState::Connected(_)) {
      self.disconnect_requested = true;

      let ws_tx = self.ws.requests.clone();
      tokio::spawn(async move {
        if ws_tx.send(WsRequest::Disconnect(DisconnectRequest)).await.is_err() {
          warn!("Failed to send the disconnect request");
        }
      });
    }
  }

  fn resume_connection(&mut self) {
    if !self.model.config.auto_resume {
      info!("auto-resume is disabled, ignoring ResumeConnection");
      return;
    }

    if self.connect_cancel.is_some() || !matches!(self.model.vpn_state, VpnState::Disconnected) {
      return;
    }

    let Some(request) = self.cached_request.clone() else {
      info!("No cached connect request to resume");
      return;
    };

    info!("Resuming the VPN connection with the cached request");

    let ws_tx = self.ws.requests.clone();
    tokio::spawn(async move {
      if ws_tx.send(WsRequest::Connect(request)).await.is_err() {
        warn!("Failed to send the resume connect request");
      }
    });
  }

  async fn send_ws(&self, request: WsRequest) {
    if self.ws.requests.send(request).await.is_err() {
      warn!("Failed to send request: WS task is gone");
    }
  }

  async fn ack(&self, reply: &mpsc::Sender<Arc<str>>, id: Option<u64>, result: Result<(), String>) {
    let Some(id) = id else { return };

    let msg = ServerMsg::Ack {
      id,
      ok: result.is_ok(),
      error: result.err(),
    };

    match serialize(&msg) {
      Ok(line) => {
        let _ = reply.send(line.into()).await;
      }
      Err(err) => warn!("Failed to serialize ack: {}", err),
    }
  }

  fn toast(&self, level: ToastLevel, title: &str, message: &str) {
    self.publish_msg(&ServerMsg::Toast {
      level,
      title: title.to_string(),
      message: message.to_string(),
    });
  }

  fn publish_snapshot(&mut self) {
    match serialize(&ServerMsg::Status(self.model.snapshot())) {
      Ok(line) => {
        let line: Arc<str> = line.into();
        let _ = self.snapshot_tx.send(line.clone());
        let _ = self.broadcast_tx.send(line);
      }
      Err(err) => warn!("Failed to serialize snapshot: {}", err),
    }
  }

  fn publish_msg(&self, msg: &ServerMsg) {
    match serialize(msg) {
      Ok(line) => {
        let _ = self.broadcast_tx.send(line.into());
      }
      Err(err) => warn!("Failed to serialize message: {}", err),
    }
  }
}

fn open_popup() {
  let Ok(exe) = std::env::current_exe() else {
    warn!("Failed to determine the gpwidget binary path");
    return;
  };

  if let Err(err) = std::process::Command::new(exe)
    .arg("popup")
    .stdin(std::process::Stdio::null())
    .stdout(std::process::Stdio::null())
    .stderr(std::process::Stdio::null())
    .spawn()
  {
    warn!("Failed to open the popup: {}", err);
  }
}

fn serialize(msg: &ServerMsg) -> anyhow::Result<String> {
  Ok(serde_json::to_string(msg)?)
}
