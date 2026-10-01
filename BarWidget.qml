import QtQuick
import Quickshell
import Quickshell.Io
import qs.Commons
import qs.Ui
import "Model.js" as Model

// The bar pill: a chat glyph, the number of peers in range, and the accent
// color while there are unread messages. Left click opens the panel, right
// click opens it ready to type, middle click turns the mesh on or off.
BarWidget {
  id: root
  moduleName: "derekross.bitchat"

  readonly property var service: bar && bar.shell && typeof bar.shell.serviceFor === "function"
    ? bar.shell.serviceFor("derekross.bitchat") : null

  readonly property bool showPeerCount: setting("showPeerCount", true) !== false
  readonly property bool linked: service ? service.linked : false
  readonly property bool running: service ? service.running : false
  readonly property int peerCount: service ? service.peers.length : 0
  readonly property int unread: service ? service.unread : 0

  readonly property string label: {
    var glyph = unread > 0 ? "󰍡" : "󰍩"
    if (running && showPeerCount && peerCount > 0) return glyph + " " + peerCount
    return glyph
  }

  readonly property string tooltip: {
    if (!service) return "The bitchat service isn't loaded. Enable the plugin with omarchy plugin enable derekross.bitchat."
    if (!linked) return "bitchatd isn't running. Click to start it, or run dist/install.sh."
    var lines = ["bitchat · " + Model.radioLine(service.radio, linked), Model.peerSummary(service.peers)]
    if (unread > 0) lines.push(unread === 1 ? "1 unread message" : unread + " unread messages")
    return lines.join("\n")
  }

  function pushSettings() {
    if (service && typeof service.configure === "function") service.configure(settings)
  }

  function injectPanel() {
    var target = panelLoader.item
    if (!target) return
    if ("bar" in target) target.bar = root.bar
    if ("settings" in target) target.settings = root.settings
    if ("anchorItem" in target) target.anchorItem = button
    if ("hostWidget" in target) target.hostWidget = root
    if ("service" in target) target.service = root.service
  }

  onBarChanged: injectPanel()
  onServiceChanged: { pushSettings(); injectPanel() }
  onSettingsChanged: { pushSettings(); injectPanel() }

  readonly property bool opened: panelLoader.item ? panelLoader.item.opened === true : false
  function open() { if (panelLoader.item) panelLoader.item.open() }
  function close() { if (panelLoader.item) panelLoader.item.close() }
  function togglePanel() { if (panelLoader.item) panelLoader.item.toggle() }
  function openToType() { if (panelLoader.item) panelLoader.item.openToType() }

  function toggleMesh() {
    if (!service) return
    if (!service.linked) { service.startDaemon(); return }
    var mode = service.daemonSettings.mode
    service.setMode(mode === "off" ? "auto" : "off")
  }

  readonly property real openPanelIndicatorWidth: button.labelWidth
  readonly property real openPanelIndicatorHeight: Math.max(Style.space(10), Math.round(Style.bar.iconSlot * 0.55))
  readonly property bool popoutSwitchClosing: panelLoader.item ? panelLoader.item.popoutSwitchClosing === true : false
  function closeForPopoutSwitch() { if (panelLoader.item) panelLoader.item.closeForPopoutSwitch() }

  implicitWidth: button.implicitWidth
  implicitHeight: button.implicitHeight

  Loader {
    id: panelLoader
    active: true
    source: Qt.resolvedUrl("Panel.qml")
    visible: false
    onLoaded: {
      root.injectPanel()
      Qt.callLater(root.injectPanel)
    }
  }

  // `omarchy-shell derekross.bitchat <fn>` for keybindings and scripts.
  IpcHandler {
    target: "derekross.bitchat"

    function open(): void { root.open() }
    function close(): void { root.close() }
    function toggle(): void { root.togglePanel() }
    function type(): void { root.openToType() }
    function send(text: string): string {
      if (!root.service || !root.service.linked) return "bitchatd isn't running"
      root.service.send(text)
      return "ok"
    }
    function status(): string {
      if (!root.service) return "no service"
      return Model.radioLine(root.service.radio, root.service.linked) + "; " + Model.peerSummary(root.service.peers)
    }
  }

  WidgetButton {
    id: button
    anchors.fill: parent
    bar: root.bar
    text: root.label
    fontSize: Style.font.caption
    active: root.unread > 0
    useActiveColor: true
    dimmed: !root.running
    tooltipText: root.tooltip

    onPressed: function(b) {
      if (b === Qt.MiddleButton) root.toggleMesh()
      else if (b === Qt.RightButton) root.openToType()
      else root.togglePanel()
    }
  }
}
