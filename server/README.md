# agentim server

```bash
cargo run --release -- --demo                        # http://127.0.0.1:8080, log in as alice / bob / carol
cargo run --release -- --config agentim.toml         # databases and units, see agentim.example.toml
cargo run --release -- --config agentim.toml migrate-unit --unit subsidiary-b
cargo test                                           # unit, acceptance, multi-database, WebSocket, kill -9
AGENTIM_TEST_PG=postgres://postgres@127.0.0.1 AGENTIM_TEST_MYSQL=mysql://user:pass@127.0.0.1 cargo test
cargo run --release --example loadtest -- --addr 127.0.0.1:8080 --clients 50 --messages 100 --streams 10
cargo run --release --example loadtest -- --addr 127.0.0.1:8080 --solo --clients 50 --messages 200
```

Options: `--config`, `--db` (single SQLite file), `--addr`, `--demo`, `--stream-idle-secs`,
`--log-format text|json`, `--metrics-token`, `--no-exit-on-storage-failure`.
Endpoints: `/ws`, `/metrics`, `/healthz`, `/readyz`.

| Path | |
|---|---|
| `src/storage.rs` | Dialects, connections, schema, durability checks |
| `src/db.rs` | All business SQL (one copy for SQLite, PostgreSQL, MySQL) |
| `src/shard.rs` | One writer per database: group commit, fault handling |
| `src/logic.rs` | Business logic run by the writers |
| `src/hub.rs` | Routing by conversation, cross-database reads, reconcile, migration |
| `src/registry.rs` | Sessions and bounded fan-out (resync on overflow) |
| `src/metrics.rs` | Prometheus metrics |
| `src/guard.rs`, `src/policy.rs` | Guardrails and policy hooks |
| `src/server.rs` | HTTP + WebSocket |
| `src/demo.rs`, `web/index.html` | Demo data, scripted agents, web client |
| `examples/agent.mjs` | External agent over the WebSocket protocol |

Design and test results: [docs/02-architecture.md](../docs/02-architecture.md) §4–§5 and [docs/05-operations.md](../docs/05-operations.md).
