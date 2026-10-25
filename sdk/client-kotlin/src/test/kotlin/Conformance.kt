// The wrapper conformance scenario (docs/12 §2.13, W1–W9), against a real
// `agentim`: the same steps every platform wrapper runs. See test.sh.

import com.agentim.client.AgentIMClient
import com.agentim.client.AgentIMException
import com.agentim.client.ClientOptions
import com.agentim.client.Messages
import com.agentim.client.TokenRefused
import java.io.File
import java.net.URI
import java.net.http.HttpClient
import java.net.http.HttpRequest
import java.net.http.HttpResponse
import java.nio.file.Files
import kotlin.system.exitProcess
import kotlinx.coroutines.async
import kotlinx.coroutines.launch
import kotlinx.coroutines.delay
import kotlinx.coroutines.runBlocking
import org.json.JSONArray
import org.json.JSONObject

var failures = 0

fun check(ok: Boolean, what: String) {
    println("${if (ok) "ok  " else "FAIL"} $what")
    if (!ok) failures++
}

suspend fun until(ms: Long = 20_000, f: suspend () -> Boolean): Boolean {
    val end = System.currentTimeMillis() + ms
    while (System.currentTimeMillis() < end) {
        if (f()) return true
        delay(25)
    }
    return f()
}

data class Person(val id: String, val name: String, val token: String)

fun sent(v: Messages) = v.list.value.filter { it.optString("status") == "sent" }.map { it.optString("content") }

fun main() {
    runBlocking { run() }
    exitProcess(if (failures == 0) 0 else 1)
}

suspend fun run() = kotlinx.coroutines.coroutineScope {
    val bin = System.getenv("AGENTIM_BIN") ?: error("AGENTIM_BIN")
    val dir = Files.createTempDirectory("agentim-kotlin-").toFile()
    val port = 19950 + (0 until 40).random()
    val url = "http://127.0.0.1:$port"
    val server = ProcessBuilder(bin, "--demo", "--db", File(dir, "a.db").path, "--addr", "127.0.0.1:$port")
        .redirectOutput(ProcessBuilder.Redirect.DISCARD)
        .redirectError(ProcessBuilder.Redirect.DISCARD)
        .start()
    val http = HttpClient.newHttpClient()
    var n = 0

    fun person(name: String): Person {
        val nm = "$name${++n}"
        val req = HttpRequest.newBuilder(URI("$url/api/dev/users"))
            .header("content-type", "application/json")
            .POST(HttpRequest.BodyPublishers.ofString("""{"name":"$nm"}"""))
            .build()
        val r = JSONObject(http.send(req, HttpResponse.BodyHandlers.ofString()).body())
        return Person(r.getJSONObject("user").getString("id"), nm, r.getString("token"))
    }

    fun client(p: Person, store: String? = null, device: String? = null, token: (suspend () -> String)? = null) =
        AgentIMClient.create(ClientOptions(url = url, deviceId = device ?: "${p.name}-d", storeDir = store, tokenProvider = token ?: { p.token }))

    suspend fun code(f: suspend () -> Unit): String = try {
        f()
        ""
    } catch (e: AgentIMException) {
        e.code
    }

    try {
        until(30_000) {
            runCatching { http.send(HttpRequest.newBuilder(URI("$url/readyz")).build(), HttpResponse.BodyHandlers.discarding()).statusCode() == 200 }.getOrDefault(false)
        }
        check(AgentIMClient.version.isNotEmpty(), "version ${AgentIMClient.version}")
        val alice = person("alice")
        val bob = person("bob")
        val a = client(alice)
        val b = client(bob)
        // B12: a reset replaces a view in one update, never empty in between.
        run {
            val up = { key: String, order: Int -> JSONObject().put("op", "upsert").put("message", JSONObject().put("key", key).put("order", order)) }
            val v = Messages(a, "b12")
            v.apply(JSONArray().put(up("a", 1)).put(up("b", 2)))
            val lists = java.util.Collections.synchronizedList(mutableListOf<List<String>>())
            val job = launch(kotlinx.coroutines.Dispatchers.Unconfined) { v.list.collect { l -> lists.add(l.map { it.getString("key") }) } }
            lists.clear()
            v.apply(JSONArray().put(up("b", 2)).put(up("c", 3)), reset = true)
            job.cancel()
            check(lists == listOf(listOf("b", "c")), "B12 a reset is one update ($lists)")
        }
        listOf(async { a.connect() }, async { b.connect() }).forEach { it.await() }
        check(until { a.connection.value.state == "connected" && b.connection.value.state == "connected" }, "W1 both connect")

        val conv = (a.call("create_conv", JSONObject().put("kind", "group").put("title", "kotlin").put("members", JSONArray().put(bob.id))) as JSONObject).getString("conv_id")
        val av = a.messages(conv)
        val r = a.send(conv, "hello from Kotlin 👋")
        check(r.getString("key").startsWith("local:") && av.list.value.any { it.optString("content") == "hello from Kotlin 👋" }, "W2 a send is shown when it returns")
        check(until { "hello from Kotlin 👋" in sent(av) }, "W2 and then sent (emoji intact)")
        val bv = b.messages(conv)
        check(until { "hello from Kotlin 👋" in sent(bv) }, "W2 the other side sees it")
        check(until { b.conversations.value.any { it.optString("conv_id") == conv } }, "W2 and has the conversation in its list")

        val mine = av.list.value.first { it.optString("content") == "hello from Kotlin 👋" }
        a.edit(conv, mine.getString("msg_id"), "hello, edited")
        check(until { sent(bv).firstOrNull() == "hello, edited" }, "W3 an edit reaches the other side")
        a.recall(conv, mine.getString("msg_id"))
        check(until { bv.list.value.firstOrNull()?.optString("state") == "recalled" }, "W3 a recall reaches the other side")

        val ops = java.util.concurrent.ConcurrentLinkedQueue<String>()
        a.on("operation") { ops.add(it.optString("result")) }.use {
            a.send("c_not_mine", "nope", clientMsgId = "bad1")
            val nv = a.messages("c_not_mine")
            check(until { nv.list.value.any { it.optString("status") == "failed" } && "failed" in ops }, "W4 a refused send fails and is reported")
            a.discard("c_not_mine", "bad1")
            check(until { nv.list.value.isEmpty() }, "W4 discard removes it")
        }

        val c1 = code { a.call("get_request", JSONObject().put("request_id", "nope")) }
        check(c1 == "not_found", "W5 an online call answers with the server's code ($c1)")
        val c2 = code { a.command(JSONObject().put("cmd", "no_such_command")) }
        check(c2 == "bad_request", "W5 an unknown command is bad_request ($c2)")

        val store = File(dir, "alice-cache").path
        client(alice, store, "alice-q").use { q -> for (t in listOf("one", "two", "three")) q.send(conv, t) }
        client(alice, store, "alice-q").use { q ->
            val v = q.messages(conv)
            check(until { v.list.value.count { it.optString("status") == "sending" } == 3 }, "W6 after a restart the queue shows before connecting")
            q.connect()
            check(until { sent(bv).filter { it in setOf("one", "two", "three") }.joinToString("|") == "one|two|three" }, "W6 and is sent once, in order")
            check(until { ((q.command(JSONObject().put("cmd", "pending")) as JSONObject).getJSONArray("ops")).length() == 0 }, "W6 and the queue is empty")
            q.setNetwork(false)
            check(until { q.connection.value.state != "connected" }, "W7 offline is not connected (${q.connection.value.state})")
            q.send(conv, "while offline")
            q.setNetwork(true)
            check(until { sent(bv).count { it == "while offline" } == 1 }, "W7 back online it is sent once")
        }

        client(alice, device = "alice-r", token = { throw TokenRefused("no") }).use { x ->
            x.connect()
            check(until { x.connection.value.state == "failed" }, "W8 a refused token stops the client")
            check(x.connection.value.reason == "unauthenticated", "W8 as unauthenticated (${x.connection.value.reason})")
        }

        val d = client(alice, device = "alice-d")
        d.close()
        d.close()
        check(code { d.connect() } == "closed", "W9 after close, commands fail with closed (close twice is fine)")
        a.close()
        b.close()
    } catch (e: Throwable) {
        println("FAIL $e")
        e.printStackTrace()
        failures++
    } finally {
        server.destroy()
        dir.deleteRecursively()
    }
}
