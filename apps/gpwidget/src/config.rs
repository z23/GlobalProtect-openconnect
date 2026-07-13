use std::path::PathBuf;

use anyhow::Context;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrowserMode {
  Embedded,
  Default,
  Named(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Config {
  pub portal: Option<String>,
  /// Gateway pin: matched against gateway name or address.
  pub gateway: Option<String>,
  /// "embedded" | "default" | browser name or path (firefox, chrome, /usr/bin/…)
  pub browser: String,
  pub hidpi: bool,
  pub notifications: bool,
  pub auto_connect: bool,
  /// Re-send the cached connect request on ResumeConnection (suspend/resume).
  pub auto_resume: bool,
  pub stats_interval_secs: u64,
  pub waybar: WaybarConfig,
  pub popup: PopupConfig,
  pub advanced: AdvancedConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct WaybarConfig {
  pub show_gateway: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct PopupConfig {
  /// Screen corner the popup anchors to: top-right, top-left, bottom-right, bottom-left.
  pub edge: String,
  /// Margin from the anchored edges, pixels: [vertical, horizontal].
  pub margin: [i32; 2],
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct AdvancedConfig {
  pub ignore_tls_errors: bool,
  /// Allow OpenSSL legacy renegotiation (same as gpclient's --fix-openssl);
  /// needed for portals behind old TLS stacks.
  pub fix_openssl: bool,
  pub client_version: Option<String>,
  pub certificate: Option<String>,
  pub sslkey: Option<String>,
  pub key_password: Option<String>,
  pub mtu: u32,
  pub reconnect_timeout: u32,
  pub disable_ipv6: bool,
  pub no_dtls: bool,
  pub hip: bool,
}

impl Default for Config {
  fn default() -> Self {
    Self {
      portal: None,
      gateway: None,
      browser: "embedded".into(),
      hidpi: false,
      notifications: true,
      auto_connect: false,
      auto_resume: true,
      stats_interval_secs: 2,
      waybar: WaybarConfig::default(),
      popup: PopupConfig::default(),
      advanced: AdvancedConfig::default(),
    }
  }
}

impl Default for WaybarConfig {
  fn default() -> Self {
    Self { show_gateway: true }
  }
}

impl Default for PopupConfig {
  fn default() -> Self {
    Self {
      edge: "top-right".into(),
      margin: [8, 8],
    }
  }
}

impl Default for AdvancedConfig {
  fn default() -> Self {
    Self {
      ignore_tls_errors: false,
      fix_openssl: false,
      client_version: None,
      certificate: None,
      sslkey: None,
      key_password: None,
      mtu: 0,
      reconnect_timeout: 300,
      disable_ipv6: false,
      no_dtls: false,
      hip: false,
    }
  }
}

impl Config {
  pub fn path() -> PathBuf {
    directories::ProjectDirs::from("com.yuezk", "GlobalProtect-openconnect", "gpwidget")
      .map(|dirs| dirs.config_dir().join("config.toml"))
      .unwrap_or_else(|| PathBuf::from("/etc/gpwidget/config.toml"))
  }

  /// Missing file yields the defaults; a malformed file is an error so a typo
  /// doesn't silently reset the portal.
  pub fn load() -> anyhow::Result<Self> {
    let path = Self::path();

    let content = match std::fs::read_to_string(&path) {
      Ok(content) => content,
      Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
      Err(err) => return Err(err).context(format!("Failed to read {}", path.display())),
    };

    toml::from_str(&content).context(format!("Failed to parse {}", path.display()))
  }

  pub fn save(&self) -> anyhow::Result<()> {
    let path = Self::path();

    if let Some(dir) = path.parent() {
      std::fs::create_dir_all(dir)?;
    }

    let content = toml::to_string_pretty(self)?;
    std::fs::write(&path, content).context(format!("Failed to write {}", path.display()))?;

    Ok(())
  }

  pub fn browser_mode(&self) -> BrowserMode {
    match self.browser.trim() {
      "" | "embedded" => BrowserMode::Embedded,
      "default" => BrowserMode::Default,
      other => BrowserMode::Named(other.to_string()),
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn defaults_are_sensible() {
    let config = Config::default();

    assert_eq!(config.browser_mode(), BrowserMode::Embedded);
    assert!(config.notifications);
    assert!(config.auto_resume);
    assert_eq!(config.stats_interval_secs, 2);
    assert_eq!(config.advanced.reconnect_timeout, 300);
  }

  #[test]
  fn parses_kebab_case_and_unknown_browser() {
    let config: Config = toml::from_str(
      r#"
        portal = "vpn.example.com"
        browser = "firefox"
        auto-connect = true

        [advanced]
        ignore-tls-errors = true
      "#,
    )
    .unwrap();

    assert_eq!(config.portal.as_deref(), Some("vpn.example.com"));
    assert_eq!(config.browser_mode(), BrowserMode::Named("firefox".into()));
    assert!(config.auto_connect);
    assert!(config.advanced.ignore_tls_errors);
  }

  #[test]
  fn roundtrips_through_save_format() {
    let mut config = Config::default();
    config.portal = Some("vpn.example.com".into());

    let text = toml::to_string_pretty(&config).unwrap();
    let parsed: Config = toml::from_str(&text).unwrap();

    assert_eq!(parsed.portal.as_deref(), Some("vpn.example.com"));
  }
}
