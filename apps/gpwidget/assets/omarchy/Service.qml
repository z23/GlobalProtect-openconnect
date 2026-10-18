import QtQuick
import Quickshell
import Quickshell.Io
import "Model.js" as Model

// One instance hosted by Omarchy, shared by every monitor. The Rust daemon
// continues to own authentication and the tunnel; this service only speaks v1
// of its public NDJSON socket protocol.
Scope {
  id: root

  property string socketPath: Quickshell.env("XDG_RUNTIME_DIR") + "/gpwidget.sock"
  property bool active: true
  property var snapshot: ({ state: "stack-down" })
  property string transportError: ""
  property string actionError: ""
  property string actionErrorState: ""
  property string otpMessage: ""
  property string selectedGateway: ""
  property bool protocolReady: false
  property bool hasStatus: false
  property int sequence: 0
  property int pendingId: 0
  property string pendingCommand: ""
  property int retryDelay: 1000
  property int now: Math.floor(Date.now() / 1000)

  readonly property bool linkUp: socket.connected
  readonly property bool ready: linkUp && protocolReady && hasStatus && transportError === ""
  readonly property string state: transportError !== "" ? "error" : String(snapshot.state || "stack-down")
  readonly property bool connected: ready && state === "connected"
  readonly property bool busy: ["authenticating", "connecting", "disconnecting"].indexOf(state) >= 0
  readonly property bool unknownState: Model.states.indexOf(state) < 0
  readonly property bool pending: pendingId !== 0 || launcher.running
  readonly property bool faulted: state === "error" || unknownState || actionError !== ""
  readonly property string errorText: transportError || actionError || String(snapshot.error || "")
  readonly property var gatewayOptions: Model.gateways(snapshot.gateways)
  readonly property string gatewayName: snapshot.gateway ? String(snapshot.gateway.name || snapshot.gateway.address || "") : ""
  readonly property string statusText: launcher.running && !ready ? "Starting VPN service…"
    : otpMessage !== "" ? "One-time password required" : Model.stateLabel(state)
  readonly property string actionText: launcher.running && !ready ? "Starting…"
    : state === "connected" ? "Disconnect" : ["authenticating", "connecting"].indexOf(state) >= 0 ? "Cancel"
    : state === "stack-down" ? "Start VPN" : "Connect"
  readonly property bool canAct: !pending && !unknownState && transportError === ""
    && state !== "disconnecting" && (!linkUp || ready)

  function failAction(message) {
    actionError = message
    actionErrorState = state
  }

  function clearPending() {
    pendingId = 0
    pendingCommand = ""
    commandTimeout.stop()
  }

  function lostConnection() {
    protocolReady = false
    hasStatus = false
    responseTimeout.stop()
    snapshot = { state: "stack-down" }
    otpMessage = ""
    if (pendingId !== 0) {
      failAction("VPN service disconnected before confirming the action")
      clearPending()
    }
    if (active) retry.restart()
  }

  function connectSocket() {
    if (!active || socket.connected) return
    socket.connected = false
    socket.connected = true
  }

  function refresh() {
    transportError = ""
    retryDelay = 1000
    if (socket.connected) {
      socket.write(JSON.stringify({ type: "get-status" }) + "\n")
      socket.flush()
      responseTimeout.restart()
    } else connectSocket()
  }

  function sendCommand(type, extra) {
    if (!ready || pending) return false
    actionError = ""
    var message = extra || {}
    message.type = type
    message.id = ++sequence
    pendingId = message.id
    pendingCommand = type
    commandTimeout.restart()
    socket.write(JSON.stringify(message) + "\n")
    socket.flush()
    return true
  }

  function connectVpn(portal) {
    var extra = {}
    if (selectedGateway !== "") extra.gateway = selectedGateway
    if (String(portal || "").trim() !== "") extra.portal = String(portal).trim()
    if (ready) return sendCommand("connect", extra)
    if (pending || linkUp || transportError !== "") return false
    actionError = ""
    var args = ["gpwidget", "connect"]
    if (extra.portal) args.push("--portal", extra.portal)
    if (extra.gateway) args.push("--gateway", extra.gateway)
    launcher.sawExit = false
    launcher.timedOut = false
    launcher.command = args
    launchTimeout.restart()
    launcher.running = true
    retryDelay = 1000
    retry.restart()
    return true
  }

  function act(portal) {
    if (!canAct) return
    if (["connected", "authenticating", "connecting"].indexOf(state) >= 0) sendCommand("disconnect")
    else connectVpn(portal)
  }

  function submitOtp(value) {
    var otp = String(value || "").trim()
    return otp !== "" && otpMessage !== "" && sendCommand("submit-otp", { otp: otp })
  }

  function receive(line) {
    var message
    try { message = JSON.parse(line) }
    catch (e) { transportError = "Invalid response from VPN service"; return }
    if (!message || typeof message !== "object") {
      transportError = "Invalid response from VPN service"
      return
    }
    switch (message.type) {
    case "hello":
      protocolReady = message.protocol === 1
      if (!protocolReady) transportError = "Unsupported VPN widget protocol · " + message.protocol
      break
    case "status":
      if (!protocolReady) return
      if (!Model.validStatus(message)) { transportError = "Invalid VPN status response"; return }
      responseTimeout.stop()
      transportError = ""
      snapshot = message
      now = Math.floor(Date.now() / 1000)
      hasStatus = true
      retryDelay = 1000
      if (actionError !== "" && state !== actionErrorState) actionError = ""
      if (state !== "authenticating") otpMessage = ""
      var options = gatewayOptions
      if (!options.some(function(g) { return g.value === selectedGateway })) {
        var current = message.gateway ? message.gateway.address : ""
        selectedGateway = options.some(function(g) { return g.value === current }) ? current
          : options.length ? options[0].value : ""
      }
      break
    case "auth-prompt":
      if (protocolReady && message.kind === "otp") otpMessage = String(message.message || "Enter your one-time password")
      break
    case "ack":
      if (message.id !== pendingId || pendingId === 0) return
      if (message.ok !== true) failAction(String(message.error || "VPN action failed"))
      else if (pendingCommand === "submit-otp") otpMessage = ""
      clearPending()
      break
    case "bye":
      Qt.callLater(function() { socket.connected = false })
      break
    }
  }

  onActiveChanged: {
    if (active) connectSocket()
    else { retry.stop(); socket.connected = false }
  }
  Component.onCompleted: if (active) Qt.callLater(connectSocket)

  Socket {
    id: socket
    path: root.socketPath
    parser: SplitParser { onRead: function(line) { root.receive(line) } }
    onConnectedChanged: {
      if (connected) { retry.stop(); responseTimeout.restart() }
      else root.lostConnection()
    }
    onError: function(error) {
      // QLocalSocket::LocalSocketError: refused, peer closed, and not found
      // (0..2) mean the optional daemon is down. Other errors are actionable.
      if (error > 2 || error < 0) root.transportError = error === 3
        ? "Permission denied reading VPN socket" : "Cannot read VPN socket (error " + error + ")"
      // Quickshell may still be unwinding QLocalSocket's error callback.
      // Closing it synchronously here can delete the underlying socket twice.
      Qt.callLater(function() {
        socket.connected = false
        root.lostConnection()
      })
    }
  }

  Timer {
    id: retry
    interval: root.retryDelay
    onTriggered: {
      root.retryDelay = Math.min(30000, root.retryDelay * 2)
      root.connectSocket()
    }
  }
  Timer {
    interval: 15000
    running: root.ready
    repeat: true
    onTriggered: root.refresh()
  }
  Timer {
    id: responseTimeout
    interval: 5000
    onTriggered: {
      root.transportError = "VPN service stopped responding"
      socket.connected = false
    }
  }
  Timer {
    id: commandTimeout
    interval: 10000
    onTriggered: {
      root.failAction("VPN action was not confirmed; check the connection status before retrying")
      root.clearPending()
    }
  }
  Timer {
    interval: 1000
    repeat: true
    running: root.connected
    onTriggered: root.now = Math.floor(Date.now() / 1000)
  }

  Process {
    id: launcher
    property bool sawExit: true
    property bool timedOut: false
    stderr: StdioCollector { id: launchError; waitForEnd: true }
    stdout: StdioCollector { waitForEnd: true }
    onExited: function(code) {
      sawExit = true
      launchTimeout.stop()
      launchKill.stop()
      if (code !== 0 && !timedOut) root.failAction(Model.errorLine(launchError.text) || "Could not start VPN")
      root.refresh()
    }
    onRunningChanged: if (!running && !sawExit) {
      launchTimeout.stop()
      root.failAction("gpwidget could not be started; check that it is installed")
    }
  }
  Timer {
    id: launchTimeout
    interval: 45000
    onTriggered: {
      launcher.timedOut = true
      root.failAction("VPN startup timed out; check authorization and retry")
      launcher.running = false
      launchKill.restart()
    }
  }
  Timer { id: launchKill; interval: 2000; onTriggered: if (launcher.running) launcher.signal(9) }
}
