# 调研：海外 IM + AI 市场与竞品

> 2026 年 10 月核实。价格和功能以各官网当时公开的信息为准，可能已经变化。标注"存疑"的条目，不同来源之间有矛盾。

## 1. 结论

1. 海外 IM 正在从"在 IM 里加 AI"转向"AI Agent 是一等公民"。
2. 流式推送、Agent 成员、上下文检索、工具调用、取消与打断、多端续传、MCP，已经是基础配置。
3. **群聊流式没有成熟的开源实现**：Telegram 的流式草稿只支持私聊；Matrix 的 MSC4471 还停留在提案阶段；Ably 的频道模型没有公开针对 10 万人群做过优化。
4. AI 的收费点正在从"AI 功能"转向"AI 治理"：token 计量、模型授权、工具审批、访问范围。
5. **"开源 + Rust + 自托管"已经有人在做**（Tuwunel、Continuwuity），OpenIM 也在宣传 AI 网关。真正的空位是：**把 Agent 治理做进协议的开源 IM。**
6. 局限：以上全部来自公开资料和厂商定价页，还没有访谈过目标用户，也没有市场规模和付费意愿的数据。

## 2. 平台动向

| 平台 | 动向 |
|---|---|
| Slack | 2026-02-17 实时搜索 API 和 MCP Server GA。按用户的 OAuth 权限访问，不需要把数据导出 |
| Telegram | `sendMessageDraft`：9.3（2025-12-31）加入，9.5（2026-03）开放给所有 bot。**只支持私聊**；草稿是 30 秒的临时预览，生成结束后必须用 `sendMessage` 落地。2026 年还新增了：群内临时消息（只对某个用户和 bot 可见，10.2）、Guest 模式（bot 能在自己不是成员的聊天里回复，10.0）、bot 之间通信（双方都开启才行）、富消息与 Thinking 块的流式（10.1）、停止生成按钮（`can_stop`，10.3） |
| Meta / WhatsApp | 2026-06 发布 Business Agent，覆盖 WhatsApp、Messenger、Instagram。用 Private Processing（机密计算 + 远程证明 + OHTTP）在端到端加密的前提下运行 AI |
| Matrix / Element | 欧洲政府采用在增加；MSC4471（流式临时事件更新，动机就是 LLM 流式）2026 年仍是提案 |
| A2A | v1.0 是首个稳定版（2026 年初），新增签名 Agent Card、多租户，支持 JSON-RPC / gRPC / HTTP+JSON 三种绑定；超过 150 家组织支持 |
| MCP | 2025-11-25 版加入 Tasks（实验性），授权改为 OAuth Client ID Metadata Documents，并新增机器对机器的 client credentials |
| AG-UI | 事件化的 Agent ↔ 前端协议：文本、工具调用、运行生命周期、状态快照和增量 |

## 3. 竞品

### 3.1 开发者 SDK / 基础设施（闭源 SaaS）

| 产品 | 免费 | 付费 | AI |
|---|---|---|---|
| **Stream** | Build：1,000 MAU、100 并发；Maker 计划每月 $100 额度 | Start $399/月、Elevate $599/月（按年付） | AI SDK：频道内托管 AI 助手，支持流式、工具调用、取消；企业版有 AI 审核附加项 |
| **Sendbird** | 试用：1,000 MAU、20 峰值并发 | Starter $349 起、Pro $499 起 | AI 业务已独立为 delight.ai，重心转向 AI 客服 |
| **Ably AI Transport** | 有免费层 | 按用量（消息数、连接数、频道时长） | 一条回复是一条可追加的消息，状态为 `streaming / complete / cancelled`；默认 40ms 合并窗口，可调范围 0–500ms；晚加入的客户端拿到累积内容；按发布的消息计费而不是按 token。官方文档承认：进程在 `run.end` 之前被杀，run 会一直处于打开状态 |
| **CometChat** | — | — | 2026-07 发布 AI Agents in Chat：自带 LLM、RAG、工具调用 |

### 3.2 团队协作

| 产品 | 要点 |
|---|---|
| **Slack**（闭源） | 免费版：90 天历史；Business+ 起有 AI 和 SAML / SCIM；Enterprise+ 有 DLP、法律保全、审计日志、HIPAA；**EKM**（客户托管密钥）是企业附加项，采用信封加密：数据密钥按组织、工作区、频道、小时划分范围，内存缓存 5 分钟 |
| **Mattermost**（开源核心） | 已取消免费版，只有限制用途的评估版。官网把 Agentic platform（自带模型、多模型、Agent 控制面、token 计量）列在 Professional 下；第三方追踪站记录 2026-05 token 计量移到 Enterprise，并新增工具策略编辑器、Agent 创建（**存疑**）。Enterprise 支持 5 万并发，Enterprise Advanced 支持 20 万并发 |
| **Rocket.Chat**（开源核心） | AI 是附加项：私有模型、RAG、AI 权限、提示词护栏、MCP；原生 Matrix 联邦 |

### 3.3 开源与 Rust

| 项目 | 要点 |
|---|---|
| **Tuwunel** | Rust 写的 Matrix 服务端，Apache-2.0，conduwuit 的官方继任者，瑞士政府资助 |
| **Continuwuity** | 社区维护的 Rust Matrix 服务端，Apache-2.0 |
| **Stoat（原 Revolt）** | 开源的 Discord 替代品，AGPL-3.0 |
| **OpenIM** | 官方定位为"IM 基础设施 + AI 交互网关"，支持多 Agent 和语音 Agent |
| **Matrix Synapse** | 官方建议生产环境用 PostgreSQL，SQLite 只用于测试或轻负载 |

## 4. 企业版标准清单（各家共性）

SAML SSO + SCIM、审计日志、数据保留策略、法律保全 / eDiscovery、数据驻留、HIPAA、客户托管密钥、专属部署 / 高可用、SLA 与 7×24 支持。

## 5. 来源

- [Telegram Bot API](https://core.telegram.org/bots/api#sendmessagedraft)、[更新日志](https://core.telegram.org/bots/api-changelog)
- [Ably：Message per response](https://ably.com/docs/ai-transport/token-streaming/message-per-response)、[Ably 计费示例](https://ably.com/docs/platform/pricing/examples/ai-chatbot.md)
- [Stream 定价](https://getstream.io/chat/pricing/)、[Sendbird 定价](https://sendbird.com/pricing/chat)、[Mattermost 定价](https://mattermost.com/pricing/)、[PricingSaaS：Mattermost 变化](https://pricingsaas.com/companies/mattermost/diffs/2026W21)、[Rocket.Chat 方案](https://www.rocket.chat/plans)
- [Slack：MCP 与实时搜索 GA](https://slack.com/blog/news/mcp-real-time-search-api-now-available)、[Slack EKM 工程解读](https://slack.engineering/engineering-dive-into-slack-enterprise-key-management)
- [Tuwunel](https://github.com/matrix-construct/tuwunel)、[Continuwuity](https://continuwuity.org/introduction)、[Stoat](https://en.wikipedia.org/wiki/Stoat_(software))、[OpenIM](https://docs.openim.io/guides/introduction)、[CometChat AI Agents](https://www.producthunt.com/products/cometchat/launches/ai-agents-in-chat)
- [MSC4471](https://github.com/matrix-org/matrix-spec-proposals/pull/4471)、[The Register：Matrix 与政府 IT](https://www.theregister.com/2026/02/09/matrix_element_secure_chat/)
- [WhatsApp Private Processing](https://engineering.fb.com/2025/04/29/security/whatsapp-private-processing/)
- [A2A 一周年（Linux Foundation）](https://linuxfoundation.org/press/a2a-protocol-surpasses-150-organizations-lands-in-major-cloud-platforms-and-sees-enterprise-production-use-in-first-year)、[MCP 授权 2025-11-25](https://modelcontextprotocol.io/specification/2025-11-25/basic/authorization)、[AG-UI](https://docs.ag-ui.com/concepts/architecture)
