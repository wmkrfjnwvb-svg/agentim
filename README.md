# AgentIM

**Open-source, AI-native instant messaging infrastructure in Rust — where people and AI agents talk, work together and stay under human control.**

**开源、AI 原生的 Rust 即时通讯底层框架：人和 AI、AI 和 AI 在这里交流协作，人始终可以介入。**

- **Messaging core**: one-to-one and group conversations with a gap-free, per-conversation event log. A message is acknowledged only after it is durably committed (fsync). Pushes are a hint; pulling by sequence number is the source of truth.
- **AI agents as first-class members**: server-assigned sender types (an agent cannot pose as a human), streaming replies with an explicit state machine, owner approval of drafts, whispers, interruption, takeover, recall, and guardrails (hop limit, loop breaker, rate limits, budgets, prompt-injection flags, DLP).
- **Your database**: SQLite, PostgreSQL or MySQL, same behaviour on all three (the same test suite runs on each). Subsidiaries with stricter privacy can be stored in their own database.
- **Operable**: Prometheus metrics, structured logs, health and readiness endpoints; tested on Linux and Windows.

Status: **single-node edition**, under active development towards an MVP. See the [plan](docs/06-plan.md).

## Quick start

```bash
cd server
cargo run --release -- --demo          # open http://127.0.0.1:8080 and log in as alice, bob or carol
```

Configuration (databases, organisation units, writer settings): [`server/agentim.example.toml`](server/agentim.example.toml). More commands: [`server/README.md`](server/README.md).

## Guarantees and how they are tested

| Guarantee | What it means | Tested by |
|---|---|---|
| No loss | An acknowledged message survives `kill -9` and restarts | Real process killed mid-traffic on Linux and Windows; failed commits inject faults and must not acknowledge |
| Order | Every member sees the same gap-free sequence | Every test checks contiguous sequence numbers and in-order pushes |
| Correct business rules | Permissions, visibility, approval, deduplication are enforced by the server | 29 business scenarios and 12 concurrency races × SQLite, PostgreSQL, MySQL |
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

**Where the limit is.** Write throughput tops out around 1,300 msg/s per database today. fsync is no longer the bottleneck (group commit puts up to hundreds of messages in one commit); the cost is the fixed overhead per SQL statement — about nine per message. Planned optimisations (fewer statements per message, fan-out off the writer thread, several writers per database) are described in [docs/02 §4.8](docs/02-architecture.md); the target is 10,000 msg/s per node.

## Documents / 文档

| Doc | Content |
|---|---|
| [01 产品](docs/01-product.md) | 定位、核心保证、功能与状态、指标、版本划分 |
| [02 架构](docs/02-architecture.md) | 消息模型、可靠性、组提交、存储抽象与多库路由、成员生命周期、集群 |
| [03 AI 原生](docs/03-ai-native.md) | Agent 身份、信任模型、流式、介入、审批、工具网关、护栏、**AI 核心组件（Agent 之间的通信、任务、记事本、管理、A2A / MCP）** |
| [04 安全与合规](docs/04-security-compliance.md) | 身份与设备、加密（静态加密、MLS 端到端加密）、海外法规、集团内的私密性 |
| [05 运维、测试与客户端](docs/05-operations.md) | 部署、日志、指标、测试矩阵与结果、SDK 与客户端、管理系统 |
| [06 迭代计划](docs/06-plan.md) | 已确认和待确认的决策、MVP 实施顺序 |
| [07 开放 API](docs/07-open-api.md) | 给开发者后端操作 IM 的服务端 API：鉴权、以谁的身份操作、接口一览、约定、测试计划 |
| [调研：市场与竞品](docs/research/market-and-competitors.md) | 平台动向、竞品功能与分层 |
| [调研：开发者需求](docs/research/developer-needs.md) | IM 开发者需要什么、同行提供了哪些开发者支持、对照现状和对 MVP 的调整 |

## License

Apache 2.0. Enterprise modules (customer-managed keys, SSO/SCIM, eDiscovery, multi-region) will be offered under a commercial licence; everything a developer needs is open source.
