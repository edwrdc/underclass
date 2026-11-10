import QtQuick
import qs.Common
import qs.DCommon.Widgets
import qs.Modules.Plugins

PluginSettings {
    id: root
    pluginId: "underclass"

    StyledText {
        width: parent.width
        text: "Underclass"
        font.pixelSize: Theme.fontSizeLarge
        font.weight: Theme.fontWeightMedium
        color: Theme.surfaceText
    }

    StyledText {
        width: parent.width
        text: "Point this at your underclass server and give it the admin (UI) token. The token is stored in DMS settings in plain text, so a token file is the better choice. On the server host itself you can leave both empty and the widget reads the local monitor socket."
        font.pixelSize: Theme.fontSizeSmall
        color: Theme.surfaceVariantText
        wrapMode: Text.WordWrap
    }

    StringSetting {
        settingKey: "serverUrl"
        label: "Server URL"
        description: "Base URL of the underclass server, without a path"
        placeholder: "https://underclass.example.com"
        defaultValue: ""
    }

    StringSetting {
        settingKey: "tokenFile"
        label: "Admin token file"
        description: "Path to a file holding the admin token (preferred). Absolute path, no ~"
        placeholder: "/home/you/.config/underclass/ui_token"
        defaultValue: ""
    }

    StringSetting {
        settingKey: "adminToken"
        label: "Admin token"
        description: "Used only when no token file is set. Visible in the process list while a fetch runs."
        placeholder: "ui token"
        defaultValue: ""
    }

    StringSetting {
        settingKey: "socketPath"
        label: "Local monitor socket (same-host only)"
        description: "Used only when Server URL is empty. Empty tries $UNDERCLASS_MONITOR_SOCKET, /run/underclass/monitor.sock, then ~/.local/share/underclass/monitor.sock"
        placeholder: "/run/underclass/monitor.sock"
        defaultValue: ""
    }

    StringSetting {
        settingKey: "pollSeconds"
        label: "Refresh interval (seconds)"
        description: "How often the snapshot is fetched"
        placeholder: "5"
        defaultValue: "5"
    }
}
