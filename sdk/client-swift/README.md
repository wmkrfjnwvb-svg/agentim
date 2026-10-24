# AgentIM client SDK for iOS and macOS

Swift over the Rust core's C ABI (`sdk/client-rust/native`, docs/12 §2). `AgentIMClient` and
`Messages` are `@Observable`, so SwiftUI redraws on their changes. Commands are `async`.
Everything the core says (state changes, events, replies) is handled in order on one queue
(main by default). So when a method returns, the state it changed is already there.

```swift
let client = try AgentIMClient(ClientOptions(
    url: "https://chat.example.com",
    deviceId: savedDeviceId,
    storeDir: cacheURL.path,                      // nil: in memory
    tokenProvider: { try await backend.token() }  // throw TokenRefused() to stop
))
let view = client.messages(convId)                // view.list: [Message]
try await client.connect()
try await client.send(convId, "hello")            // in view.list when it returns
```

- `Message.senderType == "agent"` must show the AI mark (EU AI Act art. 50, D36).
- Errors: `AgentIMError { code, message, retryable, retryAfterMs }`.
- `setNetwork(online:)` from `NWPathMonitor`, `setForeground(_:)` from `scenePhase`.

## Packaging (not built here)

Releases ship the core as an XCFramework: `aarch64-apple-ios`, `aarch64-apple-ios-sim`,
`x86_64-apple-ios`, `aarch64-apple-darwin` and `x86_64-apple-darwin`, with dSYMs. They use a
`binaryTarget` in place of the `CAgentIM` system library. This environment has no macOS or
Xcode, so the XCFramework and the simulator run are not done yet.

## Tests

`test.sh` builds the package and runs the conformance scenario (docs/12 §2.13, W1–W9) against
a real `agentim` (Linux, Swift 6.2):

```bash
sh test.sh
```
