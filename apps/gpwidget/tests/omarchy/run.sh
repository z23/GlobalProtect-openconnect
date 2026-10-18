#!/bin/sh
# Run actual Quickshell socket/process lifecycles, isolated from the user's VPN.
set -eu
root=$(CDPATH= cd -- "$(dirname "$0")/../../../.." && pwd)
shell_root=${OMARCHY_PATH:-/usr/share/omarchy}/shell
test -d "$shell_root/Ui" || { echo 'Omarchy 4 shell components are required' >&2; exit 1; }
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT HUP INT TERM
mkdir -p "$tmp/runtime" "$tmp/cache" "$tmp/bin"
chmod 700 "$tmp/runtime"
ln -s "$shell_root/Ui" "$tmp/Ui"
ln -s "$shell_root/Commons" "$tmp/Commons"
cp "$root/apps/gpwidget/tests/omarchy/shell.qml" "$tmp/shell.qml"
printf '#!/bin/sh\necho "INFO Starting VPN" >&2\necho "Error: fixture authorization denied" >&2\nexit 1\n' > "$tmp/bin/gpwidget"
chmod 755 "$tmp/bin/gpwidget"
case ${WAYLAND_DISPLAY:-} in
  /*) display_socket=$WAYLAND_DISPLAY ;;
  *) display_socket=${XDG_RUNTIME_DIR:-/run/user/$(id -u)}/${WAYLAND_DISPLAY:-wayland-0} ;;
esac
QT_QPA_PLATFORM=wayland WAYLAND_DISPLAY="$display_socket" QT_QPA_PLATFORMTHEME=generic QT_QUICK_CONTROLS_STYLE=Basic XDG_RUNTIME_DIR="$tmp/runtime" XDG_CACHE_HOME="$tmp/cache" \
  GPWIDGET_TEST_SOURCE="$root/apps/gpwidget/assets/omarchy" GPWIDGET_TEST_SOCKET="$tmp/vpn.sock" \
  PATH="$tmp/bin:$PATH" timeout --kill-after=2s 35s quickshell --no-color -p "$tmp" > "$tmp/log" 2>&1 || { cat "$tmp/log"; exit 1; }
cat "$tmp/log"
if rg '(ReferenceError|TypeError|Unable to (load|assign)|Failed to load|Cannot assign|Binding loop|Error:|FAIL:|has crashed)' "$tmp/log"; then exit 1; fi
rg 'PASS: native Omarchy widget runtime' "$tmp/log" > /dev/null
