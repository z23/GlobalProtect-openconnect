import QtQuick
import QtQuick.Controls as Controls
import Quickshell
import qs.Commons
import qs.Commons as Commons
import qs.Ui
import "Model.js" as Model

Panel {
  id: root
  moduleName: "gpwidget.vpn"
  // The host owns one service, so opening a panel on another monitor never
  // starts a second VPN action or loses an OTP prompt.
  property var service: bar && bar.shell ? bar.shell.serviceFor(moduleName) : null
  readonly property bool vertical: bar ? bar.vertical : false
  readonly property color foreground: Commons.Color.popups.text
  readonly property string fontFamily: bar ? bar.fontFamily : Style.font.family
  readonly property string state: service ? service.state : "error"
  readonly property string statusText: service ? service.statusText : "VPN widget service unavailable"
  readonly property string errorText: service ? service.errorText : "Reload the complete gpwidget Omarchy plugin, including Service.qml."
  readonly property bool faulted: !service || service.faulted
  readonly property bool showGateway: Model.boolSetting(setting("showGateway", false), false)
  readonly property bool showPortal: service && (state === "needs-setup" || state === "stack-down")
  readonly property bool showOtp: service && service.otpMessage !== ""
  readonly property bool canAct: service && service.canAct && (state !== "needs-setup" || portal.text.trim() !== "")
  readonly property string icon: faulted ? "󰦞" : service && service.busy ? "󰑐" : service && service.connected ? "󰒃" : "󰒙"
  readonly property var conn: service && service.snapshot.conn ? service.snapshot.conn : ({})
  readonly property var session: service && service.snapshot.session ? service.snapshot.session : ({})
  readonly property var sections: {
    var result = ["action"]
    if (showPortal) result.push("portal")
    if (service && (service.gatewayOptions.length > 1 || service.selectedGateway !== "")) result.push("gateway")
    if (showOtp) result.push("otp", "submit")
    result.push("refresh")
    if (service && service.ready) result.push("quit")
    return result
  }
  property string focusSection: "action"
  property bool cursorActive: false

  implicitWidth: button.implicitWidth
  implicitHeight: button.implicitHeight

  function chooseSection(section) { focusSection = section; cursorActive = true }
  function moveCursor(direction) {
    var index = sections.indexOf(focusSection)
    chooseSection(sections[(Math.max(0, index) + direction + sections.length) % sections.length])
    var target = focusSection === "portal" ? portal : focusSection === "otp" ? otp : null
    if (target) target.forceActiveFocus()
    else keyCatcher.forceActiveFocus()
  }
  function scrollCursorIntoView() {
    var target = { action: hero, portal: portal, gateway: gateway, otp: otp,
      submit: submit, refresh: refreshButton, quit: quit }[focusSection]
    if (!target || panelFlick.height <= 0) return
    var y = target.mapToItem(column, 0, 0).y
    var bottom = y + target.height
    if (y < panelFlick.contentY) panelFlick.contentY = y
    else if (bottom > panelFlick.contentY + panelFlick.height) panelFlick.contentY = bottom - panelFlick.height
    panelFlick.contentY = Math.max(0, Math.min(panelFlick.contentY, panelFlick.contentHeight - panelFlick.height))
  }
  onFocusSectionChanged: Qt.callLater(scrollCursorIntoView)
  function activateCursor() {
    if (focusSection === "action" && canAct) service.act(portal.text)
    else if (focusSection === "gateway") gateway.open()
    else if (focusSection === "portal") portal.forceActiveFocus()
    else if (focusSection === "otp") otp.forceActiveFocus()
    else if (focusSection === "submit") submitOtp()
    else if (focusSection === "refresh" && service) service.refresh()
    else if (focusSection === "quit" && service) service.sendCommand("quit")
  }
  function submitOtp() {
    if (service && service.submitOtp(otp.text)) {
      otp.text = ""
      keyCatcher.forceActiveFocus()
    }
  }
  function inputKey(event) {
    if (event.key === Qt.Key_Escape) {
      keyCatcher.forceActiveFocus()
      root.close()
      event.accepted = true
    } else if (event.key === Qt.Key_Tab || event.key === Qt.Key_Backtab) {
      moveCursor(event.key === Qt.Key_Backtab || (event.modifiers & Qt.ShiftModifier) ? -1 : 1)
      event.accepted = true
    }
  }

  onSectionsChanged: if (sections.indexOf(focusSection) < 0) focusSection = "action"
  onOpenedChanged: {
    if (opened) {
      cursorActive = false
      focusSection = "action"
      panelFlick.contentY = 0
      if (service) service.refresh()
      Qt.callLater(function() { (root.showOtp ? otp : keyCatcher).forceActiveFocus() })
    } else {
      otp.text = ""
      gateway.close()
    }
  }

  Connections {
    target: root.service
    function onOtpMessageChanged() {
      otp.text = ""
      if (root.opened && root.showOtp) Qt.callLater(function() { otp.forceActiveFocus() })
    }
  }

  WidgetButton {
    id: button
    anchors.fill: parent
    bar: root.bar
    labelVisible: false
    hasVisualContent: true
    fixedWidth: root.vertical ? -1 : Math.max(Style.bar.iconSlot, barContent.implicitWidth + Style.space(12))
    fixedHeight: root.vertical ? Style.bar.iconSlot : -1
    dimmed: !root.faulted && (!root.service || !root.service.connected) && !(root.service && root.service.busy)
    tooltipText: root.statusText + (root.service && root.service.gatewayName ? " · " + root.service.gatewayName : "")
      + (root.errorText !== "" ? "\n" + root.errorText : "")
      + "\nLeft click: details · Right click: connect/disconnect"
    onPressed: function(b) {
      if (b === Qt.LeftButton) root.toggle()
      else if (b === Qt.RightButton && root.canAct) root.service.act(portal.text)
      else if (b === Qt.MiddleButton && root.service) root.service.refresh()
    }

    Row {
      id: barContent
      anchors.centerIn: parent
      spacing: Style.space(6)
      OpticalGlyph {
        width: Style.bar.iconCanvas
        height: Style.bar.iconCanvas
        anchors.verticalCenter: parent.verticalCenter
        text: root.icon
        fontFamily: button.fontFamily
        fontSize: Style.bar.iconFont
        color: root.faulted ? button.activeColor : button.foreground
      }
      Text {
        textFormat: Text.PlainText
        anchors.verticalCenter: parent.verticalCenter
        visible: root.showGateway && !root.vertical && root.service && root.service.connected
        text: root.service ? root.service.gatewayName : ""
        width: visible ? Math.min(implicitWidth, Style.space(130)) : 0
        elide: Text.ElideRight
        color: button.foreground
        font.family: button.fontFamily
        font.pixelSize: Style.font.body
      }
    }
  }

  KeyboardPanel {
    id: panel
    anchorItem: button
    owner: root
    bar: root.bar
    open: root.opened
    focusTarget: root.showOtp ? otp : keyCatcher
    contentWidth: panel.fittedContentWidth(Style.space(390))
    contentHeight: panel.fittedContentHeight(column.implicitHeight, Style.space(620))

    PanelKeyCatcher {
      id: keyCatcher
      anchors.fill: parent
      blocked: portal.activeFocus || otp.activeFocus || gateway.popupOpen
      onMoveRequested: function(dx, dy) {
        if (dy) root.moveCursor(dy)
        else if (dx) root.switchPanel(dx)
      }
      onTabRequested: function(direction) { root.moveCursor(direction) }
      onActivateRequested: root.activateCursor()
      onCloseRequested: root.close()
      onTextKey: function(text) { if (text.toLowerCase() === "r" && root.service) root.service.refresh() }

      Flickable {
        id: panelFlick
        anchors.fill: parent
        contentWidth: width
        contentHeight: column.implicitHeight
        clip: true
        boundsBehavior: Flickable.StopAtBounds
        flickableDirection: Flickable.VerticalFlick
        interactive: contentHeight > height
        Controls.ScrollBar.vertical: Controls.ScrollBar { policy: Controls.ScrollBar.AsNeeded }

        Column {
          id: column
          width: panelFlick.width
          spacing: Style.spacing.panelGap

          PanelHero {
            id: hero
            width: parent.width
            title: "GlobalProtect"
            meta: root.statusText
            foreground: root.foreground
            fontFamily: root.fontFamily
            iconComponent: Component {
              Text {
                textFormat: Text.PlainText
                text: root.icon
                color: root.faulted ? Commons.Color.urgent : root.foreground
                font.family: root.fontFamily
                font.pixelSize: Style.font.displayLarge
              }
            }
            trailingControl: Component {
              Button {
                text: root.service ? root.service.actionText : "Unavailable"
                enabled: root.canAct
                bordered: true
                foreground: root.foreground
                hasCursor: root.cursorActive && root.focusSection === "action"
                onHovered: function(hot) { if (hot) root.chooseSection("action") }
                onClicked: root.service.act(portal.text)
              }
            }
          }

          Text {
            textFormat: Text.PlainText
            width: parent.width
            visible: root.errorText !== ""
            text: root.errorText
            wrapMode: Text.Wrap
            color: Commons.Color.urgent
            font.family: root.fontFamily
            font.pixelSize: Style.font.body
          }

          Column {
            width: parent.width
            spacing: Style.spacing.labelGap
            visible: root.showPortal
            PanelSectionHeader { text: root.state === "needs-setup" ? "PORTAL SETUP" : "PORTAL"; foreground: root.foreground }
            TextField {
              id: portal
              width: parent.width
              placeholderText: root.state === "needs-setup" ? "vpn.example.com" : "Use configured portal, or enter an address"
              foreground: root.foreground
              enabled: !root.service || !root.service.pending
              hasCursor: root.cursorActive && root.focusSection === "portal"
              onActiveFocusChanged: if (activeFocus) root.chooseSection("portal")
              onAccepted: if (root.canAct) root.service.act(text)
              Keys.onPressed: function(event) { root.inputKey(event) }
            }
            Text {
              textFormat: Text.PlainText
              width: parent.width
              text: "The portal is saved after a successful connection."
              wrapMode: Text.Wrap
              color: Qt.darker(root.foreground, 1.4)
              font.family: root.fontFamily
              font.pixelSize: Style.font.caption
            }
          }

          Dropdown {
            id: gateway
            width: parent.width
            visible: root.service && (root.service.gatewayOptions.length > 1 || root.service.selectedGateway !== "")
            label: root.service && root.service.connected ? "GATEWAY FOR NEXT CONNECTION" : "GATEWAY"
            options: [{ value: "", label: "Automatic" }].concat(root.service ? root.service.gatewayOptions : [])
            value: root.service ? root.service.selectedGateway : ""
            foreground: root.foreground
            enabled: root.service && !root.service.busy && !root.service.pending
            hasCursor: root.cursorActive && root.focusSection === "gateway"
            onHovered: function(hot) { if (hot) root.chooseSection("gateway") }
            onChanged: function(value) { root.service.selectedGateway = value; keyCatcher.forceActiveFocus() }
          }

          Column {
            width: parent.width
            visible: root.showOtp
            spacing: Style.spacing.lg
            PanelSectionHeader { text: "ONE-TIME PASSWORD"; foreground: root.foreground }
            Text {
              textFormat: Text.PlainText
              width: parent.width
              text: root.service ? root.service.otpMessage : ""
              wrapMode: Text.Wrap
              color: root.foreground
              font.family: root.fontFamily
              font.pixelSize: Style.font.body
            }
            Row {
              width: parent.width
              spacing: Style.spacing.controlGap
              TextField {
                id: otp
                width: parent.width - submit.implicitWidth - parent.spacing
                password: true
                placeholderText: "One-time password"
                foreground: root.foreground
                enabled: root.service && !root.service.pending
                hasCursor: root.cursorActive && root.focusSection === "otp"
                inputMethodHints: Qt.ImhSensitiveData | Qt.ImhNoPredictiveText
                onActiveFocusChanged: if (activeFocus) root.chooseSection("otp")
                onAccepted: root.submitOtp()
                Keys.onPressed: function(event) { root.inputKey(event) }
              }
              Button {
                id: submit
                text: "Submit"
                enabled: otp.text.trim() !== "" && root.service && !root.service.pending
                bordered: true
                foreground: root.foreground
                hasCursor: root.cursorActive && root.focusSection === "submit"
                onHovered: function(hot) { if (hot) root.chooseSection("submit") }
                onClicked: root.submitOtp()
              }
            }
          }

          Column {
            width: parent.width
            spacing: Style.spacing.lg
            visible: root.service && root.service.ready && root.state !== "needs-setup"
            PanelSeparator { foreground: root.foreground }
            PanelSectionHeader { text: "CONNECTION"; foreground: root.foreground }
            InfoRow { label: "Portal"; value: root.service ? String(root.service.snapshot.portal || "—") : "—" }
            InfoRow { label: "Gateway"; value: root.service ? root.service.gatewayName || "—" : "—" }
            InfoRow {
              label: "Gateway address"
              value: root.service && root.service.snapshot.gateway ? String(root.service.snapshot.gateway.address || "") : ""
              visible: value !== "" && root.service && value !== root.service.gatewayName
            }
            InfoRow { label: "Tunnel IP"; value: root.conn.ipv4 || "—"; visible: root.service && root.service.connected }
            InfoRow { label: "Interface"; value: root.conn.ifname || "—"; visible: root.service && root.service.connected }
            InfoRow {
              label: "Uptime"
              visible: root.service && root.service.connected
              value: root.conn.since ? Model.duration(root.service.now - root.conn.since) : "—"
            }
            InfoRow {
              label: "Session"
              visible: root.service && root.service.connected
              value: !root.session.expiresAt ? "—" : root.session.expiresAt <= root.service.now ? "Expired"
                : Model.duration(root.session.expiresAt - root.service.now) + " remaining"
              urgent: !!root.session.expiresAt && !!root.service && root.session.expiresAt - root.service.now <= (root.session.warnPriorSecs ?? 600)
            }
          }

          Column {
            width: parent.width
            visible: root.service && root.service.connected
            spacing: Style.spacing.lg
            PanelSeparator { foreground: root.foreground }
            Row {
              width: parent.width
              Traffic { width: parent.width / 2; label: "↓ DOWNLOAD"; rate: root.conn.rxRate || 0; total: root.conn.rxBytes || 0 }
              Traffic { width: parent.width / 2; label: "↑ UPLOAD"; rate: root.conn.txRate || 0; total: root.conn.txBytes || 0 }
            }
          }

          PanelSeparator { foreground: root.foreground }
          Row {
            width: parent.width
            spacing: Style.spacing.controlGap
            Button {
              id: refreshButton
              text: "Refresh"
              foreground: root.foreground
              hasCursor: root.cursorActive && root.focusSection === "refresh"
              onHovered: function(hot) { if (hot) root.chooseSection("refresh") }
              onClicked: if (root.service) root.service.refresh()
            }
            Item { width: Math.max(0, parent.width - parent.children[0].implicitWidth - quit.implicitWidth - parent.spacing * 2); height: 1 }
            Button {
              id: quit
              text: "Stop service"
              visible: root.service && root.service.ready
              enabled: root.service && !root.service.pending
              foreground: root.foreground
              hasCursor: root.cursorActive && root.focusSection === "quit"
              onHovered: function(hot) { if (hot) root.chooseSection("quit") }
              onClicked: root.service.sendCommand("quit")
            }
          }
        }
      }
    }
  }

  component InfoRow: Row {
    property string label: ""
    property string value: ""
    property bool urgent: false
    width: parent.width
    spacing: Style.spacing.controlGap
    Text {
      id: caption
      textFormat: Text.PlainText
      width: Math.max(Style.space(82), implicitWidth)
      text: parent.label
      color: Qt.darker(root.foreground, 1.4)
      font.family: root.fontFamily
      font.pixelSize: Style.font.body
    }
    Text {
      textFormat: Text.PlainText
      width: Math.max(0, parent.width - caption.width - parent.spacing)
      text: parent.value
      color: parent.urgent ? Commons.Color.urgent : root.foreground
      horizontalAlignment: Text.AlignRight
      wrapMode: Text.WrapAnywhere
      font.family: root.fontFamily
      font.pixelSize: Style.font.body
    }
  }

  component Traffic: Column {
    property string label: ""
    property real rate: 0
    property real total: 0
    spacing: Style.spacing.labelGap
    PanelSectionHeader { text: parent.label; foreground: root.foreground }
    Text {
      textFormat: Text.PlainText
      text: Model.bytes(parent.rate) + "/s"
      color: root.foreground
      font.family: root.fontFamily
      font.pixelSize: Style.font.heading
    }
    Text {
      textFormat: Text.PlainText
      text: Model.bytes(parent.total) + " total"
      color: Qt.darker(root.foreground, 1.4)
      font.family: root.fontFamily
      font.pixelSize: Style.font.bodySmall
    }
  }
}
