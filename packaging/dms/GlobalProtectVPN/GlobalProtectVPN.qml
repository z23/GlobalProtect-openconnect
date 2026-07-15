import QtQuick
import Quickshell
import Quickshell.Io
import qs.Common
import qs.Widgets
import qs.Modules.Plugins
import "./Formatting.js" as Fmt

PluginComponent {
    id: root

    property bool showGatewayName: pluginData.showGatewayName ?? true

    // Last status snapshot from the gpwidget daemon; null while the stack
    // is down (no socket) — that absence *is* the "VPN off" state.
    property var snap: null
    property bool socketUp: false
    property int cmdSeq: 0
    property int nowSecs: Math.floor(Date.now() / 1000)

    readonly property string vpnState: socketUp && snap && snap.state ? snap.state : "stack-down"
    readonly property bool isConnected: vpnState === "connected"
    readonly property bool isBusy: vpnState === "authenticating" || vpnState === "connecting" || vpnState === "disconnecting"

    function stateIcon() {
        switch (vpnState) {
        case "connected":
            return "vpn_lock";
        case "authenticating":
        case "connecting":
        case "disconnecting":
            return "sync";
        case "error":
            return "gpp_maybe";
        case "needs-setup":
            return "settings";
        default:
            return "vpn_key_off";
        }
    }

    function stateColor() {
        switch (vpnState) {
        case "connected":
            return Theme.primary;
        case "authenticating":
        case "connecting":
        case "disconnecting":
            return Theme.warning;
        case "error":
            return Theme.error;
        default:
            return Theme.surfaceVariantText;
        }
    }

    function stateLabel() {
        switch (vpnState) {
        case "connected":
            return snap && snap.gateway && snap.gateway.name ? "Connected · " + snap.gateway.name : "Connected";
        case "authenticating":
            return "Authenticating…";
        case "connecting":
            return "Connecting…";
        case "disconnecting":
            return "Disconnecting…";
        case "error":
            return "Error";
        case "needs-setup":
            return "Setup required";
        case "disconnected":
            return "Disconnected";
        default:
            return "VPN off";
        }
    }

    function sendCommand(command, extra) {
        let msg = extra || {};
        msg.type = command;
        msg.id = ++cmdSeq;
        gpSocket.send(msg);
    }

    function doConnect(gateway) {
        if (!socketUp) {
            // Stack down: `gpwidget connect` brings the whole chain up
            // (gpclient launch-gui → pkexec gpservice → daemon) and connects.
            Quickshell.execDetached(["gpwidget", "connect"]);
            return;
        }

        sendCommand("connect", gateway ? { gateway: gateway } : {});
    }

    function doDisconnect() {
        sendCommand("disconnect");
    }

    function doToggle() {
        if (!socketUp) {
            doConnect(null);
        } else {
            sendCommand("toggle");
        }
    }

    // DankSocket (qs.Common) keeps a resilient link to the gpwidget daemon.
    // Quickshell's raw Socket does NOT auto-reconnect (re-asserting connected=true
    // is a no-op), so DankSocket toggles the link with exponential backoff as the
    // VPN service comes and goes — mirroring how DMS's own services (NiriService)
    // talk to a socket.
    // DankSocket initiates only on a connected:false→true transition, so it is
    // armed here after construction. A constant initial `connected: true` does
    // NOT fire DankSocket's onConnectedChanged, so the link would never open.
    Component.onCompleted: gpSocket.connected = true

    DankSocket {
        id: gpSocket
        path: Quickshell.env("XDG_RUNTIME_DIR") + "/gpwidget.sock"

        parser: SplitParser {
            onRead: message => {
                try {
                    const msg = JSON.parse(message);
                    if (msg.type === "status") {
                        root.snap = msg;
                    } else if (msg.type === "bye") {
                        root.snap = null;
                    }
                } catch (e) {
                    console.warn("gpwidget: malformed message", e);
                }
            }
        }

        onConnectionStateChanged: {
            root.socketUp = linkUp;
            if (!linkUp) {
                root.snap = null;
            }
        }
    }

    // Local uptime/expiry tick while connected.
    Timer {
        interval: 1000
        repeat: true
        running: root.isConnected
        onTriggered: root.nowSecs = Math.floor(Date.now() / 1000)
    }

    horizontalBarPill: Component {
        Row {
            spacing: Theme.spacingXS

            DankIcon {
                name: root.stateIcon()
                size: Theme.iconSize - 6
                color: root.stateColor()
                anchors.verticalCenter: parent.verticalCenter
            }

            StyledText {
                visible: root.showGatewayName && root.isConnected && root.snap && root.snap.gateway && root.snap.gateway.name !== ""
                text: root.snap && root.snap.gateway ? root.snap.gateway.name : ""
                font.pixelSize: Theme.fontSizeSmall
                font.weight: Font.Medium
                color: Theme.surfaceVariantText
                anchors.verticalCenter: parent.verticalCenter
            }
        }
    }

    verticalBarPill: Component {
        Column {
            spacing: Theme.spacingXS

            DankIcon {
                name: root.stateIcon()
                size: Theme.iconSize - 6
                color: root.stateColor()
                anchors.horizontalCenter: parent.horizontalCenter
            }
        }
    }

    popoutContent: Component {
        PopoutComponent {
            id: popout

            headerText: "GlobalProtect VPN"
            detailsText: root.stateLabel()
            showCloseButton: true

            Column {
                width: parent.width
                spacing: Theme.spacingM

                // Action button.
                Rectangle {
                    width: parent.width
                    height: 40
                    radius: Theme.cornerRadius
                    color: {
                        if (!actionArea.enabled)
                            return Theme.surfaceContainerHigh;
                        if (root.isConnected || root.isBusy)
                            return actionArea.containsMouse ? Qt.darker(Theme.error, 1.1) : Theme.error;
                        return actionArea.containsMouse ? Qt.darker(Theme.primary, 1.1) : Theme.primary;
                    }

                    StyledText {
                        anchors.centerIn: parent
                        text: {
                            switch (root.vpnState) {
                            case "stack-down":
                                return "Start VPN";
                            case "connected":
                                return "Disconnect";
                            case "authenticating":
                            case "connecting":
                                return "Cancel";
                            case "disconnecting":
                                return "Disconnecting…";
                            default:
                                return "Connect";
                            }
                        }
                        font.weight: Font.Medium
                        color: Theme.background
                    }

                    MouseArea {
                        id: actionArea
                        anchors.fill: parent
                        hoverEnabled: true
                        enabled: root.vpnState !== "disconnecting" && root.vpnState !== "needs-setup"
                        cursorShape: enabled ? Qt.PointingHandCursor : Qt.ArrowCursor
                        onClicked: {
                            if (root.isConnected || root.isBusy)
                                root.doDisconnect();
                            else
                                root.doConnect(gatewayPicker.pickedGateway());
                        }
                    }
                }

                // Gateway picker (only when there is a choice).
                Column {
                    id: gatewayPicker
                    width: parent.width
                    spacing: Theme.spacingXS
                    visible: root.snap && root.snap.gateways && root.snap.gateways.length > 1

                    property int selectedIndex: -1

                    function pickedGateway() {
                        if (!visible || selectedIndex < 0 || !root.snap || !root.snap.gateways)
                            return null;
                        return root.snap.gateways[selectedIndex].name;
                    }

                    StyledText {
                        text: "Gateway"
                        font.pixelSize: Theme.fontSizeSmall
                        color: Theme.surfaceVariantText
                    }

                    Repeater {
                        model: root.snap ? root.snap.gateways : []

                        Rectangle {
                            required property var modelData
                            required property int index

                            width: gatewayPicker.width
                            height: 32
                            radius: Theme.cornerRadius
                            color: {
                                const current = root.snap && root.snap.gateway && root.snap.gateway.name === modelData.name;
                                const picked = gatewayPicker.selectedIndex === index;
                                if (picked || (gatewayPicker.selectedIndex < 0 && current))
                                    return Theme.surfaceContainerHighest;
                                return gwArea.containsMouse ? Theme.surfaceContainerHigh : "transparent";
                            }

                            Row {
                                anchors.verticalCenter: parent.verticalCenter
                                anchors.left: parent.left
                                anchors.leftMargin: Theme.spacingS
                                spacing: Theme.spacingS

                                DankIcon {
                                    name: root.snap && root.snap.gateway && root.snap.gateway.name === modelData.name ? "check" : "public"
                                    size: Theme.iconSize - 8
                                    color: Theme.surfaceVariantText
                                    anchors.verticalCenter: parent.verticalCenter
                                }

                                StyledText {
                                    text: modelData.name
                                    font.pixelSize: Theme.fontSizeSmall
                                    color: Theme.surfaceText
                                    anchors.verticalCenter: parent.verticalCenter
                                }
                            }

                            MouseArea {
                                id: gwArea
                                anchors.fill: parent
                                hoverEnabled: true
                                cursorShape: Qt.PointingHandCursor
                                onClicked: gatewayPicker.selectedIndex = index
                            }
                        }
                    }
                }

                // Stats.
                Rectangle {
                    width: parent.width
                    height: statsColumn.implicitHeight + Theme.spacingM * 2
                    radius: Theme.cornerRadius
                    color: Theme.surfaceContainerHigh
                    visible: root.isConnected || root.vpnState === "connecting"

                    Column {
                        id: statsColumn
                        anchors.fill: parent
                        anchors.margins: Theme.spacingM
                        spacing: Theme.spacingXS

                        Repeater {
                            model: {
                                const rows = [];
                                const s = root.snap;
                                if (!s)
                                    return rows;

                                if (s.portal)
                                    rows.push({ key: "Portal", value: s.portal });
                                if (s.gateway)
                                    rows.push({ key: "Gateway", value: s.gateway.name + " (" + s.gateway.address + ")" });
                                if (s.conn && s.conn.ipv4)
                                    rows.push({ key: "IP", value: s.conn.ipv4 + (s.conn.ifname ? " (" + s.conn.ifname + ")" : "") });
                                if (s.conn && s.conn.since)
                                    rows.push({ key: "Uptime", value: Fmt.formatDuration(root.nowSecs - s.conn.since) });
                                if (s.session && s.session.expiresAt)
                                    rows.push({ key: "Session", value: Fmt.formatExpiry(s.session.expiresAt, root.nowSecs) });
                                if (s.conn && (s.conn.rxBytes > 0 || s.conn.txBytes > 0))
                                    rows.push({ key: "Traffic", value: "↓ " + Fmt.formatBytes(s.conn.rxBytes) + " · ↑ " + Fmt.formatBytes(s.conn.txBytes) });
                                return rows;
                            }

                            Item {
                                required property var modelData

                                width: statsColumn.width
                                height: 22

                                StyledText {
                                    anchors.left: parent.left
                                    anchors.verticalCenter: parent.verticalCenter
                                    text: modelData.key
                                    font.pixelSize: Theme.fontSizeSmall
                                    color: Theme.surfaceVariantText
                                }

                                StyledText {
                                    anchors.right: parent.right
                                    anchors.verticalCenter: parent.verticalCenter
                                    text: modelData.value
                                    font.pixelSize: Theme.fontSizeSmall
                                    color: Theme.surfaceText
                                }
                            }
                        }
                    }
                }

                // Error / hint banner.
                StyledText {
                    width: parent.width
                    visible: root.vpnState === "error" || root.vpnState === "needs-setup" || root.vpnState === "stack-down"
                    text: {
                        if (root.vpnState === "error" && root.snap && root.snap.error)
                            return root.snap.error;
                        if (root.vpnState === "needs-setup")
                            return "Set the portal in ~/.config/gpwidget/config.toml";
                        if (root.vpnState === "stack-down")
                            return "The VPN service is not running. Start VPN launches it (polkit).";
                        return "";
                    }
                    font.pixelSize: Theme.fontSizeSmall
                    color: root.vpnState === "error" ? Theme.error : Theme.surfaceVariantText
                    wrapMode: Text.WordWrap
                }
            }
        }
    }

    popoutWidth: 360
    popoutHeight: 420

    // Control-center toggle.
    ccWidgetIcon: stateIcon()
    ccWidgetPrimaryText: "GlobalProtect"
    ccWidgetSecondaryText: stateLabel()
    ccWidgetIsActive: isConnected || isBusy
    onCcWidgetToggled: doToggle()
}
