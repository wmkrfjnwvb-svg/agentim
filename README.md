# AgentIM

**AI-native chat infrastructure for developers, in Rust.** Add chat and AI assistants to your own product: one-to-one and group conversations with a gap-free, ordered log, and AI agents built with any framework (OpenAI Agents SDK, Claude Agent SDK, LangGraph, Vercel AI SDK, or plain MCP) joining as members next to people. Agents stream replies, ask people to decide, wait for approval and can be taken over, stopped or destroyed at any time. One program, `agentim`, runs on one machine or as a cluster, on SQLite, PostgreSQL or MySQL. Apache 2.0, every feature free.

## Quick start

```bash
# Linux and macOS
curl -fsSL https://github.com/wmkrfjnwvb-svg/agentim/releases/latest/download/install.sh | sh
# Windows (PowerShell)
irm https://github.com/wmkrfjnwvb-svg/agentim/releases/latest/download/install.ps1 | iex
# or Docker
docker run -p 8080:8080 -v agentim:/var/lib/agentim ghcr.io/wmkrfjnwvb-svg/agentim --db /var/lib/agentim/agentim.db

agentim          # in an empty folder: creates agentim.db and its master key, prints the address and the console setup link
agentim --demo   # or try it: open http://127.0.0.1:8080 and log in as alice, bob or carol
```

The install scripts put `agentim` in `~/.agentim/bin` without root and stop if the download's SHA-256 does not match. Every release file is signed with Sigstore and has a build provenance attestation (`gh attestation verify <file> --repo wmkrfjnwvb-svg/agentim`); see [docs/12 §1](docs/12-developer-experience.md). The first release is v0.1.0; until it is out, build from source:

```bash
cd server
cargo run --release --bin agentim -- --demo
```

Back up `agentim.db.key` with the database: without it the encrypted content cannot be read. In Docker the console setup link only works from inside the container; create the first owner with `docker exec <container> agentim admin-key create --db /var/lib/agentim/agentim.db --name me --role owner` and then `agentim admin --url http://127.0.0.1:8080 --key <key> admin-create <name> --role owner` (the password is read from standard input; [docs/12 §4](docs/12-developer-experience.md)).

- **Your users log in with your own accounts**: `agentim signing-key create --name backend` prints a signing key; your backend signs each user a client token with it (an HS256 JWT, `createUserToken` in the server SDKs; no call to AgentIM) and your app connects with it. See [docs/13 §4.2](docs/13-roles-and-entry-points.md).
- **Your backend**: `agentim api-key create --name backend --scopes users:write,messages:write` prints a key for the open API (`/v1`, OpenAPI 3.1 at `/v1/openapi.json`): create users with your own ids, manage conversations, send as a user or as the system, issue agent keys. Server SDKs for [Node, Python and Go](sdk/README.md). See [docs/07-open-api.md](docs/07-open-api.md).
- **An agent**: your backend creates it with the open API (`PUT /v1/users/helper` with `{"kind": "agent", "owner": "alice"}`, or `agentim admin agent-create helper --owner alice`), which returns the agent's key once, with an MCP config block for any MCP client (Claude, Cursor, OpenAI Agents SDK; endpoint `POST /mcp`) and the WebSocket address. Agent SDKs for [Node and Python](sdk/README.md#agent-sdks) (no dependencies). See [docs/03-agents.md](docs/03-agents.md).
- **Your app**: client SDKs keep the connection, sync, the offline queue, the local cache and history for you, with one Rust core on every platform: the browser (`@agentimhq/client-web`, the core in WebAssembly, an encrypted IndexedDB cache, one connection shared by all tabs), React components (`@agentimhq/react`), Swift, Kotlin, C# (.NET, Unity), Dart (Flutter) and React Native. See [sdk/README.md](sdk/README.md#client-sdks) and [docs/12 §2](docs/12-developer-experience.md).
- **The console** at `/console`: on first start the server prints a one-time setup link (open it on the same machine) to create the first owner. Developers manage keys, people, agents, conversations (reading content needs a reason and is audited), reports and client permissions; operators see databases and the cluster. `agentim admin --url … --key …` does the same from the command line. See [docs/12 §4](docs/12-developer-experience.md).
- **Configuration** (databases, writer settings, webhooks, TLS, login): [`server/agentim.example.toml`](server/agentim.example.toml). More commands: [`server/README.md`](server/README.md).

## Features

**Done**

- **Messaging**: one-to-one and group conversations, edit, recall, replies, mentions, read state and unread counts across devices, streaming messages, custom message kinds with a plain-text fallback, signals, deduplication. Membership: remove, leave, roles, transfer, dissolve, block. A message is acknowledged only after it is durably committed; pulling by sequence number is the source of truth, pushes are a hint.
- **AI agents as members**: server-assigned sender types (an agent cannot pose as a human; AI label always shown), owner approval of drafts, asking people to decide, whispers, interruption, takeover, kill switch and destroy, agent-to-agent wake-ups with budgets, task channels, history, checkpoints and search, guardrails (hop limit, loop breaker, rate limits, budgets, prompt-injection flags, DLP), an MCP server.
- **For your backend**: open API with scoped API keys, idempotency keys, rate limits and RFC 9457 errors; webhooks (signed, ordered, at-least-once); a before-send hook; custom fields; protocol version negotiation.
- **Security**: encryption at rest on by default (AES-256-GCM, per-conversation keys, crypto-shredding on delete), client tokens your backend signs itself (JWT, no IM passwords for your users), hashed agent keys, per-device logout, built-in TLS, Origin checks.
- **Client SDKs**: one Rust core (reconnects with backoff and jitter, the offline queue sent once and in order, an encrypted local cache, paging back through history, detection of gaps), used by the browser SDK, React components, and Swift, Kotlin, C#, Dart and React Native wrappers. Each wrapper runs the same scenario against a real server in CI.
- **Releases**: static builds for Linux (x86_64, aarch64), macOS and Windows, one-command install that checks the download, Sigstore signatures and build provenance, images for amd64 and arm64 with an SBOM, the Helm chart as an OCI package; dependencies checked for vulnerabilities and licenses on every change and weekly.
- **Operations**: SQLite, PostgreSQL or MySQL with the same behaviour (the same test suite runs on each), automatic schema migration, Prometheus metrics and alerts, health and readiness endpoints, a Docker image, Docker Compose and a Helm chart; tested on Linux and Windows.

**Planned** (order in [docs/06-plan.md §3](docs/06-plan.md))

- Packages for every client SDK (npm, SwiftPM, Maven Central, NuGet, OpenUPM, pub.dev), with iOS, Android and Unity builds tested on their simulators.
- A documentation site and three example apps.
- Files and images; offline push notifications (APNs, FCM); import from other chat providers.
- A hosted cloud (later).

## Guarantees and how they are tested

| Guarantee | What it means | Tested by |
|---|---|---|
| No loss | An acknowledged message survives `kill -9` and restarts | Real process killed mid-traffic on Linux and Windows; failed commits inject faults and must not acknowledge |
| Order | Every member sees the same gap-free sequence | Every test checks contiguous sequence numbers and in-order pushes |
| Correct business rules | Permissions, visibility, approval, deduplication are enforced by the server, for clients and the open API alike | Business scenarios, membership, extensions, webhooks, devices, login and open API suites plus concurrency races × SQLite, PostgreSQL, MySQL ([coverage](docs/05-operations.md#5-测试)) |
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

## Documentation

| Doc | Content |
|---|---|
| [01 Product](docs/01-product.md) | Positioning, users, guarantees, features, pricing |
| [02 Architecture](docs/02-architecture.md) | Conversation log, writer, storage, protocol |
| [03 Agents](docs/03-agents.md) | Agents as members, approval and intervention, agent-to-agent, integrations |
| [04 Security and compliance](docs/04-security-compliance.md) | Accounts, encryption, GDPR, AI Act |
| [05 Operations](docs/05-operations.md) | Running, monitoring, tests and coverage |
| [06 Plan](docs/06-plan.md) | Status, decisions, order |
| [07 Open API](docs/07-open-api.md) | `/v1` for your backend |
| [08 Sync and client](docs/08-sync-and-client.md) | Sync protocol and client SDK rules |
| [11 Cluster](docs/11-cluster.md) | Shards, leases, failover |
| [12 Developer experience](docs/12-developer-experience.md) | Install, SDKs, UI components, console, docs site |

The docs are in Chinese.

## License

Apache 2.0.
