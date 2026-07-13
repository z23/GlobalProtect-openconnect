//! Desktop notifications (org.freedesktop.Notifications via notify-rust).
//! Absence of a notification daemon must never affect VPN operation:
//! everything here is fire-and-forget.

use log::warn;
use notify_rust::{Notification, Urgency};

#[derive(Clone, Copy)]
pub struct Notifier {
  enabled: bool,
}

impl Notifier {
  pub fn new(enabled: bool) -> Self {
    Self { enabled }
  }

  pub fn info(&self, summary: &str, body: &str) {
    self.send(summary, body, Urgency::Normal);
  }

  pub fn low(&self, summary: &str, body: &str) {
    self.send(summary, body, Urgency::Low);
  }

  pub fn critical(&self, summary: &str, body: &str) {
    self.send(summary, body, Urgency::Critical);
  }

  fn send(&self, summary: &str, body: &str, urgency: Urgency) {
    if !self.enabled {
      return;
    }

    let mut notification = Notification::new();
    notification
      .appname("GlobalProtect")
      .summary(summary)
      .body(body)
      .icon("network-vpn")
      .urgency(urgency);

    tokio::task::spawn_blocking(move || {
      if let Err(err) = notification.show() {
        warn!("Failed to show notification: {}", err);
      }
    });
  }
}
