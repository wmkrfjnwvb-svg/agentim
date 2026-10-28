// AgentIM client for iOS and macOS (docs/12 §2). The rules live in the Rust
// core; this turns its events into @Observable state on the main actor and
// its replies into async functions.
//
//   let client = try AgentIMClient(ClientOptions(
//       url: "https://chat.example.com",
//       deviceId: savedDeviceId,
//       storeDir: cacheURL.path,
//       tokenProvider: { try await backend.token() }))
//   // SwiftUI reads client.connection, client.conversations, client.messages(id).list
//   try await client.connect()
//   try await client.send(convId, "hello")   // in messages(convId).list when it returns
//
// Everything the core says (state changes, events, replies) is handled in
// order on one queue, the main queue by default: when an async method
// returns, the state it changed is already there.

import CAgentIM
import Dispatch
import Foundation
import Observation

public typealias JSONObject = [String: Any]

public struct ClientOptions {
    /// `https://chat.example.com`.
    public var url: String
    /// This device's id, the same every run (1..64 of A-Z a-z 0-9 _ -).
    public var deviceId: String
    /// Where the cache lives; nil keeps it in memory.
    public var storeDir: String?
    public var deviceName: String?
    public var platform: String?
    /// A client token from your backend; throw `TokenRefused` to stop.
    public var tokenProvider: @Sendable () async throws -> String
    public var clientName = "agentim-swift"
    public var clientVersion = "0.1.0"
    public var commandTimeout: TimeInterval = 60

    public init(url: String, deviceId: String, storeDir: String? = nil, deviceName: String? = nil, platform: String? = nil,
                tokenProvider: @escaping @Sendable () async throws -> String) {
        self.url = url
        self.deviceId = deviceId
        self.storeDir = storeDir
        self.deviceName = deviceName
        self.platform = platform
        self.tokenProvider = tokenProvider
    }
}

/// Thrown by a token provider when the backend refuses the user.
public struct TokenRefused: Error {
    public init() {}
}

/// The server's code (`forbidden`, `not_found`, `conflict`, `bad_request`, `blocked`,
/// `rate_limited`…) or the client's (`disconnected`, `timeout`, `closed`…); unknown codes
/// pass through as they are.
public struct AgentIMError: Error, CustomStringConvertible {
    public let code: String
    public let message: String
    public let retryable: Bool
    public let retryAfterMs: Int64?

    public init(code: String, message: String, retryable: Bool = false, retryAfterMs: Int64? = nil) {
        self.code = code
        self.message = message
        self.retryable = retryable
        self.retryAfterMs = retryAfterMs
    }

    init(_ e: JSONObject?) {
        code = e?["code"] as? String ?? "internal"
        message = e?["message"] as? String ?? code
        retryable = e?["retryable"] as? Bool ?? false
        retryAfterMs = (e?["retry_after_ms"] as? NSNumber)?.int64Value
    }

    public var description: String { "\(code): \(message)" }
}

public struct Connection: Equatable {
    /// closed, connecting, connected, disconnected, suspended or failed.
    public let state: String
    public let reason: String?
    public let retryAt: Int64?
}

/// One message as shown; `raw` has every field the core gives.
public struct Message {
    public let key: String
    public let msgId: String?
    public let clientMsgId: String?
    public let content: String
    /// sent, sending or failed.
    public let status: String
    /// The server state: normal, edited, recalled…
    public let state: String?
    /// human, agent, system: agent messages must show the AI mark (EU AI Act art. 50).
    public let senderType: String?
    public let order: Double
    public let raw: JSONObject

    init(_ m: JSONObject) {
        key = m["key"] as? String ?? ""
        msgId = m["msg_id"] as? String
        clientMsgId = m["client_msg_id"] as? String
        content = m["content"] as? String ?? ""
        status = m["status"] as? String ?? "sent"
        state = m["state"] as? String
        senderType = m["sender_type"] as? String
        order = (m["order"] as? NSNumber)?.doubleValue ?? 0
        raw = m
    }
}

public struct Conversation {
    public let convId: String
    public let raw: JSONObject

    init(_ c: JSONObject) {
        convId = c["conv_id"] as? String ?? ""
        raw = c
    }
}

/// One conversation's messages as shown, oldest first.
@Observable
public final class Messages {
    public let convId: String
    public private(set) var list: [Message] = []
    public private(set) var hasOlder = false
    public private(set) var olderEnd: String?
    @ObservationIgnored private var byKey: [String: Message] = [:]
    @ObservationIgnored private weak var client: AgentIMClient?

    init(_ client: AgentIMClient, _ convId: String) {
        self.client = client
        self.convId = convId
    }

    /// A view tied to no client, for tests.
    @_spi(Testing) public init(detached convId: String) {
        self.convId = convId
    }

    func reset(_ list: [Any]?, hasOlder: Bool?, olderEnd: String?) {
        byKey = [:]
        for case let m as JSONObject in list ?? [] {
            let v = Message(m)
            byKey[v.key] = v
        }
        if let h = hasOlder { self.hasOlder = h }
        if let e = olderEnd { self.olderEnd = e }
        publish()
    }

    /// A `messages` event: with `reset` the view is replaced by the changes
    /// in one update, so no one sees it empty in between.
    @_spi(Testing) public func apply(_ changes: [Any]?, reset: Bool = false) {
        if reset { byKey = [:] }
        for case let c as JSONObject in changes ?? [] {
            if c["op"] as? String == "upsert", let m = c["message"] as? JSONObject {
                let v = Message(m)
                byKey[v.key] = v
            } else if let k = c["key"] as? String {
                byKey[k] = nil
            }
        }
        publish()
    }

    private func publish() {
        list = byKey.values.sorted { $0.order < $1.order }
        publishes += 1
    }

    /// How often the list changed, for tests.
    @_spi(Testing) @ObservationIgnored public private(set) var publishes = 0

    /// The page before the oldest shown: the cache first, then the server. False at the start.
    @discardableResult
    public func loadOlder() async throws -> Bool {
        guard let client else { throw AgentIMError(code: "closed", message: "the client is closed") }
        let r = try await client.command(["cmd": "load_older", "conv_id": convId]) as? JSONObject
        hasOlder = r?["has_older"] as? Bool ?? false
        olderEnd = r?["older_end"] as? String
        return hasOlder
    }

    /// A page around a message (it stays loaded).
    public func jumpTo(_ seq: Int64) async throws {
        guard let client else { throw AgentIMError(code: "closed", message: "the client is closed") }
        _ = try await client.command(["cmd": "jump_to", "conv_id": convId, "seq": seq])
    }
}

@Observable
public final class AgentIMClient: @unchecked Sendable {
    public private(set) var connection = Connection(state: "closed", reason: nil, retryAt: nil)
    public private(set) var conversations: [Conversation] = []

    @ObservationIgnored private let o: ClientOptions
    @ObservationIgnored private let queue: DispatchQueue
    @ObservationIgnored private let lock = NSLock()
    @ObservationIgnored private var ptr: OpaquePointer?
    @ObservationIgnored private var box: Unmanaged<Box>?
    @ObservationIgnored private var pending: [UInt64: CheckedContinuation<Any?, Error>] = [:]
    @ObservationIgnored private var inOrder: [UInt64: (Any?) -> Void] = [:]
    @ObservationIgnored private var next: UInt64 = 0
    @ObservationIgnored private var generation: UInt64 = 0
    @ObservationIgnored private var views: [String: Messages] = [:]
    @ObservationIgnored private var convs: [String: JSONObject] = [:]
    @ObservationIgnored private var listeners: [String: [UUID: (JSONObject) -> Void]] = [:]

    public static var version: String { String(cString: agentim_version()) }

    /// `queue`: where state changes, events and replies are handled, in order (main by default).
    public init(_ o: ClientOptions, queue: DispatchQueue = .main) throws {
        self.o = o
        self.queue = queue
        let cfg: JSONObject = [
            "url": o.url, "device_id": o.deviceId, "device_name": o.deviceName ?? NSNull(),
            "platform": o.platform ?? ProcessInfo.processInfo.operatingSystemVersionString,
            "client_name": o.clientName, "client_version": o.clientVersion,
        ]
        let json = String(data: try JSONSerialization.data(withJSONObject: cfg), encoding: .utf8)!
        let b = Unmanaged.passRetained(Box(self))
        lock.lock()
        defer { lock.unlock() }
        let p = json.withCString { c in
            o.storeDir.map { d in d.withCString { agentim_client_new(c, $0, onNativeEvent, b.toOpaque()) } }
                ?? agentim_client_new(c, nil, onNativeEvent, b.toOpaque())
        }
        guard let p else {
            b.release()
            throw AgentIMError(code: "bad_request", message: "the client could not start (check url and device_id)")
        }
        ptr = p
        box = b
    }

    deinit { close() }

    /// Called on the SDK's thread.
    fileprivate func onEvent(_ e: JSONObject) {
        if e["type"] as? String == "need_token" {
            Task { await self.provideToken() }
            return
        }
        queue.async { self.handle(e) }
    }

    private func provideToken() async {
        var token: String?
        var refused: Int32 = 0
        do {
            token = try await o.tokenProvider()
        } catch is TokenRefused {
            refused = 1
        } catch {
            // Failed: the core tries again later.
        }
        lock.withLock {
            guard let ptr else { return }
            if let token { token.withCString { agentim_client_token(ptr, $0, 0) } } else { agentim_client_token(ptr, nil, refused) }
        }
    }

    private func handle(_ e: JSONObject) {
        let type = e["type"] as? String ?? ""
        switch type {
        case "reply":
            let id = (e["id"] as? NSNumber)?.uint64Value ?? 0
            let ok = e["ok"] as? Bool ?? false
            let result = e["result"].flatMap { $0 is NSNull ? nil : $0 }
            lock.lock()
            let apply = inOrder.removeValue(forKey: id)
            let c = pending.removeValue(forKey: id)
            lock.unlock()
            if ok { apply?(result) }
            if ok { c?.resume(returning: result) } else { c?.resume(throwing: AgentIMError(e["error"] as? JSONObject)) }
            return
        case "connection":
            connection = Connection(state: e["state"] as? String ?? "closed", reason: e["reason"] as? String,
                                    retryAt: (e["retry_at"] as? NSNumber)?.int64Value)
        case "conversations":
            if e["reset"] as? Bool == true { convs = [:] }
            for case let c as JSONObject in e["changes"] as? [Any] ?? [] {
                if let conv = c["conversation"] as? JSONObject, let id = conv["conv_id"] as? String { convs[id] = conv }
            }
            conversations = convs.values
                .filter { $0["left"] as? Bool != true && $0["dissolved"] as? Bool != true }
                .sorted { last($0) > last($1) }
                .map(Conversation.init)
        case "messages":
            views[e["conv_id"] as? String ?? ""]?.apply(e["changes"] as? [Any], reset: e["reset"] as? Bool == true)
        default:
            break
        }
        for f in listeners[type]?.values.map({ $0 }) ?? [] { f(e) }
    }

    private func last(_ c: JSONObject) -> Int64 {
        ((c["last_message"] as? JSONObject)?["created_at"] as? NSNumber)?.int64Value ?? 0
    }

    /// Any core command (`["cmd": …]`); returns its result.
    @discardableResult
    public func command(_ cmd: JSONObject) async throws -> Any? {
        try await command(cmd, applyInOrder: nil)
    }

    private func command(_ cmd: JSONObject, applyInOrder: ((Any?) -> Void)?) async throws -> Any? {
        guard JSONSerialization.isValidJSONObject(cmd), let data = try? JSONSerialization.data(withJSONObject: cmd),
              let json = String(data: data, encoding: .utf8)
        else { throw AgentIMError(code: "bad_request", message: "not a command") }
        let id = lock.withLock {
            next += 1
            return next
        }
        let timeout = DispatchWorkItem { [weak self] in self?.fail(id, AgentIMError(code: "timeout", message: "no answer from the client core", retryable: true)) }
        defer { timeout.cancel() }
        return try await withCheckedThrowingContinuation { (c: CheckedContinuation<Any?, Error>) in
            lock.lock()
            guard let ptr else {
                lock.unlock()
                c.resume(throwing: AgentIMError(code: "closed", message: "the client is closed"))
                return
            }
            pending[id] = c
            if let applyInOrder { inOrder[id] = applyInOrder }
            let r = json.withCString { agentim_client_command(ptr, id, $0) }
            lock.unlock()
            if r != 0 { fail(id, AgentIMError(code: "bad_request", message: "not a command")) }
            DispatchQueue.global().asyncAfter(deadline: .now() + o.commandTimeout, execute: timeout)
        }
    }

    /// Hands a command to the core now, without waiting for its answer;
    /// `applyInOrder` gets the result in event order.
    private func issue(_ cmd: JSONObject, applyInOrder: @escaping (Any?) -> Void) {
        guard let data = try? JSONSerialization.data(withJSONObject: cmd), let json = String(data: data, encoding: .utf8) else { return }
        lock.withLock {
            guard let ptr else { return }
            next += 1
            inOrder[next] = applyInOrder
            let id = next
            if json.withCString({ agentim_client_command(ptr, id, $0) }) != 0 { inOrder[id] = nil }
        }
    }

    private func fail(_ id: UInt64, _ e: Error) {
        lock.lock()
        let c = pending.removeValue(forKey: id)
        inOrder[id] = nil
        lock.unlock()
        c?.resume(throwing: e)
    }

    /// Events: operation, side, signal, reset, storage, event, read, mute (and the ones above).
    /// Handled on the client's queue; call `cancel()` on the result to stop.
    public func on(_ type: String, _ fn: @escaping (JSONObject) -> Void) -> Subscription {
        let id = UUID()
        queue.async { self.listeners[type, default: [:]][id] = fn }
        let q = queue
        return Subscription { [weak self] in q.async { [weak self] in self?.listeners[type]?[id] = nil } }
    }

    /// The platform's network: call on every change (NWPathMonitor).
    public func setNetwork(online: Bool) {
        lock.lock()
        defer { lock.unlock() }
        generation += 1
        if let ptr { agentim_client_network(ptr, online ? 1 : 0, generation) }
    }

    /// The app came to the foreground or went to the background (scenePhase).
    public func setForeground(_ foreground: Bool) {
        lock.lock()
        defer { lock.unlock() }
        if let ptr { agentim_client_foreground(ptr, foreground ? 1 : 0) }
    }

    /// The conversation's messages, kept up to date. Call on the client's queue (main).
    public func messages(_ convId: String) -> Messages {
        if let v = views[convId] { return v }
        let v = Messages(self, convId)
        views[convId] = v
        // Issued now, before any command that follows: the snapshot comes first.
        issue(["cmd": "messages", "conv_id": convId]) { r in
            let o = r as? JSONObject
            v.reset(o?["messages"] as? [Any], hasOlder: o?["has_older"] as? Bool, olderEnd: o?["older_end"] as? String)
        }
        return v
    }

    public func connect() async throws { try await command(["cmd": "connect"]) }
    /// Closes the connection (the client stays usable; `close()` frees it).
    public func disconnect() async throws { try await command(["cmd": "close"]) }
    /// Logs out here: closes and wipes the cache and the queue (GDPR).
    public func logout() async throws { try await command(["cmd": "logout"]) }

    /// Returns `client_msg_id` and `key` once the message is queued and shown.
    @discardableResult
    public func send(_ convId: String, _ text: String, clientMsgId: String? = nil, replyTo: String? = nil) async throws -> JSONObject {
        var c: JSONObject = ["cmd": "send", "conv_id": convId, "text": text]
        c["client_msg_id"] = clientMsgId
        c["reply_to"] = replyTo
        return try await command(c) as? JSONObject ?? [:]
    }

    /// Sends a file or a photo from a path: shown at once as uploading,
    /// sent when it is up. A photo loses its EXIF and GPS (unless
    /// `keepMetadata`) and gets a 320-pixel thumbnail (docs/02 §2.7).
    @discardableResult
    public func sendFile(_ convId: String, path: String, name: String? = nil, mime: String? = nil, caption: String? = nil, replyTo: String? = nil, keepMetadata: Bool = false) async throws -> JSONObject {
        var c: JSONObject = ["cmd": "send_file", "conv_id": convId, "path": path, "keep_metadata": keepMetadata]
        c["name"] = name
        c["mime"] = mime
        c["caption"] = caption
        c["reply_to"] = replyTo
        return try await command(c) as? JSONObject ?? [:]
    }

    /// A link (valid a few minutes) to read a file: `original` or `thumb`.
    public func fileUrl(_ convId: String, fileId: String, variant: String = "original") async throws -> JSONObject {
        try await command(["cmd": "file_url", "conv_id": convId, "file_id": fileId, "variant": variant]) as? JSONObject ?? [:]
    }

    public func edit(_ convId: String, msgId: String, _ text: String) async throws {
        try await command(["cmd": "edit", "conv_id": convId, "msg_id": msgId, "text": text])
    }

    public func recall(_ convId: String, msgId: String) async throws {
        try await command(["cmd": "recall", "conv_id": convId, "msg_id": msgId])
    }

    public func read(_ convId: String, seq: Int64? = nil) async throws {
        var c: JSONObject = ["cmd": "read", "conv_id": convId]
        c["seq"] = seq
        try await command(c)
    }

    public func retry(_ convId: String, clientMsgId: String) async throws {
        try await command(["cmd": "retry", "conv_id": convId, "client_msg_id": clientMsgId])
    }

    public func discard(_ convId: String, clientMsgId: String) async throws {
        try await command(["cmd": "discard", "conv_id": convId, "client_msg_id": clientMsgId])
    }

    /// A request that needs the server now (approvals, decisions, queries).
    @discardableResult
    public func call(_ op: String, _ args: JSONObject = [:]) async throws -> Any? {
        try await command(["cmd": "call", "op": op, "args": args])
    }

    public func debugState() async throws -> Any? { try await command(["cmd": "debug_state"]) }

    /// Stops the client; no event arrives after it returns. Waiting calls fail with `closed`.
    /// Not from the SDK's own thread.
    public func close() {
        lock.lock()
        let p = ptr
        ptr = nil
        let waiting = pending
        pending = [:]
        inOrder = [:]
        lock.unlock()
        guard let p else { return }
        agentim_client_free(p)
        box?.release()
        box = nil
        for c in waiting.values { c.resume(throwing: AgentIMError(code: "closed", message: "the client is closed")) }
    }
}

public final class Subscription {
    private var f: (() -> Void)?
    init(_ f: @escaping () -> Void) { self.f = f }
    public func cancel() {
        f?()
        f = nil
    }
}

/// Holds the client weakly for the native side.
private final class Box {
    weak var client: AgentIMClient?
    init(_ c: AgentIMClient) { client = c }
}

private func onNativeEvent(_ ctx: UnsafeMutableRawPointer?, _ json: UnsafePointer<CChar>?) {
    guard let ctx, let json else { return }
    let box = Unmanaged<Box>.fromOpaque(ctx).takeUnretainedValue()
    guard let c = box.client,
          let e = try? JSONSerialization.jsonObject(with: Data(bytes: json, count: strlen(json))) as? JSONObject
    else { return }
    c.onEvent(e)
}
