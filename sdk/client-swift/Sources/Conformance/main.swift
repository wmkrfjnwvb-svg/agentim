// The wrapper conformance scenario (docs/12 §2.13, W1–W9), against a real
// `agentim`: the same steps every platform wrapper runs. See test.sh.

@_spi(Testing) import AgentIM
import Foundation
#if canImport(FoundationNetworking)
import FoundationNetworking
#endif

var failures = 0

@MainActor
func check(_ ok: Bool, _ what: String) {
    print("\(ok ? "ok  " : "FAIL") \(what)")
    if !ok { failures += 1 }
}

@MainActor
func until(_ ms: Double = 20000, _ f: () async -> Bool) async -> Bool {
    let end = Date().addingTimeInterval(ms / 1000)
    while Date() < end {
        if await f() { return true }
        try? await Task.sleep(nanoseconds: 25_000_000)
    }
    return await f()
}

struct Person {
    let id: String
    let name: String
    let token: String
}

@MainActor
func sent(_ v: Messages) -> [String] { v.list.filter { $0.status == "sent" }.map(\.content) }

// B12: a reset replaces a view in one update, never empty in between.
do {
    func up(_ key: String, _ order: Int) -> JSONObject { ["op": "upsert", "message": ["key": key, "order": NSNumber(value: order)] as JSONObject] }
    let v = Messages(detached: "c1")
    v.apply([up("a", 1), up("b", 2)])
    let before = v.publishes
    v.apply([up("b", 2), up("c", 3)], reset: true)
    check(v.publishes == before + 1 && v.list.map(\.key) == ["b", "c"], "B12 a reset is one update (\(v.publishes - before) updates)")
}

let env = ProcessInfo.processInfo.environment
let bin = env["AGENTIM_BIN"]!
let dir = FileManager.default.temporaryDirectory.appendingPathComponent("agentim-swift-\(UUID().uuidString)")
try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
let port = 20000 + Int.random(in: 0..<100)
let url = "http://127.0.0.1:\(port)"
let server = Process()
server.executableURL = URL(fileURLWithPath: bin)
server.arguments = ["--demo", "--db", dir.appendingPathComponent("a.db").path, "--addr", "127.0.0.1:\(port)"]
server.standardOutput = FileHandle.nullDevice
server.standardError = FileHandle.nullDevice
try server.run()
var n = 0

@MainActor
func person(_ name: String) async throws -> Person {
    n += 1
    let nm = "\(name)\(n)"
    var req = URLRequest(url: URL(string: "\(url)/api/dev/users")!)
    req.httpMethod = "POST"
    req.setValue("application/json", forHTTPHeaderField: "content-type")
    req.httpBody = try JSONSerialization.data(withJSONObject: ["name": nm])
    let (data, _) = try await URLSession.shared.data(for: req)
    let r = try JSONSerialization.jsonObject(with: data) as! JSONObject
    return Person(id: (r["user"] as! JSONObject)["id"] as! String, name: nm, token: r["token"] as! String)
}

@MainActor
func client(_ p: Person, store: String? = nil, device: String? = nil, token: (@Sendable () async throws -> String)? = nil) throws -> AgentIMClient {
    let t = p.token
    return try AgentIMClient(ClientOptions(url: url, deviceId: device ?? "\(p.name)-d", storeDir: store, tokenProvider: token ?? { t }))
}

@MainActor
func code(_ f: () async throws -> Void) async -> String {
    do {
        try await f()
        return ""
    } catch let e as AgentIMError {
        return e.code
    } catch {
        return "\(error)"
    }
}

do {
    _ = await until(30000) {
        (try? await URLSession.shared.data(from: URL(string: "\(url)/readyz")!)).map { ($0.1 as? HTTPURLResponse)?.statusCode == 200 } ?? false
    }
    check(!AgentIMClient.version.isEmpty, "version \(AgentIMClient.version)")
    let alice = try await person("alice")
    let bob = try await person("bob")
    let a = try client(alice)
    let b = try client(bob)
    try await a.connect()
    try await b.connect()
    check(await until { a.connection.state == "connected" && b.connection.state == "connected" }, "W1 both connect")

    let created = try await a.call("create_conv", ["kind": "group", "title": "swift", "members": [bob.id]]) as! JSONObject
    let conv = created["conv_id"] as! String
    let av = a.messages(conv)
    let r = try await a.send(conv, "hello from Swift 👋")
    check((r["key"] as? String ?? "").hasPrefix("local:") && av.list.contains { $0.content == "hello from Swift 👋" }, "W2 a send is shown when it returns")
    check(await until { sent(av).contains("hello from Swift 👋") }, "W2 and then sent (emoji intact)")
    let bv = b.messages(conv)
    check(await until { sent(bv).contains("hello from Swift 👋") }, "W2 the other side sees it")
    check(await until { b.conversations.contains { $0.convId == conv } }, "W2 and has the conversation in its list")

    let mine = av.list.first { $0.content == "hello from Swift 👋" }!
    try await a.edit(conv, msgId: mine.msgId!, "hello, edited")
    check(await until { sent(bv).first == "hello, edited" }, "W3 an edit reaches the other side")
    try await a.recall(conv, msgId: mine.msgId!)
    check(await until { bv.list.first?.state == "recalled" }, "W3 a recall reaches the other side")

    var ops: [String] = []
    let sub = a.on("operation") { ops.append($0["result"] as? String ?? "") }
    try await a.send("c_not_mine", "nope", clientMsgId: "bad1")
    let nv = a.messages("c_not_mine")
    check(await until { nv.list.contains { $0.status == "failed" } && ops.contains("failed") }, "W4 a refused send fails and is reported")
    try await a.discard("c_not_mine", clientMsgId: "bad1")
    check(await until { nv.list.isEmpty }, "W4 discard removes it")
    sub.cancel()

    let c1 = await code { try await a.call("get_request", ["request_id": "nope"]) }
    check(c1 == "not_found", "W5 an online call answers with the server's code (\(c1))")
    let c2 = await code { try await a.command(["cmd": "no_such_command"]) }
    check(c2 == "bad_request", "W5 an unknown command is bad_request (\(c2))")

    let store = dir.appendingPathComponent("alice-cache").path
    do {
        let q = try client(alice, store: store, device: "alice-q")
        for t in ["one", "two", "three"] { try await q.send(conv, t) }
        q.close()
    }
    do {
        let q = try client(alice, store: store, device: "alice-q")
        let v = q.messages(conv)
        check(await until { v.list.filter { $0.status == "sending" }.count == 3 }, "W6 after a restart the queue shows before connecting")
        try await q.connect()
        check(await until { sent(bv).filter { ["one", "two", "three"].contains($0) }.joined(separator: "|") == "one|two|three" }, "W6 and is sent once, in order")
        check(await until { ((try? await q.command(["cmd": "pending"]) as? JSONObject)?["ops"] as? [Any])?.isEmpty == true }, "W6 and the queue is empty")
        q.setNetwork(online: false)
        check(await until { q.connection.state != "connected" }, "W7 offline is not connected (\(q.connection.state))")
        try await q.send(conv, "while offline")
        q.setNetwork(online: true)
        check(await until { sent(bv).filter { $0 == "while offline" }.count == 1 }, "W7 back online it is sent once")
        q.close()
    }

    let x = try client(alice, device: "alice-r", token: { throw TokenRefused() })
    try await x.connect()
    check(await until { x.connection.state == "failed" }, "W8 a refused token stops the client")
    check(x.connection.reason == "unauthenticated", "W8 as unauthenticated (\(x.connection.reason ?? "nil"))")
    x.close()

    let d = try client(alice, device: "alice-d")
    d.close()
    d.close()
    let c3 = await code { try await d.connect() }
    check(c3 == "closed", "W9 after close, commands fail with closed (close twice is fine)")
    a.close()
    b.close()
} catch {
    print("FAIL \(error)")
    failures += 1
}
server.terminate()
try? FileManager.default.removeItem(at: dir)
exit(failures == 0 ? 0 : 1)
