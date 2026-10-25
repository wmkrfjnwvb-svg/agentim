# AgentIM client SDK for .NET and Unity

C# over the Rust core's C ABI (`sdk/client-rust/native`, docs/12 §2). The core does
the connection, sync, offline queue, cache and history; this package turns its
events into state the UI follows and its replies into tasks.

```csharp
var client = AgentIMClient.Create(new ClientOptions {
    Url = "https://chat.example.com",
    DeviceId = savedDeviceId,
    StoreDir = cacheDir,                       // null: in memory
    TokenProvider = ct => backend.GetTokenAsync(ct),
    Context = SynchronizationContext.Current,  // events on this thread (Unity: main)
});
client.Conversations.Subscribe(list => Render(list));
var view = client.Messages(convId);
view.List.Subscribe(messages => Render(messages));
await client.ConnectAsync();
await client.SendAsync(convId, "hello");       // in view.List when it returns
```

- `Connection`, `Conversations`, `Messages(convId).List`: `Observable<T>`. `Subscribe` gives
  the current value at once, then every change.
- Errors: `AgentIMException { Code, Retryable, RetryAfterMs }`, with the server's codes
  (`forbidden`, `not_found`, `conflict`, `bad_request`, `blocked`, `rate_limited`) and the
  client's (`disconnected`, `timeout`, `closed`).
- `SetNetwork(online)` on every network change, `SetForeground(bool)` on pause and resume.
- Targets `net8.0` and `netstandard2.1` (Unity).

## Unity

`unity/` is a UPM package (Android, iOS, Windows, macOS). `unity/pack.sh <native libs…>` copies
the sources and the per-platform native libraries in. `AgentIMRunner` passes pause, resume and
network changes to the client. On iOS the library links statically (`__Internal`), and the
callback carries `MonoPInvokeCallback` for IL2CPP. Unity does not ship `System.Text.Json`; add it
with NuGetForUnity.

**WebGL**: `Runtime/Plugins/WebGL/AgentIM.jslib` implements the same C functions in the browser
over `@agentimhq/client-web`, so the C# code is the same on every platform. The WebGL page loads
the browser SDK before the Unity loader:
`<script type="module">import * as w from './agentim/index.js'; window.AgentIMWeb = w;</script>`.
A store name keeps the cache in IndexedDB (encrypted). The browser's own online and visibility
signals reach the client directly.

`unity/test/jslib.test.mjs` loads the jslib the way Unity's emscripten build does and plays the
C# side, against a real server. The package has not been run inside the Unity editor here
(there is no editor in this environment).

## Tests

`Conformance` runs the wrapper scenario (docs/12 §2.13, W1–W9) against a real `agentim`:

```bash
cargo build --manifest-path ../client-rust/Cargo.toml -p agentim-client
cargo build --manifest-path ../../server/Cargo.toml --bin agentim
dotnet run --project Conformance
```
