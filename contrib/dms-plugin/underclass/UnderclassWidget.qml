import QtQuick
import QtQuick.Layouts
import qs.Common
import qs.DCommon.Widgets
import qs.Modules.Plugins

PluginComponent {
    id: root

    layerNamespacePlugin: "underclass"

    property string socketPath: pluginData.socketPath || ""
    property string serverUrl: pluginData.serverUrl || ""
    property string adminToken: pluginData.adminToken || ""
    property string tokenFile: pluginData.tokenFile || ""
    property int pollSeconds: Math.max(2, parseInt(pluginData.pollSeconds) || 5)

    // The last good snapshot, or null before the first fetch / when the server is unreachable.
    property var snapshot: null
    property bool offline: false
    property double nowMs: Date.now()

    readonly property var accounts: snapshot ? snapshot.accounts : []
    readonly property int total: accounts.length
    readonly property int healthy: accounts.filter(a => a.status === "healthy").length
    readonly property bool poolExhausted: snapshot !== null && total > 0 && healthy === 0

    // Earliest moment a cooling account comes back, in epoch ms, or 0 when none is waiting.
    readonly property double earliestReset: {
        let best = 0;
        for (const a of accounts) {
            if (a.status === "cooling" && a.reset_at > 0 && (best === 0 || a.reset_at < best))
                best = a.reset_at;
        }
        return best;
    }

    // Fraction of the tightest quota window left on an account (0..1), or null when unknown.
    function remaining(a) {
        let left = [];
        const rl = a.quota && a.quota.rate_limit;
        if (rl) {
            for (const w of [rl.primary_window, rl.secondary_window])
                if (w && typeof w.used_percent === "number")
                    left.push(1 - w.used_percent / 100);
        }
        const o = a.observation;
        if (o) {
            for (const k of ["five_hour", "seven_day"])
                if (o[k] && typeof o[k].utilization === "number")
                    left.push(1 - o[k].utilization);
        }
        if (left.length === 0)
            return null;
        return Math.max(0, Math.min(1, Math.min.apply(null, left)));
    }

    function quotaColor(frac) {
        if (frac === null)
            return Theme.surfaceVariantText;
        if (frac <= 0.10)
            return Theme.error;
        if (frac <= 0.30)
            return Theme.warning;
        return Theme.success;
    }

    function statusColor(status) {
        if (status === "healthy")
            return Theme.success;
        if (status === "cooling")
            return Theme.warning;
        return Theme.error;
    }

    function countdown(untilMs) {
        const s = Math.max(0, Math.round((untilMs - nowMs) / 1000));
        if (s >= 86400)
            return Math.floor(s / 86400) + "d " + Math.floor((s % 86400) / 3600) + "h";
        if (s >= 3600)
            return Math.floor(s / 3600) + "h " + Math.floor((s % 3600) / 60) + "m";
        if (s >= 60)
            return Math.floor(s / 60) + "m";
        return s + "s";
    }

    function tokens(n) {
        if (n >= 1e9)
            return (n / 1e9).toFixed(1) + "B";
        if (n >= 1e6)
            return (n / 1e6).toFixed(1) + "M";
        if (n >= 1e3)
            return (n / 1e3).toFixed(1) + "k";
        return "" + n;
    }

    readonly property color pillColor: offline ? Theme.surfaceVariantText : poolExhausted ? Theme.error : (healthy < total ? Theme.warning : Theme.primary)

    readonly property string pillText: {
        if (offline)
            return "offline";
        if (snapshot === null)
            return "…";
        if (poolExhausted)
            return earliestReset > 0 ? countdown(earliestReset) : "0/" + total;
        return healthy + "/" + total;
    }

    // Positional arguments keep user settings out of the shell script text.
    // Remote mode sends the token as curl config on stdin so it never appears in curl's argv.
    // A token file wins over the inline token.
    readonly property string fetchScript: 'if [ -n "$1" ]; then
  t="$2"
  if [ -n "$4" ]; then t=$(cat "$4") || exit 1; fi
  printf "header = \\"Authorization: Bearer %s\\"\\n" "$t" | curl -fsS --max-time 4 -K - "${1%/}/admin/api/monitor"
  exit $?
fi
s="$3"
for c in "$s" "$UNDERCLASS_MONITOR_SOCKET" /run/underclass/monitor.sock "$HOME/.local/share/underclass/monitor.sock"; do
  if [ -n "$c" ] && [ -S "$c" ]; then s="$c"; break; fi
done
exec curl -fsS --max-time 4 --unix-socket "$s" http://localhost/monitor'

    function refresh() {
        Proc.runCommand("underclass.monitor", ["sh", "-c", fetchScript, "sh", serverUrl, adminToken, socketPath, tokenFile],(stdout, exitCode) => {
            if (exitCode !== 0) {
                root.offline = true;
                return;
            }
            try {
                root.snapshot = JSON.parse(stdout);
                root.offline = false;
            } catch (e) {
                root.offline = true;
            }
            root.nowMs = Date.now();
        }, 0, 8000);
    }

    Timer {
        interval: root.pollSeconds * 1000
        running: true
        repeat: true
        triggeredOnStart: true
        onTriggered: root.refresh()
    }

    // Keeps the countdown moving between fetches.
    Timer {
        interval: 1000
        running: root.poolExhausted
        repeat: true
        onTriggered: root.nowMs = Date.now()
    }

    horizontalBarPill: Component {
        Row {
            spacing: Theme.spacingXS

            DIcon {
                name: root.poolExhausted ? "hourglass_top" : "stacked_bar_chart"
                size: root.iconSize
                color: root.pillColor
                anchors.verticalCenter: parent.verticalCenter
            }

            StyledText {
                text: root.pillText
                color: root.pillColor
                font.pixelSize: root.textSize
                anchors.verticalCenter: parent.verticalCenter
            }
        }
    }

    verticalBarPill: Component {
        Column {
            spacing: Theme.spacingXS

            DIcon {
                name: root.poolExhausted ? "hourglass_top" : "stacked_bar_chart"
                size: root.iconSize
                color: root.pillColor
                anchors.horizontalCenter: parent.horizontalCenter
            }

            StyledText {
                text: root.pillText
                color: root.pillColor
                font.pixelSize: root.textSize
                anchors.horizontalCenter: parent.horizontalCenter
                horizontalAlignment: Text.AlignHCenter
            }
        }
    }

    popoutContent: Component {
        PopoutComponent {
            id: popout

            headerText: "Underclass"
            detailsText: root.offline ? "Cannot reach the monitor" : root.poolExhausted ? "Pool exhausted, requests get 429" : root.healthy + " of " + root.total + " accounts in rotation"

            Flickable {
                width: parent.width
                height: root.popoutHeight - popout.headerHeight - popout.detailsHeight - Theme.spacingXL
                contentHeight: body.implicitHeight
                clip: true

                Column {
                    id: body
                    width: parent.width
                    spacing: Theme.spacingM

                    // Requests per minute over the last hour.
                    Rectangle {
                        width: parent.width
                        height: 64
                        radius: Theme.cornerRadius
                        color: Theme.cardSurface
                        visible: root.snapshot !== null

                        readonly property var bins: root.snapshot ? root.snapshot.minute_bins : []
                        readonly property real peak: Math.max(1, Math.max.apply(null, bins.length ? bins : [1]))

                        Row {
                            anchors.fill: parent
                            anchors.margins: Theme.spacingS
                            spacing: 1
                            Repeater {
                                model: parent.parent.bins
                                Item {
                                    width: Math.max(1, (parent.width - 59) / Math.max(1, parent.parent.bins.length))
                                    height: parent.height
                                    Rectangle {
                                        anchors.bottom: parent.bottom
                                        width: parent.width
                                        height: Math.max(modelData > 0 ? 2 : 0, parent.height * modelData / parent.parent.parent.peak)
                                        radius: 1
                                        color: Theme.primary
                                    }
                                }
                            }
                        }

                        StyledText {
                            anchors.top: parent.top
                            anchors.right: parent.right
                            anchors.margins: Theme.spacingXS
                            text: "attempts / min · 60 min"
                            font.pixelSize: Theme.fontSizeSmall - 2
                            color: Theme.surfaceVariantText
                        }
                    }

                    StyledText {
                        visible: root.snapshot !== null
                        width: parent.width
                        font.pixelSize: Theme.fontSizeSmall
                        color: Theme.surfaceVariantText
                        wrapMode: Text.WordWrap
                        text: root.snapshot ? "last minute " + root.snapshot.last_minute.attempts + " req · month " + root.tokens(root.snapshot.month.input_tokens) + " in / " + root.tokens(root.snapshot.month.output_tokens) + " out" : ""
                    }

                    Repeater {
                        model: root.accounts

                        Rectangle {
                            width: body.width
                            height: accountCol.implicitHeight + Theme.spacingM * 2
                            radius: Theme.cornerRadius
                            color: Theme.cardSurface

                            readonly property var acct: modelData
                            readonly property var frac: root.remaining(modelData)

                            Column {
                                id: accountCol
                                anchors.fill: parent
                                anchors.margins: Theme.spacingM
                                spacing: Theme.spacingXS

                                Row {
                                    spacing: Theme.spacingS
                                    Rectangle {
                                        width: 8
                                        height: 8
                                        radius: 4
                                        color: root.statusColor(acct.status)
                                        anchors.verticalCenter: parent.verticalCenter
                                    }
                                    StyledText {
                                        text: acct.label
                                        color: Theme.surfaceText
                                        font.pixelSize: Theme.fontSizeMedium
                                        elide: Text.ElideRight
                                        width: Math.min(implicitWidth, accountCol.width - 90)
                                    }
                                    StyledText {
                                        text: acct.backend + (acct.plan ? " · " + acct.plan : "")
                                        color: Theme.surfaceVariantText
                                        font.pixelSize: Theme.fontSizeSmall
                                        anchors.verticalCenter: parent.verticalCenter
                                    }
                                }

                                // Quota headroom bar.
                                Rectangle {
                                    width: parent.width
                                    height: 6
                                    radius: 3
                                    color: Theme.chipSurface
                                    visible: frac !== null
                                    Rectangle {
                                        width: parent.width * (frac === null ? 0 : frac)
                                        height: parent.height
                                        radius: 3
                                        color: root.quotaColor(frac)
                                    }
                                }

                                StyledText {
                                    width: parent.width
                                    font.pixelSize: Theme.fontSizeSmall
                                    color: Theme.surfaceVariantText
                                    wrapMode: Text.WordWrap
                                    text: {
                                        let parts = [];
                                        if (acct.status !== "healthy")
                                            parts.push(acct.status === "cooling" && acct.reset_at > 0 ? "cooling, back in " + root.countdown(acct.reset_at) : acct.status.replace("_", " "));
                                        if (frac !== null)
                                            parts.push(Math.round(frac * 100) + "% left");
                                        if (acct.inflight > 0)
                                            parts.push(acct.inflight + " in flight");
                                        parts.push(acct.sticky_sessions + " sessions");
                                        parts.push(root.tokens(acct.month.input_tokens + acct.month.output_tokens) + " tok/mo");
                                        return parts.join(" · ");
                                    }
                                }
                            }
                        }
                    }

                    StyledText {
                        visible: root.snapshot !== null && root.recentErrors.length > 0
                        text: "Recent failures"
                        color: Theme.surfaceText
                        font.pixelSize: Theme.fontSizeMedium
                    }

                    Repeater {
                        model: root.recentErrors
                        StyledText {
                            width: body.width
                            elide: Text.ElideRight
                            font.pixelSize: Theme.fontSizeSmall
                            color: Theme.surfaceVariantText
                            text: modelData.status + " · " + modelData.model + (modelData.label ? " · " + modelData.label : "") + " · " + root.countdown(root.nowMs + (root.nowMs - modelData.ts)) + " ago"
                        }
                    }
                }
            }
        }
    }

    readonly property var recentErrors: snapshot ? snapshot.recent.filter(r => r.status >= 400).slice(0, 5) : []

    popoutWidth: 420
    popoutHeight: 560
}
