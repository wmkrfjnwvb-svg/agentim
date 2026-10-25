// Two people talk through the AAR (Kotlin wrapper, JNI bridge, native core)
// against a real server; logs "AGENTIM_SMOKE ok" or "AGENTIM_SMOKE FAIL …".
package com.agentim.aartest

import android.app.Activity
import android.os.Bundle
import android.util.Log
import com.agentim.client.AgentIMClient
import com.agentim.client.ClientOptions
import java.net.HttpURLConnection
import java.net.URL
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeout
import org.json.JSONArray
import org.json.JSONObject

class MainActivity : Activity() {
    private val url = BuildConfig.AGENTIM_URL

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        CoroutineScope(Dispatchers.Main).launch {
            val r = try {
                run()
            } catch (e: Exception) {
                "error $e"
            }
            Log.i("AgentIM", "AGENTIM_SMOKE ${if (r == "ok") "ok" else "FAIL $r"}")
        }
    }

    private suspend fun person(name: String): JSONObject = withContext(Dispatchers.IO) {
        val c = URL("$url/api/dev/users").openConnection() as HttpURLConnection
        c.requestMethod = "POST"
        c.doOutput = true
        c.setRequestProperty("Content-Type", "application/json")
        c.setRequestProperty("Origin", url)
        c.outputStream.use { it.write(JSONObject().put("name", name).toString().toByteArray()) }
        val code = c.responseCode
        if (code != 200) error("POST /api/dev/users: HTTP $code ${c.errorStream?.bufferedReader()?.readText()}")
        JSONObject(c.inputStream.bufferedReader().readText())
    }

    private suspend fun run(): String {
        if (AgentIMClient.version.isEmpty()) return "no version"
        // Unique names: demo mode already has alice and bob.
        val tag = System.currentTimeMillis()
        val alice = person("aar-a$tag")
        val bob = person("aar-b$tag")
        fun client(p: JSONObject, device: String) =
            AgentIMClient.create(ClientOptions(url = url, deviceId = device, tokenProvider = { p.getString("token") }))
        val a = client(alice, "alice-aar")
        val b = client(bob, "bob-aar")
        a.connect()
        b.connect()
        withTimeout(20_000) {
            a.connection.first { it.state == "connected" }
            b.connection.first { it.state == "connected" }
        }
        val created = a.call("create_conv", JSONObject().put("kind", "group").put("title", "aar")
            .put("members", JSONArray().put(bob.getJSONObject("user").getString("id")))) as JSONObject
        val conv = created.getString("conv_id")
        val text = "hello from Android 👋"
        a.send(conv, text)
        withTimeout(20_000) {
            b.messages(conv).list.first { l -> l.any { it.optString("content") == text && it.optString("status") == "sent" } }
        }
        a.close()
        b.close()
        return "ok"
    }
}
