import QtQuick
import Quickshell
import qs.Commons
import qs.Ui
import "Model.js" as Model

// The #mesh chat. Messages on the bitchat Bluetooth mesh, an input line
// (with /nick, /who, /clear), the peers in range, and radio settings.
//
// BarWidget.qml owns the bar label and hands this panel the button to
// anchor against and the service that talks to bitchatd.
Panel {
  id: root
  moduleName: "derekross.bitchat"
  manageIpc: false

  property var anchorItem: null
  property var service: null
  property var hostWidget: null
  readonly property var barIdentity: hostWidget || root

  property bool peersOpen: false
  property bool settingsOpen: false
  property string noticeText: ""
  property bool noticeError: false
  property double now: Date.now()
  property var sentHistory: []
  property int historyAt: -1

  readonly property bool linked: service ? service.linked : false
  readonly property var me: service ? service.me : ({})
  readonly property var peers: service ? service.peers : []
  readonly property var radio: service ? service.radio : ({})
  // Appended to in place (not rebuilt per message), so reading back
  // through history isn't yanked to the bottom by each new line.
  ListModel { id: chatModel }

  function rebuildChat() {
    chatModel.clear()
    var rows = Model.rows(service ? service.messages : [])
    for (var i = 0; i < rows.length; i++) chatModel.append(rows[i])
    if (messageList.pinned) Qt.callLater(messageList.positionViewAtEnd)
  }

  function addToChat(m) {
    var n = chatModel.count
    var last = n > 0 ? chatModel.get(n - 1) : null
    if (last && m.timestamp < last.timestamp) { rebuildChat(); return } // backfill: re-sort
    chatModel.append(Model.rowFor(last, m))
    while (chatModel.count > Model.MAX_MESSAGES) {
      chatModel.remove(0)
      chatModel.setProperty(0, "first", true)
    }
    if (messageList.pinned) Qt.callLater(messageList.positionViewAtEnd)
  }

  onServiceChanged: rebuildChat()
  readonly property string mode: service && service.daemonSettings.mode ? service.daemonSettings.mode : "auto"
  readonly property bool use24h: setting("clock24h", true) !== false

  readonly property color foreground: bar ? bar.foreground : Color.foreground
  readonly property color urgent: bar ? bar.urgent : Color.urgent
  readonly property color dim: Qt.darker(foreground, 1.5)
  readonly property string fontFamily: bar ? bar.fontFamily : Style.font.family
  readonly property color hoverFill: Style.hoverFillFor(foreground, Color.accent)
  // Sender colors: light on dark themes, dark on light ones.
  readonly property real nickLightness: foreground.hslLightness > 0.5 ? 0.72 : 0.38

  function nickColor(peerId) { return Qt.hsla(Model.nickHue(peerId), 0.55, root.nickLightness, 1) }

  function open() {
    root.now = Date.now()
    root.controller.show()
    Qt.callLater(function() { if (root.opened) setCenterHoverRevealSuppressed(true) })
  }

  function openToType() {
    open()
    Qt.callLater(function() { input.forceActiveFocus() })
  }

  function close() {
    setCenterHoverRevealSuppressed(false)
    root.settingsOpen = false
    root.controller.hide()
  }

  function toggle() { root.opened ? root.close() : root.open() }

  function switchPanel(direction) {
    if (root.bar && typeof root.bar.switchPanelFrom === "function")
      return root.bar.switchPanelFrom(root.barIdentity, direction)
    return false
  }

  function setCenterHoverRevealSuppressed(value) {
    if (root.bar && typeof root.bar.setCenterHoverRevealSuppressed === "function")
      root.bar.setCenterHoverRevealSuppressed(value)
    else if (root.bar && "centerHoverRevealSuppressed" in root.bar)
      root.bar.centerHoverRevealSuppressed = value
  }

  function showNotice(text, isError) {
    root.noticeText = text
    root.noticeError = isError === true
    noticeTimer.restart()
  }

  function submit() {
    var raw = input.text
    var action = Model.parseInput(raw)
    switch (action.kind) {
    case "none": return
    case "error": showNotice(action.message, true); return
    case "help": showNotice(Model.HELP, false); break
    case "who": root.peersOpen = true; showNotice(Model.peerSummary(root.peers), false); break
    case "clear": if (service) service.clearHistory(); break
    case "nick": if (service) service.setNickname(action.value); break
    case "send":
      if (!root.linked) { showNotice("bitchatd isn't running", true); return }
      service.send(action.text)
      break
    }
    root.sentHistory = [raw].concat(root.sentHistory.filter(function(x) { return x !== raw })).slice(0, 30)
    root.historyAt = -1
    input.text = ""
  }

  function recall(delta) {
    if (root.sentHistory.length === 0) return
    var at = Math.max(-1, Math.min(root.sentHistory.length - 1, root.historyAt + delta))
    root.historyAt = at
    input.text = at === -1 ? "" : root.sentHistory[at]
    input.cursorPosition = input.text.length
  }

  function handleInputKey(event) {
    if (event.key === Qt.Key_Return || event.key === Qt.Key_Enter) {
      root.submit()
      event.accepted = true
    } else if (event.key === Qt.Key_Escape) {
      if (input.text !== "") input.text = ""
      else if (root.settingsOpen) root.settingsOpen = false
      else root.close()
      event.accepted = true
    } else if (event.key === Qt.Key_Tab || event.key === Qt.Key_Backtab) {
      root.switchPanel((event.modifiers & Qt.ShiftModifier) || event.key === Qt.Key_Backtab ? -1 : 1)
      event.accepted = true
    } else if (event.key === Qt.Key_Up && (input.text === "" || root.historyAt !== -1)) {
      root.recall(1)
      event.accepted = true
    } else if (event.key === Qt.Key_Down && root.historyAt !== -1) {
      root.recall(-1)
      event.accepted = true
    } else if (event.key === Qt.Key_PageUp) {
      messageList.contentY = Math.max(0, messageList.contentY - messageList.height * 0.8)
      event.accepted = true
    } else if (event.key === Qt.Key_PageDown) {
      messageList.contentY = Math.min(Math.max(0, messageList.contentHeight - messageList.height),
        messageList.contentY + messageList.height * 0.8)
      event.accepted = true
    }
  }

  onOpenedChanged: {
    if (service) service.panelOpen = opened
    if (opened) {
      root.now = Date.now()
      Qt.callLater(function() { messageList.positionViewAtEnd() })
    } else {
      root.settingsOpen = false
    }
  }

  Connections {
    target: root.service
    function onNotice(text, isError) { root.showNotice(text, isError) }
    function onMessageAdded(msg) { root.addToChat(msg) }
    function onMessagesReset() { root.rebuildChat() }
  }

  Timer { id: noticeTimer; interval: 6000; onTriggered: root.noticeText = "" }
  Timer { interval: 30000; running: root.opened; repeat: true; onTriggered: root.now = Date.now() }

  KeyboardPanel {
    id: panel
    anchorItem: root.anchorItem
    owner: root.barIdentity
    bar: root.bar
    open: root.opened
    focusTarget: input
    contentWidth: panel.fittedContentWidth(Style.space(460))
    contentHeight: panel.fittedContentHeight(column.implicitHeight, Style.space(720))

    PanelKeyCatcher {
      id: keyCatcher
      anchors.fill: parent
      onMoveRequested: function(dx, dy) {
        if (dy !== 0) messageList.contentY = Math.max(0, Math.min(
          Math.max(0, messageList.contentHeight - messageList.height), messageList.contentY + dy * Style.space(40)))
      }
      onCloseRequested: root.settingsOpen ? root.settingsOpen = false : root.close()
      onTabRequested: function(direction) { root.switchPanel(direction) }
      onTextKey: function(t) {
        input.forceActiveFocus()
        input.insert(input.cursorPosition, t)
      }

      Column {
        id: column
        width: parent.width
        spacing: Style.space(8)

        // ---- Header: channel, who we are, radio state, actions.
        Item {
          width: parent.width
          implicitHeight: Math.max(titleColumn.implicitHeight, headerActions.implicitHeight)

          Column {
            id: titleColumn
            anchors.left: parent.left
            anchors.right: headerActions.left
            anchors.rightMargin: Style.space(8)
            anchors.verticalCenter: parent.verticalCenter
            spacing: Style.space(2)

            Row {
              spacing: Style.space(8)

              Text {
                textFormat: Text.PlainText
                text: "#mesh"
                color: root.foreground
                font.family: root.fontFamily
                font.pixelSize: Style.font.subtitle
                font.bold: true
              }

              Text {
                anchors.verticalCenter: parent.verticalCenter
                textFormat: Text.PlainText
                visible: root.linked && root.me.peerId !== undefined
                text: "as " + Model.displayName(root.me.nickname, root.me.peerId)
                color: root.dim
                font.family: root.fontFamily
                font.pixelSize: Style.font.caption
              }
            }

            Text {
              textFormat: Text.PlainText
              width: parent.width
              text: root.linked
                ? Model.peerSummary(root.peers) + " · " + Model.radioLine(root.radio, root.linked)
                : Model.radioLine(root.radio, false)
              color: root.linked && Model.isRunning(root.radio) ? root.dim : root.urgent
              font.family: root.fontFamily
              font.pixelSize: Style.font.caption
              elide: Text.ElideRight
            }
          }

          Row {
            id: headerActions
            anchors.right: parent.right
            anchors.verticalCenter: parent.verticalCenter
            spacing: Style.space(2)

            PanelActionButton {
              iconText: "󰀉"
              tooltipText: root.peersOpen ? "Hide peers" : "Show who's in range (/who)"
              foreground: root.peersOpen ? Color.accent : root.foreground
              fontFamily: root.fontFamily
              onClicked: root.peersOpen = !root.peersOpen
            }

            PanelActionButton {
              iconText: root.mode === "off" ? "󰂲" : "󰂯"
              tooltipText: root.mode === "off" ? "Turn the mesh on" : "Turn the mesh off (stops all Bluetooth use)"
              foreground: root.mode === "off" ? root.dim : root.foreground
              fontFamily: root.fontFamily
              enabled: root.linked
              opacity: enabled ? 1 : 0.4
              onClicked: if (root.service) root.service.setMode(root.mode === "off" ? "auto" : "off")
            }

            PanelActionButton {
              iconText: "󰒓"
              tooltipText: root.settingsOpen ? "Back to chat (Esc)" : "Settings"
              foreground: root.settingsOpen ? Color.accent : root.foreground
              fontFamily: root.fontFamily
              onClicked: root.settingsOpen = !root.settingsOpen
            }
          }
        }

        // ---- Daemon not running.
        Column {
          width: parent.width
          spacing: Style.space(6)
          visible: !root.linked

          Text {
            textFormat: Text.PlainText
            width: parent.width
            wrapMode: Text.WordWrap
            text: root.service && root.service.everConnected
              ? "Lost the connection to bitchatd. It restarts on its own; you can also start it now."
              : "bitchatd isn't running. Start it, or run dist/install.sh from the plugin folder if it isn't installed."
            color: root.dim
            font.family: root.fontFamily
            font.pixelSize: Style.font.bodySmall
          }

          Button {
            text: "Start bitchatd"
            bordered: true
            foreground: root.foreground
            fontFamily: root.fontFamily
            fontSize: Style.font.bodySmall
            onClicked: if (root.service) root.service.startDaemon()
          }
        }

        // ---- Settings, in place of the chat while open.
        Column {
          width: parent.width
          spacing: Style.space(8)
          visible: root.settingsOpen && root.linked

          PanelSectionHeader {
            text: "Nickname"
            foreground: root.foreground
            fontFamily: root.fontFamily
          }

          TextField {
            id: nickField
            width: parent.width
            text: root.me.nickname || ""
            placeholderText: "1-15 characters"
            maximumLength: 15
            foreground: root.foreground
            font.family: root.fontFamily
            font.pixelSize: Style.font.bodySmall
            onAccepted: if (root.service && text.trim() !== "") root.service.setNickname(text.trim())
          }

          PanelSectionHeader {
            text: "Radio"
            foreground: root.foreground
            fontFamily: root.fontFamily
          }

          Flow {
            width: parent.width
            spacing: Style.space(4)

            Repeater {
              model: [
                { id: "auto", label: "Auto", hint: "Active on AC power; power saver on battery or while Bluetooth audio plays" },
                { id: "balanced", label: "Active", hint: "Scan 8 s of every 10: finds peers fastest" },
                { id: "saver", label: "Saver", hint: "Scan 2 s of every 30. Phones can still connect to you." },
                { id: "off", label: "Off", hint: "No advertising, no scanning, no connections" }
              ]

              Button {
                required property var modelData
                text: modelData.label
                tooltipText: modelData.hint
                selected: root.mode === modelData.id
                foreground: root.foreground
                fontFamily: root.fontFamily
                fontSize: Style.font.bodySmall
                bordered: true
                onClicked: if (root.service) root.service.setMode(modelData.id)
              }
            }
          }

          Text {
            textFormat: Text.PlainText
            width: parent.width
            wrapMode: Text.WordWrap
            text: Model.radioLine(root.radio, root.linked)
              + (root.radio.links !== undefined ? " · " + root.radio.links + (root.radio.links === 1 ? " link" : " links") : "")
            color: root.dim
            font.family: root.fontFamily
            font.pixelSize: Style.font.caption
          }

          PanelSectionHeader {
            text: "Identity"
            foreground: root.foreground
            fontFamily: root.fontFamily
          }

          Text {
            textFormat: Text.PlainText
            width: parent.width
            wrapMode: Text.WrapAnywhere
            text: "Peer " + (root.me.peerId || "?") + "\nFingerprint " + (root.me.fingerprint || "?")
            color: root.dim
            font.family: root.fontFamily
            font.pixelSize: Style.font.caption
          }

          Button {
            text: "Clear chat history"
            bordered: true
            foreground: root.urgent
            fontFamily: root.fontFamily
            fontSize: Style.font.bodySmall
            onClicked: if (root.service) root.service.clearHistory()
          }
        }

        // ---- Peers in range.
        Column {
          width: parent.width
          spacing: Style.space(2)
          visible: root.peersOpen && root.linked && !root.settingsOpen

          Text {
            textFormat: Text.PlainText
            visible: root.peers.length === 0
            text: "No one in range yet. Peers show up when a phone running bitchat is nearby."
            width: parent.width
            wrapMode: Text.WordWrap
            color: root.dim
            font.family: root.fontFamily
            font.pixelSize: Style.font.caption
          }

          Repeater {
            model: root.peers

            Row {
              required property var modelData
              spacing: Style.space(6)

              Text {
                textFormat: Text.PlainText
                text: modelData.direct ? "●" : "○"
                color: modelData.direct ? Color.accent : root.dim
                font.pixelSize: Style.font.caption
                anchors.verticalCenter: parent.verticalCenter
              }

              Text {
                textFormat: Text.PlainText
                text: Model.displayName(modelData.nickname, modelData.id)
                color: root.nickColor(modelData.id)
                font.family: root.fontFamily
                font.pixelSize: Style.font.bodySmall
              }

              Text {
                textFormat: Text.PlainText
                text: modelData.direct ? "direct" : "via mesh"
                color: root.dim
                font.family: root.fontFamily
                font.pixelSize: Style.font.caption
                anchors.verticalCenter: parent.verticalCenter
              }
            }
          }

          PanelSeparator {
            width: parent.width
            foreground: root.foreground
          }
        }

        // ---- Messages.
        ListView {
          id: messageList
          width: parent.width
          height: Style.space(360)
          visible: root.linked && !root.settingsOpen
          clip: true
          spacing: Style.space(2)
          boundsBehavior: Flickable.StopAtBounds
          model: chatModel
          // Stay pinned to the newest message unless scrolled up.
          property bool pinned: true
          onMovementEnded: pinned = atYEnd

          Text {
            anchors.centerIn: parent
            width: parent.width - Style.space(24)
            visible: messageList.count === 0
            horizontalAlignment: Text.AlignHCenter
            wrapMode: Text.WordWrap
            textFormat: Text.PlainText
            text: Model.isRunning(root.radio)
              ? "Nothing yet. Say hi: anyone nearby running bitchat will see it."
              : Model.radioLine(root.radio, root.linked)
            color: root.dim
            font.family: root.fontFamily
            font.pixelSize: Style.font.bodySmall
          }

          delegate: Column {
            id: row
            required property string senderId
            required property string nickname
            required property string text
            required property double timestamp
            required property bool mine
            required property bool first
            readonly property bool mention: !mine && Model.mentions(text, root.me.nickname)
            width: messageList.width
            topPadding: first ? Style.space(6) : 0

            Row {
              visible: row.first
              spacing: Style.space(6)

              Text {
                textFormat: Text.PlainText
                text: row.mine ? "you" : Model.displayName(row.nickname, row.senderId)
                color: row.mine ? Color.accent : root.nickColor(row.senderId)
                font.family: root.fontFamily
                font.pixelSize: Style.font.caption
                font.bold: true
              }

              Text {
                textFormat: Text.PlainText
                text: Model.timeLabel(row.timestamp, root.now, root.use24h)
                color: root.dim
                font.family: root.fontFamily
                font.pixelSize: Style.font.caption
              }
            }

            Rectangle {
              width: parent.width
              height: body.implicitHeight + Style.space(4)
              radius: Style.cornerRadius
              color: row.mention ? Qt.rgba(Color.accent.r, Color.accent.g, Color.accent.b, 0.14) : "transparent"

              TextEdit {
                id: body
                x: Style.space(row.mention ? 4 : 0)
                y: Style.space(2)
                width: parent.width - x * 2
                readOnly: true
                selectByMouse: true
                textFormat: TextEdit.PlainText
                wrapMode: TextEdit.Wrap
                text: row.text
                color: root.foreground
                selectionColor: Style.selectionFillFor(root.foreground, Color.accent)
                font.family: root.fontFamily
                font.pixelSize: Style.font.bodySmall
              }
            }
          }
        }

        // ---- Notice line (command feedback, errors).
        Text {
          textFormat: Text.PlainText
          width: parent.width
          visible: root.noticeText !== ""
          text: root.noticeText
          wrapMode: Text.WordWrap
          color: root.noticeError ? root.urgent : root.dim
          font.family: root.fontFamily
          font.pixelSize: Style.font.caption
        }

        // ---- Input.
        TextField {
          id: input
          width: parent.width
          visible: root.linked && !root.settingsOpen
          placeholderText: Model.isRunning(root.radio)
            ? "Message #mesh  (/help)"
            : "The radio isn't running, so nothing you send goes out"
          foreground: root.foreground
          font.family: root.fontFamily
          font.pixelSize: Style.font.bodySmall
          Keys.onPressed: function(event) { root.handleInputKey(event) }
        }
      }
    }
  }
}
