# AgentIM server SDKs

Thin clients for the open API (`/v1`, [docs/07-open-api.md](../docs/07-open-api.md)) for your backend. No dependencies; every client
retries network errors, `429` and `503` (POSTs keep the same `Idempotency-Key`) and raises an error carrying the server's `code`
(the same codes as the WebSocket protocol). The full contract is `GET /v1/openapi.json`.

Create a key first: `agentim --config agentim.toml api-key create --name backend --scopes users:write,messages:write`.

**Logging your users in**: your users sign in with your own accounts; your backend then signs them a client token with a
signing key (`agentim signing-key create --name backend` prints its `kid` and `secret` once) and hands it to your app. No call to
AgentIM is made: `createUserToken({ kid, secret }, "user_1")` (Node), `create_user_token(kid, secret, "user_1")` (Python),
`agentim.CreateUserToken(kid, secret, "user_1", time.Hour)` (Go). Any JWT library works too: HS256, header `kid`, claims `sub`,
`iat`, `exp` ([docs/13 §4.2](../docs/13-roles-and-entry-points.md)).

| Language | Path | Example |
|---|---|---|
| Node.js 18+ (types included) | `sdk/node` | `const im = new AgentIM({ baseUrl, apiKey }); await im.upsertUser("user_1"); const token = createUserToken(key, "user_1");` |
| Python 3.9+ | `sdk/python` | `im = AgentIM(base_url, api_key); im.send_to_user("user_2", "user_1", "hi")` |
| Go 1.21+ | `sdk/go` | `im := agentim.New(baseURL, apiKey); im.SendMessage(ctx, convID, agentim.M{"as": "system", "text": "Maintenance tonight"})` |

`as` is a user id (the call follows that user's rules, like their client would) or `"system"` (manage any conversation, send
system notices; see 07 §3.2). `sdk/smoke.sh` runs the same scenario in all three languages against a fresh server; CI runs it on
every push.

## Agent SDKs

For agents rather than backends: an agent connects with its key (from the open API `PUT /v1/users/{id}` with `kind: agent`, the console or `agentim admin agent-create`) over the
WebSocket protocol, receives what concerns it in order and acts. No dependencies; at-least-once delivery (a handler that throws
gets the event again); reconnects by itself; errors carry the protocol `code`. See
[docs/03-agents.md §2.4](../docs/03-agents.md).

| Language | Path | Example |
|---|---|---|
| Node.js 22+ (types included) | `sdk/agent-node` | `const agent = new Agent({ url, key }); agent.onMessage(async (msg, ctx) => ctx.reply("hi")); await agent.start();` |
| Python 3.9+ | `sdk/agent-python` | `agent = Agent(url, key)`, `@agent.on_message async def answer(msg, ctx): await ctx.reply("hi")`, `asyncio.run(agent.run())` |

Both offer: replies and streamed replies, progress, cards and their responses, sources and the routing envelope, asking a person
to decide (`askHuman` / `ask_human`), handing off to a person, history, context, search, summaries, checkpoints, claims and
tasks; `call(op, ...)` reaches any other protocol operation. `sdk/agent_e2e.sh` runs the same end-to-end scenario in both
languages against a real server (Linux and Windows in CI).

## Client SDKs

For your app's users, on their devices. They are built on one Rust core (`client-rust/core`), which holds every rule: connect
and reconnect, sync without gaps, the offline queue (stored first, sent once and in order), the local cache, and paging back
through history. Each platform has a thin wrapper. Each wrapper gives three things the UI follows (connection, conversations,
the messages of a conversation) and methods that return once the change is shown. See
[docs/12 §2](../docs/12-developer-experience.md); what is done per platform is in its §2.14.

| Platform | Path | State |
|---|---|---|
| Browser, Node | `client-web` (`@agentimhq/client-web`) | The core in WebAssembly (146 KB gzipped), an encrypted IndexedDB cache, one connection shared by all tabs |
| React | `react` (`@agentimhq/react`) | Hooks and components (a support chat window, conversation list), themes, AI label, cards, RTL |
| React Native | `client-react-native` | JSI binding (compiles), TypeScript tested against a fake binding; not yet run on a device |
| iOS, macOS | `client-swift` | Swift package, `@Observable`; tested on Linux, the XCFramework is not built yet |
| Android, JVM | `client-kotlin` | Kotlin with a JNI bridge, `StateFlow`; tested on the JVM; the AAR is built for 4 ABIs with 16 KB pages, not yet run on an emulator |
| .NET, Unity | `client-dotnet` | C#, net8.0 and netstandard2.1, a Unity package with a WebGL bridge (`.jslib` over `client-web`); tested on .NET and the bridge in Node, not yet in the Unity editor |
| Flutter, Dart | `client-flutter` | Dart FFI, and the `agentim_flutter` plugin that builds and bundles the core; tested on the Dart VM and as a Linux Flutter app |
| Native (C) | `client-rust/native` | The runtime (WebSocket, SQLite) and the C ABI in `include/agentim.h` that the wrappers use |

Every wrapper runs the same conformance scenario against a real `agentim` (CI job `client-wrappers`).

