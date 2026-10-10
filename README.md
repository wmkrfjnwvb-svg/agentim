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
| [01 产品](docs/01-product.md) | 定位、首要场景、核心保证 G1–G6、功能需求与状态、指标、版本划分 |
| [02 架构](docs/02-architecture.md) | 消息模型（事件日志 + 侧信道）、可靠性、**组提交方案（待确认）**、存储抽象、集群 |
| [03 AI 原生](docs/03-ai-native.md) | Agent 身份、信任模型、流式、介入、**审批 v2、工具网关（待确认）**、护栏 |
| [04 安全与合规](docs/04-security-compliance.md) | 身份、TLS、静态加密与密钥层级、E2EE 立场、海外法规对照 |
| [05 运维、测试与客户端](docs/05-operations.md) | 部署、日志规范、指标、Linux/Windows 测试矩阵、客户端路线、演示与协议 |
| [06 迭代计划](docs/06-plan.md) | 待确认事项 D1–D9、实施顺序、P0 进度 |
| [调研：市场与竞品](docs/research/market-and-competitors.md) | 平台动向、竞品功能与分层（已核实） |

## Status / 状态

Single-node edition available in [`server/`](server) (validates the v0.2 model; not yet optimized for scale).
单机版已可运行（`cd server && cargo run --release -- --demo`），用于验证 v0.2 消息模型与 AI 介入模型，尚未做规模优化。
