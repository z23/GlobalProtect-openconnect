# gpwidget — bar-widget GUI for GlobalProtect-openconnect

`gpwidget` is an open-source (GPL-3.0) replacement for the proprietary
`gpgui`: a VPN status widget for Wayland bars (waybar and
DankMaterialShell), with a layer-shell popup panel, desktop notifications,
and the same browser-based Okta/SAML login flow as the official client.

```
waybar module ───────── gpwidget status --waybar ──┐
GTK4 layer-shell popup ─ gpwidget popup ───────────┤ NDJSON over
DMS plugin (QML) ──────────────────────────────────┼─ $XDG_RUNTIME_DIR/gpwidget.sock
CLI ─────────────────── gpwidget connect|disconnect┘
                                  │
                       gpwidget daemon  ← launched BY gpservice as "gpgui"
                                  │
              ┌───────────────────┼──────────────────────┐
        encrypted WS         spawns gpauth          reads OS stats
        (gpservice)      (WebKit Okta popup)    (tun iface, /sys/class/net)
```

## How it works

gpservice hands its per-launch WebSocket key only to the GUI binary it
launches itself, so `gpwidget` is installed with a `gpgui → gpwidget`
symlink. `gpclient launch-gui` starts gpservice via polkit (no password
prompt for an active local session); gpservice launches the gpwidget daemon,
which serves widgets over a unix socket. Connecting runs the standard flow —
portal prelogin → gpauth (embedded WebKit window by default; Okta 2FA
happens there) → portal config → gateway login — and hands the resulting
cookie to gpservice, which owns the tunnel.

When nothing is running there is no socket: widgets show "VPN off" and any
connect action boots the whole stack.

## Install

Built and installed with the rest of the project:

```sh
make build            # includes gpwidget (BUILD_WIDGET=1 default)
sudo make install     # installs /usr/bin/gpwidget + gpgui symlink + assets
```

## Configuration — `~/.config/gpwidget/config.toml`

```toml
portal = "vpn.example.com"   # required (or connect once with --portal)
gateway = ""                  # optional pin: gateway name or address
browser = "embedded"          # embedded | default | firefox | chrome | /path/to/browser
notifications = true
auto-connect = false          # connect as soon as the daemon starts
auto-resume = true            # replay the last connection after suspend/resume
stats-interval-secs = 2

[waybar]
show-gateway = true           # gateway name next to the icon

[popup]
edge = "top-right"            # top-left | top-right | bottom-left | bottom-right
margin = [8, 8]               # [vertical, horizontal] px from the anchored corner

[advanced]
ignore-tls-errors = false
fix-openssl = false           # allow OpenSSL legacy renegotiation (old portal TLS
                              # stacks; same as gpclient --fix-openssl). Applied at
                              # daemon start — quit + reconnect after changing it.
mtu = 0
reconnect-timeout = 300
disable-ipv6 = false
no-dtls = false
hip = false                   # enable HIP report (uses the bundled hipreport.sh)
# certificate = ""            # client certificate (pem/p12)
# sslkey = ""
# client-version = ""
```

The config is re-read on every connect attempt — no restart needed.

## waybar

Example module and CSS are installed at
`/usr/share/gpwidget/examples/waybar/` (source:
`apps/gpwidget/assets/waybar/`). The module runs
`gpwidget status --waybar` in continuous mode; left-click opens the popup,
right-click toggles the connection. States map to CSS classes:
`connected`, `connected expiring`, `connecting`, `disconnected`,
`stack-down`, `needs-setup`, `error`.

## DankMaterialShell

The plugin ships at `/usr/share/gpwidget/dms/GlobalProtectVPN` (source:
`packaging/dms/GlobalProtectVPN/`); see its README:

```sh
ln -s /usr/share/gpwidget/dms/GlobalProtectVPN \
      ~/.config/DankMaterialShell/plugins/GlobalProtectVPN
dms ipc call plugins reload globalProtectVpn
```

It provides a bar pill, a popout (status, gateway picker, stats,
connect/disconnect), and a control-center VPN toggle.

## Popup

`gpwidget popup` opens a layer-shell panel anchored to a screen corner
(configurable). Invoking it again — e.g. clicking the bar widget once more —
closes it; Esc also closes. It shows live status/stats, a gateway picker, an
OTP prompt when a gateway demands MFA, and buttons to
connect/disconnect/quit the service stack. It works with the stack down too
("Start VPN").

## CLI

```
gpwidget status [--waybar|--follow]   # one snapshot, waybar stream, or follow
gpwidget connect [--portal X] [--gateway Y]
gpwidget disconnect
gpwidget toggle
gpwidget quit                          # disconnect and stop gpservice
```

## Notes

- Username/password (non-SAML) portals are not supported by the widget —
  use `gpclient connect`.
- A CLI tunnel (`sudo gpclient connect`) runs in a separate process tree;
  the widget cannot control it.
- The session-expiry warning honors the portal's configured warning lead
  time, falling back to 10 minutes.
