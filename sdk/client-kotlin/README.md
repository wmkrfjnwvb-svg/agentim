# AgentIM client SDK for Android and the JVM

Kotlin over the Rust core's C ABI (`sdk/client-rust/native`, docs/12 §2), through a small JNI
bridge (`src/main/c/agentim_jni.c`). The core does the connection, sync, offline queue, cache
and history. This package turns its events into `StateFlow`s and its replies into suspend
functions.

```kotlin
val client = AgentIMClient.create(ClientOptions(
    url = "https://chat.example.com",
    deviceId = savedDeviceId,
    storeDir = File(context.filesDir, "agentim").path,   // null: in memory
    tokenProvider = { backend.token() },                  // throw TokenRefused to stop
))
lifecycleScope.launch { client.conversations.collect { render(it) } }
val view = client.messages(convId)
lifecycleScope.launch { view.list.collect { render(it) } }
client.connect()
client.send(convId, "hello")      // in view.list when it returns
```

- `connection`, `conversations`, `messages(id).list` are `StateFlow`s. They are updated before
  the call that changed them returns.
- Errors: `AgentIMException(code, retryable, retryAfterMs)`, with the server's codes and the
  client's (`disconnected`, `timeout`, `closed`).
- `setNetwork(online)` from a `ConnectivityManager.NetworkCallback`, `setForeground(bool)` from
  `ProcessLifecycleOwner`.
- JSON is `org.json` (built into Android; one jar on the JVM). Strings cross JNI as UTF-8 bytes,
  because JNI's modified UTF-8 breaks emoji. The SDK thread detaches from the JVM when it exits.

## Android library (AAR)

`android/build-aar.sh` builds `build/agentim-client.aar` with NDK r28c for `arm64-v8a`,
`armeabi-v7a`, `x86_64` and `x86`. It contains:

- the Rust runtime (`libagentim_client.so`) and the JNI bridge (`libagentim_jni.so`), both linked
  with 16 KB pages (required by Google Play for Android 15 and later);
- the Kotlin classes, compiled against `android.jar`;
- ProGuard rules that keep the JNI names.

The script then checks every library: each LOAD segment is 16 KB aligned, and the bridge
exports its 7 JNI functions. The app adds `kotlinx-coroutines-core`; `org.json` is part of
Android.

```bash
ANDROID_NDK=…/android-ndk-r28c ANDROID_JAR=…/android-35/android.jar KOTLINC=… sh android/build-aar.sh
```

No emulator run yet: that needs a machine with KVM (docs/06 D123).

## Tests

`test.sh` builds the JNI bridge and the wrapper with `kotlinc`. It then runs the conformance
scenario (docs/12 §2.13, W1–W9) on the JVM against a real `agentim`:

```bash
KOTLINC=…/kotlinc sh test.sh
```
