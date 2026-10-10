# agentim server (single-node edition)

```bash
cargo run --release -- --demo                 # http://127.0.0.1:8080, log in as alice / bob / carol
cargo test                                    # unit, acceptance and WebSocket tests
cargo run --release --example loadtest -- --addr 127.0.0.1:8080 --clients 50 --messages 100 --streams 10
./ci/crash_test.sh                            # kill -9 recovery (needs a release build)
```

Options: `--addr`, `--db` (SQLite file, default `agentim.db`), `--demo`, `--stream-idle-secs`.

| Path | |
|---|---|
| `src/store.rs` | SQLite storage: event log, side channel, messages, dedup, drafts, takeovers, audit |
| `src/hub.rs` | Send pipeline, fan-out, streaming state machine, intervention, guardrails |
| `src/guard.rs` | Hop limit, loop breaker, rate limit, budget |
| `src/policy.rs` | Policy hooks (injection rules, basic DLP) |
| `src/server.rs` | HTTP + WebSocket |
| `src/demo.rs` | Demo data and scripted agents |
| `web/index.html` | Demo web client |
| `examples/agent.mjs` | External agent over the WebSocket protocol |

Design, protocol and test results: [docs/06-standalone.md](../docs/06-standalone.md).
