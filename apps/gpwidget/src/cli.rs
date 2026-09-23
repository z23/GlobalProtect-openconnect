use anyhow::bail;
use clap::{Parser, Subcommand};
use gpapi::{
  clap::InfoLevelVerbosity,
  utils::{base64, env_utils},
};
use log::info;

use crate::{client, config::Config, daemon, popup};

// Shown by `gpwidget --version` when no gpservice binary can be queried.
// GuiLauncher compares the second whitespace token with gpservice's
// CARGO_PKG_VERSION and, on a mismatch, downloads the proprietary GUI over
// the gpgui path. The running binary reports gpservice's own token instead
// of this compile-time string. See `version_token`.
pub const VERSION: &str = concat!(
  env!("CARGO_PKG_VERSION"),
  " (",
  env!("GPWIDGET_GIT_COMMIT"),
  " ",
  compile_time::date_str!(),
  ")"
);

// The all-zeros key gpservice uses in debug `--no-gui` mode (gpapi only
// exports it in debug builds; gpgui-helper keeps the same local copy).
const GP_API_KEY: &[u8; 32] = &[0; 32];

#[derive(Parser)]
#[command(version = VERSION, about = "GlobalProtect VPN status widget for waybar and DMS")]
struct Cli {
  /// Read the WS api key from stdin (passed by gpservice when it launches the daemon)
  #[arg(long)]
  api_key_on_stdin: bool,

  /// Accepted for gpservice compatibility; ignored
  #[arg(long, hide = true)]
  minimized: bool,

  #[command(subcommand)]
  command: Option<Command>,

  #[command(flatten)]
  verbose: InfoLevelVerbosity,
}

#[derive(Subcommand)]
enum Command {
  /// Print VPN status (one snapshot, or a continuous stream for waybar)
  Status {
    /// Emit waybar custom-module JSON lines continuously
    #[arg(long)]
    waybar: bool,
    /// Follow status updates as JSON lines instead of printing one snapshot
    #[arg(long)]
    follow: bool,
  },
  /// Connect the VPN (starts the service stack if needed)
  Connect {
    /// Gateway name or address (overrides the configured pin)
    #[arg(long)]
    gateway: Option<String>,
    /// Portal address (persisted to the config on success)
    #[arg(long)]
    portal: Option<String>,
  },
  /// Disconnect the VPN
  Disconnect,
  /// Connect if disconnected, disconnect otherwise
  Toggle,
  /// Show the status popup panel (layer-shell); invoke again to close
  Popup,
  /// Disconnect and shut down the VPN service stack
  Quit,
}

impl Cli {
  fn read_api_key(&self) -> anyhow::Result<Vec<u8>> {
    if self.api_key_on_stdin {
      let mut api_key = String::new();
      std::io::stdin().read_line(&mut api_key)?;

      Ok(base64::decode_to_vec(api_key.trim())?)
    } else {
      // Matches gpservice's debug-only `--no-gui` key so the daemon can be
      // developed against it without the full launch chain.
      Ok(GP_API_KEY.to_vec())
    }
  }
}

fn init_logger(cli: &Cli) {
  env_logger::builder()
    .filter_level(cli.verbose.log_level_filter())
    .init();
}

pub fn run() {
  // Intercept before clap. Clap would print CARGO_PKG_VERSION, which matches
  // gpservice only when both binaries come from one workspace build.
  if is_version_flag(std::env::args().nth(1).as_deref()) {
    println!("gpwidget {}", version_token());
    return;
  }

  let cli = Cli::parse();

  init_logger(&cli);

  let result = match &cli.command {
    // GTK owns the main thread; everything async runs on runtimes it creates.
    Some(Command::Popup) => popup::run(),
    _ => run_async(cli),
  };

  if let Err(err) = result {
    eprintln!("Error: {}", err);
    std::process::exit(1);
  }
}

fn is_version_flag(arg: Option<&str>) -> bool {
  matches!(arg, Some("--version" | "-V"))
}

/// Second whitespace field of `gpservice --version` (`2.6.5`, not the commit
/// or the build date). That is the token `GuiLauncher::check_version` requires.
pub(crate) fn version_token_from_stdout(stdout: &str) -> Option<&str> {
  stdout.split_whitespace().nth(1)
}

pub(crate) fn version_token_or_fallback(stdout: Option<&str>) -> String {
  stdout
    .and_then(version_token_from_stdout)
    .unwrap_or(env!("CARGO_PKG_VERSION"))
    .to_string()
}

fn version_token() -> String {
  version_token_or_fallback(gpservice_version_stdout().as_deref())
}

fn gpservice_version_stdout() -> Option<String> {
  let mut candidates = Vec::new();

  if let Ok(exe) = std::env::current_exe() {
    if let Some(dir) = exe.parent() {
      candidates.push(dir.join("gpservice"));
    }
  }

  // Absolute path: gpservice clears the GUI environment when an env file is
  // set, so PATH may be empty.
  candidates.push(std::path::PathBuf::from("/usr/bin/gpservice"));

  for path in candidates {
    let Ok(output) = std::process::Command::new(&path).arg("--version").output() else {
      continue;
    };

    if output.status.success() {
      return Some(String::from_utf8_lossy(&output.stdout).into_owned());
    }
  }

  None
}

fn run_async(cli: Cli) -> anyhow::Result<()> {
  let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;

  runtime.block_on(async {
    match cli.command {
      None => {
        if !cli.api_key_on_stdin && !cfg!(debug_assertions) {
          bail!(
            "gpwidget's daemon mode is launched by gpservice (via `gpclient launch-gui`), not directly.\n\
             Use `gpwidget connect` to bring the VPN stack up, or `gpwidget status` to inspect it."
          );
        }

        info!("gpwidget daemon started: {}", VERSION);
        env_utils::patch_gui_runtime_env(false);

        let api_key = cli.read_api_key()?;
        let config = Config::load()?;
        let exit_code = daemon::run(api_key, config).await?;

        std::process::exit(exit_code);
      }
      Some(Command::Status { waybar, follow }) => client::status(waybar, follow).await,
      Some(Command::Connect { gateway, portal }) => client::connect(gateway, portal).await,
      Some(Command::Disconnect) => client::disconnect().await,
      Some(Command::Toggle) => client::toggle().await,
      Some(Command::Quit) => client::quit().await,
      Some(Command::Popup) => unreachable!("handled before the runtime starts"),
    }
  })
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn version_token_is_the_second_field() {
    assert_eq!(
      version_token_from_stdout("gpservice 2.6.5 (abc1234 2026-08-05)"),
      Some("2.6.5")
    );
  }

  #[test]
  fn missing_gpservice_stdout_falls_back_to_crate_version() {
    assert_eq!(version_token_or_fallback(None), env!("CARGO_PKG_VERSION"));
    assert_eq!(version_token_or_fallback(Some("gpservice")), env!("CARGO_PKG_VERSION"));
    assert_eq!(version_token_or_fallback(Some("gpservice 2.6.5 (abc)")), "2.6.5");
  }
}
