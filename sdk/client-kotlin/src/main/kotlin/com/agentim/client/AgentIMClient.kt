// AgentIM client for Android and the JVM (docs/12 §2). The rules live in the
// Rust core; this turns its events into StateFlows and its replies into
// suspend functions.
//
//   val client = AgentIMClient.create(ClientOptions(
//       url = "https://chat.example.com",
//       deviceId = savedDeviceId,
//       storeDir = File(context.filesDir, "agentim").path,
//       tokenProvider = { backend.token() },
//   ))
//   lifecycleScope.launch { client.conversations.collect { render(it) } }
//   client.connect()
//   client.send(convId, "hello")   // in messages(convId).list when it returns

package com.agentim.client

import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.atomic.AtomicLong
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import kotlinx.coroutines.withTimeoutOrNull
import org.json.JSONArray
import org.json.JSONObject

class ClientOptions(
    /** `https://chat.example.com`. */
    val url: String,
    /** This device's id, the same every run (1..64 of A-Z a-z 0-9 _ -). */
    val deviceId: String,
    /** A client token from your backend; throw [TokenRefused] to stop. */
    val tokenProvider: suspend () -> String,
    /** Where the cache lives; null keeps it in memory. */
    val storeDir: String? = null,
    val deviceName: String? = null,
    val platform: String? = null,
    val clientName: String = "agentim-kotlin",
    val clientVersion: String = "0.1.0",
    val commandTimeoutMs: Long = 60_000,
)

/** Thrown by a token provider when the backend refuses the user. */
class TokenRefused(message: String) : Exception(message)

/**
 * The server's code (`forbidden`, `not_found`, `conflict`, `bad_request`, `blocked`,
 * `rate_limited`…) or the client's (`disconnected`, `timeout`, `closed`…); unknown codes
 * pass through as they are.
 */
class AgentIMException(
    val code: String,
    message: String,
    val retryable: Boolean = false,
    val retryAfterMs: Long? = null,
) : Exception(message) {
    internal companion object {
        fun from(e: JSONObject?) = AgentIMException(
            e?.optString("code", "internal") ?: "internal",
            e?.optString("message", e.optString("code", "error")) ?: "error",
            e?.optBoolean("retryable") ?: false,
            if (e?.has("retry_after_ms") == true && !e.isNull("retry_after_ms")) e.getLong("retry_after_ms") else null,
        )
    }
}

data class Connection(
    /** closed, connecting, connected, disconnected, suspended or failed. */
    val state: String,
    val reason: String? = null,
    val retryAt: Long? = null,
)

/** One conversation's messages as shown, oldest first; each has `key`, `content`, `status` (sent, sending, failed)… */
class Messages internal constructor(private val client: AgentIMClient, val convId: String) {
    private val byKey = LinkedHashMap<String, JSONObject>()
    private val flow = MutableStateFlow<List<JSONObject>>(emptyList())
    val list: StateFlow<List<JSONObject>> = flow.asStateFlow()

    @Volatile var hasOlder = false
        private set

    @Volatile var olderEnd: String? = null
        private set

    internal fun reset(list: JSONArray?, hasOlder: Boolean?, olderEnd: String?) {
        synchronized(byKey) {
            byKey.clear()
            list?.objects()?.forEach { byKey[it.getString("key")] = it }
            hasOlder?.let { this.hasOlder = it }
            olderEnd?.let { this.olderEnd = it }
            publish()
        }
    }

    internal fun apply(changes: JSONArray?) {
        synchronized(byKey) {
            changes?.objects()?.forEach { c ->
                if (c.optString("op") == "upsert") {
                    val m = c.getJSONObject("message")
                    byKey[m.getString("key")] = m
                } else {
                    byKey.remove(c.optString("key"))
                }
            }
            publish()
        }
    }

    private fun publish() {
        flow.value = byKey.values.sortedBy { it.optDouble("order", 0.0) }
    }

    /** The page before the oldest shown: the cache first, then the server. False at the start. */
    suspend fun loadOlder(): Boolean {
        val r = client.command(JSONObject().put("cmd", "load_older").put("conv_id", convId)) as JSONObject
        hasOlder = r.optBoolean("has_older")
        olderEnd = r.optString("older_end").takeIf { !r.isNull("older_end") }
        return hasOlder
    }

    /** A page around a message (it stays loaded). */
    suspend fun jumpTo(seq: Long) = client.command(JSONObject().put("cmd", "jump_to").put("conv_id", convId).put("seq", seq))
}

class AgentIMClient private constructor(private val o: ClientOptions) : AutoCloseable {
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
    private val pending = ConcurrentHashMap<Long, CompletableDeferred<Any?>>()
    // Applied in event order, before later events (a view's first snapshot).
    private val inOrder = ConcurrentHashMap<Long, (Any?) -> Unit>()
    private val views = ConcurrentHashMap<String, Messages>()
    private val convs = LinkedHashMap<String, JSONObject>()
    private val listeners = ConcurrentHashMap<String, MutableList<(JSONObject) -> Unit>>()
    private val next = AtomicLong()
    private val generation = AtomicLong()
    private val gate = Any()
    private var ptr = 0L

    private val connectionFlow = MutableStateFlow(Connection("closed"))
    private val conversationsFlow = MutableStateFlow<List<JSONObject>>(emptyList())
    val connection: StateFlow<Connection> = connectionFlow.asStateFlow()
    val conversations: StateFlow<List<JSONObject>> = conversationsFlow.asStateFlow()

    companion object {
        /** Loads `agentim_jni` (and through it `agentim_client`) unless already loaded. */
        @JvmStatic
        fun loadLibrary() = Native.load()

        val version: String get() = Native.load().let { Native.version() }

        fun create(o: ClientOptions): AgentIMClient {
            Native.load()
            val c = AgentIMClient(o)
            val cfg = JSONObject()
                .put("url", o.url)
                .put("device_id", o.deviceId)
                .put("device_name", o.deviceName ?: JSONObject.NULL)
                .put("platform", o.platform ?: System.getProperty("os.name"))
                .put("client_name", o.clientName)
                .put("client_version", o.clientVersion)
            synchronized(c.gate) {
                val p = Native.create(cfg.toString().toByteArray(), o.storeDir?.toByteArray(), EventSink { c.onEvent(it) })
                if (p == 0L) throw AgentIMException("bad_request", "the client could not start (check url and device_id)")
                c.ptr = p
            }
            return c
        }
    }

    private fun onEvent(bytes: ByteArray) {
        val e = try {
            JSONObject(String(bytes, Charsets.UTF_8))
        } catch (_: Exception) {
            return
        }
        when (e.optString("type")) {
            "need_token" -> scope.launch { provideToken() }
            "reply" -> {
                val id = e.optLong("id")
                val ok = e.optBoolean("ok")
                val result = if (ok) e.opt("result") else null
                if (ok) inOrder.remove(id)?.invoke(result) else inOrder.remove(id)
                pending.remove(id)?.let { d ->
                    if (ok) d.complete(result.takeIf { it != JSONObject.NULL })
                    else d.completeExceptionally(AgentIMException.from(e.optJSONObject("error")))
                }
            }
            else -> dispatch(e)
        }
    }

    private suspend fun provideToken() {
        var token: String? = null
        var refused = false
        try {
            token = o.tokenProvider()
        } catch (_: TokenRefused) {
            refused = true
        } catch (_: Exception) {
            // Failed: the core tries again later.
        }
        synchronized(gate) {
            if (ptr != 0L) Native.token(ptr, token?.toByteArray(), refused)
        }
    }

    private fun dispatch(e: JSONObject) {
        when (e.optString("type")) {
            "connection" -> connectionFlow.value = Connection(
                e.optString("state", "closed"),
                e.optString("reason").takeIf { !e.isNull("reason") && e.has("reason") },
                if (e.has("retry_at") && !e.isNull("retry_at")) e.getLong("retry_at") else null,
            )
            "conversations" -> synchronized(convs) {
                if (e.optBoolean("reset")) convs.clear()
                e.optJSONArray("changes")?.objects()?.forEach { c ->
                    c.optJSONObject("conversation")?.let { convs[it.getString("conv_id")] = it }
                }
                conversationsFlow.value = convs.values
                    .filter { !it.optBoolean("left") && !it.optBoolean("dissolved") }
                    .sortedByDescending { it.optJSONObject("last_message")?.optLong("created_at") ?: 0L }
            }
            "messages" -> views[e.optString("conv_id")]?.let { v ->
                if (e.optBoolean("reset")) v.reset(null, null, null)
                v.apply(e.optJSONArray("changes"))
            }
        }
        listeners[e.optString("type")]?.let { fns ->
            val list = synchronized(fns) { fns.toList() }
            for (f in list) {
                try {
                    f(e)
                } catch (_: Exception) {
                    // The listener's own error.
                }
            }
        }
    }

    /** Any core command (`{"cmd": …}`); returns its result (a JSONObject, or null). */
    suspend fun command(cmd: JSONObject): Any? = command(cmd, null)

    private suspend fun command(cmd: JSONObject, applyInOrder: ((Any?) -> Unit)?): Any? {
        val id = next.incrementAndGet()
        val d = CompletableDeferred<Any?>()
        pending[id] = d
        if (applyInOrder != null) inOrder[id] = applyInOrder
        val r = synchronized(gate) {
            if (ptr == 0L) null else Native.command(ptr, id, cmd.toString().toByteArray())
        }
        if (r != 0) {
            pending.remove(id)
            inOrder.remove(id)
            throw if (r == null) AgentIMException("closed", "the client is closed") else AgentIMException("bad_request", "not a command")
        }
        return withTimeoutOrNull(o.commandTimeoutMs) { d.await() }.also {
            if (!d.isCompleted) {
                pending.remove(id)
                inOrder.remove(id)
                throw AgentIMException("timeout", "no answer from the client core", true)
            }
        }
    }

    /** Events: operation, side, signal, reset, storage, event, read, mute (and the ones above). */
    fun on(type: String, fn: (JSONObject) -> Unit): AutoCloseable {
        val fns = listeners.getOrPut(type) { mutableListOf() }
        synchronized(fns) { fns.add(fn) }
        return AutoCloseable { synchronized(fns) { fns.remove(fn) } }
    }

    /** The platform's network: call on every change (Android: ConnectivityManager.NetworkCallback). */
    fun setNetwork(online: Boolean) = synchronized(gate) {
        if (ptr != 0L) Native.network(ptr, online, generation.incrementAndGet())
    }

    /** The app came to the foreground or went to the background (Android: ProcessLifecycleOwner). */
    fun setForeground(foreground: Boolean) = synchronized(gate) {
        if (ptr != 0L) Native.foreground(ptr, foreground)
    }

    /** The conversation's messages, kept up to date. */
    fun messages(convId: String): Messages {
        var created = false
        val v = views.computeIfAbsent(convId) {
            created = true
            Messages(this, it)
        }
        if (created) {
            scope.launch {
                runCatching {
                    command(JSONObject().put("cmd", "messages").put("conv_id", convId)) { r ->
                        val o = r as JSONObject
                        v.reset(o.optJSONArray("messages"), o.optBoolean("has_older"), o.optString("older_end").takeIf { !o.isNull("older_end") })
                    }
                }
            }
        }
        return v
    }

    suspend fun connect() = cmd("connect")
    /** Closes the connection (the client stays usable; [close] frees it). */
    suspend fun disconnect() = cmd("close")
    /** Logs out here: closes and wipes the cache and the queue (GDPR). */
    suspend fun logout() = cmd("logout")

    /** Returns `{client_msg_id, key}` once the message is queued and shown. */
    suspend fun send(convId: String, text: String, clientMsgId: String? = null, replyTo: String? = null) =
        cmd("send", "conv_id" to convId, "text" to text, "client_msg_id" to clientMsgId, "reply_to" to replyTo) as JSONObject

    suspend fun edit(convId: String, msgId: String, text: String) = cmd("edit", "conv_id" to convId, "msg_id" to msgId, "text" to text)
    suspend fun recall(convId: String, msgId: String) = cmd("recall", "conv_id" to convId, "msg_id" to msgId)
    suspend fun read(convId: String, seq: Long? = null) = cmd("read", "conv_id" to convId, "seq" to seq)
    suspend fun retry(convId: String, clientMsgId: String) = cmd("retry", "conv_id" to convId, "client_msg_id" to clientMsgId)
    suspend fun discard(convId: String, clientMsgId: String) = cmd("discard", "conv_id" to convId, "client_msg_id" to clientMsgId)

    /** A request that needs the server now (approvals, decisions, queries). */
    suspend fun call(op: String, args: JSONObject = JSONObject()) = cmd("call", "op" to op, "args" to args)
    suspend fun debugState() = cmd("debug_state")

    private suspend fun cmd(name: String, vararg fields: Pair<String, Any?>): Any? {
        val c = JSONObject().put("cmd", name)
        for ((k, v) in fields) if (v != null) c.put(k, v)
        return command(c)
    }

    /** Stops the client; no event arrives after it returns. Waiting commands fail with `closed`. Not from an event. */
    override fun close() {
        val p = synchronized(gate) { ptr.also { ptr = 0L } }
        if (p == 0L) return
        scope.cancel()
        Native.free(p)
        for (id in pending.keys.toList()) pending.remove(id)?.completeExceptionally(AgentIMException("closed", "the client is closed"))
    }
}

/** The native side calls this with each event (UTF-8 JSON). */
fun interface EventSink {
    fun onEvent(json: ByteArray)
}

internal object Native {
    @Volatile private var loaded = false

    fun load() {
        if (loaded) return
        synchronized(this) {
            if (loaded) return
            // Tests and desktop apps can point at the library; Android packs it.
            val path = System.getProperty("agentim.jni")
            if (path != null) System.load(path) else System.loadLibrary("agentim_jni")
            loaded = true
        }
    }

    @JvmStatic external fun create(config: ByteArray, storeDir: ByteArray?, sink: EventSink): Long

    @JvmStatic external fun command(p: Long, id: Long, cmd: ByteArray): Int

    @JvmStatic external fun token(p: Long, token: ByteArray?, refused: Boolean)

    @JvmStatic external fun network(p: Long, online: Boolean, generation: Long)

    @JvmStatic external fun foreground(p: Long, foreground: Boolean)

    @JvmStatic external fun free(p: Long)

    @JvmStatic external fun version(): String
}

private fun JSONArray.objects(): List<JSONObject> = (0 until length()).mapNotNull { optJSONObject(it) }
