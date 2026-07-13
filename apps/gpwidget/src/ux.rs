//! Shared state → icon/class/label mapping and humanized formatting,
//! consumed by the waybar formatter and the GTK popup. The DMS plugin
//! mirrors these rules in js/Formatting.js.

use std::time::{SystemTime, UNIX_EPOCH};

use chrono::{Local, TimeZone};

use crate::proto::WidgetState;

pub fn unix_now() -> u64 {
  SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs()
}

pub struct StateStyle {
  /// Nerd-font glyph for waybar (Material Design shield icons).
  pub glyph: &'static str,
  pub label: &'static str,
  /// CSS class, shared by waybar and the GTK popup.
  pub class: &'static str,
}

/// `None` means the synthesized stack-down state (no daemon socket).
pub fn state_style(state: Option<WidgetState>) -> StateStyle {
  match state {
    None => StateStyle {
      glyph: "\u{f099e}", // 󰦞 shield-off
      label: "VPN off",
      class: "stack-down",
    },
    Some(WidgetState::NeedsSetup) => StateStyle {
      glyph: "\u{f099e}",
      label: "Setup required",
      class: "needs-setup",
    },
    Some(WidgetState::Disconnected) => StateStyle {
      glyph: "\u{f099e}",
      label: "Disconnected",
      class: "disconnected",
    },
    Some(WidgetState::Authenticating) => StateStyle {
      glyph: "\u{f0996}", // 󰦖 shield-sync
      label: "Authenticating…",
      class: "connecting",
    },
    Some(WidgetState::Connecting) => StateStyle {
      glyph: "\u{f0996}",
      label: "Connecting…",
      class: "connecting",
    },
    Some(WidgetState::Connected) => StateStyle {
      glyph: "\u{f099d}", // 󰦝 shield-lock
      label: "Connected",
      class: "connected",
    },
    Some(WidgetState::Disconnecting) => StateStyle {
      glyph: "\u{f0996}",
      label: "Disconnecting…",
      class: "disconnecting",
    },
    Some(WidgetState::Error) => StateStyle {
      glyph: "\u{f099f}", // 󰦟 shield-alert
      label: "Error",
      class: "error",
    },
  }
}

/// IEC units; decimals scale with magnitude: `1.23 GiB`, `87.4 MiB`, `512 KiB`.
pub fn format_bytes(bytes: u64) -> String {
  const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];

  let mut value = bytes as f64;
  let mut unit = 0;

  while value >= 1024.0 && unit < UNITS.len() - 1 {
    value /= 1024.0;
    unit += 1;
  }

  if unit == 0 {
    format!("{} B", bytes)
  } else if value < 10.0 {
    format!("{:.2} {}", value, UNITS[unit])
  } else if value < 100.0 {
    format!("{:.1} {}", value, UNITS[unit])
  } else {
    format!("{:.0} {}", value, UNITS[unit])
  }
}

pub fn format_rate(bytes_per_sec: u64) -> String {
  format!("{}/s", format_bytes(bytes_per_sec))
}

/// Two most significant units: `2d 3h`, `3h 24m`, `24m 36s`, `36s`.
pub fn format_duration_short(secs: u64) -> String {
  let (days, rem) = (secs / 86_400, secs % 86_400);
  let (hours, rem) = (rem / 3_600, rem % 3_600);
  let (minutes, seconds) = (rem / 60, rem % 60);

  match (days, hours, minutes) {
    (0, 0, 0) => format!("{}s", seconds),
    (0, 0, _) => format!("{}m {}s", minutes, seconds),
    (0, _, _) => format!("{}h {}m", hours, minutes),
    _ => format!("{}d {}h", days, hours),
  }
}

/// `in 11h 23m (07:32)`; adds the date when not today.
pub fn format_expiry(expires_at: u64, now: u64) -> String {
  let relative = if expires_at > now {
    format!("in {}", format_duration_short(expires_at - now))
  } else {
    "expired".to_string()
  };

  let Some(local) = Local.timestamp_opt(expires_at as i64, 0).single() else {
    return relative;
  };

  let today = Local.timestamp_opt(now as i64, 0).single().map(|t| t.date_naive());
  let absolute = if today == Some(local.date_naive()) {
    local.format("%H:%M").to_string()
  } else {
    local.format("%a %H:%M").to_string()
  };

  format!("{} ({})", relative, absolute)
}

/// Remaining session percentage for waybar's `percentage` field.
pub fn session_percentage(expires_at: Option<u64>, lifetime_secs: Option<u32>, now: u64) -> u8 {
  let (Some(expires_at), Some(lifetime)) = (expires_at, lifetime_secs) else {
    return 0;
  };

  if lifetime == 0 || expires_at <= now {
    return 0;
  }

  let remaining = expires_at - now;
  ((remaining * 100) / u64::from(lifetime)).min(100) as u8
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn formats_bytes_with_scaled_decimals() {
    assert_eq!(format_bytes(512), "512 B");
    assert_eq!(format_bytes(1024), "1.00 KiB");
    assert_eq!(format_bytes(90 * 1024 * 1024 + 400 * 1024), "90.4 MiB");
    assert_eq!(format_bytes(1_320_702_444), "1.23 GiB");
    assert_eq!(format_bytes(250 * 1024 * 1024 * 1024), "250 GiB");
  }

  #[test]
  fn formats_durations_with_two_units() {
    assert_eq!(format_duration_short(36), "36s");
    assert_eq!(format_duration_short(24 * 60 + 36), "24m 36s");
    assert_eq!(format_duration_short(3 * 3600 + 24 * 60), "3h 24m");
    assert_eq!(format_duration_short(2 * 86400 + 3 * 3600 + 5), "2d 3h");
  }

  #[test]
  fn session_percentage_is_remaining_over_lifetime() {
    assert_eq!(session_percentage(Some(1_000), Some(100), 950), 50);
    assert_eq!(session_percentage(Some(1_000), Some(100), 1_000), 0);
    assert_eq!(session_percentage(None, Some(100), 0), 0);
    assert_eq!(session_percentage(Some(1_000), None, 0), 0);
    // Clamped even if expires_at drifts past now + lifetime.
    assert_eq!(session_percentage(Some(10_000), Some(100), 0), 100);
  }

  #[test]
  fn expiry_is_relative_plus_absolute() {
    let now = 1_752_000_000u64;
    let text = format_expiry(now + 3_600, now);

    assert!(text.starts_with("in 1h 0m ("), "got: {}", text);
    assert_eq!(format_expiry(now - 10, now), {
      let local = Local.timestamp_opt((now - 10) as i64, 0).single().unwrap();
      format!("expired ({})", local.format("%H:%M"))
    });
  }

  #[test]
  fn every_state_has_a_distinct_class() {
    let states = [
      None,
      Some(WidgetState::NeedsSetup),
      Some(WidgetState::Disconnected),
      Some(WidgetState::Connected),
      Some(WidgetState::Error),
    ];

    let classes: Vec<_> = states.iter().map(|s| state_style(*s).class).collect();
    let mut deduped = classes.clone();
    deduped.dedup();

    assert_eq!(classes.len(), deduped.len());
  }
}
