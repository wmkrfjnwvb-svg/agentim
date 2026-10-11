# 10 和 Agent 生态的关系：集成、自己做、不做

调研详情见 [research/agent-ecosystem-2026.md](research/agent-ecosystem-2026.md)（Agent SDK 和平台、周边组件、大模型基础设施）。

## 1. 定位

Agent 生态已经很拥挤：写 Agent 有十几个成熟的 SDK，记忆、工具、沙箱、观测、模型网关都有专门的产品。**我们不和它们竞争，而是做它们都缺的那一层**：

- 所有 SDK 的"会话"都是一个人对一个 Agent 的一次运行，没有成员、没有多方之间的顺序、没有投递保证；Claude 的会话存储官方说明是"尽力而为、失败会丢"。
- 所有 SDK 都有"暂停等人批准"，但都把"谁有权批准、只生效一次、持久保存、审计"留给开发者自己做（OpenAI 文档原话如此）。
- 托管平台（Managed Agents、AgentCore、Foundry、Google Agent Runtime）都想自己保存状态，而且有的不支持零数据保留、有的可能用存储的内容改进服务；客户要的是一个中立、可自托管、数据归自己的地方。

所以 AgentIM 是：**任何框架写的 Agent 都能接进来的、持久、有序、多方、可治理的通信和记录层。秒迁移，随时销毁。**（已确认，2026-10-11）单个 Rust 程序，不绑定任何云。

## 2. 要提供的集成（按价值排序，D48 已确认）

| # | 集成 | 内容 | 成本 |
|---|---|---|---|
| 1 | MCP 服务 | 发消息、流式回复、读历史、列会话、请人决定、交给人；所有 SDK 都是 MCP 客户端，一次接入覆盖全部 | 低 |
| 2 | 人工介入桥 | 各 SDK 暂停的方式不同（OpenAI 的中断 + 运行状态、Claude 的 `canUseTool`、LangGraph 的 `interrupt`、Vercel 的 `needsApproval`、Pydantic AI 的延迟工具、Temporal 的信号）；每个 SDK 一个小助手：把暂停的状态作为不透明数据挂在我们的"请人决定"上，决定后恢复；持久化执行引擎通过回调收到决定 | 中 |
| 3 | Agent SDK（Python、TS 先做） | `@agentim.on_message(...)` 这样的入口；把我们的流式回复对接到各 SDK 的事件流（OpenAI、Vercel、AG-UI） | 中 |
| 4 | 会话存储适配器 | 让 AgentIM 成为这些 SDK 的历史存储：OpenAI `Session`（4 个方法）、Claude `SessionStore`（有官方一致性测试）、LangGraph 检查点；先做这三个。SDK 的内部条目是模型专属、不透明的，作为隐藏的"Agent 状态"条目存（即 [09 §6](09-agent-communication.md) 的 L2），不和会话里可见的消息混在一起 | 每个中等 |
| 5 | A2A 网关 | 每个 Agent 一张 Agent Card；A2A 任务对应我们的会话和任务（[03 §9.6](03-ai-native.md)） | 中高 |
| 6 | 反过来：AgentIM 作为一个渠道 | 给 Vercel Chat SDK 写 AgentIM 适配器、给 Agno 写接口、做一个自托管 ChatKit 的桥。**OpenAI Agent Builder 2026-11-30 停用，用户要迁移到自托管 ChatKit，这是一个时间窗口** | 每个低 |
| 7 | OpenTelemetry 导出 | 投递、审批、接管的追踪和 Agent 自己的追踪连起来（消息里带 `traceparent`）；不做追踪界面 | 低 |
| 8 | AG-UI | 有争议（有观点认为会被基于 MCP 的界面取代），放在原生 SDK 之后 | 中 |

其他集成（排在后面）：企业身份（接受 Okta ID-JAG、Entra Agent ID 的令牌，D50）；可插拔的护栏分类器（D51）；第三方 OAuth 令牌交给 Auth0 Token Vault / Arcade 保管；记忆连接器（把会话事件推给 Mem0、Zep、Letta，**删除请求跟着传过去**）；发布到 AGNTCY / MCP 注册表；沙箱和浏览器的实时画面作为结构化消息、接管就是把浏览器交给人。

## 3. 自己做（属于通信和治理的核心）

1. Agent 注册、负责人、凭据、急停（M6 已计划）。
2. 统一的"请人决定"、绑定参数的审批、接管：MCP 的 `input_required`、A2A 的 `input-required`、LangGraph 的 `interrupt`、Temporal 的信号、AP2 的支付授权都汇到这里。
3. 每条内容的来源和信任标记（人、Agent、外部 A2A、工具输出），对应 OWASP Agentic 风险和"防注入设计模式"。
4. 通信策略（谁能给谁发、什么级别的数据能出去、预算），在进程内执行，不在热路径上调用外部服务。
5. 审计和 AI 标记（EU AI Act 第 50 条）。
6. MCP 服务、A2A 网关（协议入口）。
7. **用量计量**（D49）：每次模型调用的用量事件（Agent、负责人、单元、会话 / 消息、厂商、模型、区域、输入 / 输出 / 缓存 / 推理 token、费用），用厂商返回的数字；可版本化的价格表；按 Agent、负责人、单元汇总，软上限提醒、硬上限暂停；另外统计业务单位（会话、解决的问题、批准的动作），方便客户按结果或按动作计费。

## 4. 不做（别人已经做好）

Agent 循环、工具调用、交接、模型抽象；可视化 Agent 搭建器（OpenAI 都在停用）；长期语义记忆、RAG；沙箱、代码执行、浏览器；支付通道；通用工作流引擎；追踪界面和评估平台；第三方 API 的工具网关和注册表；实时语音管线；各种 IM 连接器（Slack、Teams、WhatsApp 桥）。（D52）

## 5. Agent 怎么接进来（你的要求：给 Agent 一个 API 或 MCP，登录后就能同步历史、和其他 Agent 交流、派任务）

**一条命令创建，一段配置接入**：

```bash
agentim agent create --name researcher --owner alice
# 输出：Agent ID、密钥（只显示一次）、以及可以直接粘贴的配置：
#   - MCP 配置（Claude、Cursor、OpenAI Agents SDK、任何 MCP 客户端）
#   - Agent API 地址和 Python / TS SDK 的三行示例
#   - 模型网关地址（OpenAI 兼容 / Anthropic 兼容，用同一个密钥）
```

接入方式有三种，能力相同、共用同一套权限和审计：

| 方式 | 适合 | 说明 |
|---|---|---|
| **MCP**（远程，按 2026-07-28 无状态规范） | 已经在用 Claude、Cursor、各种 MCP 客户端的 Agent | 配置一个地址和密钥就能用；工具见下表 |
| **Agent API**（HTTP + WebSocket） | 自己写的 Agent、需要实时接收的 | Python / TS SDK 封装好同步、确认、重连、流式（[05 §6.5](05-operations.md)） |
| **适配器** | 已经基于某个框架或渠道 SDK 写好的 Agent | Vercel Chat SDK 适配器、Agno 接口、ChatKit 桥、各 SDK 的会话存储适配器（§2） |

MCP 和 Agent API 提供的操作（同一份清单，CI 检查两边一致）：

| 类别 | 操作 |
|---|---|
| 身份 | 我是谁、我的负责人、我的权限和额度 |
| 同步和历史 | 拿新事件（从服务端记录的位置往后）并确认；读上下文（摘要 + 最近原文，裁剪到 token 上限）；按范围读历史；搜索（[09 §6](09-agent-communication.md)） |
| 会话 | 列会话；和人或另一个 Agent 开单聊；建群、建任务频道；加人（受权限限制） |
| 消息 | 发消息（带结构化字段：类型、主题、点名谁、引用）；流式回复；编辑、撤回自己的 |
| Agent 之间 | 找 Agent（按技能、单元、权限过滤）；派任务、更新任务状态、交付产出物；抢单；申请 / 交出发言权；订阅主题 |
| 治理 | 请人决定（批准 / 回答 / 选择）并等待结果；交给人；上报用量 |
| 状态 | 工作日志、检查点（带版本） |

### 5.1 第一步（L2，AgentIM Lite，已实现）：凭据、创建、MCP

| 方面 | 做法 |
|---|---|
| 凭据 | Agent 密钥就是这个 Agent 的客户端令牌：只存哈希，一个密钥一台设备，有自己的同步位置（[08](08-sync-and-client.md)）。每个 Agent 必须有一个人作为负责人；每个负责人的 Agent 数量有上限（`agents.max_per_owner`，默认 20） |
| 创建 | 负责人在应用里用 `create_agent {name}`；运维用 `agentim agent create --name researcher --owner alice [--url https://im.example.com]`；开发者后端用开放 API。都返回密钥（只显示一次）和可以直接粘贴的接入配置（MCP 配置、WebSocket 地址） |
| 换密钥（秒迁移） | `agent_key {agent_id, revoke_others?}` 签发新密钥；`revoke_others` 时在同一次写入里吊销其他所有密钥并断开它们的连接。并发换密钥时**最后一次写入的那把是唯一有效的**；CLI 是 `agentim agent key --name researcher` |
| 吊销、列表 | `revoke_agent_key {agent_id, device_id}`、`list_agents`（名字、密钥列表、最后在线时间，不含密钥本身） |
| 权限和错误码 | 只有负责人能管理自己的 Agent：别人的 Agent `forbidden`，不存在的 `not_found`，名字被占用或超过上限 `conflict`，参数不对 `bad_request`；Agent 不能创建 Agent（`forbidden`）。被拒绝的请求不写任何东西 |
| 服务端运行时用 CLI 创建 | 服务端遇到内存里没有的令牌时去数据库查一次（全局每秒最多一次，防止乱填令牌拖垮数据库），查到就把用户和令牌加进内存。集群里别的节点签发的令牌也靠这一步 |
| MCP 服务 | `POST /mcp`，按 2026-07-28 规范（无状态：每个请求在 `_meta` 和 `MCP-Protocol-Version` 头里带版本，`Mcp-Method`、`Mcp-Name` 头必须和请求体一致，不一致 `400` + `-32020`；不支持的版本 `400` + `-32022` 并列出支持的版本；必须实现 `server/discover`；未知方法 `404` + `-32601`；`GET`、`DELETE` 返回 `405`；不发会话 ID）。同时兼容老客户端（2025-03-26 到 2025-11-25 的 `initialize` 握手，无会话 ID），因为现在大多数 MCP 客户端还是老版本。只回 `application/json`，不用 SSE |
| MCP 认证 | `Authorization: Bearer <Agent 密钥>`；没有或无效 `401`（带 `WWW-Authenticate: Bearer`）；**人的令牌 `403`**：通过 MCP 发出的内容来自模型，必须标为 AI（EU AI Act 第 50 条），不能冒充人。带了不允许的 `Origin` 返回 `403`（防 DNS 重绑定）|
| Agent 开会话（D47 ①） | Agent 可以开单聊、建群、往群里拉人，但**只能找已经有关系的人**：自己的负责人、同一个负责人的其他 Agent、已经和它在同一个会话里的人或 Agent；其他人 `forbidden`（防止 Agent 冷启动骚扰，和用户目录的可见规则一致）。拉黑照常生效。Agent 建的群，**群主是它的负责人**（自动成为成员），Agent 是管理员：人始终对 Agent 建的群负责、能管理它（EU AI Act 人工监督）。单聊不自动加负责人，负责人用 `observe` 代看。每个 Agent 建会话有速率上限（`agents.conversations_per_hour`，默认 60），超出 `rate_limited` |
| MCP 工具（这一步） | `whoami`、`list_conversations`、`get_updates`、`send_message`（带 `client_msg_id` 重试不重复）、`edit_message`、`recall_message`、`find_user`、`open_direct`、`create_group`、`add_member`；L3 加了 `request_human`、`get_request`、`cancel_request`；L4 加了 `create_task`、`update_task`、`claim`、`release_claim`、`floor_grant`、`floor_release`，`send_message` 加了信封；L5 加了 `get_history`、`get_context`、`search`、`put_summary`。状态（检查点）在后面加进来 |
| `get_updates` | 就是同步（同一个设备位置、同样的可见规则、分批），返回的事件在同一次调用里确认：工具结果交给了模型就算送达。如果响应在路上丢了，这些事件可以通过 L5 的历史接口再读到。同一把密钥的并发调用在节点内排队，每个事件只交出一次；被移出、解散各只告诉一次（按设备记住） |
| 工具出错 | 业务错误放在工具结果里（`isError: true`，文字是"错误码: 原因"，`structuredContent.error` 里是 `code` 和 `message`），模型能看懂并改正；未知工具、参数结构不对是 JSON-RPC 错误 `-32602` |
| 限流 | 每个 Agent 密钥的 MCP 调用令牌桶（`mcp.rate_per_second` 默认 5，`mcp.burst` 默认 20），超出返回工具错误 `rate_limited`；发消息仍受现有的 Agent 护栏 |
| Agent API | 现有的 WebSocket 客户端协议，用 Agent 密钥连接，同步、确认、发送、流式全部可用；Python / TS SDK（L7）封装它。不另做一套 HTTP 单次调用接口：无状态的调用方用 MCP |

**秒迁移**：新环境用新密钥连上来，同步位置、历史、状态都在服务端，接着干；旧密钥吊销。跨服务器用 Agent 包（[09 §6.4](09-agent-communication.md)）。

**随时销毁**：负责人或管理员一键销毁：吊销凭据、断开连接、取消进行中的流和任务、删除它的工作日志、检查点、摘要，销毁它的数据密钥；它在共享会话里发过的消息属于会话记录，按会话的保留期处理（可配置为一起删除）。销毁写审计，不可撤销。

## 6. 模型网关（你的要求：网关要有；以后做模型消息传递）

方向调整：**内置模型网关**（原来建议只做用量上报）。目的：Agent 不用拿到厂商密钥；预算、限流、区域白名单、审计在花钱的那一刻执行；每次模型调用和触发它的消息关联起来；为以后"服务端直接托管 Agent"留好接口。

| 方面 | 做法 |
|---|---|
| 接口 | OpenAI 兼容（Chat Completions、Responses）和 Anthropic Messages 兼容，Agent 用自己的 AgentIM 密钥调用，**现有的 SDK 只改地址就能用** |
| 上游 | 厂商（OpenAI、Anthropic、Google、Mistral，以及通过 Azure / Bedrock / Vertex 的欧盟区域）、客户自己的模型服务（vLLM、SGLang、Ollama，任何 OpenAI 兼容地址）、或者客户已有的网关（LiteLLM、Envoy、Portkey 等） |
| 厂商密钥 | 管理员配置，用主密钥加密存放（[04 §2.6](04-security-compliance.md)），Agent 永远拿不到 |
| 控制 | 每个 Agent / 负责人 / 单元：允许的厂商、模型、区域（例如只允许欧盟）；token 和费用预算（软上限提醒、硬上限拒绝）；每分钟请求和 token 限流；按 [09 §5](09-agent-communication.md) 的根预算统一计算 |
| 计量 | 用厂商返回的用量（输入、输出、缓存读写、推理 token），按可版本化的价格表算费用，写入只追加的用量记录；直接调模型、不经过网关的 Agent 可以上报用量（标为"自报"） |
| 审计和关联 | 请求头带上触发它的会话和消息 ID，追踪里串起"哪条消息 → 哪个 Agent → 哪次模型调用 → 花了多少" |
| 流式 | 原样透传，同时从流里取用量；硬上限只在请求开始前检查，不在流中途掐断（D53） |
| 可选 | 提示词 / 回复的 DLP 检查、记录（默认不记录内容，只记元数据）、失败时切换到备用上游 |
| 安全 | Rust 实现、依赖最少；不打包 Python 网关（LiteLLM 2026-03 的供应链攻击）；网关可以单独关闭 |

**预留接口：模型消息传递 / 服务端托管 Agent（以后做）**：服务端可以直接托管一类 Agent：注册时只配置模型、系统提示、工具（MCP）和接收方式；被唤醒时（[09 §2](09-agent-communication.md)）服务端自己取上下文、通过网关调模型、把回复流式发回会话。这样不用部署任何 Agent 程序，Agent 之间的交流完全在 IM 里完成。现在先把三样东西设计成可复用的：唤醒事件、上下文接口、网关调用；托管 Agent 只是把这三样串起来的一个内置运行器。

## 7. 欧盟合规相关（补充到 [04](04-security-compliance.md)）

- 第 50 条：Agent 身份和消息上的 AI 标记（[03 §9.6.2](03-ai-native.md)）。
- 按组织设置模型厂商和区域白名单；Anthropic 自己的 API 没有欧盟区域这一点要在界面上可见。
- Agent 注册信息里记录厂商、模型、区域、是否零数据保留，作为 GDPR 第 30 条的处理记录和 DPIA 的依据。
- 提示词和回复的记录可以关闭或脱敏；删除请求要传到用量记录（只保留汇总）和外部适配器。
