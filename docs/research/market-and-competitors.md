# 调研：海外 IM + AI 市场与竞品

> 调研时间：2026 年 10 月。价格与功能以各官网当时公开信息为准，可能已变化。

## 1. 结论

1. 海外 IM 正在从"在 IM 里加 AI"转向"AI Agent 是一等公民"。
2. 流式推送、Agent 成员、上下文检索、工具调用、取消/打断、多设备续传、MCP 已是**基础配置**。
3. 群聊流式是空白：Telegram 的流式草稿只在私聊生效；Stream、Ably 是通用频道模型，未针对 10 万人群做扇出优化。
4. AI 的收费点正在从"AI 功能"转向"AI 治理"（token 计量、模型授权、工具审批、访问范围）。
5. 商业 SaaS 闭源，开源 IM 的 AI 多为应用层外挂。**开源 + 自托管 + 协议层 AI 原生 + 大群流式**是空位。

## 2. 平台动向

| 平台 | 动向 |
|---|---|
| Slack | 发布实时搜索 API 与 MCP Server，Agent 可在不批量导出数据的情况下访问对话与文件；定位为"人与 Agent 协作的系统" |
| Telegram | Bot API 9.5（2026-03）推出原生流式（`sendMessageDraft`），此前需反复编辑消息模拟；草稿流式仅私聊有效 |
| Meta / WhatsApp | 2026-06 发布 Business Agent 及 Business Agent Platform，覆盖 WhatsApp、Messenger、Instagram，直接进入 AI 客服层 |
| A2A 协议 | Linux Foundation 托管，一周年时超过 150 家组织支持，集成进 Google、Microsoft、AWS 平台；与 MCP 互补 |

## 3. 竞品功能与分层

### 3.1 开发者 SDK / 基础设施（闭源 SaaS）

**Stream**
- 免费：1,000 MAU、100 并发，全部聊天功能；另有面向小团队（<5 人、月收入 <1 万美元）的免费 Maker 计划。
- 付费：Start（$399/月）、Elevate（$599/月，增加多租户、高级搜索、HIPAA、翻译）。
- 企业：AI 审核、99.999% SLA、专属服务器、SAML/SSO、7×24 支持。
- AI：提供 AI SDK，在频道内托管 AI 助手，支持流式（`partialUpdateMessage`）、输入指示、工具调用、取消、Mem0 长期记忆。
- 规律：规模能力不收费，收费的是用量、合规和服务。

**Sendbird**
- 免费试用：1,000 MAU、峰值并发 20。
- 付费：Starter（$349/月起）、Pro（$499/月起，增加翻译、数据导出、图片审核、超级群）。
- 企业：专属服务器、附加产品、多级支持。
- 限制：普通群每频道 100 人（专属实例 300 人），消息保留 6 个月。
- AI：AI 业务已独立为 delight.ai 品牌，重心转向 AI 客服。

**Ably（AI Transport）**
- 免费：200 并发、每月 600 万条消息，保留 1 天。
- 付费：Standard（$29/月 + 用量）、Pro（$399/月 + 用量）；保留 30 天 / 365 天。
- 企业：无限容量、99.999% SLA、专属集群、SAML/SCIM、数据区域约束、审计日志。
- AI：**AI Transport 全部功能在所有层级可用**——一条回复是一条可追加的消息，合并推送（rollup），断线续传，取消、打断、并发轮次、工具调用、多设备会话、对话分支。按发布消息计费而非按 token。

### 3.2 团队协作

**Slack（闭源）**
- 免费：90 天历史、10 个应用，无 AI。
- Pro（$7.25/人/月）：无限历史、摘要、会议纪要、第三方 AI 助手接入。
- Business+（$15/人/月）：Slackbot Agent、AI 搜索、AI 工作流、SAML、SCIM、数据驻留、全量导出。
- Enterprise+：企业搜索、数据防泄漏、信息隔离、法律保全、Discovery API、审计日志、HIPAA，EKM 附加。

**Mattermost（开源核心）**
- 免费：限制用途的评估版。
- Professional：协作、访问控制、**Agent 平台**（自带模型、多模型、Agent 控制面、token 计量、MCP Server）。
- Enterprise：合规（保留、法律保全、合规导出）、高可用（5 万并发）、联邦。
- Enterprise Advanced：20 万并发、零信任、气隙部署、涉密信息管控。
- AI 治理亮点：按用户/组/团队授权模型并追踪 token；工具策略编辑器（按场景要求审批或禁用工具）；用户调用 Agent 时委托自身权限；终端用户可创建并共享 Agent。

**Rocket.Chat（开源核心）**
- 免费：Starter，面向评估、小企业、非营利。
- 付费：按行业分为 Commercial / Government / Defense。
- AI 为附加项：自带私有模型、摘要、RAG 问答、AI 权限、来源引用、提示词护栏、混合搜索、MCP。
- 原生 Matrix 联邦。

## 4. 企业版标准清单（各家共性）

SAML SSO + SCIM、审计日志、数据保留策略、法律保全 / eDiscovery 导出、数据驻留、HIPAA、专属部署 / 高可用、SLA 与 7×24 支持。

## 5. 来源

- [Stream Chat Pricing](https://getstream.io/chat/pricing/)
- [Stream Chat AI SDK](https://getstream.io/chat/docs/sdk/react/guides/ai-integrations/stream-chat-ai-sdk/)
- [Sendbird Chat Pricing](https://sendbird.com/pricing/chat)
- [Ably Pricing](https://ably.com/pricing)
- [Ably AI Transport – Token streaming](https://ably.com/docs/ai-transport/token-streaming)
- [Slack Pricing](https://slack.com/pricing)
- [Slack MCP Server & Real-time Search API](https://slack.dev/?p=8315)
- [Mattermost Pricing](https://mattermost.com/pricing/)
- [Rocket.Chat Plans](https://www.rocket.chat/plans)
- [Meta Business Agent（ppc.land）](https://ppc.land/meta-business-agent-brings-ai-customer-service-to-whatsapp-globally/)
- [A2A 一周年（Linux Foundation）](https://linuxfoundation.org/press/a2a-protocol-surpasses-150-organizations-lands-in-major-cloud-platforms-and-sees-enterprise-production-use-in-first-year)
