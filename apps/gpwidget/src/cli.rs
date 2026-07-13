use anyhow::bail;
use clap::{Parser, Subcommand};
use gpapi::{
  clap::InfoLevelVerbosity,
  utils::{base64, env_utils},
};
use log::info;

use crate::{client, config::Config, daemon, popup};

// The second whitespace token must equal gpservice's CARGO_PKG_VERSION:
// GuiLauncher::check_version parses `split_whitespace().nth(1)` from
// `gpwidget --version` before launching us as the GUI.
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
  env_logger::builder().filter_level(cli.verbose.log_level_filter()).init();
}

pub fn run() {
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
