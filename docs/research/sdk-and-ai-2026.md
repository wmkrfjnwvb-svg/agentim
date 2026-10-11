# 调研：客户端 SDK 与海外 AI 组件（2026-10）

四个问题：① 客户端要不要本地存储，存什么、怎么存；② SDK 应该提供哪些 API 和回调（hook），开发者才能在上面开发；③ 海外的 AI 组件现在是什么情况，我们的 M6 该做什么；④ 只同步、不读历史时，新成员、新设备、Agent 能看到多少历史，同步位置和确认怎么做。

来源标注：**[已核实]** 本次调研查看了官方文档或原始来源（附链接）；**[二手]** 只有媒体、论坛或厂商宣传，引用前应再核实；**[推断]** 我们自己的判断。结论写进了设计文档：SDK 见 [05 §6](../05-operations.md)，AI 组件见 [03 §9](../03-ai-native.md)。

## 1. 客户端本地存储

### 1.1 各家做法

| 产品 | 默认 | 存什么 | 引擎 | 本地加密 | 和服务端对齐 | 离线发送 | 容量 | 退出登录 |
|---|---|---|---|---|---|---|---|---|
| Sendbird v4 | 可选开启 | 会话、消息、未发出的消息 | SQLite / IndexedDB [推断] | Android 可选 SQLCipher；iOS 文件保护；JS 自己提供加解密函数 | 拉变更补小缺口；缺 300 条以上通知应用重建 | 自动重发，3 天后变失败 | 默认 256 MB，按会话最久未用淘汰 | 清空缓存 |
| Stream | 默认开启 | 会话、消息、表情、成员、草稿 | Android Room、iOS Core Data [推断]、Flutter drift、RN op-sqlite | RN 可选 SQLCipher，钥匙放 Keychain / Keystore，打不开就重建，绝不退回明文 | 重连后补拉离线期间的事件 | 8 类操作排队，12 小时过期 | 无说明 | 断开时可清空；每个用户一个库 |
| matrix-rust-sdk（Element X） | 默认开启 | 状态、加密密钥、消息（分块 + 显式记录缺口）、媒体 | SQLite；Web 用 IndexedDB | 应用层逐值加密（XChaCha20-Poly1305，索引用带密钥的哈希），口令放 Keychain | 缺口是一等记录，往回翻时补 | 发送队列 | 媒体按大小、过期、总量淘汰 | 删目录 |
| Telegram TDLib | 分级开关 | 文件 → 联系人和群 → 消息，逐级开启 | SQLite [推断] | 数据库密钥参数 | 有序的更新流 | 有 | 按大小、时间、数量清理 | 退出 |
| Signal | 默认开启 | 全部 | SQLCipher | 随机密钥，用 Keystore / 系统钥匙串包装 [二手] | 端到端加密，不适用 | 有 | 无 | 解绑 |
| WhatsApp | 默认开启 | 全部 | SQLite / Core Data | 只靠系统沙盒，备份才加密 [二手]；删除的行会残留在 WAL 里 [二手] | — | 有 | 用户手动 | 卸载 |
| Twilio / PubNub / CometChat | 不存 | 只在内存 | — | — | 从服务端补拉历史 | 内存 | — | — |

来源：[Sendbird 本地缓存](https://docs.sendbird.com/docs/chat/v4/ios/local-caching/overview-local-caching)、[Sendbird 缓存配置](https://sendbird.com/docs/chat/v4/android/ref/-sendbird-chat/com.sendbird.android.caching/-local-cache-config/index.html)、[Stream Android 离线](https://getstream.io/chat/docs/sdk/android/client/guides/offline-support/)、[Stream iOS 离线](https://getstream.io/chat/docs/sdk/ios/client/offline-support/)、[Stream RN 加密](https://getstream.io/chat/docs/sdk/react-native/basics/encrypting-the-offline-database/)、[matrix-sdk 存储加密](https://docs.rs/matrix-sdk-store-encryption/latest/matrix_sdk_store_encryption/)、[TDLib 参数](https://core.telegram.org/tdlib/docs/classtd_1_1td__api_1_1set_tdlib_parameters.html)、[TDLib 清理](https://core.telegram.org/tdlib/docs/classtd_1_1td__api_1_1optimize_storage.html)、[浏览器里的 SQLite + OPFS](https://powersync.com/blog/sqlite-persistence-on-the-web)。以上除标注外都已核实。

### 1.2 结论

- **给人用的客户端都要本地存储**：会话列表秒开、离线能看历史、断网时能先发（排队）。只有 Twilio、PubNub、CometChat 不做，CometChat 用户呼声最高的需求就是离线。
- **服务端的 Agent 不需要本地库**：TDLib 的分级开关、Twilio 的纯内存都说明"可以不落盘"是正当需求。Agent 不落盘还少一份个人数据，GDPR 负担更小。
- **我们的优势**：每个会话的 seq 连续，SDK 能准确知道自己缺了哪一段（Sendbird 和 Ably 只能猜），缺口可以自动补齐。
- **要避免的坑**：退回明文、悄悄退回内存、离线队列无限增长、悄悄丢弃、删除的数据残留在 WAL 里。

## 2. SDK 的 API 和回调

### 2.1 各家共有的部分

| 方面 | 做法 |
|---|---|
| 入口 | 一个客户端对象；`connect(用户, 取令牌的回调)`；令牌快过期或失效时 SDK 自动再调用回调（Sendbird 要求在限定时间内返回，Twilio 提前发"即将过期"事件）[已核实] |
| 对象 | 客户端 → 会话 → 消息，另有用户、成员；方法挂在对象上（`conversation.send(...)`）；参数用带类型的对象 |
| 功能分组 | 用户、会话、成员、消息（发、改、撤、表情、回复）、分页历史、已读、正在输入、在线状态、文件、推送令牌、举报和拉黑、搜索 |
| 事件 | 订阅时返回一个"取消订阅"的句柄（Ably、PubNub）[已核实]；按字符串 ID 注册的方式（Sendbird、CometChat）容易泄漏和重复收到，CometChat 文档专门提醒 [已核实] |
| 连接状态和数据连续性分开 | 连接状态是一个枚举；"可能漏了数据"另有事件（Ably `onDiscontinuity`、Sendbird `onHugeGapDetected`）[已核实] |
| 消息的本地状态 | 待发送 → 已发送 / 失败（能否重发）；手动重发、取消；乐观显示（Matrix `SendingFailed{可恢复}`、Sendbird `isResendable`）[已核实] |
| 发送前的处理链 | Stream 的输入框中间件：可以按名字插在某一步之前、可以移除 [已核实] |
| 错误 | 错误码 + 信息 + 是否可重试；Sendbird、Stream 用数字码，跨语言很难对应 [已核实] |
| 界面层 | 核心 SDK → 无界面的状态层（会话列表、消息列表）→ 各平台的 hooks（React `useMessages` 等）→ 可选的界面组件库（Sendbird UIKit）[已核实] |
| Agent 相关 | Stream：流式更新消息 + "思考中 / 生成中 / 查资料 / 出错"状态和"停止生成"按钮 [已核实]；Vercel AI SDK `useChat`：状态 `submitted / streaming / ready / error`、停止、重新生成、工具调用、**工具审批的回复** [已核实]；AG-UI：开始 / 内容 / 结束三段式事件、工具调用事件 [已核实]；CopilotKit：界面上让人回答，答案作为工具结果返回 [已核实] |
| 版本 | 弃用至少保留一个小版本再在大版本删除（Twilio）；PubNub 1.0 悄悄改了 `join()` 的语义，是反面例子 [已核实] |
| 测试支持 | 大多数厂商没有官方的模拟服务端 [推断] |

来源：[Sendbird 认证](https://sendbird.com/docs/chat/sdk/v4/javascript/application/authenticating-a-user/authentication)、[Sendbird 连接回调](https://docs.sendbird.com/docs/chat/sdk/v4/javascript/ref/classes/_sendbird_chat.ConnectionHandler.html)、[Sendbird 错误码](https://docs.sendbird.com/docs/chat/sdk/v4/javascript/error-codes)、[Stream 令牌](https://getstream.io/chat/docs/javascript/tokens_and_authentication/)、[Stream 输入框中间件](https://getstream.io/chat/docs/sdk/react/components/message-composer/message-composer-class/)、[Ably 连接](https://ably.com/docs/chat/connect)、[Ably 消息](https://ably.com/docs/chat/rooms/messages)、[PubNub 1.0 迁移](https://www.pubnub.com/docs/chat/chat-sdk/migrate/js-chat-v10-migration-guide.md)、[Twilio 更新日志](https://www.twilio.com/docs/conversations/javascript/changelog)、[Vercel useChat](https://ai-sdk.dev/docs/reference/ai-sdk-ui/use-chat)、[AG-UI 事件](https://docs.ag-ui.com/concepts/events)、[CopilotKit](https://docs.copilotkit.ai/reference/hooks/useHumanInTheLoop)、[matrix-sdk-ui](https://docs.rs/matrix-sdk-ui/latest/matrix_sdk_ui/sync_service/index.html)。

### 2.2 结论

回答"没有 API 和 hook 别人没法开发"：对。SDK 要提供**完整的操作 API**、**带类型的事件订阅**、**几个固定位置的 hook**（令牌、发送前、收到后的显示转换、限流、日志），以及**给 Agent 用的扩展**（流式、草稿审批、耳语、接管、停止生成）。另外提供**测试工具包**（进程内服务端 + 故障注入），这是大多数厂商的空白。

## 3. 海外 AI 组件现状

### 3.1 协作平台和聊天 API 厂商

| 谁 | 做了什么 | 对我们的意义 |
|---|---|---|
| Slack | MCP 服务器和实时搜索 API 2026-02 正式发布 [已核实]；流式消息接口 `chat.startStream / appendStream / stopStream`，带"任务卡片"和"计划"展示 Agent 的步骤 [已核实]；2025-05 起非市场应用读历史限 1 次/分钟 [已核实] | 流式回复 + 展示步骤已经是基本体验；Slack 把数据锁在里面，自托管、数据归自己是我们的机会 |
| Microsoft | Agent 365（$15/人/月）：Agent 登记、权限、生命周期（不活跃的过期、没有负责人的标记、有风险的阻止）、审计 [已核实]；Entra Agent ID：每个 Agent 有**人类担保人**、负责人，担保人离职时有流程 [已核实] | 企业对 Agent 治理的标准模板 |
| Linear | Agents API：Issue 委派给 Agent 时 Agent 是"代办"，**人仍然是负责人**；Agent 会话状态 pending / active / awaitingInput / error / complete；Agent 发出带类型的"活动"（思考、动作、提问、回复、错误），约 10 秒内要先确认 [已核实] | 和我们的"任务"最接近，可以直接参考 |
| Notion、Zoom、Google | 团队 Agent、定时 Agent；MCP 和 A2A 已是协作套件的标配 [已核实 / 二手] | |
| Stream、CometChat、Sendbird、Intercom、Twilio | Agent 作为会话用户 + 流式回复（Stream）；用 AG-UI 接入任意 Agent（CometChat）[已核实]；客服场景的**转人工 + 交接说明**（Intercom Fin、Sendbird）[已核实 / 二手] | 都是闭源 SaaS，偏客服；没有可自托管的、人和 Agent 多方协作 + 审批 / 耳语 / 接管 / 治理 |

来源：[Slack MCP](https://slack.com/blog/news/mcp-real-time-search-api-now-available)、[Slack 流式](https://docs.slack.dev/reference/methods/chat.startStream/)、[Slack 限流](https://docs.slack.dev/changelog/2025/05/29/rate-limit-changes-for-non-marketplace-apps)、[Agent 365](https://www.microsoft.com/en-us/microsoft-agent-365)、[Entra Agent ID](https://learn.microsoft.com/en-us/entra/agent-id/whats-new-agent-id)、[Linear Agents](https://linear.app/developers/agents)、[Stream AI SDK](https://getstream.io/chat/docs/sdk/react/guides/ai-integrations/stream-chat-ai-sdk/)、[CometChat AG-UI](https://www.cometchat.com/docs/ai-agents/cometchat-ag-ui-byoa)、[Intercom 交接](https://www.intercom.com/learning-center/ai-human-collaboration-procedures-handoffs)。

### 3.2 Agent 框架和基础设施

- **人工介入已经是所有框架的标准动作**：工具需要审批 → 运行暂停、状态保存 → 批准 / 拒绝 / 修改后继续。OpenAI Agents SDK、Google ADK、LangGraph `interrupt()`、Cloudflare Agents 都有 [已核实 / 二手]。**但没有一家提供"多人、持久、可审计、送到对的人面前"的处理界面**；LangChain 的 Agent Inbox 是单人的。
- OpenAI AgentKit：ChatKit 是可嵌入的聊天界面，带表单、按钮等组件 [已核实]。
- AWS Bedrock AgentCore：运行、记忆、网关（MCP）、身份（令牌保管）、观测（OpenTelemetry 导出）[已核实]。
- 记忆：Mem0、Letta、Zep、AgentCore Memory 竞争激烈，各家测评互相矛盾 [二手]。
- 身份：Okta Agent SSO（Cross App Access）[二手]、Auth0 for AI Agents（令牌保管、人不在线时的异步授权）[已核实]。
- Gartner 预测 2027 年底前超过 40% 的 Agent 项目会被砍掉，原因包括成本、价值不清和**风险控制不足** [二手]。

### 3.3 协议

| 协议 | 现状 |
|---|---|
| MCP | 最新规范 **2026-07-28** [已核实]：改为无状态（去掉初始化握手和会话 ID）、**去掉了流的断点续传**（流断了正在进行的请求就丢了）、长任务（Tasks）成为官方扩展、追问改为"多轮请求"（服务端返回 `input_required`，客户端带答案重发）、动态客户端注册被弃用；MCP Apps（沙箱里的界面）是第一个官方扩展 [已核实]；MCP 已移交 Linux 基金会下的 Agentic AI Foundation [二手] |
| A2A | v1.0（2026-04）第一个稳定版：多租户、可签名的 Agent Card、150 多家组织，已进入 Azure、Copilot Studio、AgentCore [已核实]；IBM 的 ACP 已并入 A2A [二手] |
| AG-UI | CometChat 用它接 Agent [已核实]；号称 Google、微软、AWS 采用 [二手] |
| OpenTelemetry GenAI 约定 | 仍是开发中状态，没有稳定版 [二手] |

### 3.4 EU AI Act

- **第 50 条（透明度义务）2026-08-02 已经生效，没有被推迟** [已核实，[来源](https://www.goodwinlaw.com/en/insights/publications/2026/08/alerts-technology-dpc-eu-ai-act-transparency-obligations-now-in-force)]：要告诉用户他在和 AI 交互（明显的除外）；AI 生成的文本、音频、图片、视频要有**机器可读的标记**，2026-08-02 之前已上市的生成式系统可延到 **2026-12-02**；罚款最高 1500 万欧元或全球营业额 3%。
- 高风险系统的义务推迟到 2027-12-02 / 2028-08-02。
- 对我们 [推断]：Agent 发出的每条内容（包括流式中间状态、被批准的草稿）都要带服务端写入、不能被去掉的 AI 标记；导出、Webhook、开放 API、以后的联邦都要带上；人接管后发的才是人类内容。

### 3.5 结论：我们能补的空白 [推断]

1. 中立、开源、可自托管、数据在欧盟的**人和 Agent 多方协作空间**。
2. **持久、多人的人工介入界面**：各框架只负责暂停，把请求送到对的人、只有一个决定生效、可审计、再让 Agent 继续，这件事没人做，正好是 IM 的强项。MCP 去掉了断点续传，我们不丢、有序的日志可以补上。
3. **会话层的治理**：跳数、防循环、预算、DLP、耳语、接管，别家没有；Agent 365 只管身份层，而且绑在微软生态里。
4. **人能看见、能介入的跨组织 Agent 协作**（A2A 本身对人不透明）。
5. 内置的 AI Act 第 50 条合规。

## 4. 只同步、不读历史（2026-10-11 补充调研）

结论已写进 [08 §6](../08-sync-and-client.md)，这里只列来源。

- 新成员看历史：[Slack 单聊加人](https://slack.com/help/articles/1500002969782-Add-people-to-a-direct-message)、[Teams 群聊加人可选带多少历史](https://support.microsoft.com/en-us/office/0c71b32b-c050-4930-a887-5afbe742b3d8)、[Telegram 新成员历史可见性](https://core.telegram.org/method/channels.togglePreHistoryHidden)、Matrix `m.room.history_visibility`（[规范](https://spec.matrix.org/v1.16/client-server-api/)）、[Google Chat 群历史](https://workspaceupdates.googleblog.com/2020/11/changes-to-google-chat-group.html)、[腾讯云 IM 群类型](https://www.tencentcloud.com/document/product/1047/60011)；WhatsApp 拉人时带最近 25–100 条 [二手，[来源](https://m.gsmarena.com/whatsapp_introduces_new_group_message_history_feature-news-71641.php)]。
- 新设备：[WhatsApp 多设备](https://engineering.fb.com/2021/07/14/security/whatsapp-multi-device/)；Signal 关联设备可迁移全部消息和 45 天媒体 [二手]。
- 同步和确认：[Telegram 更新机制](https://core.telegram.org/api/updates)、[Telegram 确认](https://core.telegram.org/mtproto/service_messages_about_messages)、[Matrix Simplified Sliding Sync（MSC4186）](https://github.com/matrix-org/matrix-spec-proposals/blob/erikj/sss/proposals/4186-simplified-sliding-sync.md)、[Discord 状态码](https://docs.discord.com/developers/topics/opcodes-and-status-codes)、[WhatsApp 隐私政策（未投递保留 30 天）](https://www.whatsapp.com/legal/privacy-policy)。
- 已读和未读：[Telegram 已读事件](https://core.telegram.org/constructor/updateReadHistoryInbox)、[Matrix 推送网关](https://spec.matrix.org/v1.9/push-gateway-api)、[Slack 通知](https://slack.com/help/articles/360042339754)、[Stream 未读](https://getstream.io/chat/docs/javascript/unread/)、[Sendbird 已读](https://docs.sendbird.com/docs/chat/sdk/v4/android/message/marking-messages-as-read-in-a-group-channel/mark-messages-as-read)。
- 保留期：[Mattermost 保留策略](https://docs.mattermost.com/administration-guide/comply/data-retention-policy.html)、[Mattermost 法律保全](https://docs.mattermost.com/administration-guide/comply/legal-hold)。
- Agent 读历史：[Telegram Bot API](https://core.telegram.org/bots/api#getting-updates)（更新只保留 24 小时、没有读历史的接口）、[Telegram 隐私模式](https://core.telegram.org/bots/features#privacy-mode)、[Slack 2025 限流](https://docs.slack.dev/changelog/2025/05/29/rate-limit-changes-for-non-marketplace-apps)、[Slack AI 应用](https://docs.slack.dev/ai/developing-ai-apps)、[Teams Agent 默认只收 @](https://learn.microsoft.com/en-us/microsoftteams/platform/agents-in-teams/enable-receive-all-chat-messages)、[Discord 消息](https://docs.discord.com/developers/resources/message)、[Google Chat 读消息需管理员批准](https://developers.google.com/workspace/chat/authenticate-authorize-chat-app)、[Zulip 读消息](https://zulip.com/api/get-messages)、[Stream AI 示例](https://github.com/GetStream/chat-ai-samples)、[Intercom Fin 记忆](https://fin.ai/help/en/articles/16004462-fin-memory)、[OpenAI 对话状态](https://developers.openai.com/api/docs/guides/conversation-state)、[OpenAI Assistants 停用](https://developers.openai.com/api/docs/deprecations)、[Claude Managed Agents](https://platform.claude.com/docs/en/managed-agents/overview)。

