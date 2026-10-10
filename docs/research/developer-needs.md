# 调研：IM 开发者需要什么，同行提供了哪些开发者支持

> 2026-10-10。来源见文末。多数资料来自厂商自己的文档和比较文章（会偏向自家），结论以多家一致的部分为准；没有找到正式的开发者问卷数据，**上线前应做 10–20 位开发者访谈来验证**（[06 §4](../06-plan.md)）。

## 1. 同行概况

| 产品 | 形态 | 开发者口碑（多家资料一致的部分） |
|---|---|---|
| Stream（GetStream） | 云服务，只有专属云，不能自托管 | 开发者体验和 UI 组件最好；SDK 覆盖 React、React Native、Flutter、SwiftUI、Jetpack Compose；有 AI 流式消息的官方方案 |
| Sendbird | 云服务，自托管只在企业合同里 | 企业首选，功能最全、审核和合规最强、文档成熟；最贵，并发连接数被卡在月活的 5–10%，超出另收费；2026 年转向"AI 客服平台" |
| CometChat | 云服务，企业版可自托管 | 聊天、语音、视频打包，价格较低，上手快；UI 组件不如 Stream 精致 |
| 腾讯云 IM（TRTC Chat） | 云服务 | 发送前回调（可放行、拒绝、静默丢弃、改写）、自定义消息、云端自定义数据、UIKit |
| OpenIM | 开源，自托管 | 服务端 REST API、发送前后回调、客户端 SDK（含本地存储）；**没有 UI，没有注册登录**，需要自己做；部分音视频不开源 |
| Matrix | 开源协议 + 多个实现 | 去中心化、联邦；生态有大量桥接（Slack、WhatsApp 等）和机器人 |

**开发者最普遍的痛点**：按月活和并发连接计费，费用难以预估，超出部分不透明（中型用户每年 4 万到 15 万美元的量级）；只能用云服务，数据不在自己手里，也很难迁移；OpenIM 这样的开源方案又需要自己补 UI 和账号体系。**这正是"开源 + 自托管 + 开箱可用"的 AgentIM 的机会。**

## 2. 开发者需要的能力清单，以及 AgentIM 的现状

### 2.1 接入与身份

| 能力 | 同行做法 | AgentIM 现状 | 建议 |
|---|---|---|---|
| **用开发者自己的用户体系登录** | 开发者后端用应用密钥签发用户令牌（Stream 的 `createToken`、腾讯云的 UserSig）；开发阶段有免签的开发令牌 | 没有；原计划先做内置的账号密码 | **最优先**。嵌入别人 App 的场景，开发者已经有用户系统，不会再让用户注册一遍 IM 账号。内置账号密码保留，给独立部署用 |
| 服务端 REST API（开放 API） | 都有：建用户、建会话、发系统消息、管理成员、审核、导出 | 只有 WebSocket 协议和演示接口 | 单独设计，和管理 API 分开，方案见 [07](../07-open-api.md) |
| 服务端 SDK | Node、Python、Go、Java、PHP、Ruby、.NET | 没有 | 由 OpenAPI 生成，先做 Node、Python、Go、Java |
| 快速开始 | 十分钟教程、示例应用、免费开发额度 | 有演示，没有教程 | 一条命令的 Docker 启动、各平台的十分钟教程、示例应用 |

### 2.2 客户端 SDK 与 UI

| 能力 | 同行做法 | AgentIM 现状 | 建议 |
|---|---|---|---|
| 离线与重连 | 本地缓存、离线时排队发送、恢复后按顺序发出且不重复；业内评测认为这是 SDK 质量的分水岭 | 服务端已保证（seq、去重、补拉、resync）；客户端 SDK 还没有 | Rust 核心 SDK 必须带本地存储和离线发送队列，并在弱网环境下测试 |
| **UI 组件库** | Stream、Sendbird、CometChat、腾讯云都有；支持主题和逐个替换组件 | 只有演示页面 | **开发者选型的关键**。MVP 1 先做 React 组件库（Web 客户端就用它搭），之后是 React Native、Flutter、iOS、Android |
| 自定义附件 / 自定义渲染 | 按类型注册渲染组件 | 扩展机制已设计（[02 §2.6](../02-architecture.md)） | UI 组件库提供"按消息类型注册渲染器"的接口 |

### 2.3 消息功能（用户默认会有）

| 能力 | 同行 | AgentIM |
|---|---|---|
| 单聊、群聊、已读、未读、编辑、撤回、引用、@ | 都有 | ✅ |
| 文件、图片、语音、视频消息，链接预览 | 都有 | ❌（M5） |
| 表情回应、线程、置顶、投票 | 都有 | ❌ |
| 正在输入、在线状态 | 都有 | ❌ |
| 搜索 | 都有 | ❌ |
| 翻译、斜杠命令、GIF | Stream 等有 | ❌，放之后 |
| 语音视频通话 | CometChat 打包；Stream、Sendbird 单独产品 | 不做；以后提供和 LiveKit 等的集成 |

### 2.4 服务端扩展与集成

| 能力 | 同行做法 | AgentIM |
|---|---|---|
| 事件 Webhook（发送后） | 都有；Stream 还能投递到 SQS / SNS | 已设计（D20） |
| **发送前拦截** | Stream 的 Before Message Send（可以改写或拦截，带签名验证）；腾讯云的发送前回调（放行、拒绝、静默丢弃、改写，可返回自定义错误码） | 有进程内的策略钩子；没有对外的 HTTP 钩子 | 加入扩展机制（D20）：同步 HTTP 钩子，有超时和"超时后放行还是拒绝"的配置 |
| 自定义字段 | 用户、会话、消息上都能挂自定义数据 | 没有 | 加入 D20 |
| 会话类型与权限配置 | Stream 的会话类型：每种类型一套角色和权限 | 角色写死在代码里 | 先做固定角色（M1），可配置的权限放之后 |
| 多租户 | Stream、Sendbird 都有 | 数据模型里有 `tenant_id`，功能没做 | 托管云阶段再做 |

### 2.5 推送、审核与合规

| 能力 | 同行做法 | AgentIM |
|---|---|---|
| 离线推送 | APNs、FCM（国内还有华为、小米等厂商通道）、Web Push；推送模板、免打扰 | ❌ | Web Push 和桌面通知放 MVP 1；APNs、FCM 随手机端 |
| 审核 | 关键词和正则过滤、AI 审核、举报、审核队列、封禁和禁言、慢速模式；允许接入自己的审核服务 | 有 DLP 和注入检测；举报和审核队列在 M10 | 审核也通过发送前拦截接入第三方 |
| 导入导出 | 从其他厂商迁移的导入工具；GDPR 导出和删除接口 | ❌ | 导出和删除在 M10；导入工具（至少从 Stream、Sendbird 的导出格式导入）放 MVP 2 |
| 数据驻留 | Sendbird 建应用时要选一个区域 | 自托管天然满足；多库路由可以按单元分区域 | 文档里写清楚 |

### 2.6 运维与可观测

| 能力 | 同行做法 | AgentIM |
|---|---|---|
| 控制台 | 数据浏览、日志、用量 | 管理系统已设计（M9） |
| 限流说明 | 文档列出每个接口的限额 | 有 Agent 限流；人的限流和接口限额没有文档 | 补全并写进协议文档 |
| 状态页、SLA | 云服务都有 | 自托管不适用；企业版提供支持 SLA |

### 2.7 AI 相关（我们的差异化）

| 同行 | 做法 |
|---|---|
| Stream | 服务端先发一条空消息，再不断局部更新它来实现流式输出；有"思考中 / 生成中 / 出错"的状态事件和取消事件；提供 AI SDK 和 LangChain SDK |
| Sendbird | 转型为 AI 客服平台：AI Agent、转人工、无代码搭建 |
| 其他 | 基本没有 |

**我们已经比 Stream 更进一步**：流式是一等的消息状态（不是靠反复改消息），中断和超时都有明确的终态，还有审批、接管、耳语、护栏和审计；AI 核心组件（[03 §9](../03-ai-native.md)）还要加上任务、记事本、MCP、A2A。需要补的是"开发者用起来顺手"的部分：Python 和 TypeScript 的 Agent SDK、和主流框架（LangGraph、OpenAI Agents SDK、CrewAI 等）的集成示例，以及 AI 状态事件（思考中、调用工具中）。

## 3. 对 MVP 1 的调整建议

| # | 调整 | 理由 |
|---|---|---|
| 1 | **M3 先做"开发者后端签发令牌"**（应用密钥签名，带过期时间和权限范围，开发模式有免签令牌），再做内置账号密码 | 嵌入场景最先遇到；同行全都是这样做的 |
| 2 | M3 同时做**开放 API**（[07](../07-open-api.md)）和 Node、Python、Go 的服务端 SDK | 开发者的后端需要建用户、建会话、发系统消息 |
| 3 | D20 增加**发送前拦截钩子**和**自定义字段** | Stream、腾讯云、OpenIM 都有，审核和业务定制都靠它 |
| 4 | M8 把 Web 客户端做成 **React UI 组件库**（主题、组件替换、按消息类型注册渲染器） | UI 组件是开发者选型的关键，OpenIM 正是缺这一块 |
| 5 | M8 加入 Web Push 和桌面通知 | 用户期待 |
| 6 | M11 加入**十分钟教程、示例应用、一条命令的 Docker 启动** | 快速上手决定第一印象 |
| 7 | MVP 2 加入从其他厂商导入数据的工具 | 降低迁移成本，直接针对"被锁定"的痛点 |

## 来源

- [Stream：Sendbird 替代品对比](https://getstream.io/blog/sendbird-competitor-alternatives/)
- [Stream：CometChat 替代品对比](https://getstream.io/blog/cometchat-alternatives/)
- [Stream：9 款 Chat SDK 对比](https://getstream.io/blog/chat-sdk-comparison/)
- [Stream：Sendbird 定价分析](https://getstream.io/blog/what-i-learned-researching-chat-api-pricing/)
- [Stream：Before Message Send Webhook](https://getstream.io/chat/docs/node/before_message_send_webhook/)
- [Stream：AI Message Streaming](https://getstream.io/chat/docs/node/ai-message-streaming/)
- [Stream：React Message UI 主题](https://getstream.io/chat/docs/sdk/react/guides/theming/message-ui/)
- [CometChat：Sendbird 替代品](https://www.cometchat.com/blog/sendbird-alternatives)
- [CometChat：最佳 Chat SDK](https://www.cometchat.com/blog/best-chat-sdks)
- [Jotform：CometChat 与 Sendbird 对比](https://www.jotform.com/ai/agents/cometchat-vs-sendbird/)
- [apiscout：GetStream、Sendbird、CometChat 对比](https://apiscout.dev/guides/getstream-vs-sendbird-vs-cometchat-chat-api-2026)
- [SpotSaaS：Sendbird 与 Stream Chat 对比](https://www.spotsaas.com/compare/sendbird-vs-stream-chat)
- [QuickBlox：2026 年如何选择 Chat SDK](https://quickblox.com/blog/choosing-the-right-chat-sdk-what-developers-need-to-know/)
- [Agora Chat](https://www.agora.io/en/products/chat/)
- [CometChat 聊天产品页](https://www.cometchat.com/chat-and-messaging)
- [Vendr：Sendbird 价格](https://www.vendr.com/marketplace/sendbird)
- [Ethora：2026 年 Chat API 对比](https://ethora.com/blog/best-chat-api-for-your-app-in-2026-honest-comparison-cost-ai-self-hosting/)
- [Fora Soft：Chat API 自建与购买](https://www.forasoft.com/blog/article/how-chat-apis-save-you-time-and-money-1734)
- [OpenIM 功能说明](https://docs.openim.io/guides/introduction/features)
- [OpenIM 产品介绍](https://docs.openim.io/guides/introduction/product)
- [腾讯云 IM 发送前回调](https://www.tencentcloud.com/jp/document/product/647/74347)
- [YesPress：Sendbird 概况](https://yespress.io/sendbird.md)
- [eesel：Sendbird AI 评测](https://www.eesel.ai/pt/blog/sendbird-ai-analise-2026)
