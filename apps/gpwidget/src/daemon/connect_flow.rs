//! The portal → SAML auth → gateway login orchestration, ending in a
//! `WsRequest::Connect` handed to gpservice.
//!
//! This mirrors gpclient's `apps/gpclient/src/connect/*` flow, minus the
//! interactive TTY prompts: SAML auth pops the gpauth browser window and the
//! gateway-MFA edge is answered over the widget socket.

use std::fmt;

use anyhow::{Context, bail};
use gpapi::{
  credential::Credential,
  gateway::{Gateway, GatewayLogin, GatewayLoginContext, GatewaySelection, gateway_login_with_context},
  gp_params::GpParams,
  os_profile::{OsProfile, runtime_client_os},
  portal::{Prelogin, PreloginOptions, SamlPrelogin, prelogin, retrieve_config},
  process::auth_launcher::SamlAuthLauncher,
  service::{
    request::{ConnectRequest, WsRequest},
    vpn_env::VpnEnv,
    vpn_state::ConnectInfo,
  },
};
use log::{info, warn};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::config::{BrowserMode, Config};

const OTP_TIMEOUT_SECS: u64 = 120;

/// Marker error for user-initiated cancellation (no error toast).
#[derive(Debug)]
pub struct Cancelled;

impl fmt::Display for Cancelled {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "cancelled")
  }
}

impl std::error::Error for Cancelled {}

pub fn is_cancelled(err: &anyhow::Error) -> bool {
  err.downcast_ref::<Cancelled>().is_some() || err.to_string().contains("Authentication cancelled")
}

/// Messages the flow reports back to the daemon core.
pub enum FlowMsg {
  /// Gateway login demands an OTP; answer through `respond`.
  OtpPrompt {
    message: String,
    respond: oneshot::Sender<String>,
  },
  /// The ConnectRequest was handed to gpservice; cache it for resume.
  Submitted { request: Box<ConnectRequest> },
  Failed {
    error: String,
    cancelled: bool,
  },
}

pub struct FlowCtx {
  pub portal: String,
  pub gateway_override: Option<String>,
  pub config: Config,
  pub vpn_env: VpnEnv,
  pub ws_tx: mpsc::Sender<WsRequest>,
  pub flow_tx: mpsc::Sender<FlowMsg>,
  pub cancel: CancellationToken,
}

pub fn spawn(ctx: FlowCtx) -> tokio::task::JoinHandle<()> {
  tokio::spawn(async move {
    let flow_tx = ctx.flow_tx.clone();

    match run(&ctx).await {
      Ok(request) => {
        let _ = flow_tx.send(FlowMsg::Submitted { request }).await;
      }
      Err(err) => {
        let cancelled = is_cancelled(&err);

        if cancelled {
          info!("Connect flow cancelled");
        } else {
          warn!("Connect flow failed: {:?}", err);
        }

        let _ = flow_tx
          .send(FlowMsg::Failed {
            error: format!("{:#}", err),
            cancelled,
          })
          .await;
      }
    }
  })
}

async fn run(ctx: &FlowCtx) -> anyhow::Result<Box<ConnectRequest>> {
  let portal = ctx.portal.as_str();
  let config = &ctx.config;

  let os_profile = build_os_profile(ctx);
  let gp_params = build_gp_params(config, &os_profile);
  let external_browser = config.browser_mode() != BrowserMode::Embedded;

  // 1. Portal prelogin → SAML request.
  info!("Portal prelogin: {}", portal);
  let portal_prelogin = cancellable(
    &ctx.cancel,
    prelogin(
      portal,
      &gp_params,
      PreloginOptions::default().external_browser_requested(external_browser),
    ),
  )
  .await
  .context("Portal prelogin failed")?;

  // 2. SAML auth via gpauth (browser window pops here).
  let cred = obtain_credential(ctx, &portal_prelogin, portal, false, &os_profile).await?;

  // 3. Portal config → gateways.
  let mut portal_config = cancellable(&ctx.cancel, retrieve_config(portal, &cred, &gp_params))
    .await
    .context("Failed to retrieve the portal config")?;

  portal_config.sort_gateways(portal_prelogin.region());

  let auth_cookie = portal_config.auth_cookie().clone();
  let allow_extend_session = portal_config.allow_extend_session().unwrap_or(false);
  let portal_default_browser = portal_config.default_browser().unwrap_or(false);

  if portal_config.gateways().is_empty() {
    bail!("The portal returned no gateways");
  }

  // 4. Gateway selection: command override > config pin > preferred by region.
  let pin = ctx.gateway_override.as_deref().or(config.gateway.as_deref());
  let (selected_gateway, selection) = match pin {
    Some(pin) => {
      let gateway = portal_config
        .find_gateway(pin)
        .with_context(|| format!("Cannot find gateway: {}", pin))?;
      (gateway.clone(), GatewaySelection::Manual)
    }
    None => (
      portal_config.find_preferred_gateway(portal_prelogin.region()).clone(),
      GatewaySelection::Auto,
    ),
  };

  info!("Selected gateway: {}", selected_gateway);

  // 5. Gateway login: portal cookie first, gateway prelogin (SAML) fallback.
  let mut gw_params = gp_params.clone();
  gw_params.set_is_gateway(true);

  let gateway_browser_allowed = portal_default_browser || external_browser;
  let context = GatewayLoginContext::new(&selected_gateway, selection).with_connect_method(portal_config.connect_method());

  let cookie = gateway_login_with_fallback(
    ctx,
    &selected_gateway,
    &auth_cookie.can_authenticate_gateway().then(|| (&auth_cookie).into()),
    &gw_params,
    &context,
    gateway_browser_allowed,
    &os_profile,
  )
  .await?;

  // 6. Hand the finished cookie to gpservice.
  let gateways: Vec<Gateway> = portal_config.gateways().into_iter().cloned().collect();
  let info = ConnectInfo::new(portal.to_string(), selected_gateway, gateways);

  let advanced = &config.advanced;
  let csd_wrapper = advanced.hip.then(|| ctx.vpn_env.csd_wrapper.clone()).flatten();

  let request = ConnectRequest::new(info, cookie)
    .with_os_profile(&os_profile)
    .with_vpnc_script(ctx.vpn_env.vpnc_script.clone())
    .with_hip(advanced.hip)
    .with_csd_wrapper(csd_wrapper)
    .with_csd_uid(uzers::get_current_uid())
    .with_allow_extend_session(allow_extend_session)
    .with_certificate(advanced.certificate.clone())
    .with_sslkey(advanced.sslkey.clone())
    .with_key_password(advanced.key_password.clone())
    .with_mtu(advanced.mtu)
    .with_reconnect_timeout(advanced.reconnect_timeout)
    .with_disable_ipv6(advanced.disable_ipv6)
    .with_no_dtls(advanced.no_dtls);

  let request = Box::new(request);

  ctx
    .ws_tx
    .send(WsRequest::Connect(request.clone()))
    .await
    .context("Failed to send the connect request to gpservice")?;

  info!("Connect request sent to gpservice");

  Ok(request)
}

fn build_os_profile(ctx: &FlowCtx) -> OsProfile {
  // Host identity comes from gpservice's VpnEnv so the gateway sees the same
  // host-id regardless of which component authenticates.
  let mut builder =
    OsProfile::builder(runtime_client_os()).host_identity(ctx.vpn_env.host_info.host_identity.clone());

  if let Some(client_version) = ctx.config.advanced.client_version.as_deref() {
    builder = builder.client_version(client_version.to_string());
  }

  builder.build()
}

fn build_gp_params(config: &Config, os_profile: &OsProfile) -> GpParams {
  let advanced = &config.advanced;
  let mut builder = GpParams::builder(os_profile.clone());

  builder
    .ignore_tls_errors(advanced.ignore_tls_errors)
    .certificate(advanced.certificate.clone())
    .sslkey(advanced.sslkey.clone())
    .key_password(advanced.key_password.clone());

  builder.build()
}

async fn obtain_credential(
  ctx: &FlowCtx,
  prelogin: &Prelogin,
  server: &str,
  gateway_browser_allowed: bool,
  os_profile: &OsProfile,
) -> anyhow::Result<Credential> {
  match prelogin {
    Prelogin::Saml(saml) => saml_auth(ctx, saml, server, prelogin.is_gateway(), gateway_browser_allowed, os_profile).await,
    Prelogin::Standard(_) => bail!(
      "This server uses username/password authentication, which gpwidget does not support yet — use `gpclient connect` instead"
    ),
  }
}

async fn saml_auth(
  ctx: &FlowCtx,
  saml: &SamlPrelogin,
  server: &str,
  is_gateway: bool,
  gateway_browser_allowed: bool,
  os_profile: &OsProfile,
) -> anyhow::Result<Credential> {
  let config = &ctx.config;
  let advanced = &config.advanced;

  // External browser only works when the server advertises support (and,
  // for gateways, when the portal allows it); otherwise fall back to the
  // embedded webview rather than failing.
  let external_supported = saml.support_default_browser() && (!is_gateway || gateway_browser_allowed);
  let browser_mode = config.browser_mode();

  let (named_browser, use_default_browser) = match &browser_mode {
    BrowserMode::Embedded => (None, false),
    BrowserMode::Default if external_supported => (None, true),
    BrowserMode::Named(name) if external_supported => (Some(name.as_str()), false),
    other => {
      warn!(
        "Browser mode {:?} not supported by {} — falling back to the embedded webview",
        other, server
      );
      (None, false)
    }
  };

  info!(
    "SAML auth: server={}, gateway={}, browser={}",
    server,
    is_gateway,
    if use_default_browser {
      "default"
    } else {
      named_browser.unwrap_or("embedded")
    }
  );

  let launcher = SamlAuthLauncher::new(server)
    .auth_executable(Some(&ctx.vpn_env.auth_executable))
    .gateway(is_gateway)
    .saml_request(saml.saml_request())
    .os_profile(os_profile)
    .fix_openssl(advanced.fix_openssl)
    .ignore_tls_errors(advanced.ignore_tls_errors)
    .certificate(advanced.certificate.as_deref())
    .sslkey(advanced.sslkey.as_deref())
    .key_password(advanced.key_password.as_deref())
    .browser(named_browser)
    .hidpi(config.hidpi)
    .default_browser(use_default_browser);

  // Dropping the launch future kills gpauth (kill_on_drop), so cancellation
  // closes the login window.
  cancellable(&ctx.cancel, launcher.launch())
    .await
    .context("SAML authentication failed")
}

#[allow(clippy::too_many_arguments)]
async fn gateway_login_with_fallback(
  ctx: &FlowCtx,
  gateway: &Gateway,
  portal_cred: &Option<Credential>,
  gw_params: &GpParams,
  context: &GatewayLoginContext,
  gateway_browser_allowed: bool,
  os_profile: &OsProfile,
) -> anyhow::Result<String> {
  if let Some(portal_cred) = portal_cred {
    match login_gateway(ctx, gateway.server(), portal_cred, gw_params, context).await {
      Ok(cookie) => {
        info!("Gateway login with portal auth cookies succeeded");
        return Ok(cookie);
      }
      Err(err) if is_cancelled(&err) => return Err(err),
      Err(err) => {
        info!(
          "Gateway login with portal auth cookies failed ({}), falling back to gateway prelogin",
          err
        );
      }
    }
  } else {
    info!("Portal config did not provide gateway auth cookies; using gateway prelogin flow");
  }

  let external_browser = ctx.config.browser_mode() != BrowserMode::Embedded;
  let gateway_prelogin = cancellable(
    &ctx.cancel,
    prelogin(
      gateway.server(),
      gw_params,
      PreloginOptions::default()
        .external_browser_requested(external_browser)
        .gateway_external_browser_allowed(gateway_browser_allowed),
    ),
  )
  .await
  .context("Gateway prelogin failed")?;

  let gateway_cred = obtain_credential(ctx, &gateway_prelogin, gateway.server(), gateway_browser_allowed, os_profile).await?;

  login_gateway(ctx, gateway.server(), &gateway_cred, gw_params, context).await
}

async fn login_gateway(
  ctx: &FlowCtx,
  gateway: &str,
  cred: &Credential,
  gw_params: &GpParams,
  context: &GatewayLoginContext,
) -> anyhow::Result<String> {
  let mut gw_params = gw_params.clone();

  loop {
    let login = cancellable(
      &ctx.cancel,
      gateway_login_with_context(gateway, cred, &gw_params, context),
    )
    .await?;

    match login {
      GatewayLogin::Cookie(cookie) => return Ok(cookie),
      GatewayLogin::Mfa(message, input_str) => {
        info!("Gateway login requires MFA: {}", message);

        let otp = request_otp(ctx, &message).await?;
        gw_params.set_input_str(&input_str);
        gw_params.set_otp(&otp);

        info!("Retrying gateway login with MFA...");
      }
    }
  }
}

async fn request_otp(ctx: &FlowCtx, message: &str) -> anyhow::Result<String> {
  let (respond, otp_rx) = oneshot::channel();

  ctx
    .flow_tx
    .send(FlowMsg::OtpPrompt {
      message: message.to_string(),
      respond,
    })
    .await
    .map_err(|_| anyhow::Error::new(Cancelled))?;

  let otp = cancellable(&ctx.cancel, async {
    tokio::time::timeout(std::time::Duration::from_secs(OTP_TIMEOUT_SECS), otp_rx)
      .await
      .context("Timed out waiting for the one-time password")?
      .context("OTP prompt was dismissed")
  })
  .await?;

  Ok(otp)
}

async fn cancellable<T>(
  cancel: &CancellationToken,
  fut: impl Future<Output = anyhow::Result<T>>,
) -> anyhow::Result<T> {
  tokio::select! {
    _ = cancel.cancelled() => Err(anyhow::Error::new(Cancelled)),
    result = fut => result,
  }
}
