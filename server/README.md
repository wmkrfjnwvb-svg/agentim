# AgentIM server

One program, `agentim`, from a single node to a cluster:

| Crate | |
|---|---|
| `crates/agentim-core` | The core library: messaging, sync, agents, governance, storage; all tests and examples |
| `crates/agentim-cluster` | Several nodes on shared databases (docs/11-cluster.md) |
| `crates/agentim` | The `agentim` program |

```bash
cargo run --release --bin agentim -- --demo          # http://127.0.0.1:8080, log in as alice / bob / carol
cargo run --release --bin agentim -- --config agentim.toml   # databases and units, see agentim.example.toml
cargo run --release --bin agentim -- --config agentim.toml migrate-unit --unit subsidiary-b
cargo run --release --bin agentim -- --config agentim.toml admin-key create --name ops --role owner   # a key for `agentim admin`, also while the server runs
cargo run --release --bin agentim -- admin --key adk… agent-create helper --owner alice   # agent key + MCP settings, through the admin API
(cd console && npm ci && npm run build)              # the console at /console is embedded by the next cargo build
cargo test                                           # unit, acceptance, scenarios, concurrency, multi-database, WebSocket, kill -9
AGENTIM_TEST_PG=postgres://postgres@127.0.0.1 AGENTIM_TEST_MYSQL=mysql://user:pass@127.0.0.1 cargo test
cargo run --release --example loadtest -- --addr 127.0.0.1:8080 --clients 50 --messages 100 --streams 10
cargo run --release --example loadtest -- --addr 127.0.0.1:8080 --solo --clients 50 --messages 200
cargo run --release --example scale -- --addr 127.0.0.1:8080 --scenario connections --users 10000 --storm
bench/single-node.sh                                 # full single-node benchmark (see the top-level README)
```

AgentIM Lite runs on one database; several databases (organisation units)
need AgentIM. The data is the same: moving between them is replacing the
binary.

Options: `--config`, `--db` (single SQLite file), `--addr`, `--demo`, `--stream-idle-secs`,
`--log-format text|json`, `--metrics-token`, `--no-exit-on-storage-failure`.
Endpoints: `/ws`, `/mcp` (MCP for agent keys), `/metrics`, `/healthz`, `/readyz`, `/v1` (open API).

| Path (in `crates/agentim-core`) | |
|---|---|
| `src/storage.rs` | Dialects, connections, schema, durability checks |
| `src/db.rs` | All business SQL (one copy for SQLite, PostgreSQL, MySQL) |
| `src/shard.rs` | One writer per database: group commit, fault handling |
| `src/logic.rs` | Business logic run by the writers |
| `src/hub.rs` | Routing by conversation, cross-database reads, reconcile, migration |
| `src/registry.rs` | Sessions and bounded fan-out (resync on overflow) |
| `src/edition.rs`, `src/cli.rs` | AgentIM Lite / AgentIM, the shared command line |
| `src/metrics.rs` | Prometheus metrics |
| `src/guard.rs`, `src/policy.rs` | Guardrails and policy hooks |
| `src/hub/sync.rs` | Sync and acknowledgements per device |
| `src/hub/agents.rs`, `src/mcp.rs` | Agent registration and keys, the MCP endpoint |
| `src/server.rs`, `src/open_api.rs` | HTTP + WebSocket, open API |
| `src/demo.rs`, `web/index.html` | Demo data, scripted agents, web client |
| `examples/agent.mjs` | External agent over the WebSocket protocol |

Design and test results: [docs/02-architecture.md](../docs/02-architecture.md) §4–§5 and [docs/05-operations.md](../docs/05-operations.md).
