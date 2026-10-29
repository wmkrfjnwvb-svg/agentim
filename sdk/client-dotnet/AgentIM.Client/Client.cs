// AgentIM client for .NET and Unity (docs/12 §2). The rules live in the
// Rust core; this turns its events into state the UI follows and its
// replies into tasks.
//
//   var client = AgentIMClient.Create(new ClientOptions {
//       Url = "https://chat.example.com",
//       DeviceId = savedDeviceId,
//       StoreDir = Path.Combine(Application.persistentDataPath, "agentim"),
//       TokenProvider = ct => myBackend.GetTokenAsync(ct),
//       // Unity: events on the main thread.
//       Context = SynchronizationContext.Current,
//   });
//   client.Conversations.Subscribe(list => Render(list));
//   await client.ConnectAsync();
//   await client.SendAsync(convId, "hello");   // shown at once, sent when it can be

using System;
using System.Collections.Concurrent;
using System.Collections.Generic;
using System.Linq;
using System.Runtime.InteropServices;
using System.Text.Json.Nodes;
using System.Threading;
using System.Threading.Tasks;

[assembly: System.Runtime.CompilerServices.InternalsVisibleTo("Conformance")]

namespace AgentIM.Client
{
    /// <summary>How the client starts: the server address, this device's id, the cache, the token provider and how it names itself.</summary>
    public sealed class ClientOptions
    {
        /// <summary><c>https://chat.example.com</c>.</summary>
        public string Url { get; set; } = "";
        /// <summary>This device's id, the same every run (1..64 of A-Z a-z 0-9 _ -).</summary>
        public string DeviceId { get; set; } = "";
        /// <summary>A name for this device in the user's device list, like "Maya's iPhone".</summary>
        public string? DeviceName { get; set; }
        /// <summary>The platform reported to the server (<c>ios</c>, <c>android</c>, <c>windows</c>…), for the device list and push.</summary>
        public string? Platform { get; set; }
        /// <summary>Where the cache lives; null keeps it in memory.</summary>
        public string? StoreDir { get; set; }
        /// <summary>A client token from your backend; throw <see cref="TokenRefusedException"/> to stop.</summary>
        public Func<CancellationToken, Task<string>> TokenProvider { get; set; } = _ => Task.FromResult("");
        /// <summary>Where state changes and events are delivered (Unity: the main thread). Null: the SDK's thread.</summary>
        public SynchronizationContext? Context { get; set; }
        /// <summary>This app's name, sent to the server with each connection (for diagnostics and minimum-version rules).</summary>
        public string ClientName { get; set; } = "agentim-dotnet";
        /// <summary>This app's version, sent with <c>ClientName</c>.</summary>
        public string ClientVersion { get; set; } = "0.1.0";
        /// <summary>How long a command waits for the core.</summary>
        public TimeSpan CommandTimeout { get; set; } = TimeSpan.FromSeconds(60);
    }

    /// <summary>Thrown by a token provider when the backend refuses the user.</summary>
    public sealed class TokenRefusedException : Exception
    {
        public TokenRefusedException(string message) : base(message) { }
    }

    /// <summary>
    /// The server's code (<c>forbidden</c>, <c>not_found</c>, <c>conflict</c>, <c>bad_request</c>,
    /// <c>blocked</c>, <c>rate_limited</c>…) or the client's (<c>disconnected</c>, <c>timeout</c>,
    /// <c>closed</c>…); unknown codes pass through as they are.
    /// </summary>
    public sealed class AgentIMException : Exception
    {
        /// <summary>The error code: the server's (<c>forbidden</c>, <c>not_found</c>, <c>conflict</c>, <c>bad_request</c>, …) or the client's (<c>disconnected</c>, <c>timeout</c>, <c>closed</c>, <c>unauthenticated</c>).</summary>
        public string Code { get; }
        /// <summary>Whether the same request may work later.</summary>
        public bool Retryable { get; }
        /// <summary>How long the server asked to wait before trying again.</summary>
        public long? RetryAfterMs { get; }

        public AgentIMException(string code, string message, bool retryable = false, long? retryAfterMs = null) : base(message)
        {
            Code = code;
            Retryable = retryable;
            RetryAfterMs = retryAfterMs;
        }

        internal static AgentIMException From(JsonNode? e) => new AgentIMException(
            (string?)e?["code"] ?? "internal",
            (string?)e?["message"] ?? (string?)e?["code"] ?? "error",
            (bool?)e?["retryable"] ?? false,
            (long?)e?["retry_after_ms"]);
    }

    /// <summary>A value the UI follows: <see cref="Value"/> stays the same object until it changes.</summary>
    public sealed class Observable<T>
    {
        private readonly object gate = new object();
        private readonly List<Action<T>> fns = new List<Action<T>>();
        private T value;

        internal Observable(T v) => value = v;

        public T Value
        {
            get { lock (gate) return value; }
        }

        /// <summary>Called with the current value now, then with every change; dispose to stop.</summary>
        public IDisposable Subscribe(Action<T> fn)
        {
            T now;
            lock (gate)
            {
                fns.Add(fn);
                now = value;
            }
            Safe(fn, now);
            return new Unsubscribe(() => { lock (gate) fns.Remove(fn); });
        }

        internal void Set(T v)
        {
            Action<T>[] list;
            lock (gate)
            {
                value = v;
                list = fns.ToArray();
            }
            foreach (var f in list) Safe(f, v);
        }

        private static void Safe(Action<T> f, T v)
        {
            try { f(v); }
            catch { /* A listener's error is the listener's. */ }
        }
    }

    internal sealed class Unsubscribe : IDisposable
    {
        private Action? f;
        public Unsubscribe(Action f) => this.f = f;
        /// <summary>Stops the subscription.</summary>
        public void Dispose() => Interlocked.Exchange(ref f, null)?.Invoke();
    }

    /// <summary>The connection state and why: <c>state</c> is <c>closed</c>, <c>connecting</c>, <c>connected</c>, <c>disconnected</c>, <c>suspended</c> or <c>failed</c>.</summary>
    public sealed class Connection
    {
        /// <summary>closed, connecting, connected, disconnected, suspended or failed.</summary>
        public string State { get; }
        /// <summary>Why it is not connected (an error code), when it is not.</summary>
        public string? Reason { get; }
        /// <summary>When the next reconnect attempt is due (Unix ms), while waiting to reconnect.</summary>
        public long? RetryAt { get; }

        public Connection(string state, string? reason = null, long? retryAt = null)
        {
            State = state;
            Reason = reason;
            RetryAt = retryAt;
        }
    }

    /// <summary>One conversation's messages as shown, oldest first; each has <c>key</c>, <c>content</c>, <c>status</c> (sent, sending, failed)…</summary>
    public sealed class Messages
    {
        private readonly AgentIMClient client;
        private readonly object gate = new object();
        private Dictionary<string, JsonObject> byKey = new Dictionary<string, JsonObject>();

        /// <summary>The conversation these messages belong to.</summary>
        public string ConvId { get; }
        /// <summary>The messages as shown, oldest first, kept up to date.</summary>
        public Observable<IReadOnlyList<JsonObject>> List { get; } = new Observable<IReadOnlyList<JsonObject>>(Array.Empty<JsonObject>());
        /// <summary>Whether older messages can still be loaded.</summary>
        public bool HasOlder { get; private set; }
        /// <summary>Why nothing older can be loaded: <c>start</c> (the beginning) or <c>retention</c> (deleted by the retention period).</summary>
        public string? OlderEnd { get; private set; }

        internal Messages(AgentIMClient client, string convId)
        {
            this.client = client;
            ConvId = convId;
        }

        internal void Reset(JsonArray? list, bool? hasOlder, string? olderEnd)
        {
            lock (gate)
            {
                byKey = new Dictionary<string, JsonObject>();
                foreach (var m in list ?? new JsonArray())
                    if (m is JsonObject o) byKey[(string)o["key"]!] = o;
                if (hasOlder != null) HasOlder = hasOlder.Value;
                if (olderEnd != null) OlderEnd = olderEnd;
            }
            Publish();
        }

        /// <summary>A <c>messages</c> event: with <paramref name="reset"/> the view is replaced
        /// by the changes in one update, so no one sees it empty in between.</summary>
        internal void Apply(JsonArray? changes, bool reset = false)
        {
            lock (gate)
            {
                if (reset) byKey = new Dictionary<string, JsonObject>();
                foreach (var c in changes ?? new JsonArray())
                {
                    if ((string?)c?["op"] == "upsert" && c?["message"] is JsonObject m) byKey[(string)m["key"]!] = m;
                    else if (c?["key"] is JsonNode k) byKey.Remove((string)k!);
                }
            }
            Publish();
        }

        private void Publish()
        {
            JsonObject[] list;
            lock (gate) list = byKey.Values.OrderBy(m => (double?)m["order"] ?? 0).ToArray();
            List.Set(list);
        }

        /// <summary>The page before the oldest shown: the cache first, then the server. False at the start.</summary>
        public async Task<bool> LoadOlderAsync()
        {
            var r = await client.CommandAsync(new JsonObject { ["cmd"] = "load_older", ["conv_id"] = ConvId }).ConfigureAwait(false);
            HasOlder = (bool?)r?["has_older"] ?? false;
            OlderEnd = (string?)r?["older_end"];
            return HasOlder;
        }

        /// <summary>A page around a message (it stays loaded).</summary>
        public Task<JsonNode?> JumpToAsync(long seq) =>
            client.CommandAsync(new JsonObject { ["cmd"] = "jump_to", ["conv_id"] = ConvId, ["seq"] = seq });
    }

    /// <summary>The AgentIM client for one account on one device: keeps the connection, syncs, sends the offline queue in order, keeps the local cache, and turns everything into observable state.</summary>
    public sealed class AgentIMClient : IDisposable
    {
        // Kept alive for the process: native code holds a pointer to it.
        private static readonly Native.EventCallback Callback = OnNativeEvent;

        private readonly ClientOptions o;
        private readonly TaskCompletionSource<bool> started = new TaskCompletionSource<bool>(TaskCreationOptions.RunContinuationsAsynchronously);
        private readonly ConcurrentDictionary<ulong, TaskCompletionSource<JsonNode?>> pending = new ConcurrentDictionary<ulong, TaskCompletionSource<JsonNode?>>();
        // Applied in event order, before later events (a view's first snapshot).
        private readonly ConcurrentDictionary<ulong, Action<JsonNode?>> inOrder = new ConcurrentDictionary<ulong, Action<JsonNode?>>();
        private readonly ConcurrentDictionary<string, Messages> views = new ConcurrentDictionary<string, Messages>();
        private readonly Dictionary<string, JsonObject> convs = new Dictionary<string, JsonObject>();
        private readonly ConcurrentDictionary<string, List<Action<JsonObject>>> listeners = new ConcurrentDictionary<string, List<Action<JsonObject>>>();
        private readonly CancellationTokenSource stopping = new CancellationTokenSource();
        private readonly object nativeGate = new object();
        private IntPtr ptr;
        private GCHandle self;
        private long next;
        private long generation;

        /// <summary>The connection state (<c>closed</c>, <c>connecting</c>, <c>connected</c>, <c>disconnected</c>, <c>suspended</c>, <c>failed</c>) and why; subscribe to follow it.</summary>
        public Observable<Connection> Connection { get; } = new Observable<Connection>(new Connection("closed"));
        /// <summary>The conversation list, newest first, kept up to date.</summary>
        public Observable<IReadOnlyList<JsonObject>> Conversations { get; } = new Observable<IReadOnlyList<JsonObject>>(Array.Empty<JsonObject>());

        /// <summary>The version of the native core.</summary>
        public static string Version => Native.FromUtf8(Native.agentim_version());

        private AgentIMClient(ClientOptions o) => this.o = o;

        // A short name like the other SDKs send (OSDescription on macOS is a
        // whole kernel version line).
        static string OsName() =>
            RuntimeInformation.IsOSPlatform(OSPlatform.Windows) ? "windows"
            : RuntimeInformation.IsOSPlatform(OSPlatform.OSX) ? "macos"
            : RuntimeInformation.IsOSPlatform(OSPlatform.Linux) ? "linux"
            : RuntimeInformation.OSDescription;

        /// <summary>Starts a client with its options: the server address, this device's id, the token provider and the cache. Nothing connects until <c>ConnectAsync</c>.</summary>
        public static AgentIMClient Create(ClientOptions o)
        {
            var c = new AgentIMClient(o);
            var cfg = new JsonObject
            {
                ["url"] = o.Url,
                ["device_id"] = o.DeviceId,
                ["device_name"] = o.DeviceName,
                ["platform"] = o.Platform ?? OsName(),
                ["client_name"] = o.ClientName,
                ["client_version"] = o.ClientVersion,
            };
            c.self = GCHandle.Alloc(c);
            var p = Native.agentim_client_new(Native.Utf8(cfg.ToJsonString()), o.StoreDir == null ? null : Native.Utf8(o.StoreDir), Callback, GCHandle.ToIntPtr(c.self));
            if (p == IntPtr.Zero)
            {
                c.self.Free();
                throw new AgentIMException("bad_request", "the client could not start (check url and device_id)");
            }
            c.ptr = p;
            c.started.SetResult(true);
            return c;
        }

#if UNITY_5_3_OR_NEWER
        [AOT.MonoPInvokeCallback(typeof(Native.EventCallback))]
#endif
        private static void OnNativeEvent(IntPtr ctx, IntPtr json)
        {
            try
            {
                if (GCHandle.FromIntPtr(ctx).Target is AgentIMClient c && JsonNode.Parse(Native.FromUtf8(json)) is JsonObject e)
                    c.OnEvent(e);
            }
            catch
            {
                // Nothing unwinds into native code.
            }
        }

        private void OnEvent(JsonObject e)
        {
            switch ((string?)e["type"])
            {
                case "need_token":
                    _ = ProvideToken();
                    return;
                case "reply":
                    // Answers go straight to the waiting task (it resumes on its own context).
                    var rid = (ulong?)e["id"] ?? 0;
                    if (inOrder.TryRemove(rid, out var apply) && (bool?)e["ok"] == true)
                    {
                        var result = e["result"];
                        if (o.Context != null) o.Context.Post(_ => apply(result), null);
                        else apply(result);
                    }
                    if (pending.TryRemove(rid, out var t))
                    {
                        if ((bool?)e["ok"] == true) t.TrySetResult(e["result"]?.DeepClone());
                        else t.TrySetException(AgentIMException.From(e["error"]));
                    }
                    return;
            }
            if (o.Context != null) o.Context.Post(_ => Dispatch(e), null);
            else Dispatch(e);
        }

        private async Task ProvideToken()
        {
            await started.Task.ConfigureAwait(false);
            string? token = null;
            var refused = 0;
            try
            {
                token = await o.TokenProvider(stopping.Token).ConfigureAwait(false);
            }
            catch (TokenRefusedException)
            {
                refused = 1;
            }
            catch
            {
                // Failed: the core tries again later.
            }
            lock (nativeGate)
            {
                if (ptr != IntPtr.Zero) Native.agentim_client_token(ptr, token == null ? null : Native.Utf8(token), refused);
            }
        }

        private void Dispatch(JsonObject e)
        {
            switch ((string?)e["type"])
            {
                case "connection":
                    Connection.Set(new Connection((string?)e["state"] ?? "closed", (string?)e["reason"], (long?)e["retry_at"]));
                    break;
                case "conversations":
                    lock (convs)
                    {
                        if ((bool?)e["reset"] == true) convs.Clear();
                        foreach (var c in e["changes"]?.AsArray() ?? new JsonArray())
                            if (c?["conversation"] is JsonObject conv) convs[(string)conv["conv_id"]!] = conv;
                    }
                    PublishConversations();
                    break;
                case "messages":
                    if (views.TryGetValue((string?)e["conv_id"] ?? "", out var v))
                    {
                        v.Apply(e["changes"]?.AsArray(), (bool?)e["reset"] == true);
                    }
                    break;
            }
            if (listeners.TryGetValue((string?)e["type"] ?? "", out var fns))
            {
                Action<JsonObject>[] list;
                lock (fns) list = fns.ToArray();
                foreach (var f in list)
                {
                    try { f(e); }
                    catch { /* The listener's own error. */ }
                }
            }
        }

        private void PublishConversations()
        {
            JsonObject[] list;
            lock (convs)
            {
                list = convs.Values
                    .Where(c => (bool?)c["left"] != true && (bool?)c["dissolved"] != true)
                    .OrderByDescending(c => (long?)c["last_message"]?["created_at"] ?? 0)
                    .ToArray();
            }
            Conversations.Set(list);
        }

        /// <summary>Any core command (<c>{"cmd": …}</c>); completes with its result.</summary>
        public Task<JsonNode?> CommandAsync(JsonObject cmd) => CommandAsync(cmd, null);

        private async Task<JsonNode?> CommandAsync(JsonObject cmd, Action<JsonNode?>? applyInOrder)
        {
            var id = (ulong)Interlocked.Increment(ref next);
            var t = new TaskCompletionSource<JsonNode?>(TaskCreationOptions.RunContinuationsAsynchronously);
            pending[id] = t;
            if (applyInOrder != null) inOrder[id] = applyInOrder;
            int r;
            lock (nativeGate)
            {
                if (ptr == IntPtr.Zero)
                {
                    pending.TryRemove(id, out _);
                    inOrder.TryRemove(id, out _);
                    throw new AgentIMException("closed", "the client is disposed");
                }
                r = Native.agentim_client_command(ptr, id, Native.Utf8(cmd.ToJsonString()));
            }
            if (r != 0)
            {
                pending.TryRemove(id, out _);
                inOrder.TryRemove(id, out _);
                throw new AgentIMException("bad_request", "not a command");
            }
            var done = await Task.WhenAny(t.Task, Task.Delay(o.CommandTimeout)).ConfigureAwait(false);
            if (done != t.Task)
            {
                pending.TryRemove(id, out _);
                inOrder.TryRemove(id, out _);
                throw new AgentIMException("timeout", "no answer from the client core", true);
            }
            return await t.Task.ConfigureAwait(false);
        }

        /// <summary>Events: operation, side, signal, reset, storage, event, read, mute (and the ones above).</summary>
        public IDisposable On(string type, Action<JsonObject> fn)
        {
            var fns = listeners.GetOrAdd(type, _ => new List<Action<JsonObject>>());
            lock (fns) fns.Add(fn);
            return new Unsubscribe(() => { lock (fns) fns.Remove(fn); });
        }

        /// <summary>The platform's network: call on every change (Unity: Application.internetReachability).</summary>
        public void SetNetwork(bool online)
        {
            lock (nativeGate)
            {
                if (ptr != IntPtr.Zero) Native.agentim_client_network(ptr, online ? 1 : 0, (ulong)Interlocked.Increment(ref generation));
            }
        }

        /// <summary>The app came to the foreground or went to the background (Unity: OnApplicationPause).</summary>
        public void SetForeground(bool foreground)
        {
            lock (nativeGate)
            {
                if (ptr != IntPtr.Zero) Native.agentim_client_foreground(ptr, foreground ? 1 : 0);
            }
        }

        /// <summary>The conversation's messages, kept up to date.</summary>
        public Messages Messages(string convId)
        {
            var created = false;
            var v = views.GetOrAdd(convId, id =>
            {
                created = true;
                return new Messages(this, id);
            });
            if (created)
            {
                _ = CommandAsync(
                    new JsonObject { ["cmd"] = "messages", ["conv_id"] = convId },
                    r => v.Reset(r?["messages"]?.AsArray(), (bool?)r?["has_older"], (string?)r?["older_end"]));
            }
            return v;
        }

        /// <summary>Connects and keeps connecting: reconnects with backoff when the network or the server goes, syncs what it missed, sends the offline queue.</summary>
        public Task<JsonNode?> ConnectAsync() => Cmd("connect");
        /// <summary>Disconnects; the queue and the cache stay. <c>ConnectAsync</c> again picks up where it stopped.</summary>
        public Task<JsonNode?> CloseAsync() => Cmd("close");
        /// <summary>Logs out here: closes and wipes the cache and the queue (GDPR).</summary>
        public Task<JsonNode?> LogoutAsync() => Cmd("logout");

        /// <summary>Returns <c>{client_msg_id, key}</c> once the message is queued and shown.</summary>
        public Task<JsonNode?> SendAsync(string convId, string text, string? clientMsgId = null, string? replyTo = null) =>
            Cmd("send", new JsonObject { ["conv_id"] = convId, ["text"] = text, ["client_msg_id"] = clientMsgId, ["reply_to"] = replyTo });

        /// <summary>Sends a file or a photo from a path: shown at once as uploading, sent when it
        /// is up. A photo loses its EXIF and GPS (unless <paramref name="keepMetadata"/>) and gets a
        /// 320-pixel thumbnail (docs/02 §2.7).</summary>
        public Task<JsonNode?> SendFileAsync(string convId, string path, string? name = null, string? mime = null,
            string? caption = null, string? replyTo = null, bool keepMetadata = false) =>
            Cmd("send_file", new JsonObject
            {
                ["conv_id"] = convId, ["path"] = path, ["name"] = name, ["mime"] = mime,
                ["caption"] = caption, ["reply_to"] = replyTo, ["keep_metadata"] = keepMetadata,
            });

        /// <summary>A link (valid a few minutes) to read a file: <c>original</c> or <c>thumb</c>.</summary>
        public Task<JsonNode?> FileUrlAsync(string convId, string fileId, string variant = "original") =>
            Cmd("file_url", new JsonObject { ["conv_id"] = convId, ["file_id"] = fileId, ["variant"] = variant });

        /// <summary>Edits one of my messages (by <c>msgId</c>, or by <c>clientMsgId</c> while it is still queued); the change shows at once.</summary>
        public Task<JsonNode?> EditAsync(string convId, string msgId, string text) =>
            Cmd("edit", new JsonObject { ["conv_id"] = convId, ["msg_id"] = msgId, ["text"] = text });

        /// <summary>Recalls one of my messages for everyone; a message still in the queue is simply not sent.</summary>
        public Task<JsonNode?> RecallAsync(string convId, string msgId) =>
            Cmd("recall", new JsonObject { ["conv_id"] = convId, ["msg_id"] = msgId });

        /// <summary>Marks the conversation read up to <c>seq</c> (default: the newest) on all my devices.</summary>
        public Task<JsonNode?> ReadAsync(string convId, long? seq = null) =>
            Cmd("read", new JsonObject { ["conv_id"] = convId, ["seq"] = seq });

        /// <summary>Sends a failed message again (the same <c>client_msg_id</c>, so it is never doubled).</summary>
        public Task<JsonNode?> RetryAsync(string convId, string clientMsgId) =>
            Cmd("retry", new JsonObject { ["conv_id"] = convId, ["client_msg_id"] = clientMsgId });

        /// <summary>Drops a failed or waiting message from the queue.</summary>
        public Task<JsonNode?> DiscardAsync(string convId, string clientMsgId) =>
            Cmd("discard", new JsonObject { ["conv_id"] = convId, ["client_msg_id"] = clientMsgId });

        /// <summary>A request that needs the server now (approvals, decisions, queries).</summary>
        public Task<JsonNode?> CallAsync(string op, JsonObject? args = null) =>
            Cmd("call", new JsonObject { ["op"] = op, ["args"] = args ?? new JsonObject() });

        /// <summary>Everything that helps report a problem: the connection, positions, the queue and recent failures.</summary>
        public Task<JsonNode?> DebugStateAsync() => Cmd("debug_state");

        private Task<JsonNode?> Cmd(string name, JsonObject? fields = null)
        {
            var c = new JsonObject { ["cmd"] = name };
            if (fields != null)
                foreach (var kv in fields.ToArray())
                    if (kv.Value != null)
                    {
                        fields.Remove(kv.Key);
                        c[kv.Key] = kv.Value;
                    }
            return CommandAsync(c);
        }

        /// <summary>Stops the client; no event arrives after it returns. Waiting commands fail with <c>closed</c>.</summary>
        public void Dispose()
        {
            IntPtr p;
            lock (nativeGate)
            {
                p = ptr;
                ptr = IntPtr.Zero;
            }
            if (p == IntPtr.Zero) return;
            stopping.Cancel();
            Native.agentim_client_free(p);
            self.Free();
            foreach (var id in pending.Keys.ToArray())
                if (pending.TryRemove(id, out var t)) t.TrySetException(new AgentIMException("closed", "the client is disposed"));
        }
    }
}
