# AgentIM

**Open-source, AI-native instant messaging infrastructure in Rust — where people and AI agents talk, work together and stay under human control.**

**开源、AI 原生的 Rust 即时通讯底层框架：人和 AI、AI 和 AI 在这里交流协作，人始终可以介入。**

**The durable, ordered, multi-party and governed communication and record layer for agents built with any framework. Move an agent in seconds; destroy it at any time.**

**任何框架写的 Agent 都能接进来的、持久、有序、多方、可治理的通信和记录层。秒迁移，随时销毁。**

- **Messaging core**: one-to-one and group conversations with a gap-free, per-conversation event log. A message is acknowledged only after it is durably committed (fsync). Pushes are a hint; pulling by sequence number is the source of truth.
- **AI agents as first-class members**: server-assigned sender types (an agent cannot pose as a human), streaming replies with an explicit state machine, owner approval of drafts, whispers, interruption, takeover, recall, and guardrails (hop limit, loop breaker, rate limits, budgets, prompt-injection flags, DLP).
- **Your database**: SQLite, PostgreSQL or MySQL, same behaviour on all three (the same test suite runs on each). Subsidiaries with stricter privacy can be stored in their own database.
- **For developers**: an open API (`/v1`, OpenAPI 3.1 at `/v1/openapi.json`) for your backend: create users with your own ids, issue login tokens, manage conversations, send as a user or as the system, batch send, audited history. API keys with scopes, idempotency keys, rate limits, RFC 9457 errors. Server SDKs for [Node, Python and Go](sdk/README.md). Webhooks (signed, ordered, at-least-once) and a before-send hook; custom message kinds with a plain-text fallback; protocol version negotiation.
- **Accounts and security**: encryption at rest on by default (AES-256-GCM, per-conversation keys, crypto-shredding on delete), hashed tokens with per-device logout, built-in passwords (Argon2id) with TOTP two-factor login, built-in TLS, Origin checks.
- **Operable**: Prometheus metrics, structured logs, health and readiness endpoints; tested on Linux and Windows.

Status: **single-node edition**, under active development towards an MVP. See the [plan](docs/06-plan.md).

## Quick start

```bash
cd server
cargo run --release --bin agentim-lite -- --demo   # AgentIM Lite; open http://127.0.0.1:8080 and log in as alice, bob or carol
cargo run --release --bin agentim -- --demo        # AgentIM (Lite plus the platform modules)
```

Open API: `agentim api-key create --name backend --scopes users:write,tokens:issue,messages:write` prints a key; see [docs/07-open-api.md](docs/07-open-api.md).

Connect an agent: `agentim agent create --name helper --owner alice --url http://127.0.0.1:8080` prints the agent's key once, with an MCP config block for any MCP client (Claude, Cursor, OpenAI Agents SDK; endpoint `POST /mcp`) and the WebSocket address. People can do the same in the app (`create_agent`); see [docs/10-agent-ecosystem.md §5.1](docs/10-agent-ecosystem.md). Agent SDKs for Node and Python (no dependencies): [sdk/README.md](sdk/README.md#agent-sdks).

Configuration (databases, organisation units, writer settings, webhooks, TLS, accounts): [`server/agentim.example.toml`](server/agentim.example.toml). More commands: [`server/README.md`](server/README.md).

## Guarantees and how they are tested

| Guarantee | What it means | Tested by |
|---|---|---|
| No loss | An acknowledged message survives `kill -9` and restarts | Real process killed mid-traffic on Linux and Windows; failed commits inject faults and must not acknowledge |
| Order | Every member sees the same gap-free sequence | Every test checks contiguous sequence numbers and in-order pushes |
| Correct business rules | Permissions, visibility, approval, deduplication are enforced by the server, for clients and the open API alike | Business scenarios, membership, extensions, webhooks, devices, accounts and open API suites plus concurrency races × SQLite, PostgreSQL, MySQL ([coverage](docs/05-operations.md#5-测试)) |
| Slow clients cannot hurt others | A client that stops reading loses only its own pushes and is told to resync | Scale tests in CI, nightly |
| Scale | Tens of thousands of connections per node, large groups | 20,000 connections with a reconnect storm, 2,000-member groups, nightly in CI |

## Single-node performance

Measured with [`server/bench/single-node.sh`](server/bench/single-node.sh) — run it on your own hardware to get your numbers. Machine: 4-core Intel Xeon @ 2.1 GHz cloud VM, 15 GB RAM, the load generator on the same machine. Databases: SQLite (bundled), PostgreSQL 16, MariaDB 10.11 (MySQL protocol). Every acknowledgement waits for fsync. Every run also checks correctness: gap-free logs, exactly-once messages, in-order pushes, nothing missing.

**Writes and fan-out** (throughput / ACK latency p50 · p99)

| Scenario | SQLite | PostgreSQL | MySQL (MariaDB) |
|---|---|---|---|
| 1 client, own conversation | 582 msg/s · 1.5 · 3.5 ms | 604 msg/s · 1.5 · 3.0 ms | 385 msg/s · 2.2 · 5.7 ms |
| 10 clients | 1,080 msg/s · 8.2 · 13 ms | 995 msg/s · 8.5 · 14 ms | 660 msg/s · 13 · 26 ms |
| 50 clients | 1,218 msg/s · 35 · 60 ms | 1,219 msg/s · 36 · 58 ms | 919 msg/s · 47 · 80 ms |
| 200 clients | 1,320 msg/s · 137 · 182 ms | 1,327 msg/s · 134 · 192 ms | 1,034 msg/s · 170 · 253 ms |
| Group of 50, everyone sending, 10 agents streaming (251k pushes) | 1,151 msg/s · 38 · 56 ms | 794 msg/s · 53 · 101 ms | 817 msg/s · 55 · 73 ms |
| Group of 200, everyone sending, 10 agents streaming (1.2M pushes) | 685 msg/s · 166 · 765 ms | 641 msg/s · 153 · 1,000 ms | 514 msg/s · 235 · 962 ms |

**Connections and delivery** (end-to-end = from the sender's send to each receiver's push)

| Scenario | SQLite | PostgreSQL | MySQL (MariaDB) |
|---|---|---|---|
| 10,000 connections, groups of 100, 1% sending — end-to-end p50 · p99 | 3 · 21 ms | 4 · 11 ms | 4 · 14 ms |
| …all 10,000 dropped at once and reconnected (no message lost) | 1.0 s | 1.4 s | 1.4 s |
| Server memory per connection | 13.6 KB | 13.5 KB | 13.5 KB |
| 1,000-member group, 50 msg/s (50k pushes/s) — end-to-end p50 · p99 | 7 · 13 ms | 7 · 17 ms | 7 · 15 ms |
| 200-member group, 20 members stop reading, 2 KB messages — others' p50 · p99 | 4 · 12 ms | 4 · 14 ms | 5 · 29 ms |
| Restart after `kill -9`, process start to ready | 236 ms | 128 ms | 131 ms |

Nightly CI on a standard GitHub runner adds 20,000 connections with a reconnect storm (all back in 2.3 s, p99 7 ms) and a 2,000-member group (12M deliveries, p99 43 ms); see [docs/05 §5.4](docs/05-operations.md).

**Encryption at rest costs about 2%.** It is on by default (AES-256-GCM, envelope keys). Same machine, SQLite, write path (`loadtest --solo --clients 50 --messages 400`, two runs each): **818 msg/s off, 800 msg/s on**, ack p50 53 ms in both, p99 91–109 ms vs 94–98 ms.

**Where the limit is.** Write throughput tops out around 1,300 msg/s per database today. fsync is no longer the bottleneck (group commit puts up to hundreds of messages in one commit); the cost is the fixed overhead per SQL statement — about nine per message. Planned optimisations (fewer statements per message, fan-out off the writer thread, several writers per database) are described in [docs/02 §4.8](docs/02-architecture.md); the target is 10,000 msg/s per node.

## Cluster

Several nodes share the databases; each conversation is written by the node holding its shard (lease with fencing), any node answers any client, and a node that dies is replaced within about 15 s with no acknowledged message lost ([docs/11](docs/11-cluster.md)). Planned operations (adding or draining a node, rolling upgrades) cost no request. With database replicas, a message is acknowledged only once the replicas have it, so a database failover keeps every acknowledged message. Deploy with [`deploy/`](deploy): a Docker image, a three-node Docker Compose setup, a Helm chart, and Prometheus alerts; `agentim cluster status` and `agentim cluster drain --node <id>` for operators.

Measured with [`server/bench/cluster.sh`](server/bench/cluster.sh): the same machine as above, all nodes, the database and one load generator per node on it (so this shows the overhead of forwarding, not the scaling of separate machines). Throughput / ACK latency p50 · p99:

| | 1 node | 3 nodes |
|---|---|---|
| PostgreSQL, write path (50 clients per node) | 874 msg/s · 53 · 78 ms | 1,988 msg/s · 42 · 164 ms |
| PostgreSQL, group of 50 per node, all sending | 882 msg/s · 51 · 73 ms | 1,038 msg/s · 105 · 517 ms |
| MySQL (MariaDB), write path | 680 msg/s · 68 · 105 ms | 1,359 msg/s · 68 · 254 ms |
| MySQL (MariaDB), group of 50 per node | 600 msg/s · 76 · 131 ms | 965 msg/s · 128 · 515 ms |
