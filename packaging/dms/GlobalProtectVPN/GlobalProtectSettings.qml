import QtQuick
import qs.Common
import qs.Widgets
import qs.Modules.Plugins

PluginSettings {
    id: root
    pluginId: "globalProtectVpn"

    StyledText {
        width: parent.width
        text: "GlobalProtect VPN"
        font.pixelSize: Theme.fontSizeLarge
        font.weight: Font.Bold
        color: Theme.surfaceText
    }

    StyledText {
        width: parent.width
        text: "Presentation options only — connection settings (portal, gateway pin, browser mode) live in ~/.config/gpwidget/config.toml so the waybar module, popup, CLI and this plugin all share one source of truth."
        font.pixelSize: Theme.fontSizeSmall
        color: Theme.surfaceVariantText
        wrapMode: Text.WordWrap
    }

    ToggleSetting {
        settingKey: "showGatewayName"
        label: "Show gateway name on the bar"
        description: "Display the connected gateway next to the icon"
        defaultValue: true
    }
}
