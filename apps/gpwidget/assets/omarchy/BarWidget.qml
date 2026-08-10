import QtQuick
import Quickshell
import Quickshell.Io
import qs.Commons
import qs.Ui

// Native Omarchy bar adapter for the gpwidget daemon. The full controls stay
// in gpwidget's GTK layer-shell popup, while this item follows Omarchy's bar
// geometry, colors, tooltip and multi-monitor conventions.
BarWidget {
  id: root
  moduleName: "gpwidget.vpn"

  property var snapshot: ({ state: "stack-down" })
  property bool refreshQueued: false

  readonly property string vpnState: String(snapshot.state || "stack-down")
  readonly property bool connected: vpnState === "connected"
  readonly property bool busy: vpnState === "authenticating"
    || vpnState === "connecting" || vpnState === "disconnecting"
  readonly property string gatewayName: snapshot.gateway && snapshot.gateway.name
    ? String(snapshot.gateway.name) : ""
  readonly property bool showGateway: setting("showGateway", true) === true

  function stateLabel() {
    switch (vpnState) {
    case "connected": return gatewayName === "" ? "Connected" : "Connected · " + gatewayName
    case "authenticating": return "Authenticating…"
    case "connecting": return "Connecting…"
    case "disconnecting": return "Disconnecting…"
    case "disconnected": return "Disconnected"
    case "needs-setup": return "Setup required"
    case "error": return snapshot.error ? "Error · " + snapshot.error : "Error"
    default: return "VPN off"
    }
  }

  function label() {
    if (root.vertical) return "VPN"
    if (connected && showGateway && gatewayName !== "") return "VPN  " + gatewayName
    if (busy) return "VPN  …"
    if (vpnState === "error") return "VPN  !"
    return "VPN"
  }

  function refresh() {
    if (statusProcess.running) {
      refreshQueued = true
      return
    }
    statusProcess.running = true
  }

  implicitWidth: button.implicitWidth
  implicitHeight: button.implicitHeight

  Timer {
    interval: Math.max(1, Number(root.setting("refreshIntervalSec", 3))) * 1000
    running: true
    repeat: true
    triggeredOnStart: true
    onTriggered: root.refresh()
  }

  Process {
    id: statusProcess
    command: ["gpwidget", "status"]
    running: false

    stdout: StdioCollector {
      waitForEnd: true
      onStreamFinished: {
        try {
          var value = JSON.parse(String(text || "{}"))
          root.snapshot = value && typeof value === "object" ? value : ({ state: "stack-down" })
        } catch (e) {
          root.snapshot = ({ state: "error", error: "Invalid status response" })
        }
      }
    }

    onExited: {
      if (root.refreshQueued) {
        root.refreshQueued = false
        Qt.callLater(root.refresh)
      }
    }
  }

  WidgetButton {
    id: button
    anchors.fill: parent
    bar: root.bar
    text: root.label()
    // Omarchy's active colour is the urgent/error accent (red in the stock
    // themes), so reserve it for a real VPN error rather than connectivity.
    active: root.vpnState === "error"
    dimmed: root.vpnState === "stack-down" || root.vpnState === "disconnected"
    horizontalMargin: 8.5
    tooltipText: root.stateLabel() + "\nLeft click: details · Right click: connect/disconnect"

    onPressed: function(b) {
      if (b === Qt.RightButton) {
        Quickshell.execDetached(["gpwidget", "toggle"])
        refreshDelay.restart()
      } else {
        Quickshell.execDetached(["gpwidget", "popup"])
      }
    }
  }

  Timer {
    id: refreshDelay
    interval: 500
    repeat: false
    onTriggered: root.refresh()
  }
}
