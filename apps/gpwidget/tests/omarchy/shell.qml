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
  onStepChanged: console.log("Runtime step " + step)
  property var status: ({ type: "status", state: "disconnected", portal: "vpn.example",
    gateways: [{ name: "Australia", address: "au.example" }, { name: "Europe", address: "eu.example" }] })

  function check(value, label) {
    if (!value) { console.error("FAIL at step " + step + ": " + label); Qt.quit() }
    checks++
  }
  function write(message) { client.write(JSON.stringify(message) + "\n"); client.flush() }
  function acknowledge(ok) { write({ type: "ack", id: lastCommand.id, ok: ok, error: ok ? null : "fixture rejection" }) }
  function setState(state) { status = Object.assign({}, status, { state: state }); write(status) }

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
        root.client = peer
        peer.write(JSON.stringify({ type: "hello", version: "fixture", protocol: 1 }) + "\n" + JSON.stringify(root.status) + "\n")
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
        server.active = true
        root.step++
        break
      case 14:
        if (!s.ready) return
        root.check(true, "socket reconnects after daemon restart")
        root.write({ type: "hello", protocol: 2 })
        root.step++
        break
      case 15:
        if (s.ready) return
        root.check(s.faulted && !s.canAct, "protocol mismatch fails closed")
        root.write({ type: "hello", protocol: 1 })
        root.setState("future-state")
        root.step++
        break
      case 16:
        if (s.state !== "future-state") return
        root.check(s.unknownState && s.faulted && !s.canAct, "unknown states are never presented as VPN off")
        root.client.write("malformed\n"); root.client.flush()
        root.step++
        break
      case 17:
        if (!s.transportError) return
        root.check(!s.ready && s.faulted, "malformed response visible")
        root.setState("disconnected")
        root.step++
        break
      case 18:
        if (!s.ready) return
        root.check(true, "valid status recovers transport")
        root.write({ type: "auth-prompt", kind: "otp", message: "Second prompt" })
        root.step++
        break
      case 19:
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
      case 20:
        if (s.linkUp) return
        s.transportError = ""
        s.act("fixture.example")
        root.step++
        break
      case 21:
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
