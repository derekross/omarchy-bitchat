.pragma library

// Pure helpers for the bitchat panel: names, colors, times, mentions,
// slash commands. No Qt here, so node can test it (tests/model.test.js).

var MAX_TEXT_BYTES = 4000
var MAX_MESSAGES = 300
// Messages from one sender closer together than this share a header.
var GROUP_GAP_MS = 5 * 60 * 1000

function shortId(peerId) {
  return String(peerId || "").slice(0, 4)
}

// "alice#1a2b": the nickname plus enough of the peer ID to tell two
// alices apart, the convention bitchat uses.
function displayName(nickname, peerId) {
  var nick = String(nickname || "").trim()
  if (nick === "") nick = "anon"
  var tag = shortId(peerId)
  return tag === "" ? nick : nick + "#" + tag
}

// A stable hue in [0, 1) per peer, so each sender keeps a color.
function nickHue(peerId) {
  var s = String(peerId || "")
  var h = 5381
  for (var i = 0; i < s.length; i++) h = ((h * 33) ^ s.charCodeAt(i)) >>> 0
  return (h % 360) / 360
}

function pad2(n) { return n < 10 ? "0" + n : String(n) }

var DAYS = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"]

// "14:05" today, "Tue 14:05" this week, "Sep 3 14:05" before that.
function timeLabel(ms, nowMs, use24h) {
  var d = new Date(ms)
  var now = new Date(nowMs)
  var h = d.getHours()
  var clock = use24h === false
    ? ((h % 12) || 12) + ":" + pad2(d.getMinutes()) + (h < 12 ? " AM" : " PM")
    : pad2(h) + ":" + pad2(d.getMinutes())
  var startOfToday = new Date(now.getFullYear(), now.getMonth(), now.getDate()).getTime()
  if (ms >= startOfToday) return clock
  if (ms >= startOfToday - 6 * 86400000) return DAYS[d.getDay()] + " " + clock
  var months = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"]
  return months[d.getMonth()] + " " + d.getDate() + " " + clock
}

function escapeRegExp(s) {
  return String(s).replace(/[.*+?^${}()|[\]\\]/g, "\\$&")
}

// Does the text mention this nickname as @nick (optionally @nick#abcd)?
function mentions(text, nickname) {
  var nick = String(nickname || "").trim()
  if (nick === "") return false
  var re = new RegExp("(^|[^\\w@])@" + escapeRegExp(nick) + "(#[0-9a-f]{4})?(?![\\w])", "i")
  return re.test(String(text || ""))
}

function utf8Length(s) {
  var n = 0
  s = String(s)
  for (var i = 0; i < s.length; i++) {
    var c = s.charCodeAt(i)
    if (c < 0x80) n += 1
    else if (c < 0x800) n += 2
    else if (c >= 0xd800 && c <= 0xdbff) { n += 4; i++ }
    else n += 3
  }
  return n
}

// What the input line asks for.
//   {kind: "send", text} | {kind: "nick", value} | {kind: "clear"} |
//   {kind: "who"} | {kind: "help"} | {kind: "error", message} | {kind: "none"}
function parseInput(raw) {
  var text = String(raw || "")
  var trimmed = text.trim()
  if (trimmed === "") return { kind: "none" }
  if (trimmed.indexOf("//") === 0) trimmed = trimmed.slice(1)
  else if (trimmed.charAt(0) === "/") {
    var space = trimmed.indexOf(" ")
    var cmd = (space === -1 ? trimmed : trimmed.slice(0, space)).toLowerCase()
    var arg = space === -1 ? "" : trimmed.slice(space + 1).trim()
    if (cmd === "/nick" || cmd === "/name") {
      if (arg === "") return { kind: "error", message: "Usage: /nick <name>" }
      return { kind: "nick", value: arg }
    }
    if (cmd === "/clear") return { kind: "clear" }
    if (cmd === "/who" || cmd === "/w" || cmd === "/peers") return { kind: "who" }
    if (cmd === "/help" || cmd === "/?") return { kind: "help" }
    return { kind: "error", message: "Unknown command " + cmd + ". Try /help." }
  }
  if (utf8Length(trimmed) > MAX_TEXT_BYTES) return { kind: "error", message: "That message is too long for the mesh." }
  return { kind: "send", text: trimmed }
}

var HELP = "/nick <name> change your name · /who list peers · /clear clear history · //text send a line starting with /"

// One row of the message list: the message plus whether it starts a new
// group (sender changed, or a long pause since `prev`).
function rowFor(prev, m) {
  return {
    id: String(m.id),
    senderId: String(m.senderId),
    nickname: String(m.nickname),
    text: String(m.text),
    timestamp: Number(m.timestamp),
    mine: m.mine === true,
    first: !prev || prev.senderId !== m.senderId || m.timestamp - prev.timestamp > GROUP_GAP_MS
  }
}

function rows(messages) {
  var out = []
  for (var i = 0; i < messages.length; i++) out.push(rowFor(i > 0 ? messages[i - 1] : null, messages[i]))
  return out
}

// Desktop notifications only for news: sync can backfill hours-old messages.
var NOTIFY_MAX_AGE_MS = 5 * 60 * 1000

// Remote text as a notify-send style argument: never let it start with "-",
// where the helper would read it as an option.
function safeArg(s) {
  s = String(s || "")
  return s.charAt(0) === "-" ? "\u2011" + s.slice(1) : s
}

// Append one message, keeping the list ordered, deduplicated and bounded.
function appendMessage(list, msg) {
  for (var i = list.length - 1; i >= 0 && i >= list.length - 50; i--)
    if (list[i].id === msg.id) return list
  var out = list.slice()
  var at = out.length
  while (at > 0 && out[at - 1].timestamp > msg.timestamp) at--
  out.splice(at, 0, msg)
  if (out.length > MAX_MESSAGES) out = out.slice(out.length - MAX_MESSAGES)
  return out
}

function peerSummary(peers) {
  var n = peers ? peers.length : 0
  if (n === 0) return "No one nearby"
  var direct = 0
  for (var i = 0; i < n; i++) if (peers[i].direct) direct++
  var s = n === 1 ? "1 peer" : n + " peers"
  if (direct > 0 && direct < n) s += " (" + direct + " direct)"
  return s
}

// One line on what the radio is doing, for the tooltip and panel header.
function radioLine(radio, linked) {
  if (!linked) return "bitchatd isn't running"
  if (!radio || !radio.state) return "Starting…"
  switch (radio.state) {
  case "off": return "Mesh is off"
  case "starting": return "Starting the radio…"
  case "adapterOff": return "Bluetooth is off"
  case "noAdapter": return "No Bluetooth adapter"
  case "error": return "Radio error" + (radio.detail ? ": " + radio.detail : "")
  }
  var mode = radio.effective === "saver" ? "Power saver" : "Active"
  var why = []
  if (radio.effective === "saver" && radio.onBattery) why.push("on battery")
  if (radio.effective === "saver" && radio.audio) why.push("Bluetooth audio")
  return mode + (why.length ? " (" + why.join(", ") + ")" : "")
}

function isRunning(radio) {
  return !!radio && radio.state === "running"
}

// Count messages from others newer than the last time the panel was open.
function unreadCount(messages, lastSeenMs) {
  var n = 0
  for (var i = messages.length - 1; i >= 0; i--) {
    var m = messages[i]
    if (m.timestamp <= lastSeenMs) break
    if (!m.mine) n++
  }
  return n
}

// Should this incoming message raise a desktop notification?
function shouldNotify(msg, setting, myNickname, panelOpen, nowMs) {
  if (!msg || msg.mine || panelOpen) return false
  if (nowMs !== undefined && nowMs - msg.timestamp > NOTIFY_MAX_AGE_MS) return false
  if (setting === "all") return true
  if (setting === "mentions") return mentions(msg.text, myNickname)
  return false
}

function truncate(s, n) {
  s = String(s || "")
  return s.length > n ? s.slice(0, n - 1) + "…" : s
}
