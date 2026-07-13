//! The layer-shell status popup (`gpwidget popup`).
//!
//! Single-instance with toggle semantics: invoking `gpwidget popup` while a
//! popup is open closes it (waybar on-click behaves like a toggle). Works
//! with or without the daemon; when the stack is down it offers to start it.

mod panel;

use std::fs::File;
use std::io::{Read, Write};
use std::path::PathBuf;

use anyhow::Context;
use gtk4::{gio, glib, prelude::*};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use log::warn;

use crate::config::Config;
use crate::proto::{ClientMsg, ServerMsg};

/// Popup-side view of the world, delivered to the GTK main loop.
pub enum UiEvent {
  Msg(ServerMsg),
  /// No daemon socket: synthesized stack-down state.
  StackDown,
}

pub fn run() -> anyhow::Result<()> {
  // Toggle semantics via a pid-file lock: if another popup holds it,
  // ask it to close and exit.
  let lock_path = lock_path();
  let mut lock_file = File::options()
    .create(true)
    .read(true)
    .write(true)
    .truncate(false)
    .open(&lock_path)
    .context(format!("Failed to open {}", lock_path.display()))?;

  if let Err(err) = lock_file.try_lock() {
    let mut pid = String::new();
    let _ = lock_file.read_to_string(&mut pid);
    let pid = pid.trim();

    log::info!("Popup already open (pid {}, lock error {err:?}), closing it", pid);

    if !pid.is_empty() {
      let _ = std::process::Command::new("kill").args(["-TERM", pid]).status();
    }

    return Ok(());
  }

  lock_file.set_len(0)?;
  write!(lock_file, "{}", std::process::id())?;

  let config = Config::load().unwrap_or_default();

  let app = gtk4::Application::builder()
    .application_id("com.yuezk.gpwidget.popup")
    .flags(gio::ApplicationFlags::NON_UNIQUE)
    .build();

  let popup_config = config.popup.clone();
  app.connect_activate(move |app| build_window(app, &popup_config));

  // SIGTERM from a toggling second instance must close the window.
  glib::unix_signal_add_local(15, glib::clone!(
    #[weak]
    app,
    #[upgrade_or]
    glib::ControlFlow::Break,
    move || {
      app.quit();
      glib::ControlFlow::Break
    }
  ));

  app.run_with_args::<&str>(&[]);

  drop(lock_file);
  let _ = std::fs::remove_file(&lock_path);

  Ok(())
}

fn lock_path() -> PathBuf {
  crate::proto::socket_path().with_file_name("gpwidget-popup.lock")
}

fn build_window(app: &gtk4::Application, popup_config: &crate::config::PopupConfig) {
  let window = gtk4::ApplicationWindow::builder()
    .application(app)
    .title("GlobalProtect")
    .default_width(360)
    .resizable(false)
    .build();

  window.add_css_class("gp-popup");

  window.init_layer_shell();
  window.set_layer(Layer::Top);
  window.set_namespace(Some("gpwidget"));
  window.set_keyboard_mode(KeyboardMode::OnDemand);

  let [margin_v, margin_h] = popup_config.margin;
  let (vertical_edge, horizontal_edge) = anchor_edges(&popup_config.edge);
  window.set_anchor(vertical_edge, true);
  window.set_anchor(horizontal_edge, true);
  window.set_margin(vertical_edge, margin_v);
  window.set_margin(horizontal_edge, margin_h);

  load_css();

  // Esc closes.
  let key_controller = gtk4::EventControllerKey::new();
  key_controller.connect_key_pressed(glib::clone!(
    #[weak]
    window,
    #[upgrade_or]
    glib::Propagation::Proceed,
    move |_, key, _, _| {
      if key == gtk4::gdk::Key::Escape {
        window.close();
        glib::Propagation::Stop
      } else {
        glib::Propagation::Proceed
      }
    }
  ));
  window.add_controller(key_controller);

  let (ui_tx, ui_rx) = async_channel::unbounded::<UiEvent>();
  let (cmd_tx, cmd_rx) = async_channel::unbounded::<ClientMsg>();

  std::thread::spawn(move || io_thread(ui_tx, cmd_rx));

  let panel = panel::Panel::build(cmd_tx);
  window.set_child(Some(panel.root()));

  glib::spawn_future_local(async move {
    while let Ok(event) = ui_rx.recv().await {
      panel.handle_event(event);
    }
  });

  window.present();
}

fn anchor_edges(edge: &str) -> (Edge, Edge) {
  match edge {
    "top-left" => (Edge::Top, Edge::Left),
    "bottom-left" => (Edge::Bottom, Edge::Left),
    "bottom-right" => (Edge::Bottom, Edge::Right),
    _ => (Edge::Top, Edge::Right),
  }
}

fn load_css() {
  let Some(display) = gtk4::gdk::Display::default() else {
    return;
  };

  let provider = gtk4::CssProvider::new();
  provider.load_from_data(include_str!("style.css"));
  gtk4::style_context_add_provider_for_display(&display, &provider, gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION);
}

/// Socket IO on a dedicated tokio runtime; the GTK main loop stays clean.
fn io_thread(ui_tx: async_channel::Sender<UiEvent>, cmd_rx: async_channel::Receiver<ClientMsg>) {
  let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
    Ok(runtime) => runtime,
    Err(err) => {
      warn!("Failed to build the IO runtime: {}", err);
      return;
    }
  };

  runtime.block_on(async move {
    loop {
      match crate::client::SocketClient::try_open().await {
        Ok(Some(mut client)) => {
          loop {
            tokio::select! {
              msg = client.next_msg() => {
                match msg {
                  Ok(Some(msg)) => {
                    if ui_tx.send(UiEvent::Msg(msg)).await.is_err() {
                      return;
                    }
                  }
                  Ok(None) | Err(_) => break,
                }
              }

              cmd = cmd_rx.recv() => {
                let Ok(cmd) = cmd else { return };

                if client.send(&cmd).await.is_err() {
                  break;
                }
              }
            }
          }
        }

        _ => {
          if ui_tx.send(UiEvent::StackDown).await.is_err() {
            return;
          }

          // While the stack is down, a Connect request means "bring it up":
          // delegate to `gpwidget connect`, which handles launch-gui + wait.
          tokio::select! {
            cmd = cmd_rx.recv() => {
              if let Ok(ClientMsg::Connect { .. }) = cmd {
                start_stack_detached();
              }
            }
            _ = tokio::time::sleep(std::time::Duration::from_secs(2)) => {}
          }
        }
      }
    }
  });
}

fn start_stack_detached() {
  let Ok(exe) = std::env::current_exe() else {
    return;
  };

  if let Err(err) = std::process::Command::new(exe)
    .arg("connect")
    .stdin(std::process::Stdio::null())
    .stdout(std::process::Stdio::null())
    .stderr(std::process::Stdio::null())
    .spawn()
  {
    warn!("Failed to start the VPN stack: {}", err);
  }
}
