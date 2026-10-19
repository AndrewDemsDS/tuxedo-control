// SPDX-License-Identifier: MIT
// Fan-curve editor: drag points, double-click empty space to add a point,
// double-click a point to remove it (at least two stay). Points never go
// below the daemon's safety floor (red dashed line).
import QtQuick
import QtQuick.Controls as QQC2
import QtQuick.Layouts
import org.kde.kirigami as Kirigami
import org.andreasincode.tuxedocontrol

Kirigami.Dialog {
    id: dlg
    title: i18n("Fan curve: %1", profName)
    standardButtons: Kirigami.Dialog.Save | Kirigami.Dialog.Cancel
    preferredWidth: Kirigami.Units.gridUnit * 34
    padding: Kirigami.Units.largeSpacing

    signal saved(string name, bool ok)

    property string profName: ""
    property string perf: "enthusiast"
    property var kbd: null
    property var charge: null
    property var pts: []

    function openFor(p) {
        profName = p.name
        perf = p.perf
        kbd = p.kbd ?? null
        charge = p.charge ?? null
        pts = p.curve.map(x => [x[0], x[1]])
        normalise()
        open()
    }

    function normalise() {
        const a = pts.map(p => [Math.max(0, Math.min(100, Math.round(p[0]))),
                                Math.max(Backend.safetyFloor(Math.round(p[0])), Math.max(0, Math.min(100, Math.round(p[1]))))])
        a.sort((x, y) => x[0] - y[0])
        pts = a
        canvas.requestPaint()
    }

    onAccepted: {
        normalise()
        dlg.saved(profName, Backend.saveProfile(profName, perf, pts, kbd, charge))
    }

    ColumnLayout {
        spacing: Kirigami.Units.smallSpacing

        Canvas {
            id: canvas
            Layout.fillWidth: true
            Layout.preferredHeight: Kirigami.Units.gridUnit * 18
            Layout.minimumWidth: Kirigami.Units.gridUnit * 24
            readonly property real pad: Kirigami.Units.gridUnit * 2.5
            property int dragIdx: -1
            readonly property real hit: Kirigami.Units.gridUnit * 0.8

            function toPx(t, d) {
                return [pad + t / 100 * (width - 2 * pad), height - pad - d / 100 * (height - 2 * pad)]
            }
            function toData(x, y) {
                return [Math.round((x - pad) / (width - 2 * pad) * 100), Math.round((height - pad - y) / (height - 2 * pad) * 100)]
            }
            function hitIndex(x, y) {
                for (let i = 0; i < dlg.pts.length; ++i) {
                    const p = toPx(dlg.pts[i][0], dlg.pts[i][1])
                    if (Math.hypot(p[0] - x, p[1] - y) <= hit)
                        return i
                }
                return -1
            }

            onPaint: {
                const ctx = getContext("2d")
                ctx.reset()
                const fg = Kirigami.Theme.textColor
                const dim = Kirigami.Theme.disabledTextColor
                ctx.font = Kirigami.Theme.smallFont.pixelSize + "px sans-serif"

                // grid + tick labels
                ctx.lineWidth = 1
                for (let v = 0; v <= 100; v += 20) {
                    let a = toPx(v, 0), b = toPx(v, 100)
                    ctx.strokeStyle = Qt.rgba(dim.r, dim.g, dim.b, 0.35)
                    ctx.beginPath(); ctx.moveTo(a[0], a[1]); ctx.lineTo(b[0], b[1]); ctx.stroke()
                    ctx.fillStyle = dim
                    ctx.textAlign = "center"
                    ctx.fillText(v + "°C", a[0], a[1] + 16)
                    a = toPx(0, v); b = toPx(100, v)
                    ctx.beginPath(); ctx.moveTo(a[0], a[1]); ctx.lineTo(b[0], b[1]); ctx.stroke()
                    ctx.textAlign = "right"
                    ctx.fillText(v + "%", a[0] - 6, a[1] + 4)
                }

                // safety floor
                ctx.strokeStyle = Kirigami.Theme.negativeTextColor
                ctx.setLineDash([4, 4])
                ctx.beginPath()
                for (let t = 0; t <= 100; ++t) {
                    const p = toPx(t, Backend.safetyFloor(t))
                    if (t === 0) ctx.moveTo(p[0], p[1]); else ctx.lineTo(p[0], p[1])
                }
                ctx.stroke()
                ctx.setLineDash([])

                // current temperature marker
                const temp = Math.max(Backend.status.cpu_temp ?? 0, Backend.status.gpu_temp ?? 0)
                if (temp > 0) {
                    const a = toPx(temp, 0), b = toPx(temp, 100)
                    ctx.strokeStyle = Kirigami.Theme.neutralTextColor
                    ctx.beginPath(); ctx.moveTo(a[0], a[1]); ctx.lineTo(b[0], b[1]); ctx.stroke()
                }

                // curve
                ctx.strokeStyle = Kirigami.Theme.highlightColor
                ctx.lineWidth = 2
                ctx.beginPath()
                for (let i = 0; i < dlg.pts.length; ++i) {
                    const p = toPx(dlg.pts[i][0], dlg.pts[i][1])
                    if (i === 0) ctx.moveTo(p[0], p[1]); else ctx.lineTo(p[0], p[1])
                }
                ctx.stroke()
                ctx.fillStyle = Kirigami.Theme.highlightColor
                for (let i = 0; i < dlg.pts.length; ++i) {
                    const p = toPx(dlg.pts[i][0], dlg.pts[i][1])
                    ctx.beginPath(); ctx.arc(p[0], p[1], i === dragIdx ? 7 : 5, 0, 2 * Math.PI); ctx.fill()
                }
                ctx.fillStyle = fg
                ctx.textAlign = "center"
                ctx.fillText(i18n("temperature"), width / 2, height - 4)
            }

            Connections {
                target: Backend
                function onStatusChanged() { if (dlg.opened) canvas.requestPaint() }
            }

            MouseArea {
                anchors.fill: parent
                onPressed: mouse => { canvas.dragIdx = canvas.hitIndex(mouse.x, mouse.y); canvas.requestPaint() }
                onPositionChanged: mouse => {
                    const i = canvas.dragIdx
                    if (i < 0) return
                    let d = canvas.toData(mouse.x, mouse.y)
                    const lo = i > 0 ? dlg.pts[i - 1][0] + 1 : 0
                    const hi = i < dlg.pts.length - 1 ? dlg.pts[i + 1][0] - 1 : 100
                    const t = Math.max(lo, Math.min(hi, d[0]))
                    const duty = Math.max(Backend.safetyFloor(t), Math.max(0, Math.min(100, d[1])))
                    const a = dlg.pts.slice()
                    a[i] = [t, duty]
                    dlg.pts = a
                    canvas.requestPaint()
                }
                onReleased: { canvas.dragIdx = -1; canvas.requestPaint() }
                onDoubleClicked: mouse => {
                    const i = canvas.hitIndex(mouse.x, mouse.y)
                    const a = dlg.pts.slice()
                    if (i >= 0) {
                        if (a.length > 2) a.splice(i, 1)
                    } else {
                        a.push(canvas.toData(mouse.x, mouse.y))
                    }
                    dlg.pts = a
                    dlg.normalise()
                }
            }
        }

        QQC2.Label {
            Layout.fillWidth: true
            wrapMode: Text.WordWrap
            font: Kirigami.Theme.smallFont
            color: Kirigami.Theme.disabledTextColor
            text: i18n("Drag points to edit. Double-click to add or remove a point. The red dashed line is the safety floor, and the vertical line is the current temperature.")
        }
        QQC2.Label {
            Layout.fillWidth: true
            text: dlg.pts.map(p => p[0] + "°C→" + p[1] + "%").join("   ")
            font.family: "monospace"
            wrapMode: Text.WordWrap
        }
    }
}
