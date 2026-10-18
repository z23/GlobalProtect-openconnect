import QtQuick
import Quickshell
import Quickshell.Io

ShellRoot {
  id: root
  property var service: null
  property var widget: null
  property var client: null
  property var lastCommand: ({})
  property int step: 0
  property int checks: 0
  property int gatewayStep: 0
  property bool gatewayChecksComplete: false
  property int protocol: 1
  property int connections: 0
  property int mismatchConnections: 0
  property double mismatchSince: 0
  onStepChanged: console.log("Runtime step " + step)
  property var status: ({ type: "status", state: "disconnected", portal: "vpn.example",
    gateways: [{ name: "Australia", address: "au.example" }, { name: "Europe", address: "eu.example" }] })

  function check(value, label) {
    if (!value) {
      console.error("FAIL: step " + step + ", gateway step " + gatewayStep + ": " + label)
      Qt.quit()
      throw new Error(label)
    }
    checks++
  }
  function write(message) { client.write(JSON.stringify(message) + "\n"); client.flush() }
  function acknowledge(ok) { write({ type: "ack", id: lastCommand.id, ok: ok, error: ok ? null : "fixture rejection" }) }
  function setState(state) { status = Object.assign({}, status, { state: state }); write(status) }

  // Exercise connects before any manual choice, then selection invalidation.
  // The original harness picked a gateway before its first connect and could
  // not detect an implicit pin being manufactured from status updates.
  function checkGatewaySelection(s) {
    switch (gatewayStep) {
    case 0:
      if (!s.ready) return false
      check(s.selectedGateway === "", "multiple gateways do not imply a manual choice")
      s.act()
      gatewayStep++
      break
    case 1:
      if (lastCommand.type !== "connect") return false
      check(lastCommand.gateway === undefined, "default connect leaves selection to daemon configuration")
      acknowledge(true)
      lastCommand = {}
      status = Object.assign({}, status, { gateway: { name: "Australia", address: "au.example" },
        gateways: [{ name: "Australia", address: "au.example" }] })
      write(status)
      gatewayStep++
      break
    case 2:
      if (s.pending || s.gatewayOptions.length !== 1) return false
      check(s.selectedGateway === "", "current single gateway does not become a manual choice")
      s.act()
      gatewayStep++
      break
    case 3:
      if (lastCommand.type !== "connect") return false
      check(lastCommand.gateway === undefined, "single-gateway connect remains automatic")
      acknowledge(true)
      lastCommand = {}
      status = Object.assign({}, status, { gateways: [
        { name: "Australia", address: "au.example" }, { name: "Europe", address: "eu.example" }] })
      write(status)
      gatewayStep++
      break
    case 4:
      if (s.pending || s.gatewayOptions.length !== 2) return false
      s.selectedGateway = "missing.example"
      s.act()
      gatewayStep++
      break
    case 5:
      if (lastCommand.type !== "connect") return false
      check(lastCommand.gateway === undefined && s.selectedGateway === "", "unlisted gateway is never sent")
      acknowledge(true)
      lastCommand = {}
      gatewayStep++
      break
    case 6:
      if (s.pending) return false
      s.selectedGateway = "eu.example"
      s.connectVpn("portal-b.example")
      gatewayStep++
      break
    case 7:
      if (lastCommand.type !== "connect") return false
      check(lastCommand.portal === "portal-b.example" && lastCommand.gateway === undefined,
        "explicit portal change drops old portal's gateway")
      acknowledge(true)
      lastCommand = {}
      s.selectedGateway = "eu.example"
      status = Object.assign({}, status, { portal: "portal-b.example" })
      write(status)
      gatewayStep++
      break
    case 8:
      if (s.pending || s.snapshot.portal !== "portal-b.example") return false
      check(s.selectedGateway === "", "portal status change clears selection even when address is still listed")
      s.selectedGateway = "eu.example"
      status = Object.assign({}, status, { gateways: [{ name: "Australia", address: "au.example" }] })
      write(status)
      gatewayStep++
      break
    case 9:
      if (s.gatewayOptions.length !== 1) return false
      check(s.selectedGateway === "", "gateway disappearing from status clears selection")
      status = Object.assign({}, status, { gateways: [
        { name: "Australia", address: "au.example" }, { name: "Europe", address: "eu.example" }] })
      write(status)
      gatewayStep++
      break
    case 10:
      if (s.gatewayOptions.length !== 2) return false
      s.selectedGateway = "eu.example"
      s.selectedGateway = ""
      s.act()
      gatewayStep++
      break
    case 11:
      if (lastCommand.type !== "connect") return false
      check(lastCommand.gateway === undefined, "Automatic clears a previous manual override")
      acknowledge(true)
      lastCommand = {}
      status = Object.assign({}, status, { portal: "vpn.example" })
      write(status)
      gatewayStep++
      break
    case 12:
      return !s.pending && s.snapshot.portal === "vpn.example"
    }
    return false
  }

  QtObject {
    id: bar
    property color foreground: "#eeeeee"
    property color barForeground: foreground
    property color urgent: "#ff0000"
    property string fontFamily: "monospace"
    property string position: "top"
    property bool vertical: false
    property bool foregroundAnimationEnabled: false
    property int barSize: 26
    property var activePopout: null
    property var clickTargets: []
    function registerClickTarget(target) {}
    function unregisterClickTarget(target) {}
    function hideTooltip(target) {}
    function showTooltip(target, text) {}
    function requestPopout(owner) { activePopout = owner }
    function releasePopout(owner) { activePopout = null }
    function switchPanelFrom(owner, direction) { return true }
  }

  SocketServer {
    id: server
    active: true
    path: Quickshell.env("GPWIDGET_TEST_SOCKET")
    handler: Socket {
      id: peer
      onConnectedChanged: if (connected) {
        root.connections++
        root.client = peer
        peer.write(JSON.stringify({ type: "hello", version: "fixture", protocol: root.protocol }) + "\n" + JSON.stringify(root.status) + "\n")
        peer.flush()
      }
      parser: SplitParser {
        onRead: function(line) {
          var message = JSON.parse(line)
          if (message.type === "get-status") root.write(root.status)
          else root.lastCommand = message
        }
      }
    }
  }

  Component.onCompleted: {
    var source = "file://" + Quickshell.env("GPWIDGET_TEST_SOURCE")
    var component = Qt.createComponent(source + "/Service.qml")
    check(component.status === Component.Ready, component.errorString())
    service = component.createObject(root, { socketPath: Quickshell.env("GPWIDGET_TEST_SOCKET") })
    check(service !== null, "service created")
    component = Qt.createComponent(source + "/BarWidget.qml")
    check(component.status === Component.Ready, component.errorString())
    widget = component.createObject(root, { service: service, bar: bar })
    check(widget !== null, "native panel created with real Omarchy components")
  }

  Timer {
    interval: 40
    repeat: true
    running: true
    onTriggered: {
      var s = root.service
      if (!s) return
      if (!root.gatewayChecksComplete) {
        root.gatewayChecksComplete = root.checkGatewaySelection(s)
        return
      }
      switch (root.step) {
      case 0:
        if (!s.ready) return
        root.check(s.state === "disconnected", "initial socket greeting/status")
        root.check(s.gatewayOptions.length === 2, "gateway list")
        s.selectedGateway = "eu.example"
        s.act(" vpn.example ")
        root.step++
        break
      case 1:
        if (root.lastCommand.type !== "connect") return
        root.check(root.lastCommand.portal === "vpn.example" && root.lastCommand.gateway === "eu.example", "portal and gateway command")
        root.check(s.pending, "wait for command acknowledgment")
        root.write({ type: "ack", id: root.lastCommand.id + 1, ok: true })
        root.step++
        break
      case 2:
        root.check(s.pending, "unrelated acknowledgment ignored")
        root.acknowledge(true)
        root.setState("authenticating")
        root.write({ type: "auth-prompt", kind: "otp", message: "Enter your code" })
        root.step++
        break
      case 3:
        if (s.otpMessage === "" || s.pending) return
        root.check(s.otpMessage === "Enter your code", "OTP prompt retained by shared service")
        root.check(s.selectedGateway === "eu.example", "selection survives status updates")
        s.submitOtp(" 123456 ")
        root.step++
        break
      case 4:
        if (root.lastCommand.type !== "submit-otp") return
        root.check(root.lastCommand.otp === "123456", "OTP delivered over socket")
        root.acknowledge(false)
        root.step++
        break
      case 5:
        if (s.pending) return
        root.check(s.actionError === "fixture rejection" && s.otpMessage !== "", "OTP rejection visible, prompt retained")
        root.lastCommand = {}
        s.submitOtp("654321")
        root.step++
        break
      case 6:
        if (root.lastCommand.type !== "submit-otp") return
        root.acknowledge(true)
        root.status = Object.assign({}, root.status, { gateway: { name: "Europe", address: "eu.example" },
          conn: { since: Math.floor(Date.now() / 1000) - 60, ipv4: "10.0.0.2", ifname: "tun0", rxBytes: 1024, txBytes: 2, rxRate: 100, txRate: 20 },
          session: { expiresAt: Math.floor(Date.now() / 1000) + 600 } })
        root.setState("connected")
        root.step++
        break
      case 7:
        if (!s.connected) return
        root.check(s.otpMessage === "" && s.actionError === "", "successful OTP and new state clear errors")
        root.check(s.snapshot.conn.rxBytes === 1024, "connection stats")
        root.status = Object.assign({}, root.status, { conn: Object.assign({}, root.status.conn, { rxBytes: 4096 }) })
        root.write(root.status)
        root.step++
        break
      case 8:
        if (s.snapshot.conn.rxBytes !== 4096) return
        root.check(true, "counter-only status updates reach panel")
        s.act()
        root.step++
        break
      case 9:
        if (root.lastCommand.type !== "disconnect") return
        root.acknowledge(false)
        root.write(root.status)
        root.step++
        break
      case 10:
        if (s.pending) return
        root.check(s.actionError === "fixture rejection", "command error survives same-state refresh")
        root.setState("disconnecting")
        root.step++
        break
      case 11:
        if (s.state !== "disconnecting") return
        root.check(s.actionError === "", "state change clears stale command error")
        root.check(!s.canAct, "disconnecting cannot submit duplicate actions")
        root.setState("disconnected")
        root.step++
        break
      case 12:
        if (s.state !== "disconnected") return
        s.sendCommand("disconnect")
        server.active = false
        root.step++
        break
      case 13:
        if (s.linkUp) return
        root.check(s.state === "stack-down" && !s.connected, "socket loss clears stale connected state")
        root.check(!s.pending && s.actionError !== "", "socket loss fails pending command")
        root.check(s.selectedGateway === "", "daemon loss clears manual gateway selection")
        server.active = true
        root.step++
        break
      case 14:
        if (!s.ready) return
        root.check(true, "socket reconnects after daemon restart")
        root.check(s.selectedGateway === "", "daemon restart does not manufacture a gateway pin")
        root.protocol = 2
        root.mismatchConnections = root.connections
        root.mismatchSince = Date.now()
        root.write({ type: "hello", protocol: 2 })
        root.step++
        break
      case 15:
        if (Date.now() - root.mismatchSince < 6500) return
        root.check(s.faulted && !s.canAct, "protocol mismatch fails closed")
        root.check(s.transportError === "Unsupported VPN widget protocol · 2", "protocol mismatch survives response timeout")
        root.check(!s.linkUp && root.connections === root.mismatchConnections, "incompatible daemon is not retried automatically")
        root.protocol = 1
        s.refresh()
        root.step++
        break
      case 16:
        if (!s.ready) return
        root.check(root.connections === root.mismatchConnections + 1 && s.transportError === "",
          "manual refresh negotiates a fresh connection after protocol upgrade")
        root.setState("future-state")
        root.step++
        break
      case 17:
        if (s.state !== "future-state") return
        root.check(s.unknownState && s.faulted && !s.canAct, "unknown states are never presented as VPN off")
        root.client.write("malformed\n"); root.client.flush()
        root.step++
        break
      case 18:
        if (!s.transportError) return
        root.check(!s.ready && s.faulted, "malformed response visible")
        root.setState("disconnected")
        root.step++
        break
      case 19:
        if (!s.ready) return
        root.check(true, "valid status recovers transport")
        root.write({ type: "auth-prompt", kind: "otp", message: "Second prompt" })
        root.step++
        break
      case 20:
        if (!s.otpMessage) return
        root.widget.destroy()
        var component = Qt.createComponent("file://" + Quickshell.env("GPWIDGET_TEST_SOURCE") + "/BarWidget.qml")
        root.widget = component.createObject(root, { service: s, bar: bar })
        root.check(root.widget.showOtp, "recreating a bar preserves shared OTP prompt")
        root.setState("disconnected")
        s.active = false
        server.active = false
        root.step++
        break
      case 21:
        if (s.linkUp) return
        s.transportError = ""
        s.act("fixture.example")
        root.step++
        break
      case 22:
        if (s.pending || s.actionError !== "fixture authorization denied") return
        root.check(true, "CLI startup failures surface the concrete error")
        console.log("PASS: native Omarchy widget runtime · " + root.checks + " checks")
        Qt.quit()
        break
      }
    }
  }
  Timer { interval: 30000; running: true; onTriggered: { console.error("FAIL: timed out at step " + root.step); Qt.quit() } }
}
