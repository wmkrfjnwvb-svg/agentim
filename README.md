# AgentIM

**Open-source, AI-native instant messaging infrastructure written in Rust.**
**开源、AI 原生的 Rust 即时通讯底层框架。**

> Free for individuals. Enterprise-ready. AI agents as first-class citizens — with humans always able to step in.
>
> 个人免费 · 企业可控 · AI 原生：AI 可以与任何人、任何 AI 交流，人可以随时介入、过滤、监控。

## Core guarantees / 核心保证

| | |
|---|---|
| **Stable / 稳定** | Only one strongly-consistent layer (Multi-Raft conversation shards); everything else is stateless or soft state. |
| **No message loss / 不丢消息** | A message is ACKed only after a Raft quorum commit. Push is a hint, pull is the source of truth. |
| **Group messaging / 群发** | Groups up to 100,000 members (default 100–1,000), gateway-level fan-out. |
| **Ordering / 消息顺序** | Monotonic per-conversation sequence numbers assigned atomically with persistence. |

**Scale target / 规模目标**: 1M+ concurrent connections, horizontally scalable clusters.
**License / 协议**: Apache 2.0 (open core: community edition fully open; enterprise modules commercial).

## Documents / 文档

| Doc | Content |
|---|---|
| [01 产品需求](docs/01-product-requirements.md) | 定位、目标用户、核心需求、非功能指标 |
| [02 架构设计](docs/02-architecture.md) | 消息模型、可靠性与顺序、群聊扩散、整体分层、容量估算 |
| [03 AI 原生能力](docs/03-ai-native.md) | Agent 身份、流式消息、人工介入、过滤、监控、安全护栏 |
| [04 版本划分](docs/04-editions.md) | 社区版与企业版的功能边界、开源核心原则 |
| [调研：市场与竞品](docs/research/market-and-competitors.md) | 海外 IM + AI 需求、竞品功能与分层对比 |
| [05 需求与架构评审](docs/05-review.md) | 调研核实、P0/P1/P2 修订项、v0.2 消息模型、实施路线 |
| [06 单机版](docs/06-standalone.md) | 单机版设计、协议、测试与实测结果 |

## Status / 状态

Single-node edition available in [`server/`](server) (validates the v0.2 model; not yet optimized for scale).
单机版已可运行（`cd server && cargo run --release -- --demo`），用于验证 v0.2 消息模型与 AI 介入模型，尚未做规模优化。
