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
