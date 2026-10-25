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

## Flutter plugin

`agentim_flutter/` is an FFI plugin. Its CMake builds the Rust core with cargo (on Android one
target per ABI, with 16 KB pages) and the shim, and bundles both into the app. The app then
uses `package:agentim_flutter/agentim_flutter.dart` without `libraryDir`.

`agentim_flutter/test.sh` builds `example/` for Linux and runs it under Xvfb against a real
server. The Android, iOS, macOS and Windows plugin builds have not been run here; iOS and macOS
need the XCFramework (A9). Flutter web uses `@agentimhq/client-web` instead.

## Tests

`test.sh` builds the shim and checks format and analysis. It then runs the conformance
scenario (docs/12 §2.13, W1–W9) against a real `agentim`:

```bash
DART=…/dart sh test.sh
```
