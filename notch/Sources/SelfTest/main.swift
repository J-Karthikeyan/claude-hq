import Foundation
import HQNotchCore

// Assertion runner for HQNotchCore (the CLT ships neither XCTest nor swift-testing).
//   hqnotch-selftest                 store / protocol checks
//   hqnotch-selftest vectors.json    + creature parity against HQ (tools/parity_vectors.py)

var failures = 0
var checks = 0
func expect(_ ok: @autoclosure () -> Bool, _ what: String, file: String = #file, line: Int = #line) {
    checks += 1
    if !ok() { failures += 1; print("FAIL \(what) (\((file as NSString).lastPathComponent):\(line))") }
}

func msg(_ json: String) -> HookMessage {
    try! JSONDecoder().decode(HookMessage.self, from: Data(json.utf8))
}

// MARK: stage thresholds and line positions

expect(Creature.stage(forPrompts: 0) == 0, "0 prompts = egg")
expect(Creature.stage(forPrompts: 3) == 1, "3 prompts = stage 1")
expect(Creature.stage(forPrompts: 69) == 3, "69 prompts = stage 3")
expect(Creature.stage(forPrompts: 70) == 4, "70 prompts = apex")
expect(Creature(species: 0, stage: 2, shiny: false).dex(sessionId: "x") == 25, "Pichu line stage 2 = Pikachu")
expect(Creature(species: 4, stage: 4, shiny: false).dex(sessionId: "abc") == 196, "Eevee branch default for 'abc'")
expect(Creature.hashStr("héllo") == 4_058_363_231, "hashStr matches JS for non-ASCII")
expect(Creature(species: -1, stage: 9, shiny: false) == Creature(species: 47, stage: 4, shiny: false), "clamps")
let urls = Creature(species: 0, stage: 4, shiny: true).spriteURLs(sessionId: "s")
expect(urls.first?.absoluteString.hasSuffix("/animated/shiny/26.gif") == true, "shiny animated URL")
expect(urls.count == 2, "animated + static fallback")

// evolution floor (HQ's Pokédex maxStage)
let floorJSON = #"{"species":[{"species":38,"maxStage":4},{"species":0,"maxStage":0},{"species":-1,"maxStage":9},{"species":"x"}]}"#
let floor = Creature.parseFloor(Data(floorJSON.utf8))
expect(floor[38] == 4 && floor[0] == nil, "floor keeps only maxStage > 0")
expect(floor[38] == 4 && floor.count == 2 && floor[47] == 4, "negative species wrapped, stage clamped")
let electrike = Creature(species: 38, stage: 0, shiny: false)
expect(electrike.dex(sessionId: "s") == 309, "raw stage 0 = Electrike")
expect(electrike.floored(floor).dex(sessionId: "s") == 310, "floored to stage 4 = Manectric")
expect(electrike.floored(floor).name(sessionId: "s") == "Manectric", "floored name")
expect(Creature(species: 38, stage: 4, shiny: false).floored([38: 1]).stage == 4, "floor never lowers")
expect(electrike.floored([:]) == electrike, "no floor = unchanged")

// MARK: store lifecycle

var st = SessionStore()
let t0 = Date(timeIntervalSince1970: 1_000_000)
st.apply(msg(#"{"v":1,"event":"SessionStart","sessionId":"s1","cwd":"/Users/me/proj","ppid":42,"env":{"KITTY_WINDOW_ID":"7"}}"#), now: t0)
expect(st.sessions["s1"]?.status == .idle, "start = idle")
expect(st.sessions["s1"]?.folder == "proj", "folder from cwd")
expect(st.sessions["s1"]?.env["KITTY_WINDOW_ID"] == "7", "env kept")
st.apply(msg(#"{"event":"UserPromptSubmit","sessionId":"s1","prompt":"fix the bug"}"#), now: t0)
expect(st.sessions["s1"]?.status == .working && st.sessions["s1"]?.promptCount == 1, "prompt = working")
expect(st.sessions["s1"]?.lastPrompt == "fix the bug", "last prompt")
st.apply(msg(#"{"event":"PreToolUse","sessionId":"s1","tool":{"tool":"Edit","target":"a.py"}}"#), now: t0)
expect(st.sessions["s1"]?.now == "Edit · a.py", "now label")
st.apply(msg(#"{"event":"PostToolUse","sessionId":"s1","tool":{"tool":"Edit","target":"a.py"},"toolUseId":"t1","plus":3,"minus":1}"#), now: t0)
expect(st.sessions["s1"]?.ticker.first?.plus == 3 && st.sessions["s1"]?.ticker.first?.minus == 1, "ticker counts")
for i in 0..<10 {
    st.apply(msg(#"{"event":"PostToolUse","sessionId":"s1","tool":{"tool":"Read","target":"f\#(i)"},"toolUseId":"r\#(i)"}"#), now: t0)
}
expect(st.sessions["s1"]?.ticker.count == SessionStore.tickerMax, "ticker capped")
expect(st.sessions["s1"]?.ticker.first?.target == "f9", "newest first")

// permission round trip
let perm = msg(#"{"event":"PermissionRequest","sessionId":"s1","tool":{"tool":"Bash","target":"rm x"},"toolInput":{"command":"rm x"},"canAlways":true,"question":false,"wantsReply":true}"#)
let fx = st.addPending(id: 9, perm, now: t0)
expect(fx == [.needsYou(sessionId: "s1")], "needs-you effect")
expect(st.sessions["s1"]?.status == .needs && st.pending.count == 1, "pending = needs")
expect(st.pending.first?.canAlways == true && st.pending.first?.isQuestion == false, "pending fields")
st.apply(msg(#"{"event":"PreToolUse","sessionId":"s1","tool":{"tool":"Bash","target":"rm x"}}"#), now: t0)
expect(st.sessions["s1"]?.status == .needs, "still needs while pending")
st.resolvePending(id: 9, now: t0)
expect(st.pending.isEmpty && st.sessions["s1"]?.status == .working, "resolved = working")

// answered in the terminal: the tool runs, the card goes
st.addPending(id: 10, perm, now: t0)
st.apply(msg(#"{"event":"PostToolUse","sessionId":"s1","tool":{"tool":"Bash","target":"rm x"},"toolUseId":"b1"}"#), now: t0)
expect(st.pending.isEmpty && st.sessions["s1"]?.status == .working, "PostToolUse clears matching pending")

// questions
let ask = msg(#"{"event":"PermissionRequest","sessionId":"s1","tool":{"tool":"AskUserQuestion","target":""},"question":true,"wantsReply":true,"toolInput":{"questions":[{"question":"Which?","header":"Pick","multiSelect":false,"options":[{"label":"A","description":"first"},{"label":"B"}]},{"question":"Also?","multiSelect":true,"options":[{"label":"X"},{"label":"Y"}]}]}}"#)
st.addPending(id: 11, ask, now: t0)
let q = st.pending.first { $0.id == 11 }
expect(q?.questions.count == 2 && q?.questions[0].options.map(\.label) == ["A", "B"], "questions parsed")
expect(q?.questions[1].multiSelect == true && q?.questions[0].options[0].detail == "first", "question fields")

// stop clears pending and celebrates, then settles
let stopFx = st.apply(msg(#"{"event":"Stop","sessionId":"s1"}"#), now: t0)
expect(stopFx == [.finished(sessionId: "s1")] && st.pending.isEmpty, "stop = finished, pending cleared")
st.tick(now: t0.addingTimeInterval(SessionStore.doneSecs + 1))
expect(st.sessions["s1"]?.status == .idle, "done settles to idle")
st.apply(msg(#"{"event":"StopFailure","sessionId":"s1","error":"rate_limit"}"#), now: t0)
expect(st.sessions["s1"]?.status == .error && st.sessions["s1"]?.now == "rate_limit", "stop failure")
st.apply(msg(#"{"event":"SubagentStart","sessionId":"s1"}"#), now: t0)
st.apply(msg(#"{"event":"SubagentStart","sessionId":"s1"}"#), now: t0)
st.apply(msg(#"{"event":"SubagentStop","sessionId":"s1"}"#), now: t0)
expect(st.sessions["s1"]?.subagents == 1, "subagent count")
st.apply(msg(#"{"event":"Notification","sessionId":"s1","notificationType":"permission_prompt","message":"Claude needs your permission"}"#), now: t0)
expect(st.sessions["s1"]?.status == .needs, "permission notification = needs")
st.apply(msg(#"{"event":"SessionEnd","sessionId":"s1"}"#), now: t0)
expect(st.sessions["s1"] == nil, "end removes")
expect(st.apply(msg(#"{"event":"Stop","sessionId":""}"#)).isEmpty && st.sessions.isEmpty, "empty session id ignored")

// ordering: needs before working before idle
var o = SessionStore()
o.apply(msg(#"{"event":"SessionStart","sessionId":"idle"}"#), now: t0)
o.apply(msg(#"{"event":"UserPromptSubmit","sessionId":"work"}"#), now: t0)
o.addPending(id: 1, msg(#"{"event":"PermissionRequest","sessionId":"need","tool":{"tool":"Bash","target":""},"wantsReply":true}"#), now: t0)
expect(o.ordered.map(\.id) == ["need", "work", "idle"], "card order")

// stale sessions drop, but never one with an open request
o.tick(now: t0.addingTimeInterval(7 * 3600))
expect(o.sessions.keys.sorted() == ["need"], "stale dropped except pending")

// HQ merge
let hqJSON = #"{"sessions":[{"sessionId":"need","title":"Fix the bug","status":"needs","kind":"interactive","pid":5,"stale":false,"promptCount":40,"creature":{"species":7,"stage":3,"shiny":true}},{"sessionId":"old","status":"stale","kind":"archived","stale":true,"creature":{"species":1,"stage":0}},{"sessionId":"new","status":"working","kind":"interactive","pid":9,"stale":false,"cwd":"/x/y","now":"Bash","promptCount":2,"creature":{"species":3,"stage":0,"shiny":false}}]}"#
let hq = HQSession.parse(Data(hqJSON.utf8))
expect(hq.count == 3 && hq.filter(\.live).map(\.sessionId) == ["need", "new"], "HQ live filter")
o.mergeHQ(hq, now: t0)
expect(o.sessions["need"]?.creature == Creature(species: 7, stage: 3, shiny: true), "HQ creature wins")
expect(o.sessions["need"]?.promptCount == 40, "HQ prompt count")
expect(o.sessions["need"]?.label == "Fix the bug" && o.sessions["new"]?.label == "y", "label = HQ title, else folder")
expect(o.sessions["new"]?.folder == "y" && o.sessions["new"]?.status == .working, "HQ seeds live sessions")
expect(o.sessions["old"] == nil, "archived not seeded")

// MARK: plans, diffs, phone alerts

// a plan waiting for approval
var pl = SessionStore()
let planMsg = msg(##"{"event":"PermissionRequest","sessionId":"p","tool":{"tool":"ExitPlanMode","target":""},"wantsReply":true,"toolInput":{"plan":"# Plan\n1. **Do** it"}}"##)
pl.addPending(id: 3, planMsg, now: t0)
expect(pl.pending.first?.isPlan == true && pl.pending.first?.plan == "# Plan\n1. **Do** it", "plan parsed")
expect(pl.pending.first?.isQuestion == false && pl.sessions["p"]?.now == "Has a plan for you", "plan card label")
pl.addPending(id: 4, msg(#"{"event":"PermissionRequest","sessionId":"p","tool":{"tool":"ExitPlanMode","target":""},"wantsReply":true}"#), now: t0)
expect(pl.pending.last?.plan == "", "plan without text still a plan card")
pl.addPending(id: 5, msg(#"{"event":"PermissionRequest","sessionId":"p","tool":{"tool":"Bash","target":"ls"},"wantsReply":true,"toolInput":{"plan":"x"}}"#), now: t0)
expect(pl.pending.last?.isPlan == false, "only ExitPlanMode is a plan")

// a diff rides on its ticker item
var dfs = SessionStore()
dfs.apply(msg(#"{"event":"PostToolUse","sessionId":"d","tool":{"tool":"Edit","target":"a.py"},"toolUseId":"e1","plus":1,"minus":1,"diff":{"edits":[{"old":"a\nb\nc","new":"a\nB\nc"}],"truncated":false}}"#), now: t0)
expect(dfs.sessions["d"]?.ticker.first?.diff == EditDiff(edits: [.init(old: "a\nb\nc", new: "a\nB\nc")]), "diff kept on the ticker")
dfs.apply(msg(#"{"event":"PostToolUse","sessionId":"d","tool":{"tool":"Read","target":"a.py"},"toolUseId":"r1"}"#), now: t0)
expect(dfs.sessions["d"]?.ticker.first?.diff == nil, "no diff for a read")
expect(msg(#"{"event":"PostToolUse","sessionId":"d","diff":"junk"}"#).diff == nil, "a bad diff is ignored")

// diff lines
expect(DiffBuilder.lines(old: "a\nb\nc", new: "a\nB\nc") == [.context("a"), .removed("b"), .added("B"), .context("c")], "one changed line")
expect(DiffBuilder.lines(old: "", new: "x\ny\n") == [.added("x"), .added("y")], "a new file is all added")
expect(DiffBuilder.lines(old: "1\n2\n3\n4\n5\nold\n6\n7\n8\n9", new: "1\n2\n3\n4\n5\nnew\n6\n7\n8\n9") ==
       [.gap(3), .context("4"), .context("5"), .removed("old"), .added("new"), .context("6"), .context("7"), .gap(2)], "context is two lines, the rest a gap")
expect(DiffBuilder.lines(old: "same", new: "same") == [.context("same")], "no change")
let two = DiffBuilder.lines(EditDiff(edits: [.init(old: "a", new: "b"), .init(old: "c", new: "d")], truncated: true))
expect(two == [.removed("a"), .added("b"), .gap(0), .removed("c"), .added("d"), .more], "edits separated, truncation shown")
let big = DiffBuilder.lines(EditDiff(edits: [.init(old: "", new: Array(repeating: "l", count: 900).joined(separator: "\n"))]))
expect(big.count == DiffBuilder.maxLines + 1 && big.last == .more, "capped at maxLines")

// phone alerts: fixed text, nothing from a session
expect(PhoneAlert.from(.needsYou(sessionId: "secret-session")).body == "A Claude session needs you", "needs-you text")
expect(PhoneAlert.from(.finished(sessionId: "x")) == .finished && PhoneAlert.from(.failed(sessionId: "x")) == .failed, "effect -> alert")
expect(PhoneAlert.allCases.allSatisfy { !$0.body.contains("secret") && $0.title == "Claude HQ" }, "content-free")
var db = PhoneDebounce()
expect(db.allow(.needsYou, now: t0) && !db.allow(.needsYou, now: t0.addingTimeInterval(10)), "debounced within 30 s")
expect(db.allow(.finished, now: t0.addingTimeInterval(10)), "each kind on its own clock")
expect(db.allow(.needsYou, now: t0.addingTimeInterval(31)), "allowed again after 30 s")
let topic = PhoneTopic.make(Array(0..<20).map { UInt8($0*13 % 256) })
expect(topic != nil && PhoneTopic.isValid(topic!) && topic!.count == 23, "topic shape")
expect(PhoneTopic.make([1, 2, 3]) == nil, "too few random bytes")
expect(!PhoneTopic.isValid("hq-short") && !PhoneTopic.isValid("HQ-ABCDEFGHIJKLMNOPQRST") && !PhoneTopic.isValid("hq-abcdefghijklmnopqr1s"), "bad topics")
let tp = "hq-abcdefghijklmnopqrst"
expect(PhoneTopic.url(server: "https://ntfy.sh", topic: tp)?.absoluteString == "https://ntfy.sh/" + tp, "ntfy url")
expect(PhoneTopic.url(server: " https://push.example.com/ntfy/ ", topic: tp)?.absoluteString == "https://push.example.com/ntfy/" + tp, "self-hosted path")
expect(PhoneTopic.url(server: "http://10.0.0.2:8080", topic: tp) != nil, "http on your own network")
for bad in ["ftp://ntfy.sh", "ntfy.sh", "https://", "https://u:p@ntfy.sh", "https://ntfy.sh?x=1", "javascript:alert(1)"] {
    expect(PhoneTopic.url(server: bad, topic: tp) == nil, "bad server \(bad)")
}
expect(PhoneTopic.url(server: "https://ntfy.sh", topic: "../etc") == nil, "bad topic in url")

// MARK: parity with HQ

if CommandLine.arguments.count > 1 {
    let data = try! Data(contentsOf: URL(fileURLWithPath: CommandLine.arguments[1]))
    let root = try! JSONSerialization.jsonObject(with: data) as! [String: Any]
    let salt = root["salt"] as! String
    let rows = root["rows"] as! [[String: Any]]
    var megas = 0
    for r in rows {
        let sid = r["sid"] as! String, prompts = r["prompts"] as! Int
        let c = Creature.local(sessionId: sid, prompts: prompts, salt: salt)
        expect(c.species == r["species"] as! Int, "species \(sid)")
        expect(c.stage == r["stage"] as! Int, "stage \(sid)")
        expect(c.shiny == r["shiny"] as! Bool, "shiny \(sid)")
        expect(Int(Creature.hashStr(sid)) == r["hashStr"] as! Int, "hashStr \(sid)")
        expect(c.dex(sessionId: sid) == r["dex"] as! Int, "dex \(sid) stage \(c.stage)")
        expect(c.name(sessionId: sid) == r["name2d"] as! String, "2D name \(sid)")
        expect(c.name(sessionId: sid, pack: .threeD) == r["name3d"] as! String, "3D name \(sid) stage \(c.stage)")
        expect(c.spriteURLs(sessionId: sid, pack: .threeD).first?.absoluteString == r["src3d"] as? String,
               "3D sprite \(sid): \(c.spriteURLs(sessionId: sid, pack: .threeD).first?.absoluteString ?? "-") vs \(r["src3d"] ?? "-")")
        if c.megaForm(sessionId: sid, pack: .threeD) != nil { megas += 1 }
    }
    expect(megas > 0, "vectors include Mega forms")
    print("parity: \(rows.count) sessions (\(megas) Megas) checked against dashboard.py + 06-trainer-packs.js, 2D and 3D")
}

print(failures == 0 ? "OK: \(checks) checks" : "FAILED: \(failures) of \(checks) checks")
exit(failures == 0 ? 0 : 1)
