import QtQuick
import Quickshell
import Quickshell.Io
import "Model.js" as Model

// One connection to bitchatd for the whole shell. The bar widgets (one per
// monitor) and their panels read state from here and act through call().
Item {
  id: root

  // Injected by omarchy-shell.
  property var shell: null
  property var manifest: null

  readonly property string socketPath: Quickshell.env("XDG_RUNTIME_DIR") + "/bitchat.sock"

  // Our own record of the link: Socket.connected is also the *requested*
  // state, so it can read true after a failed attempt.
  property bool linked: false
  property bool everConnected: false

  // Daemon state (see bitchatd `status`).
  property var me: ({})
  property var peers: []
  property var messages: []
  property var radio: ({})
  property var daemonSettings: ({})

  // Widget settings, pushed by the bar widget.
  property string notifyMode: "mentions"
  property bool persistHistory: true
  property bool settled: false

  // Unread: messages from others since the panel was last open.
  property bool panelOpen: false
  property double lastSeen: Date.now()
  readonly property int unread: Model.unreadCount(messages, lastSeen)
  readonly property bool running: linked && Model.isRunning(radio)

  signal notice(string text, bool isError)
  // For the panel's list model: one appended message, or start over.
  signal messageAdded(var msg)
  signal messagesReset()

  function configure(s) {
    s = s || {}
    root.notifyMode = String(s.notify !== undefined ? s.notify : "mentions")
    var persist = s.persistHistory !== false
    var changed = persist !== root.persistHistory
    root.persistHistory = persist
    root.settled = true
    if (root.linked && (changed || root.daemonSettings.persistHistory !== persist))
      root.call("setPersistHistory", { enabled: persist }, null)
  }

  // Settle anyway if the widget isn't on the bar.
  Timer { interval: 30000; running: !root.settled; onTriggered: root.settled = true }

  function markRead() { root.lastSeen = Date.now() }
  onPanelOpenChanged: if (panelOpen) markRead()

  property int _nextId: 1
  property var _callbacks: ({})

  function call(method, params, done) {
    if (!root.linked || !root.sock) {
      if (done) done("bitchatd isn't running", null)
      return
    }
    var id = _nextId++
    if (done) _callbacks[id] = done
    root.sock.write(JSON.stringify({ id: id, method: method, params: params || {} }) + "\n")
    root.sock.flush()
  }

  // call() with a notice on error.
  function run(method, params, onOk) {
    call(method, params, function(err, result) {
      if (err) root.notice(err, true)
      else if (onOk) onOk(result)
    })
  }

  function send(text) { run("send", { text: text }) }
  function setNickname(nick) { run("setNickname", { nickname: nick }, function() { root.notice("You are now " + nick, false) }) }
  function setMode(mode) { run("setMode", { mode: mode }) }
  function clearHistory() { run("clearHistory", {}) }

  function startDaemon() {
    Quickshell.execDetached(["systemctl", "--user", "start", "bitchat.service"])
    reconnectNow()
  }

  function applySnapshot(s) {
    s = s || {}
    root.me = s.me || {}
    root.peers = s.peers || []
    root.messages = s.messages || []
    root.messagesReset()
    root.radio = s.radio || {}
    root.daemonSettings = s.settings || {}
    if (root.settled && root.daemonSettings.persistHistory !== undefined
        && root.daemonSettings.persistHistory !== root.persistHistory)
      root.call("setPersistHistory", { enabled: root.persistHistory }, null)
  }

  function handle(msg) {
    if (msg.id !== undefined && msg.event === undefined) {
      var cb = _callbacks[msg.id]
      if (cb) {
        delete _callbacks[msg.id]
        cb(msg.error !== undefined ? String(msg.error) : null, msg.result)
      }
      return
    }
    var d = msg.data
    switch (msg.event) {
    case "message":
      var before = root.messages
      root.messages = Model.appendMessage(before, d)
      if (root.messages === before) break // already had it
      root.messageAdded(d)
      if (root.panelOpen) root.markRead()
      if (Model.shouldNotify(d, root.notifyMode, root.me.nickname, root.panelOpen, Date.now())) notifyMessage(d)
      break
    case "peers": root.peers = d || []; break
    case "status": root.radio = d || {}; break
    case "identity": root.me = d || {}; break
    case "settings": root.daemonSettings = d || {}; break
    case "cleared": root.messages = []; root.messagesReset(); break
    case "resync": root.call("status", null, function(err, s) { if (!err) root.applySnapshot(s) }); break
    }
  }

  function notifyMessage(m) {
    var title = Model.displayName(m.nickname, m.senderId) + " on #mesh"
    Quickshell.execDetached(["omarchy-notification-send", "--app-name", "Bitchat", "-g", "󰍡",
      Model.safeArg(title), Model.safeArg(Model.truncate(m.text, 200))])
  }

  // Quickshell's Socket won't retry after a failed attempt, so each attempt
  // gets a fresh Socket. It's built from a string rather than a component in
  // this file: when the plugin is updated the shell clears its component
  // cache, but this service (keepLoaded) lives on and must still be able to
  // make sockets.
  property var sock: null
  readonly property string sockQml: "import QtQuick; import Quickshell.Io; Socket {"
    + " property var owner: null;"
    + " parser: SplitParser { onRead: function(line) { if (owner) owner.onSocketLine(line) } }"
    + " onConnectedChanged: if (owner) owner.onSocketConnected(connected);"
    + " onError: if (owner) owner.onSocketConnected(false) }"

  function onSocketLine(line) {
    var msg
    try { msg = JSON.parse(line) } catch (e) { return }
    root.handle(msg)
  }

  function onSocketConnected(up) {
    if (up === root.linked) return
    root.linked = up
    if (up) {
      root.everConnected = true
      root._callbacks = ({})
      root.call("subscribe", null, function(err, s) {
        if (err) return
        root.applySnapshot(s)
        // History from before this shell session isn't news.
        if (root.lastSeen === 0 || !root._seededUnread) {
          root._seededUnread = true
          root.markRead()
        }
      })
    } else {
      root.radio = ({})
      root.peers = []
    }
  }
  property bool _seededUnread: false

  // The daemon may start after the shell, or restart; keep trying.
  Timer {
    interval: 2000
    running: !root.linked
    repeat: true
    triggeredOnStart: true
    onTriggered: root.reconnectNow()
  }

  function reconnectNow() {
    if (root.linked) return
    if (root.sock) {
      root.sock.owner = null
      root.sock.destroy()
      root.sock = null
    }
    try {
      var s = Qt.createQmlObject(root.sockQml, root, "BitchatSocket")
      s.owner = root
      s.path = root.socketPath
      root.sock = s
      s.connected = true
    } catch (e) {
      console.warn("bitchat: could not create a socket: " + e)
    }
  }
}
