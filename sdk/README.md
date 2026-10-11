# AgentIM server SDKs

Thin clients for the open API (`/v1`, [docs/07-open-api.md](../docs/07-open-api.md)) for your backend. No dependencies; every client
retries network errors, `429` and `503` (POSTs keep the same `Idempotency-Key`) and raises an error carrying the server's `code`
(the same codes as the WebSocket protocol). The full contract is `GET /v1/openapi.json`.

Create a key first: `agentim --config agentim.toml api-key create --name backend --scopes users:write,tokens:issue,messages:write`.

| Language | Path | Example |
|---|---|---|
| Node.js 18+ (types included) | `sdk/node` | `const im = new AgentIM({ baseUrl, apiKey }); await im.upsertUser("user_1"); const { token } = await im.issueToken("user_1");` |
| Python 3.9+ | `sdk/python` | `im = AgentIM(base_url, api_key); im.send_to_user("user_2", "user_1", "hi")` |
| Go 1.21+ | `sdk/go` | `im := agentim.New(baseURL, apiKey); im.SendMessage(ctx, convID, agentim.M{"as": "system", "text": "Maintenance tonight"})` |

`as` is a user id (the call follows that user's rules, like their client would) or `"system"` (manage any conversation, send
system notices; see 07 §3.2). `sdk/smoke.sh` runs the same scenario in all three languages against a fresh server; CI runs it on
every push.

## Agent SDKs

For agents rather than backends: an agent connects with its key (`agentim agent create --name helper --owner alice`) over the
WebSocket protocol, receives what concerns it in order and acts. No dependencies; at-least-once delivery (a handler that throws
gets the event again); reconnects by itself; errors carry the protocol `code`. See
[docs/10-agent-ecosystem.md §5.2](../docs/10-agent-ecosystem.md).

| Language | Path | Example |
|---|---|---|
| Node.js 22+ (types included) | `sdk/agent-node` | `const agent = new Agent({ url, key }); agent.onMessage(async (msg, ctx) => ctx.reply("hi")); await agent.start();` |
| Python 3.9+ | `sdk/agent-python` | `agent = Agent(url, key)`, `@agent.on_message async def answer(msg, ctx): await ctx.reply("hi")`, `asyncio.run(agent.run())` |

Both offer: replies and streamed replies, progress, cards and their responses, sources and the routing envelope, asking a person
to decide (`askHuman` / `ask_human`), handing off to a person, history, context, search, summaries, checkpoints, claims and
tasks; `call(op, ...)` reaches any other protocol operation. `sdk/agent_e2e.sh` runs the same end-to-end scenario in both
languages against a real server (Linux and Windows in CI).
