// SPDX-License-Identifier: MIT
import QtQuick
import QtQuick.Controls as QQC2
import QtQuick.Layouts
import QtQuick.Dialogs
import org.kde.kirigami as Kirigami
import org.andreasincode.tuxedocontrol

Kirigami.ApplicationWindow {
    id: root
    title: i18n("TUXEDO Control")
    width: Kirigami.Units.gridUnit * 30
    height: Kirigami.Units.gridUnit * 40
    minimumWidth: Kirigami.Units.gridUnit * 22

    readonly property var st: Backend.status
    readonly property var pl: Backend.profiles
    readonly property var profileList: pl.profiles || []
    readonly property var activeProfile: profileList.find(p => p.name === pl.active) || null
    readonly property bool readOnly: st.read_only === true
    readonly property var builtins: ["Max Energy Save", "Quiet", "Office", "High Performance"]

    readonly property var perfModes: [
        { id: "power_save", label: i18n("Power save") },
        { id: "enthusiast", label: i18n("Balanced") },
        { id: "overboost", label: i18n("Performance") }
    ]
    readonly property var chargeModes: [
        { id: "stationary", label: i18n("Stationary (80%)") },
        { id: "balanced", label: i18n("Balanced (90%)") },
        { id: "high_capacity", label: i18n("High capacity (100%)") }
    ]
    readonly property var powerLabels: ({
        "power-saver": i18n("Power saver"),
        "balanced": i18n("Balanced"),
        "performance": i18n("Performance")
    })

    function notify(resp, okText) {
        showPassiveNotification(resp && resp.startsWith("OK") ? okText : i18n("Failed: %1", resp || i18n("daemon not reachable")))
        Backend.refresh()
    }

    pageStack.initialPage: Kirigami.ScrollablePage {
        title: i18n("TUXEDO Control")

        actions: [
            Kirigami.Action {
                text: i18n("Edit fan curve…")
                icon.name: "document-edit"
                enabled: root.activeProfile !== null && Backend.daemonUp
                onTriggered: curveEditor.openFor(root.activeProfile)
            },
            Kirigami.Action {
                text: i18n("New profile…")
                icon.name: "list-add"
                displayHint: Kirigami.DisplayHint.AlwaysHide
                enabled: Backend.daemonUp
                onTriggered: newDialog.open()
            },
            Kirigami.Action {
                text: i18n("Make active profile the default")
                icon.name: "starred-symbolic"
                displayHint: Kirigami.DisplayHint.AlwaysHide
                enabled: Backend.daemonUp && root.pl.active !== root.pl["default"]
                onTriggered: root.notify(Backend.send("SETDEFAULT " + root.pl.active), i18n("Default: %1", root.pl.active))
            },
            Kirigami.Action {
                text: i18n("Delete active profile")
                icon.name: "edit-delete"
                displayHint: Kirigami.DisplayHint.AlwaysHide
                enabled: Backend.daemonUp && root.builtins.indexOf(root.pl.active) < 0
                onTriggered: root.notify(Backend.send("DELPROFILE " + root.pl.active), i18n("Deleted: %1", root.pl.active))
            },
            Kirigami.Action {
                text: i18n("Import TUXEDO Control Center profiles…")
                icon.name: "document-import"
                displayHint: Kirigami.DisplayHint.AlwaysHide
                enabled: Backend.daemonUp
                onTriggered: importDialog.open()
            }
        ]

        ColumnLayout {
            spacing: Kirigami.Units.largeSpacing

            Kirigami.InlineMessage {
                Layout.fillWidth: true
                visible: !Backend.daemonUp
                type: Kirigami.MessageType.Error
                text: i18n("tuxedo-controld is not running. Start it with: sudo systemctl start tuxedo-control")
            }
            Kirigami.InlineMessage {
                Layout.fillWidth: true
                visible: Backend.daemonUp && root.readOnly
                type: Kirigami.MessageType.Warning
                text: i18n("This board model (id %1) is not validated. Fan and EC writes are disabled.", root.st.model_id)
            }

            Kirigami.FormLayout {
                Layout.fillWidth: true
                enabled: Backend.daemonUp

                Kirigami.Separator { Kirigami.FormData.isSection: true; Kirigami.FormData.label: i18n("Status") }
                QQC2.Label {
                    Kirigami.FormData.label: i18n("CPU:")
                    text: i18n("%1 °C, fan %2%", root.st.cpu_temp ?? "-", root.st.cpu_fan ?? "-")
                }
                QQC2.Label {
                    Kirigami.FormData.label: i18n("GPU:")
                    text: i18n("%1 °C, fan %2%", root.st.gpu_temp ?? "-", root.st.gpu_fan ?? "-")
                }
                QQC2.Label {
                    Kirigami.FormData.label: i18n("Curve target:")
                    text: root.st.manual === null || root.st.manual === undefined
                        ? i18n("%1%", root.st.curve_pct ?? "-")
                        : i18n("%1% (manual hold %2%)", root.st.curve_pct ?? "-", root.st.manual)
                }

                Kirigami.Separator { Kirigami.FormData.isSection: true; Kirigami.FormData.label: i18n("Power") }
                QQC2.ComboBox {
                    id: powerCombo
                    Kirigami.FormData.label: i18n("TLP power profile:")
                    enabled: Backend.powerProfiles.length > 0
                    model: Backend.powerProfiles.map(p => root.powerLabels[p] || p)
                    currentIndex: Backend.powerProfiles.indexOf(Backend.powerProfile)
                    onActivated: idx => {
                        if (!Backend.setPowerProfile(Backend.powerProfiles[idx]))
                            showPassiveNotification(i18n("TLP refused the power profile change"))
                    }
                }
                QQC2.Label {
                    visible: Backend.powerProfiles.length === 0
                    text: i18n("tlp-pd is not running")
                    color: Kirigami.Theme.disabledTextColor
                }
                QQC2.ComboBox {
                    Kirigami.FormData.label: i18n("Fan profile:")
                    model: root.profileList.map(p => p.name === root.pl["default"] ? i18n("%1 (default)", p.name) : p.name)
                    currentIndex: root.profileList.findIndex(p => p.name === root.pl.active)
                    onActivated: idx => {
                        const name = root.profileList[idx].name
                        root.notify(Backend.send("ACTIVATE " + name), i18n("Activated: %1", name))
                    }
                }
                QQC2.Label {
                    Layout.maximumWidth: Kirigami.Units.gridUnit * 18
                    wrapMode: Text.WordWrap
                    font: Kirigami.Theme.smallFont
                    color: Kirigami.Theme.disabledTextColor
                    text: i18n("tuxedo-tlp-controld-sync switches the fan profile when the TLP profile changes. A pick here holds until the next TLP switch.")
                }
                QQC2.ComboBox {
                    Kirigami.FormData.label: i18n("EC performance mode:")
                    enabled: !root.readOnly
                    model: root.perfModes.map(m => m.label)
                    currentIndex: root.perfModes.findIndex(m => m.id === root.st.profile)
                    onActivated: idx => root.notify(Backend.send("PROFILE " + root.perfModes[idx].id), i18n("Performance mode: %1", root.perfModes[idx].label))
                }

                Kirigami.Separator { Kirigami.FormData.isSection: true; Kirigami.FormData.label: i18n("Fan") }
                QQC2.Switch {
                    id: autoSwitch
                    Kirigami.FormData.label: i18n("Automatic:")
                    text: i18n("Follow the profile's temperature curve")
                    enabled: !root.readOnly
                    checked: root.st.manual === null || root.st.manual === undefined
                    onToggled: {
                        if (checked)
                            root.notify(Backend.send("FANAUTO"), i18n("Fan follows the curve"))
                        else
                            root.notify(Backend.send("FANMANUAL " + Math.round(manualSlider.value)), i18n("Fan held at %1%", Math.round(manualSlider.value)))
                    }
                }
                RowLayout {
                    Kirigami.FormData.label: i18n("Manual speed:")
                    enabled: !autoSwitch.checked && !root.readOnly
                    QQC2.Slider {
                        id: manualSlider
                        Layout.preferredWidth: Kirigami.Units.gridUnit * 12
                        from: 0; to: 100; stepSize: 5
                        value: root.st.manual ?? 40
                        onMoved: holdTimer.restart()
                    }
                    QQC2.Label { text: i18n("%1%", Math.round(manualSlider.value)) }
                    Timer {
                        id: holdTimer
                        interval: 250
                        onTriggered: Backend.send("FANMANUAL " + Math.round(manualSlider.value))
                    }
                }

                Kirigami.Separator {
                    Kirigami.FormData.isSection: true
                    Kirigami.FormData.label: i18n("Keyboard backlight")
                    visible: (root.st.kbd_max ?? 0) > 0
                }
                RowLayout {
                    Kirigami.FormData.label: i18n("Brightness:")
                    visible: (root.st.kbd_max ?? 0) > 0
                    QQC2.Slider {
                        id: kbdSlider
                        Layout.preferredWidth: Kirigami.Units.gridUnit * 12
                        from: 0; to: root.st.kbd_max ?? 0; stepSize: 1; snapMode: QQC2.Slider.SnapAlways
                        value: root.st.kbd ?? 0
                        onMoved: Backend.send("KBDSET " + Math.round(value))
                    }
                    QQC2.Label { text: Math.round(kbdSlider.value) + " / " + (root.st.kbd_max ?? 0) }
                }

                Kirigami.Separator { Kirigami.FormData.isSection: true; Kirigami.FormData.label: i18n("Battery") }
                QQC2.ComboBox {
                    Kirigami.FormData.label: i18n("Charge ceiling:")
                    enabled: (root.st.charges ?? "") !== ""
                    model: root.chargeModes.map(m => m.label)
                    currentIndex: root.chargeModes.findIndex(m => m.id === root.st.charge)
                    onActivated: idx => root.notify(Backend.send("CHARGE " + root.chargeModes[idx].id), i18n("Charging: %1", root.chargeModes[idx].label))
                }
                QQC2.Label {
                    Layout.maximumWidth: Kirigami.Units.gridUnit * 18
                    wrapMode: Text.WordWrap
                    font: Kirigami.Theme.smallFont
                    color: Kirigami.Theme.disabledTextColor
                    text: i18n("TLP sets this at boot from STOP_CHARGE_THRESH_BAT0 in /etc/tlp.d/01-tuxedo-charge.conf. A change here lasts until TLP applies it again.")
                }
            }
        }
    }

    CurveEditor {
        id: curveEditor
        onSaved: (name, ok) => showPassiveNotification(ok ? i18n("Saved curve: %1", name) : i18n("Couldn't save curve"))
    }

    Kirigami.PromptDialog {
        id: newDialog
        title: i18n("New profile")
        subtitle: i18n("Starts from a quiet curve and the balanced EC mode. Edit the curve after.")
        standardButtons: Kirigami.Dialog.Ok | Kirigami.Dialog.Cancel
        QQC2.TextField {
            id: newName
            placeholderText: i18n("Profile name")
        }
        onAccepted: {
            const name = newName.text.trim()
            if (name === "")
                return
            const ok = Backend.saveProfile(name, "enthusiast", [[25, 0], [50, 0], [62, 24], [80, 60], [90, 100]], null, null)
            showPassiveNotification(ok ? i18n("Created: %1", name) : i18n("Couldn't create profile"))
            newName.text = ""
        }
    }

    FileDialog {
        id: importDialog
        title: i18n("Import TUXEDO Control Center profiles")
        nameFilters: [i18n("TCC profile export (*.json)"), i18n("All files (*)")]
        onAccepted: {
            const path = decodeURIComponent(selectedFile.toString().replace(/^file:\/\//, ""))
            root.notify(Backend.send("IMPORTTCC " + path), i18n("Imported TUXEDO Control Center profiles"))
        }
    }
}
