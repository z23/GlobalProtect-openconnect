# GlobalProtect VPN — DankMaterialShell plugin

A bar widget for [DankMaterialShell](https://github.com/AvengeMedia/DankMaterialShell)
showing GlobalProtect VPN status with a click popout (status, gateway picker,
stats, connect/disconnect) and a control-center toggle.

It is a thin client of the `gpwidget` daemon from GlobalProtect-openconnect:
it reads newline-delimited JSON from `$XDG_RUNTIME_DIR/gpwidget.sock` and
sends commands back over the same socket. When the socket is absent (VPN
service not running) the widget renders "VPN off" and *Start VPN* launches
the whole stack via `gpwidget connect` (polkit prompt-free for active local
sessions).

## Requirements

- `gpwidget` (and the rest of GlobalProtect-openconnect) installed — the
  `gpwidget` binary must be on `PATH`.
- A portal configured in `~/.config/gpwidget/config.toml` (or connect once
  from the CLI: `gpwidget connect --portal vpn.example.com`).
- DankMaterialShell with the plugin system (tested against DMS ≥ 1.5).

## Install

```sh
ln -s /usr/share/gpwidget/dms/GlobalProtectVPN \
      ~/.config/DankMaterialShell/plugins/GlobalProtectVPN
dms ipc call plugins reload globalProtectVpn   # or Settings → Plugins → Scan
```

(From a source checkout, symlink `packaging/dms/GlobalProtectVPN` instead.)

Then enable **GlobalProtect VPN** in DMS Settings → Plugins and add the
widget to a bar. The control-center toggle appears automatically.

## Settings

Only presentation options live in DMS (gateway name on the pill). Connection
configuration (portal, gateway pin, browser mode, notifications) lives in
`~/.config/gpwidget/config.toml`, shared with the waybar module, the GTK
popup and the CLI.

## Socket protocol

The canonical schema is `apps/gpwidget/src/proto.rs`; `Formatting.js` mirrors
`apps/gpwidget/src/ux.rs` formatting rules. If you bump one, bump the other.

Status message example:

```json
{"type":"status","state":"connected",
 "portal":"vpn.example.com",
 "gateway":{"name":"AU-Perth","address":"gw.example.com"},
 "gateways":[{"name":"AU-Perth","address":"gw.example.com"}],
 "conn":{"since":1760310000,"ifname":"tun0","ipv4":"10.8.1.23",
         "rxBytes":1310720000,"txBytes":91750400,"rxRate":0,"txRate":0},
 "session":{"expiresAt":1760353200,"lifetimeSecs":43200,
            "warnPriorSecs":3600,"allowExtend":true},
 "error":null}
```

Commands: `{"type":"connect","gateway":"NAME"?}`, `{"type":"disconnect"}`,
`{"type":"toggle"}`, `{"type":"quit"}`, `{"type":"get-status"}`,
`{"type":"submit-otp","otp":"123456"}` (each may carry an `id` for an ack).

## Notes

- The DMS plugin API (`PluginComponent`, `PopoutComponent`, settings
  components) moves quickly; this plugin was written against the API
  documented at danklinux.com in July 2026 and the idioms used by the
  first-party `dms-plugins` repository. If a DMS update renames a property,
  the fix is almost certainly confined to `GlobalProtectVPN.qml`.
- OTP-based gateway MFA prompts are surfaced by the GTK popup
  (`gpwidget popup`), not by this plugin, in v1.
