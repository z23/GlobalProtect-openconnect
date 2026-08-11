import QtQuick
import Quickshell
import Quickshell.Io
import qs.Ui

// Native Omarchy bar adapter for the gpwidget daemon. The full controls stay
// in gpwidget's GTK layer-shell popup, while this item follows Omarchy's bar
// geometry, colors, tooltip and multi-monitor conventions.
BarWidget {
  id: root
  moduleName: "gpwidget.vpn"

  // Every state the daemon is known to report. Anything outside this set gets
  // the fault presentation rather than falling through to idle — a renamed or
  // added state from a version-mismatched daemon must not read as "VPN off"
  // while the tunnel is actually up.
  readonly property var knownStates: [
    "stack-down", "needs-setup", "disconnected", "authenticating",
    "connecting", "connected", "disconnecting", "error"
  ]

  property var snapshot: ({ state: "stack-down" })
  // Identity of the payload behind `snapshot`. Steady state is the common case,
  // so re-assigning an equivalent snapshot every poll would re-evaluate every
  // dependent binding in every bar instance for zero visual change.
  //
  // Keyed on the three fields this widget renders rather than the whole
  // payload: a connected snapshot also carries byte counters and rates that
  // tick every poll, so hashing the raw JSON would never match. Anything added
  // to stateLabel()/label()/tooltip() must be added to snapshotKeyOf() too, or
  // it will freeze at whatever value it had when the state last changed.
  // Empty until the first snapshot lands; snapshotKeyOf() never returns empty
  // because a payload without a truthy `state` is rejected as invalid.
  property string snapshotKey: ""
  // Why the last `gpwidget toggle` failed. Deliberately not folded into
  // `snapshot`: the next poll overwrites that within one interval, and the
  // whole point is that the user learns why their click did nothing.
  property string actionError: ""
  // The state the failure was observed in, so it can be cleared the moment the
  // daemon moves anywhere else. Clearing on connected/busy alone would strand a
  // permanent fault after a disconnect that reported failure but took effect.
  property string actionErrorState: ""

  readonly property string vpnState: String(snapshot.state || "stack-down")
  readonly property bool connected: vpnState === "connected"
  readonly property bool busy: vpnState === "authenticating"
    || vpnState === "connecting" || vpnState === "disconnecting"
  readonly property bool unknownState: knownStates.indexOf(vpnState) === -1
  readonly property bool faulted: vpnState === "error" || unknownState || actionError !== ""
  readonly property string gatewayName: snapshot.gateway && snapshot.gateway.name
    ? String(snapshot.gateway.name) : ""
  readonly property bool showGateway: boolSetting("showGateway", true)

  // shell.json is hand-edited, so a setting arrives as whatever the user typed.
  // Accept the JSON booleans plus the obvious string and number spellings
  // instead of silently reading `"true"` as false.
  function boolSetting(name, fallback) {
    var value = root.setting(name, fallback)
    if (typeof value === "string") {
      var text = value.trim().toLowerCase()
      return ["", "false", "0", "no", "off"].indexOf(text) === -1
    }
    return !!value
  }

  // Pick the actual cause out of a CLI stderr dump for the tooltip, which is
  // single-line per entry.
  //
  // Not the first line: gpwidget defaults to InfoLevelVerbosity, so the launch
  // path emits `INFO ... Starting VPN service stack via ...` before anything
  // goes wrong, while cli.rs prints the real cause last as `Error: ...`. Taking
  // the head would report the banner as the failure in precisely the
  // polkit-denied case this text exists to explain.
  function errorLine(text) {
    var lines = String(text || "").split("\n")
      .map(function(line) { return line.trim() })
      .filter(function(line) { return line !== "" })
    if (lines.length === 0) return ""

    var chosen = lines[lines.length - 1]
    for (var i = lines.length - 1; i >= 0; i--) {
      if (lines[i].indexOf("Error:") === 0) {
        chosen = lines[i].slice("Error:".length).trim()
        break
      }
    }
    return chosen.length > 120 ? chosen.slice(0, 119) + "…" : chosen
  }

  // NUL-joined so gateway or error text containing the separator cannot make
  // two distinct snapshots hash alike.
  function snapshotKeyOf(value) {
    return [
      String(value.state),
      value.gateway && value.gateway.name ? String(value.gateway.name) : "",
      value.error ? String(value.error) : ""
    ].join("\u0000")
  }

  function applySnapshot(value) {
    var key = snapshotKeyOf(value)
    if (key === root.snapshotKey) return
    root.snapshotKey = key
    root.snapshot = value
  }

  function setError(message) {
    root.applySnapshot({ state: "error", error: message })
  }

  function setActionError(message) {
    root.actionError = message
    root.actionErrorState = root.vpnState
  }

  function stateLabel() {
    switch (vpnState) {
    case "connected": return gatewayName === "" ? "Connected" : "Connected · " + gatewayName
    case "authenticating": return "Authenticating…"
    case "connecting": return "Connecting…"
    case "disconnecting": return "Disconnecting…"
    case "disconnected": return "Disconnected"
    case "needs-setup": return "Setup required"
    case "error": return snapshot.error ? "Error · " + snapshot.error : "Error"
    case "stack-down": return "VPN off"
    default: return "Unknown state · " + vpnState
    }
  }

  function label() {
    if (root.vertical) return "VPN"
    if (faulted) return "VPN  !"
    if (connected && showGateway && gatewayName !== "") return "VPN  " + gatewayName
    if (busy) return "VPN  …"
    return "VPN"
  }

  function tooltip() {
    var lines = [stateLabel()]
    if (actionError !== "") lines.push("Last action failed · " + actionError)
    lines.push("Left click: details · Right click: connect/disconnect")
    return lines.join("\n")
  }

  function refresh() {
    if (statusProcess.running) return
    statusProcess.sawExit = false
    statusProcess.timedOut = false
    statusWatchdog.restart()
    statusProcess.running = true
  }

  function toggle() {
    if (toggleProcess.running) return
    root.actionError = ""
    root.actionErrorState = ""
    toggleProcess.sawExit = false
    toggleProcess.running = true
    refreshDelay.restart()
  }

  // A move away from the state the failure was seen in means something actually
  // happened, so a stale "your last click failed" would outlive the condition it
  // described. Anchoring on the recorded state rather than clearing on every
  // change keeps a poll landing mid-toggle from erasing a fresh error.
  onVpnStateChanged: {
    if (root.actionError !== "" && root.vpnState !== root.actionErrorState) {
      root.actionError = ""
      root.actionErrorState = ""
    }
  }

  implicitWidth: button.implicitWidth
  implicitHeight: button.implicitHeight

  Timer {
    id: pollTimer
    // `refreshIntervalSec` is hand-edited too. A non-numeric value yields NaN,
    // which coerces to 0 on the Timer's int interval — on a repeating,
    // triggeredOnStart timer that is a hot loop spawning back-to-back status
    // processes, so fall back rather than clamp.
    //
    // The upper clamp is not cosmetic: an interval in milliseconds that exceeds
    // int range wraps through ToInt32, and 2^29 seconds lands back on exactly
    // 0 ms — the same hot loop, reached from the opposite end.
    interval: {
      var seconds = Number(root.setting("refreshIntervalSec", 3))
      if (!isFinite(seconds)) seconds = 3
      return Math.min(3600, Math.max(1, seconds)) * 1000
    }
    running: true
    repeat: true
    triggeredOnStart: true
    onTriggered: root.refresh()
  }

  // `gpwidget status` blocks on its socket read with no timeout of its own, so
  // a wedged daemon would otherwise freeze polling forever on a stale label.
  Timer {
    id: statusWatchdog
    interval: Math.max(4000, pollTimer.interval * 2)
    repeat: false
    onTriggered: {
      if (!statusProcess.running) return
      statusProcess.timedOut = true
      // Quickshell maps `running = false` to terminate() — SIGTERM, which
      // gpwidget does not handle, so the blocked read is torn down and `exited`
      // still arrives (and is ignored, below).
      statusProcess.running = false
      root.setError("gpwidget status timed out")
      statusKill.restart()
    }
  }

  // SIGTERM is a request. If the process ignores it or is wedged, `running`
  // never clears and refresh() early-returns for the rest of the session, since
  // the watchdog is only ever re-armed from inside refresh(). Escalate, as
  // Quickshell's own Process docs prescribe for this case.
  Timer {
    id: statusKill
    interval: 2000
    repeat: false
    onTriggered: if (statusProcess.running) statusProcess.signal(9)
  }

  Process {
    id: statusProcess
    command: ["gpwidget", "status"]
    running: false

    // Set in onExited so onRunningChanged can tell a real exit apart from
    // QProcess::FailedToStart, which emits runningChanged and no exit at all.
    property bool sawExit: false
    property bool timedOut: false

    // Buffer stdout until exit so we can reject non-zero status and avoid
    // treating missing/failed gpwidget as a healthy stack-down snapshot.
    stdout: StdioCollector {
      id: statusStdout
      waitForEnd: true
    }

    // gpwidget prints the concrete cause here — socket permissions, a serde
    // error from a version-mismatched daemon. Without a collector Quickshell
    // closes the channel and the diagnostic is unrecoverable from the widget.
    stderr: StdioCollector {
      id: statusStderr
      waitForEnd: true
    }

    onExited: function(exitCode) {
      statusProcess.sawExit = true
      statusWatchdog.stop()
      // The process is gone, so escalation is moot — and leaving it armed would
      // fire SIGKILL at whatever pid the next poll happens to get.
      statusKill.stop()

      // The watchdog caused this exit and already reported something more
      // useful than the signal that killed it.
      if (statusProcess.timedOut) return

      if (exitCode !== 0) {
        root.setError(root.errorLine(statusStderr.text) || "gpwidget status failed")
        return
      }

      var raw = String(statusStdout.text || "").trim()
      if (raw === "") {
        root.setError("Empty status response")
        return
      }

      try {
        var value = JSON.parse(raw)
        if (value && typeof value === "object" && !Array.isArray(value) && value.state) {
          root.applySnapshot(value)
        } else {
          root.setError("Invalid status response")
        }
      } catch (e) {
        root.setError("Invalid status response")
      }
    }

    onRunningChanged: {
      // FailedToStart — gpwidget missing or not on PATH — only flips `running`
      // back to false. There is no exit code to map, so without this the widget
      // would sit on its initial stack-down snapshot and look benign.
      if (!statusProcess.running && !statusProcess.sawExit) {
        statusWatchdog.stop()
        statusKill.stop()
        root.setError("gpwidget not found on PATH")
      }
    }
  }

  // With the stack down, `toggle` escalates to `gpclient launch-gui` behind
  // pkexec and can take 30s, so it gets no watchdog — but its stderr is the
  // only signal the user has when polkit is denied or the stack never comes up,
  // and the daemon isn't around to deliver a toast.
  Process {
    id: toggleProcess
    command: ["gpwidget", "toggle"]
    running: false

    property bool sawExit: false

    stderr: StdioCollector {
      id: toggleStderr
      waitForEnd: true
    }

    onExited: function(exitCode) {
      toggleProcess.sawExit = true
      if (exitCode !== 0) {
        root.setActionError(root.errorLine(toggleStderr.text) || "gpwidget toggle failed")
      }
      root.broadcast("refresh")
    }

    onRunningChanged: {
      if (!toggleProcess.running && !toggleProcess.sawExit) {
        root.setActionError("gpwidget not found on PATH")
      }
    }
  }

  // Omarchy builds one widget instance per bar surface per monitor. Without the
  // broadcast, the clicked bar would update while every other screen kept the
  // contradictory old state until its own poll came round.
  Timer {
    id: refreshDelay
    interval: 500
    repeat: false
    onTriggered: root.broadcast("refresh")
  }

  WidgetButton {
    id: button
    anchors.fill: parent
    bar: root.bar
    text: root.label()
    // Omarchy's active colour is the urgent/error accent (red in the stock
    // themes), so reserve it for a real VPN error rather than connectivity.
    active: root.faulted
    // An unconfigured portal is not a working tunnel; without needs-setup here
    // it renders pixel-identical to a connected one whenever the gateway name
    // is hidden.
    //
    // Never dim a fault. WidgetButton multiplies dimmed state to 0.45 opacity,
    // and the two overlap exactly where it hurts — a failed toggle against a
    // down stack would wash the urgent accent out to nearly invisible.
    dimmed: !root.faulted
      && (root.vpnState === "stack-down" || root.vpnState === "disconnected"
        || root.vpnState === "needs-setup")
    tooltipText: root.tooltip()

    // WidgetButton's MouseArea accepts middle-click too and forwards it here;
    // anything but the two documented buttons is ignored rather than falling
    // through to the popup.
    onPressed: function(b) {
      if (b === Qt.RightButton) root.toggle()
      else if (b === Qt.LeftButton) Quickshell.execDetached(["gpwidget", "popup"])
    }
  }
}
