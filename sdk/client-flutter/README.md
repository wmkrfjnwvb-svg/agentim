# AgentIM client SDK for Flutter and Dart

Dart FFI over the Rust core's C ABI (`sdk/client-rust/native`, docs/12 §2). Events arrive
through `NativeCallable.listener`, in the core's order, on the isolate's event loop. The small
C shim `src/agentim_dart.c` copies each event, because the core's string is gone by the time
Dart runs.

```dart
final client = AgentIMClient.create(ClientOptions(
  url: 'https://chat.example.com',
  deviceId: savedDeviceId,
  storeDir: '${(await getApplicationSupportDirectory()).path}/agentim',
  tokenProvider: () => backend.token(),   // throw TokenRefused() to stop
));
StreamBuilder(stream: client.messages(convId).list.watch(), builder: …);
await client.connect();
await client.send(convId, 'hello');       // in messages(convId).list when it returns
```

- `Observed<T>`: `value`, plus `watch()`, a stream of the current value and then every change.
- `setNetwork(online)` from `connectivity_plus`, `setForeground(bool)` from
  `AppLifecycleListener`.

## Packaging (not built here)

A Flutter FFI plugin bundles `agentim_client` and `agentim_dart` for each platform: Android
(4 ABIs, 16 KB pages), iOS (static, `DynamicLibrary.process()`), macOS, Windows and Linux.
Flutter web uses `@agentim/client-web` instead. This environment has no Android or iOS
toolchain, so the plugin packaging is not done yet. The wrapper runs on the Dart VM.

## Tests

`test.sh` builds the shim and checks format and analysis. It then runs the conformance
scenario (docs/12 §2.13, W1–W9) against a real `agentim`:

```bash
DART=…/dart sh test.sh
```
