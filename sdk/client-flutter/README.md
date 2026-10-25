# AgentIM client SDK for Flutter and Dart

Dart FFI over the Rust core's C ABI (`sdk/client-rust/native`, docs/12 §2). Events arrive
through `NativeCallable.listener`, in the core's order, on the isolate's event loop; the core's
`agentim_dart_new` gives each event as a copy, because Dart reads it after the native call
returned. The same package works in Flutter apps and in Dart programs; Flutter web uses
`@agentimhq/client-web` instead.

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

## The native core

The build hook `hook/build.dart` provides the core for the platform being built (Dart 3.10+,
[hooks](https://dart.dev/tools/hooks)); Flutter bundles it into the app. No Rust toolchain or
plugin is needed:

- The published package has `hook/prebuilt.json`: the library for Android (arm64-v8a,
  armeabi-v7a, x86_64, x86; 16 KB pages), iOS (device, simulator), Linux (x64, arm64; glibc
  2.28+), macOS (x64, arm64) and Windows (x64) is downloaded from the `sdk-v<version>` GitHub
  release and checked against the SHA-256 recorded there. A mismatch fails the build.
- In this repository there is no `prebuilt.json`, and the hook builds the core with cargo from
  `../client-rust` (other targets need their cargo linker configured).

`prebuilt.sh` writes `prebuilt.json` from the release files (`sdk-packages.yml`).

On Android the app needs `<uses-permission android:name="android.permission.INTERNET" />` in
`android/app/src/main/AndroidManifest.xml` (Flutter adds it to debug builds only).

## Tests

`test.sh` checks format and analysis, then runs the conformance scenario (docs/12 §2.13,
W1–W9) against a real `agentim`. `test-package.sh <library>` does the same with the package as
published: a copy outside the repository whose hook downloads the given library from a
`file://` URL and checks it; then a wrong SHA-256 must fail the build. `example/test.sh` builds
a Flutter Linux app and runs it under Xvfb against a real server.

```bash
DART=…/dart sh test.sh
sh test-package.sh ../client-rust/target/debug/libagentim_client.so
```
