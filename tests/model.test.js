// Run with: node --test tests/
const test = require("node:test")
const assert = require("node:assert/strict")
const fs = require("node:fs")
const path = require("node:path")
const vm = require("node:vm")

function loadModel() {
  const source = fs.readFileSync(path.join(__dirname, "..", "Model.js"), "utf8")
    .replace(/^\.pragma library\s*$/m, "")
  const ctx = {}
  vm.createContext(ctx)
  vm.runInContext(source, ctx)
  return ctx
}

const M = loadModel()
const same = (a, e, msg) => assert.deepEqual(JSON.parse(JSON.stringify(a)), e, msg)

const NOW = new Date(2026, 8, 30, 14, 0, 0).getTime()
const msg = (id, senderId, ts, extra) => Object.assign({ id, senderId, nickname: "n" + senderId, text: "hi", timestamp: ts, mine: false }, extra || {})

test("display names", () => {
  assert.equal(M.displayName("alice", "1a2b3c4d5e6f7a8b"), "alice#1a2b")
  assert.equal(M.displayName("  ", "1a2b3c4d5e6f7a8b"), "anon#1a2b")
  assert.equal(M.displayName("bob", ""), "bob")
})

test("ambiguous nicknames get a longer tag", () => {
  const peers = [{ nickname: "alice" }, { nickname: "Alice " }, { nickname: "bob" }]
  const amb = M.ambiguousNames(peers)
  assert.ok(M.isAmbiguous(amb, "ALICE"))
  assert.ok(!M.isAmbiguous(amb, "bob"))
  assert.equal(M.displayName("alice", "1a2b3c4d5e6f7a8b", true), "alice#1a2b3c4d")
})

test("markup is escaped for notifications", () => {
  assert.equal(M.escapeMarkup("<b>hi</b> & 'x'"), "&lt;b&gt;hi&lt;/b&gt; &amp; &#39;x&#39;")
})

test("nick hue is stable and in range", () => {
  const a = M.nickHue("1a2b3c4d5e6f7a8b")
  assert.equal(a, M.nickHue("1a2b3c4d5e6f7a8b"))
  assert.ok(a >= 0 && a < 1)
  assert.notEqual(a, M.nickHue("ffffffffffffffff"))
})

test("time labels", () => {
  assert.equal(M.timeLabel(new Date(2026, 8, 30, 9, 5).getTime(), NOW), "09:05")
  assert.equal(M.timeLabel(new Date(2026, 8, 30, 21, 5).getTime(), NOW, false), "9:05 PM")
  assert.equal(M.timeLabel(new Date(2026, 8, 29, 9, 5).getTime(), NOW), "Tue 09:05")
  assert.equal(M.timeLabel(new Date(2026, 8, 1, 9, 5).getTime(), NOW), "Sep 1 09:05")
})

test("mentions", () => {
  assert.ok(M.mentions("hey @raven", "raven"))
  assert.ok(M.mentions("@Raven#55c3 ping", "raven"))
  assert.ok(M.mentions("(@raven)", "raven"))
  assert.ok(!M.mentions("hey raven", "raven"))
  assert.ok(!M.mentions("hey @ravenous", "raven"))
  assert.ok(!M.mentions("mail@raven", "raven"))
  assert.ok(M.mentions("hi @a.b", "a.b"))
  assert.ok(!M.mentions("anything", ""))
})

test("input parsing", () => {
  same(M.parseInput("  hello  "), { kind: "send", text: "hello" })
  same(M.parseInput(""), { kind: "none" })
  same(M.parseInput("/nick  raven "), { kind: "nick", value: "raven" })
  assert.equal(M.parseInput("/nick").kind, "error")
  same(M.parseInput("/CLEAR"), { kind: "clear" })
  same(M.parseInput("/who"), { kind: "who" })
  same(M.parseInput("/help"), { kind: "help" })
  same(M.parseInput("//shrug"), { kind: "send", text: "/shrug" })
  assert.equal(M.parseInput("/bogus").kind, "error")
  assert.equal(M.parseInput("x".repeat(4001)).kind, "error")
  assert.equal(M.parseInput("é".repeat(2001)).kind, "error")
  assert.equal(M.parseInput("é".repeat(2000)).kind, "send")
})

test("utf8 length", () => {
  assert.equal(M.utf8Length("abc"), 3)
  assert.equal(M.utf8Length("é"), 2)
  assert.equal(M.utf8Length("€"), 3)
  assert.equal(M.utf8Length("😀"), 4)
})

test("rows group by sender and time", () => {
  const r = M.rows([
    msg("1", "a", NOW),
    msg("2", "a", NOW + 1000),
    msg("3", "b", NOW + 2000),
    msg("4", "b", NOW + 2000 + 6 * 60 * 1000),
  ])
  same(r.map(x => x.first), [true, false, true, true])
})

test("append keeps order, dedups and bounds", () => {
  let list = [msg("1", "a", NOW), msg("3", "a", NOW + 20)]
  list = M.appendMessage(list, msg("2", "b", NOW + 10))
  same(list.map(m => m.id), ["1", "2", "3"])
  assert.equal(M.appendMessage(list, msg("2", "b", NOW + 10)), list)
  let big = []
  for (let i = 0; i < M.MAX_MESSAGES + 5; i++) big = M.appendMessage(big, msg(String(i), "a", NOW + i))
  assert.equal(big.length, M.MAX_MESSAGES)
  assert.equal(big[0].id, "5")
})

test("unread counts only others' newer messages", () => {
  const list = [msg("1", "a", NOW), msg("2", "me", NOW + 1, { mine: true }), msg("3", "b", NOW + 2)]
  assert.equal(M.unreadCount(list, NOW - 1), 2)
  assert.equal(M.unreadCount(list, NOW + 1), 1)
  assert.equal(M.unreadCount(list, NOW + 5), 0)
})

test("notification rules", () => {
  const m = msg("1", "a", NOW, { text: "yo @raven" })
  assert.ok(M.shouldNotify(m, "mentions", "raven", false))
  assert.ok(!M.shouldNotify(m, "mentions", "raven", true))
  assert.ok(!M.shouldNotify(msg("2", "a", NOW), "mentions", "raven", false))
  assert.ok(M.shouldNotify(msg("2", "a", NOW), "all", "raven", false))
  assert.ok(!M.shouldNotify(m, "off", "raven", false))
  assert.ok(!M.shouldNotify(Object.assign({}, m, { mine: true }), "all", "raven", false))
})

test("old backfilled messages don't notify", () => {
  const m = msg("1", "a", NOW - 10 * 60 * 1000)
  assert.ok(!M.shouldNotify(m, "all", "raven", false, NOW))
  assert.ok(M.shouldNotify(msg("2", "a", NOW - 1000), "all", "raven", false, NOW))
})

test("safe notification arguments", () => {
  assert.equal(M.safeArg("--image=/x"), "\u2011-image=/x")
  assert.equal(M.safeArg("-u"), "\u2011u")
  assert.equal(M.safeArg("hello -x"), "hello -x")
})

test("rowFor groups against the previous message", () => {
  assert.equal(M.rowFor(null, msg("1", "a", NOW)).first, true)
  assert.equal(M.rowFor(msg("1", "a", NOW), msg("2", "a", NOW + 1)).first, false)
  assert.equal(M.rowFor(msg("1", "a", NOW), msg("2", "b", NOW + 1)).first, true)
})

test("radio line", () => {
  assert.equal(M.radioLine({}, false), "bitchatd isn't running")
  assert.equal(M.radioLine({ state: "running", effective: "balanced" }, true), "Active")
  assert.equal(M.radioLine({ state: "running", effective: "saver", onBattery: true, audio: true }, true), "Power saver (on battery, Bluetooth audio)")
  assert.equal(M.radioLine({ state: "adapterOff" }, true), "Bluetooth is off")
  assert.equal(M.radioLine({ state: "error", detail: "boom" }, true), "Radio error: boom")
})

test("peer summary", () => {
  assert.equal(M.peerSummary([]), "No one nearby")
  assert.equal(M.peerSummary([{ direct: true }]), "1 peer")
  assert.equal(M.peerSummary([{ direct: true }, { direct: false }]), "2 peers (1 direct)")
})
