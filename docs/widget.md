# gpwidget — bar-widget GUI for GlobalProtect-openconnect

`gpwidget` is an open-source (GPL-3.0) replacement for the proprietary
`gpgui`: a VPN status widget for Wayland bars (waybar, Omarchy Shell and
DankMaterialShell), with a layer-shell popup panel, desktop notifications,
and the same browser-based Okta/SAML login flow as the official client.

```
waybar module ───────── gpwidget status --waybar ──┐
GTK4 layer-shell popup ─ gpwidget popup ───────────┤
Omarchy panel (QML) ──────────────────────────────┤ NDJSON over
DMS plugin (QML) ──────────────────────────────────┼─ $XDG_RUNTIME_DIR/gpwidget.sock
CLI ─────────────────── gpwidget connect|disconnect┘
                                  │
                       gpwidget daemon  ← launched BY `gpclient launch-gui` as "gpgui"
                                  │
              ┌───────────────────┼──────────────────────┐
      Noise-encrypted WS     spawns gpauth          reads OS stats
        (gpservice)      (WebKit Okta popup)    (tun iface, /sys/class/net)
```

## How it works

`gpclient launch-gui` starts gpservice via polkit (no password prompt for
an active local session), receives a per-launch session credential from it,
and passes that credential only to the GUI binary it launches itself. The
install therefore provides `/usr/bin/gpgui` as a regular-file wrapper that
execs the `gpwidget` next to it. The gpwidget daemon authenticates to
gpservice's WebSocket with the credential and serves widgets over a unix
socket. Connecting runs the standard flow — portal prelogin → gpauth
(embedded WebKit window by default; Okta 2FA happens there) → portal config
→ gateway login — and hands the authenticated gateway to gpservice, which
owns the tunnel. Quitting gpwidget ends `gpclient launch-gui`, which stops
gpservice.

When nothing is running there is no socket: widgets show "VPN off" and any
connect action boots the whole stack.

## Install

Built and installed with the rest of the project:

```sh
make build            # includes gpwidget (BUILD_WIDGET=1 default)
sudo make install     # installs /usr/bin/gpwidget, a gpgui wrapper, and assets
```

### RPM (EL10 / Fedora-family)

```sh
sudo dnf install -y rpm-build rpmdevtools \
    gcc-c++ cargo make libtool autoconf automake gettext pkg-config \
    openssl-devel libxml2-devel zlib-devel lz4-devel gnutls-devel \
    p11-kit-devel nettle-devel gmp-devel \
    webkit2gtk4.1-devel gtk4-devel gtk4-layer-shell-devel librsvg2-devel
git submodule update --init --recursive   # bundled openconnect sources
make rpm                                  # output in .build/rpm/
sudo dnf install -y .build/rpm/globalprotect-openconnect-*.$(uname -m).rpm
```

Remove any stray manual installs first (`ls /usr/local/bin/gp*`) — binaries
in /usr/local/bin shadow the packaged ones on most PATHs.

### Arch Linux (incl. Omarchy)

Arch `extra/globalprotect-openconnect` and AUR `globalprotect-openconnect-git`
are upstream packages and do **not** ship `gpwidget`. Build this fork:

```sh
sudo pacman -S --needed \
    base-devel rust git \
    gtk4 gtk4-layer-shell webkit2gtk-4.1 \
    openssl gnutls libxml2 zlib lz4 p11-kit nettle gmp polkit
git submodule update --init crates/openconnect/deps/openconnect
make pkgbuild                                 # output in .build/pkgbuild/
sudo pacman -U .build/pkgbuild/globalprotect-openconnect-*.pkg.tar.zst
```

`make pkgbuild` builds with `BUILD_GUI_HELPER=0` (gpwidget, no proprietary
GUI updater) and Arch `rust` rather than the repo's rustup pin. Direct:

```sh
make build BUILD_GUI_HELPER=0
sudo make install BUILD_GUI_HELPER=0
```

Works on **x86_64** and **aarch64**. After install, copy the Omarchy plugin
as below (or the waybar module under `/usr/share/gpwidget/examples/waybar/`).

## Autostart (optional)

Start the stack at login so the widget shows live status immediately (the
VPN is not dialed unless `auto-connect = true`):

- **niri**: `spawn-at-startup "gpclient" "launch-gui"` in
  `~/.config/niri/config.kdl`, or
- **systemd user unit** (Hyprland / Omarchy / UWSM sessions import
  `graphical-session.target`): copy
  `/usr/share/gpwidget/examples/systemd/gpwidget-stack.service` to
  `~/.config/systemd/user/` and `systemctl --user enable --now gpwidget-stack`.

Without autostart everything still works — the first click on the widget
brings the stack up on demand.

## Configuration — `~/.config/gpwidget/config.toml`

```toml
portal = "vpn.example.com"   # required (or connect once with --portal)
gateway = ""                  # optional pin: gateway name or address
browser = "embedded"          # embedded | default | firefox | chrome | /path/to/browser
notifications = true
auto-connect = false          # connect as soon as the daemon starts
auto-resume = true            # reconnect the last gateway after suspend/resume (re-authenticates)
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
hip = false                   # enable HIP report (generated by gpservice)
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

## Omarchy 4

The native Omarchy Shell plugin ships at
`/usr/share/gpwidget/examples/omarchy`. Install it as a user-owned plugin and
add it to the right side of the bar:

```sh
mkdir -p ~/.config/omarchy/plugins/gpwidget.vpn
cp /usr/share/gpwidget/examples/omarchy/{manifest.json,BarWidget.qml,Service.qml,Model.js} \
   ~/.config/omarchy/plugins/gpwidget.vpn/
# Discover the new plugin before enabling (hot-reload can race a fresh copy).
omarchy-shell shell rescanPlugins
omarchy plugin enable gpwidget.vpn --section right
```

The icon and native Quickshell panel follow Omarchy's theme, font, spacing,
and corner settings. The panel opens beside the clicked widget on its monitor
and uses the shell's focus, outside-click dismissal, and panel switching.
Left-click opens or closes it; right-click connects, cancels authentication,
or disconnects; middle-click refreshes status.

The panel provides portal setup, gateway selection for the next connection,
connection/IP/interface details, live traffic and session countdowns, OTP
entry, and a button to stop the VPN service. Authentication still uses the
existing browser flow. The gateway address is shown only when it differs from
the gateway's display name. Command failures remain visible until retried or the
connection changes state. OTP values are sent only over the daemon socket
and are cleared from the input on submission or panel close.

The setup portal is saved before the connection attempt, even if the attempt
fails. Its draft is cleared when the field hides; subsequent connects use the
configured portal unless you enter an address in the visible setup field.

A shared service maintains one socket connection across all bar monitors,
with automatic reconnection when the daemon restarts. Status is pushed by the
daemon; there is no status subprocess polling. `refreshIntervalSec` from older
plugin installations is ignored. **Copy all four plugin files when upgrading.**
When upgrading from the old GTK-launching plugin, run `omarchy restart shell`
once after copying the files so Qt discovers the new QML service and model.
Subsequent edits to the existing plugin files hot-reload normally.

The default bar item is icon-only. Enable the connected gateway label in the
bar editor or on the layout entry in `~/.config/omarchy/shell.json`:

```json
{ "id": "gpwidget.vpn", "showGateway": true }
```

inside `bar.layout.right` (or whichever section the widget occupies).

Keyboard controls inside the panel: Up/Down or j/k and Tab/Shift+Tab move
between controls; Enter or Space activates; Left/Right or h/l switch to
neighboring shell panels; R refreshes; Esc closes. Text inputs keep normal
editing keys and use Enter to connect or submit the OTP. The gateway dropdown
uses its own arrow/j/k navigation and Enter to select. Selecting a gateway
while connected takes effect on the next connection. The default, **Automatic**,
uses the configured gateway pin or the portal's preferred gateway. Only an
explicit dropdown choice overrides that behavior; the choice is cleared when
the daemon disconnects, the portal changes, or that gateway is no longer listed.
An incompatible daemon protocol leaves a visible error and pauses reconnects;
use Refresh after updating the plugin or daemon to negotiate again.

The GTK popup remains available through `gpwidget popup` for Waybar and other
bars.

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
