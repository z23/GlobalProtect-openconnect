use std::cell::RefCell;
use std::rc::Rc;

use gtk4::{glib, prelude::*};

use crate::proto::{ClientMsg, ServerMsg, StatusMsg, ToastLevel, WidgetState};
use crate::ux;

use super::UiEvent;

pub struct Panel {
  root: gtk4::Box,
  inner: Rc<Inner>,
}

struct Inner {
  cmd_tx: async_channel::Sender<ClientMsg>,
  state: RefCell<PanelState>,

  dot: gtk4::Label,
  state_label: gtk4::Label,
  action_button: gtk4::Button,

  gateway_row: gtk4::Box,
  gateway_dropdown: gtk4::DropDown,
  gateway_names: RefCell<Vec<String>>,

  stats_grid: gtk4::Grid,
  portal_value: gtk4::Label,
  gateway_value: gtk4::Label,
  ip_value: gtk4::Label,
  uptime_value: gtk4::Label,
  session_value: gtk4::Label,
  traffic_value: gtk4::Label,

  error_label: gtk4::Label,

  otp_revealer: gtk4::Revealer,
  otp_message: gtk4::Label,
  otp_entry: gtk4::Entry,
}

#[derive(Default)]
struct PanelState {
  status: Option<StatusMsg>,
  stack_down: bool,
  starting: bool,
}

impl Panel {
  pub fn root(&self) -> &gtk4::Box {
    &self.root
  }

  pub fn build(cmd_tx: async_channel::Sender<ClientMsg>) -> Self {
    let root = gtk4::Box::new(gtk4::Orientation::Vertical, 12);
    root.set_margin_top(16);
    root.set_margin_bottom(16);
    root.set_margin_start(16);
    root.set_margin_end(16);

    // Header: ● State · Gateway ─────────── [Action]
    let header = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
    let dot = gtk4::Label::new(Some("●"));
    dot.set_css_classes(&["state-dot", "stack-down"]);
    let state_label = gtk4::Label::new(Some("VPN off"));
    state_label.set_css_classes(&["state-label"]);
    state_label.set_hexpand(true);
    state_label.set_halign(gtk4::Align::Start);
    let action_button = gtk4::Button::with_label("Start VPN");
    action_button.add_css_class("suggested-action");
    header.append(&dot);
    header.append(&state_label);
    header.append(&action_button);

    // Gateway picker (visible when there is a choice).
    let gateway_row = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
    let gateway_caption = gtk4::Label::new(Some("Gateway"));
    gateway_caption.add_css_class("dim-label");
    let gateway_dropdown = gtk4::DropDown::from_strings(&[]);
    gateway_dropdown.set_hexpand(true);
    gateway_row.append(&gateway_caption);
    gateway_row.append(&gateway_dropdown);
    gateway_row.set_visible(false);

    // Stats grid.
    let stats_grid = gtk4::Grid::new();
    stats_grid.set_row_spacing(6);
    stats_grid.set_column_spacing(18);
    stats_grid.set_visible(false);

    let mut row = 0;
    let mut stat_row = |caption: &str| -> gtk4::Label {
      let key = gtk4::Label::new(Some(caption));
      key.add_css_class("dim-label");
      key.set_halign(gtk4::Align::Start);

      let value = gtk4::Label::new(None);
      value.set_halign(gtk4::Align::End);
      value.set_hexpand(true);
      value.set_selectable(true);
      value.set_ellipsize(gtk4::pango::EllipsizeMode::Middle);

      stats_grid.attach(&key, 0, row, 1, 1);
      stats_grid.attach(&value, 1, row, 1, 1);
      row += 1;

      value
    };

    let portal_value = stat_row("Portal");
    let gateway_value = stat_row("Gateway");
    let ip_value = stat_row("IP");
    let uptime_value = stat_row("Uptime");
    let session_value = stat_row("Session");
    let traffic_value = stat_row("Traffic");

    // Error banner.
    let error_label = gtk4::Label::new(None);
    error_label.set_css_classes(&["error-label"]);
    error_label.set_wrap(true);
    error_label.set_visible(false);
    error_label.set_halign(gtk4::Align::Start);

    // OTP prompt (gateway MFA edge case).
    let otp_box = gtk4::Box::new(gtk4::Orientation::Vertical, 6);
    let otp_message = gtk4::Label::new(None);
    otp_message.set_wrap(true);
    otp_message.set_halign(gtk4::Align::Start);
    let otp_input_row = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
    let otp_entry = gtk4::Entry::new();
    otp_entry.set_hexpand(true);
    otp_entry.set_placeholder_text(Some("One-time password"));
    let otp_submit = gtk4::Button::with_label("Submit");
    otp_submit.add_css_class("suggested-action");
    otp_input_row.append(&otp_entry);
    otp_input_row.append(&otp_submit);
    otp_box.append(&otp_message);
    otp_box.append(&otp_input_row);
    let otp_revealer = gtk4::Revealer::new();
    otp_revealer.set_child(Some(&otp_box));
    otp_revealer.set_reveal_child(false);

    // Footer: version · config · quit.
    let footer = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
    let version_label = gtk4::Label::new(Some(&format!("gpwidget {}", env!("CARGO_PKG_VERSION"))));
    version_label.add_css_class("dim-label");
    version_label.set_hexpand(true);
    version_label.set_halign(gtk4::Align::Start);
    let config_button = gtk4::Button::from_icon_name("document-edit-symbolic");
    config_button.set_tooltip_text(Some("Edit configuration"));
    config_button.add_css_class("flat");
    let quit_button = gtk4::Button::from_icon_name("system-shutdown-symbolic");
    quit_button.set_tooltip_text(Some("Quit the VPN service"));
    quit_button.add_css_class("flat");
    footer.append(&version_label);
    footer.append(&config_button);
    footer.append(&quit_button);

    root.append(&header);
    root.append(&gateway_row);
    root.append(&gtk4::Separator::new(gtk4::Orientation::Horizontal));
    root.append(&stats_grid);
    root.append(&error_label);
    root.append(&otp_revealer);
    root.append(&footer);

    let inner = Rc::new(Inner {
      cmd_tx,
      state: RefCell::new(PanelState::default()),
      dot,
      state_label,
      action_button: action_button.clone(),
      gateway_row,
      gateway_dropdown,
      gateway_names: RefCell::new(Vec::new()),
      stats_grid,
      portal_value,
      gateway_value,
      ip_value,
      uptime_value,
      session_value,
      traffic_value,
      error_label,
      otp_revealer: otp_revealer.clone(),
      otp_message,
      otp_entry: otp_entry.clone(),
    });

    action_button.connect_clicked(glib::clone!(
      #[strong]
      inner,
      move |_| inner.on_action()
    ));

    let submit_otp = glib::clone!(
      #[strong]
      inner,
      move || {
        let otp = inner.otp_entry.text().trim().to_string();
        if otp.is_empty() {
          return;
        }

        inner.send(ClientMsg::SubmitOtp { id: None, otp });
        inner.otp_entry.set_text("");
        inner.otp_revealer.set_reveal_child(false);
      }
    );
    let submit_otp_click = submit_otp.clone();
    otp_submit.connect_clicked(move |_| submit_otp_click());
    otp_entry.connect_activate(move |_| submit_otp());

    config_button.connect_clicked(|_| {
      let path = crate::config::Config::path();
      let _ = std::process::Command::new("xdg-open").arg(path).spawn();
    });

    quit_button.connect_clicked(glib::clone!(
      #[strong]
      inner,
      move |_| inner.send(ClientMsg::Quit { id: None })
    ));

    // Local 1s uptime tick while connected.
    glib::timeout_add_seconds_local(
      1,
      glib::clone!(
        #[strong]
        inner,
        move || {
          inner.refresh_uptime();
          glib::ControlFlow::Continue
        }
      ),
    );

    Self { root, inner }
  }

  pub fn handle_event(&self, event: UiEvent) {
    match event {
      UiEvent::StackDown => {
        let mut state = self.inner.state.borrow_mut();
        state.stack_down = true;
        state.status = None;
        drop(state);
        self.inner.refresh();
      }
      UiEvent::Msg(msg) => match msg {
        ServerMsg::Status(status) => {
          let mut state = self.inner.state.borrow_mut();
          state.stack_down = false;
          state.starting = false;
          state.status = Some(status);
          drop(state);
          self.inner.refresh();
        }
        ServerMsg::AuthPrompt { message, .. } => {
          self.inner.otp_message.set_text(&message);
          self.inner.otp_revealer.set_reveal_child(true);
          self.inner.otp_entry.grab_focus();
        }
        ServerMsg::Toast { level, title, message } => {
          if matches!(level, ToastLevel::Warn | ToastLevel::Error) {
            self.inner.show_error(&format!("{}: {}", title, message));
          }
        }
        ServerMsg::Ack { ok: false, error, .. } => {
          self.inner.show_error(error.as_deref().unwrap_or("Command failed"));
        }
        ServerMsg::Bye => {
          let mut state = self.inner.state.borrow_mut();
          state.stack_down = true;
          state.status = None;
          drop(state);
          self.inner.refresh();
        }
        _ => {}
      },
    }
  }
}

impl Inner {
  fn send(&self, msg: ClientMsg) {
    let _ = self.cmd_tx.send_blocking(msg);
  }

  fn effective_state(&self) -> Option<WidgetState> {
    let state = self.state.borrow();
    if state.stack_down { None } else { state.status.as_ref().and_then(|s| s.state) }
  }

  fn on_action(&self) {
    match self.effective_state() {
      None => {
        self.state.borrow_mut().starting = true;
        self.send(ClientMsg::Connect {
          id: None,
          portal: None,
          gateway: None,
        });
        self.action_button.set_sensitive(false);
        self.state_label.set_text("Starting VPN service…");
      }
      Some(WidgetState::Disconnected) | Some(WidgetState::Error) | Some(WidgetState::NeedsSetup) => {
        self.send(ClientMsg::Connect {
          id: None,
          portal: None,
          gateway: self.selected_gateway(),
        });
      }
      Some(WidgetState::Authenticating) | Some(WidgetState::Connecting) | Some(WidgetState::Connected) => {
        self.send(ClientMsg::Disconnect { id: None });
      }
      Some(WidgetState::Disconnecting) => {}
    }
  }

  /// The picked gateway, only when it is an actual choice.
  fn selected_gateway(&self) -> Option<String> {
    let names = self.gateway_names.borrow();
    if names.len() < 2 {
      return None;
    }

    names.get(self.gateway_dropdown.selected() as usize).cloned()
  }

  fn show_error(&self, message: &str) {
    self.error_label.set_text(message);
    self.error_label.set_visible(true);
  }

  fn refresh_uptime(&self) {
    let state = self.state.borrow();
    let Some(status) = &state.status else { return };

    if status.state == Some(WidgetState::Connected) {
      if let Some(conn) = &status.conn {
        let uptime = ux::unix_now().saturating_sub(conn.since);
        self.uptime_value.set_text(&ux::format_duration_short(uptime));
      }
    }
  }

  fn refresh(&self) {
    let state = self.state.borrow();
    let effective = if state.stack_down {
      None
    } else {
      state.status.as_ref().and_then(|s| s.state)
    };
    let style = ux::state_style(effective);

    self.dot.set_css_classes(&["state-dot", style.class]);

    // Header label: state, plus gateway when connected.
    let header_text = match (effective, state.status.as_ref().and_then(|s| s.gateway.as_ref())) {
      (Some(WidgetState::Connected), Some(gateway)) if !gateway.name.is_empty() => {
        format!("{} · {}", style.label, gateway.name)
      }
      _ => style.label.to_string(),
    };
    self.state_label.set_text(&header_text);

    // Action button.
    let (action, action_class, sensitive) = match effective {
      None => ("Start VPN", "suggested-action", !state.starting),
      Some(WidgetState::NeedsSetup) => ("Connect", "suggested-action", false),
      Some(WidgetState::Disconnected) | Some(WidgetState::Error) => ("Connect", "suggested-action", true),
      Some(WidgetState::Authenticating) | Some(WidgetState::Connecting) => ("Cancel", "destructive-action", true),
      Some(WidgetState::Connected) => ("Disconnect", "destructive-action", true),
      Some(WidgetState::Disconnecting) => ("Disconnecting…", "flat", false),
    };
    self.action_button.set_label(action);
    self.action_button.set_css_classes(&[action_class]);
    self.action_button.set_sensitive(sensitive);

    // Gateway picker.
    let gateways = state
      .status
      .as_ref()
      .map(|s| s.gateways.clone())
      .unwrap_or_default();
    {
      let mut names = self.gateway_names.borrow_mut();
      let new_names: Vec<String> = gateways.iter().map(|g| g.name.clone()).collect();

      if *names != new_names {
        let refs: Vec<&str> = new_names.iter().map(String::as_str).collect();
        self.gateway_dropdown.set_model(Some(&gtk4::StringList::new(&refs)));

        // Keep the current gateway selected where possible.
        if let Some(current) = state.status.as_ref().and_then(|s| s.gateway.as_ref()) {
          if let Some(idx) = new_names.iter().position(|name| *name == current.name) {
            self.gateway_dropdown.set_selected(idx as u32);
          }
        }

        *names = new_names;
      }
    }
    self.gateway_row.set_visible(gateways.len() > 1);

    // Stats.
    let show_stats = matches!(effective, Some(WidgetState::Connecting) | Some(WidgetState::Connected));
    self.stats_grid.set_visible(show_stats);

    if let Some(status) = state.status.as_ref() {
      self.portal_value.set_text(status.portal.as_deref().unwrap_or("—"));

      match &status.gateway {
        Some(gateway) => self.gateway_value.set_text(&format!("{} ({})", gateway.name, gateway.address)),
        None => self.gateway_value.set_text("—"),
      }

      match &status.conn {
        Some(conn) => {
          match (&conn.ipv4, &conn.ifname) {
            (Some(ipv4), Some(ifname)) => self.ip_value.set_text(&format!("{} ({})", ipv4, ifname)),
            (Some(ipv4), None) => self.ip_value.set_text(ipv4),
            _ => self.ip_value.set_text("—"),
          }

          let now = ux::unix_now();
          self.uptime_value.set_text(&ux::format_duration_short(now.saturating_sub(conn.since)));

          let traffic = if conn.rx_rate > 0 || conn.tx_rate > 0 {
            format!(
              "↓ {} ({}) · ↑ {} ({})",
              ux::format_bytes(conn.rx_bytes),
              ux::format_rate(conn.rx_rate),
              ux::format_bytes(conn.tx_bytes),
              ux::format_rate(conn.tx_rate)
            )
          } else {
            format!("↓ {} · ↑ {}", ux::format_bytes(conn.rx_bytes), ux::format_bytes(conn.tx_bytes))
          };
          self.traffic_value.set_text(&traffic);
        }
        None => {
          self.ip_value.set_text("—");
          self.uptime_value.set_text("—");
          self.traffic_value.set_text("—");
        }
      }

      match status.session.as_ref().and_then(|s| s.expires_at) {
        Some(expires_at) => {
          self.session_value.set_text(&ux::format_expiry(expires_at, ux::unix_now()));
        }
        None => self.session_value.set_text("—"),
      }
    }

    // Error banner: sticky while the daemon reports one; cleared otherwise.
    let error = state.status.as_ref().and_then(|s| s.error.clone());
    match (effective, error) {
      (Some(WidgetState::Error), Some(error)) => {
        self.error_label.set_text(&error);
        self.error_label.set_visible(true);
      }
      (Some(WidgetState::NeedsSetup), _) => {
        self
          .error_label
          .set_text(&format!("Set the portal in {}", crate::config::Config::path().display()));
        self.error_label.set_visible(true);
      }
      _ => self.error_label.set_visible(false),
    }

    // A state change ends any pending OTP prompt.
    if !matches!(effective, Some(WidgetState::Authenticating)) {
      self.otp_revealer.set_reveal_child(false);
    }
  }
}
