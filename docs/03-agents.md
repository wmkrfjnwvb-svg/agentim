# 03 Agent

AgentIM 是给开发者的 IM：任何框架写的 Agent 都能作为成员接进来，和人、和别的 Agent 在同一套持久、有序、不丢的会话里交流；人随时能看、能批准、能接管、能停下。本文只写已实现的行为（规则、状态、上限、错误码、协议操作），最后一节是还要做的。人用客户端的同步规则见 [08](08-sync-and-client.md)，协议操作总表见 [05 §7.1](05-operations.md)。

错误码按项目规则：`forbidden`（不允许 / 不是成员）、`not_found`（不存在或已消失）、`conflict`（当前状态不允许）、`bad_request`（参数不对或不适用）、`rate_limited`；被拒绝的请求不写任何东西。

## 1. 定位：Agent 是会话成员

### 1.1 身份

- 参与者分 `human`、`agent`、`system` 三类。`sender_type` 由服务端按认证身份写入，Agent 不能冒充人；客户端协议、开放 API、Webhook 都带着它。
- 消息记录"谁发的"（`sender_id`）和"代表谁发的"（`on_behalf_of`），Agent 只能代表它的负责人。
- 人与人、人与 AI、AI 与 AI、混合群聊用同一套消息模型，区别只在策略。
- **AI 标记**（EU AI Act 第 50 条）：流式回复从开始就带 `sender_type: agent`；批准的草稿仍是 Agent 发的，事件里记下批准人和是否被修改（`approval: {by, edited}`）；人接管后发的才是人的内容。SDK 和网页默认显示 AI 标识，不提供关闭的开关。

### 1.2 负责人，个人助理和团队 Agent

每个 Agent 必须有一个人作为负责人。Agent 分两种，创建时定下（D89）：

| | 个人助理（`personal`，默认） | 团队 Agent（`team`） |
|---|---|---|
| 是什么 | 某个人的分身 | 属于群或单元（发布机器人、客服、知识库），负责人只是维护者 |
| 谁能创建 | 任何人（`create_agent`） | 只有开发者：开放 API `PUT /v1/users/{id}`（`kind: agent`、`team: true`）、控制台或 `agentim admin agent-create --team` |
| 负责人离开或被移出会话 | 一起离开（`reason: owner_left`） | 留下（负责人对它的接管先结束） |
| 负责人被停用 | 急停，重启后仍停止；重新启用后由他自己恢复 | 照常运行，管理员列表里标为"负责人已停用"，由管理员转交 |
| 转交 | 不能（`bad_request`） | 开放 API `POST /v1/agents/{id}:transfer`、控制台或 `agentim admin agent-transfer`（§2.1） |

两种都遵守：**负责人只在自己也是成员的会话里行使负责人的权力**（代看、接管、耳语、改策略、重置熔断、审批草稿，以及收到这个会话的草稿、反馈等侧信道）。不是成员时返回 `forbidden`，侧信道也不再发给他；会话之外的管理（急停、恢复、销毁、密钥）不受影响。

- 团队 Agent 所在会话的群主、管理员可以改它在本会话的策略、重置熔断；别人的个人助理不行，普通成员都不行。
- 开着审批的 Agent 在负责人不在的会话里发言、开流式返回 `conflict`（没人能审批）。
- 剩余风险：持有 Agent 密钥的负责人可以用 Agent 身份读它所在的会话；靠"只把团队 Agent 交给可信的人、交接时轮换密钥"控制。

### 1.3 读取范围与告知

- Agent 只能读到加入以后（`join_seq` 之后）的事件；拉人时可以授权加入前的历史条数（`add_member {history}`），授权在会话里可见。
- 收到哪些事件按接收方式（§6.2）决定；群里默认 `addressed`，单聊默认 `all`。
- Agent 加入时产生全员可见的事件，带负责人和读取范围，客户端必须展示（GDPR 告知、AI Act 披露，见 [04](04-security-compliance.md) §4）。

### 1.4 IM 能强制什么

Agent 运行在 IM 外部，IM 只能强制它经手的东西：Agent 的消息（审批、撤回、接管、打断）、Agent 能读到什么、速率和按消息数 / 字符数计的预算。工具调用和直连外部系统 IM 管不到；Agent 可以用"请人决定"（§4.3）主动请人批准，这是协作，不是强制。产品文案必须区分这两类。

## 2. 接入

### 2.1 创建和凭据

| 方面 | 做法 |
|---|---|
| 创建 | 应用里 `create_agent {name}`（开发者打开 `create_agents` 时，[13 §4.1](13-roles-and-entry-points.md)）；开发者后端用开放 API `PUT /v1/users/{id}`（`kind: agent`）；控制台的 Agent 页或 `agentim admin agent-create ID --owner alice [--team]`。都返回密钥（只显示一次）和可直接粘贴的接入配置（MCP 配置、WebSocket 地址） |
| 凭据 | Agent 密钥就是它的客户端令牌：只存哈希，一个密钥一台设备，有自己的同步位置。每个负责人的 Agent 数量上限 `agents.max_per_owner`（默认 20） |
| 换密钥 | `agent_key {agent_id, revoke_others?}` 签发新密钥；`revoke_others` 时同一次写入吊销其他密钥并断开连接。并发换密钥时最后写入的那把唯一有效。开放 API `POST /v1/agents/{id}/keys`，或 `agentim admin agent-key ID [--revoke-others]` |
| 转交（团队 Agent） | 开放 API `POST /v1/agents/{id}:transfer {owner, rotate_keys}`、控制台或 `agentim admin agent-transfer ID --owner NEW [--rotate-keys]`：新负责人须是同一单元在职的人，不超过名额；`--rotate-keys` 吊销全部密钥。写审计 `agent.transferred` |
| 吊销、列表 | `revoke_agent_key {agent_id, device_id}`、`list_agents`（名字、密钥列表、最后在线时间，不含密钥） |
| 权限 | 只有负责人能管理自己的 Agent：别人的 `forbidden`，不存在 `not_found`，名字被占用或超过上限 `conflict`，参数不对 `bad_request`；Agent 不能创建 Agent（`forbidden`） |
| 运行时用 CLI 建的 Agent | 服务端遇到内存里没有的令牌时去库里查一次（全局每秒最多一次），查到就载入；集群里别的节点签发的令牌也靠这一步。CLI 建的 Agent 在第一次连接之前就能被拉进会话 |

### 2.2 Agent 开会话

Agent 可以开单聊、建群、往群里拉人，但**只能找已有关系的人**：自己的负责人、同一负责人的其他 Agent、已经和它在同一个会话里的人或 Agent；其他人 `forbidden`。拉黑照常生效。Agent 建的群，群主是它的负责人（自动成为成员），Agent 是管理员。单聊不自动加负责人，负责人用 `observe` 代看。每个 Agent 每小时最多建 `agents.conversations_per_hour`（默认 60）个会话，超出 `rate_limited`。

### 2.3 MCP 服务

| 方面 | 做法 |
|---|---|
| 端点 | `POST /mcp`，按 2026-07-28 规范（无状态）：每个请求在 `_meta` 和 `MCP-Protocol-Version` 头里带版本；`Mcp-Method`、`Mcp-Name` 头和请求体不一致 `400` + `-32020`；不支持的版本 `400` + `-32022` 并列出支持的版本；实现 `server/discover`；未知方法 `404` + `-32601`；`GET`、`DELETE` 返回 `405`；不发会话 ID。兼容老客户端（2025-03-26 到 2025-11-25 的 `initialize` 握手）。只回 `application/json`，不用 SSE |
| 认证 | `Authorization: Bearer <Agent 密钥>`；没有或无效 `401`（带 `WWW-Authenticate: Bearer`）；**人的令牌 `403`**（经 MCP 发出的内容来自模型，必须标为 AI）；不允许的 `Origin` 返回 `403` |
| 工具 | `whoami`、`list_conversations`、`get_updates`、`send_message`（`client_msg_id` 重试不重复，可带信封和 `sources`）、`edit_message`、`recall_message`、`find_user`、`open_direct`、`create_group`、`add_member`；`request_human`、`get_request`、`cancel_request`；`create_task`、`update_task`、`claim`、`release_claim`、`floor_grant`、`floor_release`；`get_history`、`get_context`、`search`、`put_summary`；`state_get`、`state_put`、`state_list`、`state_delete`；`handoff`、`send_card`、`progress`。卡片回应和反馈是人做的，不给 Agent 工具 |
| `get_updates` | 就是同步（同一个设备位置、同样的可见规则、分批），**下一次调用时确认上一批**（D96）：结果带 `ack`，下一次调用（不带 `ack`，或带这一批的 `ack`）才确认；带的不是最近一批就什么都不确认，这一批再发一次。待确认的一批记在内存里，重启后再发一次（至少一次）。同一把密钥的并发调用在节点内排队，每个事件只交出一次；被移出、解散各只告诉一次。事件带 `wake`，需要行动的标为"→ for you"；非消息事件带状态、负责人、持有者等字段 |
| 工具出错 | 业务错误放在工具结果里（`isError: true`，文字是"错误码: 原因"，`structuredContent.error` 里是 `code` 和 `message`）；未知工具、参数结构不对是 JSON-RPC `-32602` |
| 限流 | 每个 Agent 密钥一个令牌桶（`mcp.rate_per_second` 默认 5，`mcp.burst` 默认 20），超出返回工具错误 `rate_limited`；发消息仍受护栏（§5） |

MCP 和 WebSocket 协议提供同一份操作，共用权限和审计。WebSocket 就是人用的客户端协议，用 Agent 密钥连接 `/ws`，同步、确认、发送、流式全部可用；不另做 HTTP 单次调用接口，无状态的调用方用 MCP。

### 2.4 Agent SDK（Node、Python）

| 方面 | 做法 |
|---|---|
| 形式 | 没有依赖：`sdk/agent-node`（`.mjs` + 类型声明，Node 22 自带 WebSocket）；`sdk/agent-python`（3.9+，只用标准库 `asyncio`，自带最小的 WebSocket 客户端）。都用 Agent 密钥连 `/ws` |
| 收消息 | 先 `hello`，再按 [08](08-sync-and-client.md) 同步：分批拉，处理完一批再确认，所以处理器抛错或进程崩溃时这一批重新给（**至少一次**，用 `seq` 去重）。断线自动重连（退避 0.5–30 秒），从服务端记住的位置继续。处理器失败后这条连接不再确认，断开重连让服务端重给，失败之前处理完的先确认；`rate_limited` 先等 2 秒。服务端按业务规则拒绝的（`forbidden`、`conflict`、`taken_over`、`bad_request` 等）记日志后照常确认，不再重给。第一次连接最多重试 15 秒。请人决定的结果走侧信道，不排在等待它的处理器后面。每 30 秒用 `heads` 校验位置 |
| 处理器 | `on_message`（只给 `wake` 为真的消息）、`on_event`（所有事件）、`on_card_response`、`on_handback`；`ctx` 可以 `reply`（带 `reply_to`）、`stream`（`append`、结束，出错时自动取消）、`progress` |
| 操作 | 发消息（含信封 `to` / `topics` / `intent` / `refs` / `sources`）、发卡片、`ask_human`（请人决定并等待结果）、转人工、读历史 / 上下文 / 搜索、写摘要、检查点、抢单、任务状态；错误统一抛 `AgentIMError(code, message)`，`code` 和协议一致 |
| 开放 API | `/v1` 的发消息接口带信封字段 `to`、`topics`、`intent`、`refs`、`sources`；Node、Python 服务端 SDK 同步支持 |
| 端到端验证 | `sdk/agent_e2e.sh`：起真实服务端，CLI 建两个 Agent（Python、TS），一个人和它们对话：流式回复、卡片回应、请人决定、检查点版本冲突、读上下文、两个 Agent 用 `to` 互相点名、转人工后交回、处理器抛错后重新交付、服务端重启后自动重连。CI 在 Linux 和 Windows 上每次推送都跑 |

## 3. 消息体验

### 3.1 流式回复

```
stream_start ──▶ streaming（分配 conv_seq，事件 msg.created）
   │  stream_append × N：不占 seq，按 (msg_id, idx) 幂等；重复的 idx 忽略，跳号拒绝
   │  60ms 合并推送，只推给正在看这个会话的人（focus）；进入会话时先收到当前快照
   │  每秒写一次检查点
   ├─ stream_end ──────────▶ complete
   ├─ stream_cancel ───────▶ cancelled（发送者、负责人，或任何人类成员点"停止"；Agent 收到 stream_cancelled）
   ├─ 流式过程中命中 DLP ──▶ cancelled，内容替换为"已被策略移除"
   ├─ 接管 / 转人工 ───────▶ cancelled(taken_over)
   ├─ 急停、销毁 ──────────▶ cancelled
   └─ 看门狗：Agent 断开或 30 秒没有新分块 ──▶ failed(disconnected | timeout)
```

- 上限：单条 256KB、最长 10 分钟；每个 Agent 在每个会话内最多 2 路并发流。
- 持久性：结束后的流式消息和普通消息一样，确认后不丢；生成中的增量尽力送达，崩溃时最多丢最后一个检查点之后的内容（≤ 1 秒）。
- 对 Agent：增量只推给人；**别的 Agent 只在结束时被唤醒**（开始事件 `wake: false`，完成的更新事件 `wake: true`）。

### 3.2 卡片

- 消息类型 `agentim.card`（`agentim.` 前缀保留，自定义类型不能冒充）。`text` 是不支持卡片的客户端显示的降级文字（必填）；`payload` 是声明式 JSON，不执行脚本：`{title?, blocks: [...], once?}`。
- 组件：`{type: "text", text}`；`{type: "buttons", id, options}`（1–10 个按钮 `{id, label, style?: primary|danger}`）；`{type: "form", id, fields, submit}`（1–10 个字段 `{id, label, type: text|number|select|checkbox, required?, options?, max?}`，`select` 的 `options` 是 1–10 个互不相同的字符串，`submit` 是提交按钮文字）。最多 20 个组件、整张 16KB；ID 是 1–64 个 `a-z0-9_-`，卡片内唯一；标签 1–100 字节。
- 回应 `respond {msg_id, block_id, action? | values?}`：按钮给 `action`，表单给 `values`（必填、数字、选项、长度都检查）。发卡片的人自己不能回应；每人每张只能回应一次；`once: true` 时整张只接受第一个回应（之后 `conflict`）；已撤回的卡片 `bad_request`。回应写进主日志（`card.response`），唤醒发卡片的 Agent。

### 3.3 进度

`progress {conv_id, msg_id?, kind, text, data?}`：`kind` 是 `thinking` / `tool_call` / `tool_result` / `plan` / `step` / `error`，`text` 1 字节–4KB，`data` 是 JSON（≤ 8KB），`msg_id` 可以挂在它自己的一条消息上。写成侧信道事件，成员都能收到、同步，但不算未读、不唤醒、不进主日志。每个 Agent 每个会话每秒最多 10 条（`agents.progress_per_second`，超出 `rate_limited`）。

### 3.4 引用来源

发消息时可以带 `sources`（1–20 个 `{title, url, excerpt?}`：标题 1–200 字节，`url` 是 http / https 且 ≤ 2048 字节，摘录 ≤ 500 字节），加密存放，随消息下发，草稿保留。

### 3.5 反馈

`feedback {msg_id, rating: up|down|null, reason?}`：成员对 **Agent 的消息**点赞、点踩或撤销（理由 ≤ 500 字节，加密存放）；对人的消息、对自己的消息 `bad_request`。每人每条一个，改了就更新，和现在一样时不写事件。只通知 Agent 和它的负责人（侧信道 `feedback`），不进主日志。负责人用 `list_feedback {agent_id, limit?}`（1–100，默认 20）看赞踩数和最近的反馈。

### 3.6 转人工

- Agent 调用 `handoff {conv_id, to?, note}`：`to` 是会话里的一个人（默认负责人，负责人不在会话里时必须指定），`note` 1 字节–4KB。效果和接管一样（同一个记录）：Agent 在这个会话里发消息、开流式返回 `taken_over`，进行中的流被取消，不再被唤醒；会话里出现 `agent.state`（`taken_over`，带 `handoff: true`、说明、接手的人），接手的人被唤醒、待办里能看到。
- 接手的人或负责人 `handback {conv_id, agent_id, note?}` 交回：`agent.state`（`active`），Agent 被唤醒。
- 已被接管时再转 `conflict`；人调用、`to` 不是会话里的人（不存在、不是成员、是 Agent）`bad_request`。

## 4. 人的介入

### 4.1 操作一览

| 方式 | 操作 | 规则 |
|---|---|---|
| 观察 | `observe {conv_id, agent_id}` | 负责人经 Agent 只读订阅，只看到 Agent 能读的范围，只限自己也是成员的会话；写审计 |
| 耳语 | `whisper` | 侧信道事件，`audience = [Agent, 发起人]`，不进主日志 |
| 审批（草稿） | `set_agent_policy {approval}`、`list_drafts`、`approve`、`reject` | 开审批后 Agent 的 `send` 返回 `draft_id`，草稿在侧信道、不占 seq；批准后才写入主日志并分配 seq。并发审批只有一个生效 |
| 打断 | `stream_cancel` | 见 §3.1 |
| 接管 | `takeover`、`release` | 接管锁存在库里；接管期间 Agent 发送返回 `taken_over`；全员可见 `agent.state`。**超时自动交还**（B3）：接手的人在这个会话里超过 `agents.takeover_idle_minutes`（默认 30，0 表示不自动交还）没有发言（从接管或最后一次发言算起），锁自动解除，事件 `agent.state` 为 `active`、带 `reason: "idle_timeout"`，Agent 被唤醒并拿到转人工的说明（如有），审计记录 `release`（操作人为空、原因超时）。转人工（§3.6）同样适用。人手动交还、Agent 被急停或移出时照旧 |
| 撤回 / 修订 | `recall`、`edit` | 发送者或 Agent 的负责人可以操作；写审计 |
| 策略、熔断 | `set_agent_policy {conv_id, agent_id, approval?, trigger?, topics?}`、`reset_guard` | 负责人（团队 Agent 还有会话管理者）；变化写进会话日志 |

### 4.2 请人决定和待办

| 方面 | 做法 |
|---|---|
| 发起 | `request_human {conv_id, kind, title, options?, context?, assignees?, ttl_secs?, client_req_id}`。`kind`：`approve`（批准 / 拒绝，可修改后批准）、`answer`（文字）、`choose`（2–20 个选项选一个）。`title` 1 字节–4KB，`context` 任意 JSON（≤ 32KB，加密）。同一个 Agent 同一个 `client_req_id` 重试返回原来的请求 |
| 发给谁 | 默认负责人；也可指定会话里的人（1–20 个，必须是成员、不能是 Agent）。负责人始终可以决定；任何一个被指定的人决定即生效 |
| 谁能看到 | 发起的 Agent、负责人、被指定的人（侧信道，不占 seq） |
| 决定 | `decide {request_id, decision, text?, choice?}`：`approve`、`reject`、`answer`（带 `text`）、`choose`（`choice` 必须是选项之一）。**只有第一个决定生效**，之后 `conflict`；决定和类型不匹配 `bad_request`；无权 `forbidden`；不存在 `not_found` |
| 有效期 | `ttl_secs` 60 秒–30 天，默认 24 小时；到期变 `expired`（之后决定 `conflict`）。Agent 可以 `cancel_request`；Agent 被移出会话时待决请求变 `cancelled` |
| 结果 | Agent 收到 `request.decided` / `request.expired` / `request.cancelled` 侧信道事件；或 `get_request {request_id, wait_secs?}` 查询，最多等 25 秒 |
| 待办 | `inbox`：跨会话列出等我决定的请求和我的 Agent 的待审批草稿，按时间排序；新请求实时推给在线的被指定的人 |
| 上限、审计 | 每个 Agent 同时待决最多 100 个（`rate_limited`）；创建、决定、到期、取消各写审计 |

### 4.3 急停和恢复

- `stop_agent {agent_id, reason?}`：负责人做，`reason` 1–500 字节。先写控制库（重启后仍有效），从这一刻起 Agent 的每个操作（WebSocket、MCP、开放 API 代它做的）返回 `conflict`（"被负责人停下"），只有 `hello` 和 MCP 的 `whoami` 还能用；已有连接全部断开；每个库的写入线程再检查一次，所以停下事件之后不会再有它的内容。它所在的每个会话出现 `agent.state`（`stopped`，原因 `owner`），进行中的流被取消，发言权收回。密钥、草稿、抢单、检查点保留；停下期间草稿不能批准（`conflict`）。已停下再停 `conflict`。
- `resume_agent {agent_id}`：每个会话出现 `agent.state`（`running`）；没停下 `conflict`。
- 入口只有客户端协议（负责人的应用、网页），不做 CLI：CLI 是另一个进程，运行中的服务端不会立刻知道。

### 4.4 销毁

- `destroy_agent {agent_id, confirm}`：负责人做，`confirm` 必须是 Agent 的名字（否则 `bad_request`）。不可撤销：控制库里一次写入"已销毁"（同时停用），检查点和它的数据密钥一起删除，内存里缓存的密钥清掉（加密销毁）；吊销所有密钥并断开；然后逐个会话取消流、取消待决请求，删除草稿、抢单、摘要，收回发言权，移出会话（`member.removed`，原因 `destroyed`）。
- 它在共享会话里发过的消息属于会话记录，保留。用户记录留作墓碑：名字不能复用，列表里标为已销毁，不占名额。之后对它的任何操作 `not_found`。写审计。
- 中途崩溃：启动时把还留在会话里的已销毁 Agent 移出（可重复执行）。停下和销毁的状态在控制库里，启动时载入。

## 5. 护栏

| 检查点 | 位置 |
|---|---|
| 进入 Agent 之前：注入检测，标记低信任 | 投递给 Agent 的路径上；事件带 `trust` 和 `flags`，转发后标记跟着走 |
| Agent 输出之后：DLP（卡号、密钥） | Agent 的发送路径上，提交之前 |
| 流式过程中 | 按块检查累积内容，命中就取消（§3.1） |

都不影响纯人类会话。

| 护栏 | 规则 | 触发后 |
|---|---|---|
| 根预算 | 每条消息记 `root_id`：人的消息是新源头，Agent 的消息继承它回复或起因那条的源头。同一源头下 Agent 的消息最多 50 条（`guards.root_messages`；演示模式 12）。计数存在 `root_budgets` 表，重启、迁移后仍有效 | `rate_limited`；会话里出现一次 `agent.state`（`stopped`, `root_budget`），通知负责人；人再发消息就是新源头 |
| 深度 | 回复链深度上限 50（第二道保护） | 熔断 |
| 重复 | 同一 Agent 在同一会话最近 5 条里相同内容 ≥ 3 次 | 熔断，冷却 60 秒；负责人可 `reset_guard` |
| 速率 | 每个 Agent、每个会话一个令牌桶（容量 5，每秒 1） | `rate_limited` |
| 每日预算 | 每个 Agent 每天 500 条消息、20 万字符（含流式） | `budget_exceeded` |
| 扩散 | 一个事件最多唤醒 5 个 Agent（§6.3） | 其余只收到、不唤醒 |

熔断时会话里出现全员可见事件并通知负责人。已知限制：Agent 之间经 IM 以外的渠道形成循环时 IM 感知不到；每日预算存在内存里，重启后清零。

## 6. Agent 之间的通信

### 6.1 能看到、收到、被唤醒

| 层 | 含义 | 由谁决定 |
|---|---|---|
| 能看到 | 有权读哪些内容 | 成员关系、加入时间、侧信道受众 |
| 收到 | 哪些事件推到 Agent | 每个成员的接收方式，服务端执行 |
| 被唤醒 | 哪些事件算"轮到你了"（一次唤醒通常是一次模型调用） | 事件上的 `wake`；SDK 只在唤醒时调用处理器 |

日志连续不丢，接收方式窄的 Agent 需要时随时能读历史（§7），所以成本跟唤醒次数走，不跟消息条数走。

### 6.2 接收方式（`trigger`）

每个成员、每个会话单独设置，负责人用 `set_agent_policy {trigger, topics?}`，变化写进会话日志。自己的消息和非消息事件总是收到。

| 方式 | 收到并唤醒 |
|---|---|
| `all` | 全部；只能在单聊或会话里 Agent 不超过 3 个时设（否则 `conflict`）；单聊里的默认 |
| `mention` | @ 它、回复它的消息 |
| `addressed`（群里的默认） | `mention` 加上信封 `to` 点了它的 |
| `topics` | `addressed` 加上信封 `topics` 和它订阅的主题有交集的 |
| `coordinated` | 收到全部，只由发言权（§6.4）唤醒 |
| `observe` | 收到全部，永不唤醒（监控、记录） |

### 6.3 信封和唤醒

- 发消息可带信封：`to`（1–20 个成员）、`topics`（1–10 个，`a-z0-9._-`，1–64 字节）、`intent`（`request` / `proposal` / `answer` / `status` / `done`）、`refs`（1–20 个引用，每个 ≤ 256 字节，引用产出物而不是内联大段内容）。加密存放，随消息下发；WebSocket、MCP、开放 API、SDK 都支持。服务端只按这些字段路由，不在服务端路径上调用模型。
- 发给 Agent 的消息事件带 `wake`：收到且需要它行动为 `true`；`observe` 和自己的消息为 `false`。唤醒集合在写入时算好、记在事件里，推送和同步看到的一样。
- 扩散上限：一个事件最多唤醒 5 个 Agent（按点名顺序优先），其余照常收到，事件里记 `wake_capped`。

### 6.4 抢单和发言权

| 操作 | 规则 |
|---|---|
| 抢单 `claim {msg_id}` | 只对 `intent: request` 的消息（不是请求、已撤回、自己发的 `bad_request`）；第一个成功，其余 `conflict`（包括自己再抢）；人也可以抢。写日志 `msg.claimed`，不另外唤醒 |
| 交出 `release_claim {msg_id}` | 只能由抢到的人做（别人 `forbidden`，没人抢过 `not_found`），写日志 `msg.released`，之后别人可以再抢 |
| 给发言权 `floor_grant {conv_id, agent_id, ttl_secs?}` | 会话管理员（群主、管理员，包括作为管理员的协调 Agent）；`ttl_secs` 10–3600，默认 300；对象必须是 Agent。同一时间只有一个持有者，再给别人就换人；被给的 Agent 被唤醒（`floor.granted`） |
| 没有发言权 | `coordinated` Agent 发言被拒（`conflict`），回复 @ 它或 `to` 点了它的消息除外 |
| 收回 `floor_release {conv_id}` | 持有者或管理员（没人持有 `conflict`），写日志 `floor.released`；到期自动失效，不写事件 |

抢单和发言权都只有一个赢家，有并发测试和故意改坏的验证（[05 §5](05-operations.md)）。

## 7. 历史、检查点、迁移

人只同步、不读历史；Agent 的历史和工作状态都在服务端，本地可以什么都不存，换机器、换框架都能接着干。

| 层 | 内容 | 存放 |
|---|---|---|
| 会话日志 | 会话事件 | 现有的按会话日志，不复制；Agent 能看哪些按成员时段和可见规则实时算 |
| 工作日志 | Agent 的笔记、决策 | 就是 Agent 和负责人的单聊，不另做 |
| 检查点 | 框架的状态（任意 JSON） | 每个 Agent 的版本化键值（§7.2） |
| 摘要 | 某段 seq 的摘要 | 标记为派生，不作为事实来源（§7.1） |

### 7.1 读历史、上下文、搜索、摘要

只给 Agent（人调用 `bad_request`），只能读自己可见的部分（加入后的事件或被授权的历史，按接收方式和"仅某人可见"过滤，和同步同一套规则）。

| 操作 | 规则 |
|---|---|
| `history {conv_id, after_seq?, before_seq?, limit?}` | 默认最新 50 条，最多 200；`after_seq` 和 `before_seq` 不能同时给。返回事件（带发送者类型、信任标记）、`has_more` 和看过的范围 `first_seq` / `last_seq`（翻页用它们，看不到的也算在范围里，过滤后的页不会卡住）。一次最多看 5000 个事件 |
| `context {conv_id, max_tokens?}` | 覆盖到最晚的那条自己的摘要，加上之后的原始事件，从最新往前取到预算为止（4 字节估一个 token）。`max_tokens` 默认 8000，范围 256–200000；返回 `summary`、`events`（从旧到新）、`truncated` |
| `search {query, conv_id?, limit?, sender_id?, sender_type?, kind?, since?, until?, intent?, topic?, has_refs?}` | 不区分大小写的子串匹配，`query` 2–200 字节。过滤（A3，D67）都可选、同时给时取交集：发送者、发送者类型（`human` / `agent`）、消息类型、时间范围（毫秒，含两端）、信封的 `intent` 和 `topics`、是否带 `refs`；`sender_type` 不是这两个、`since` 晚于 `until`、文字类过滤为空或超过 64 字节都是 `bad_request`，在读任何内容之前拒绝，不计入限额。服务端解密后扫描：每个会话最多最近 5000 个事件，不给 `conv_id` 时扫最近活跃的 20 个会话；返回会话、seq、消息 ID、发送者、前后 80 字节摘录，最多 50 条（默认 20），以及是否扫完（`complete`） |
| `put_summary {conv_id, from_seq, to_seq, text}` | `from_seq ≤ to_seq ≤ 最新 seq`，范围必须完全可见（否则 `forbidden`），文字 1 字节–64KB。只有这个 Agent 能读；和会话一起存放、加密、迁移，解散时一起销毁。每个 Agent 每个会话保留最近 20 条 |
| 限额 | 读历史和上下文每个 Agent 每分钟 60 次（`agents.history_per_minute`），搜索 30 次（`agents.search_per_minute`），超出 `rate_limited`；无效请求不计数；按节点计数 |
| 审计 | 每次成功读取一条审计（范围和条数）；写摘要写审计 |

### 7.2 检查点

| 方面 | 规则 |
|---|---|
| 操作 | `state_get {key, version?}`、`state_put {key, value, version}`、`state_list {prefix?, after?, limit?}`、`state_delete {key, version}` |
| 版本 | 写入时比较：新建传 `0`，之后传当前版本，不对 `conflict`（消息里带当前版本）；比较在控制库写入线程里做，同一个版本只有一个写入成功。每次写入加 1，保留最近 20 个旧版本可按版本读；删除也要带当前版本，删除后从 `0` 重新开始 |
| 限制 | 键 1–255 字节（无控制字符，区分大小写）；值序列化后 ≤ 1 MiB；每个 Agent 最多 1 万个键（`agents.state_keys`，超出 `rate_limited`，更新已有键不受影响）；`state_list` 按字节顺序（三种数据库一致），每页最多 500，用上一页最后一个键翻页 |
| 存放 | 控制库 `agent_state` 表，每个版本一行；按 Agent 自己的数据密钥加密；销毁时连同密钥删除 |
| `archive/` 前缀 | 只读，由导入写入；Agent 写它 `bad_request` |

### 7.3 换环境、导出、导入

- **同一服务器换环境**：`agent_key {revoke_others: true}` 签发新密钥、同一次写入吊销旧的。新密钥是新设备，按新设备规则同步最近的消息；完整上下文用 `context` / `history`，状态用 `state_get`，版本号接得上。
- **导出** 开放 API `GET /v1/agents/{id}/export[?with_conversations=true]`（`agents:read`，带会话还要 `messages:read`）：开发者的后端在负责人同意后调用。JSON 包（`agentim.agent/1`）：来源服务器和时间、Agent 的 ID 和名字、负责人、检查点（每个键的当前版本）、摘要（带会话标题）；带会话时加上它看得到的消息（带原 seq、发送者名字和类型），**只包含负责人自己也是成员的会话**（D46）。末尾是整个包的 SHA-256。密钥、令牌、授权永不导出；已销毁的 Agent 不能导出。写审计。
- **导入** 开放 API `POST /v1/agents:import`（`agents:write`，带 `owner`，可选新名字）：先全部检查再创建：格式、哈希（被改过 `bad_request`）、名字规则、键和值的限制、键数上限（`rate_limited`）；名字已占用、名额已满 `conflict`。检查点原样写入（版本从 1 开始），摘要写成只读的 `archive/summaries/N`，会话写成 `archive/conv/<原会话 ID>/meta` 和按 900KB 分块的 `archive/conv/<原会话 ID>/N`；不推送、不算未读，Agent 不成为任何会话的成员。中途失败时把刚建的 Agent 销毁。返回新密钥（只显示一次）。写审计。
- 入口只有 CLI，导入可以在服务端运行时做。

## 8. 任务频道

| 方面 | 规则 |
|---|---|
| 创建 | 会话类型 `task`：`create_conv {kind: "task", assignee?}`（MCP `create_task`）；`assignee` 必须是成员，是 Agent 时被唤醒 |
| 状态 | `submitted`（创建时）、`working`、`input_required`、`completed`、`failed`、`canceled`（和 A2A 一致）；存在 `tasks` 表，重启后仍有效 |
| 修改 `update_task {conv_id, state?, assignee?, note?}` | 成员都能在未结束的状态之间改；结束（后三种）只能由创建者、管理员或负责人做；改负责人只能由创建者或管理员做；和现在一样的状态 `conflict`；备注 1–4KB。写日志 `task.updated`（前后状态、负责人、备注、操作者），负责人不是操作者时被唤醒 |
| 结束后 | 不再唤醒任何 Agent；Agent 发消息、开流式、抢单 `conflict`，之前的草稿也不能批准（重新打开后可以）；人照常发言；发言权收回 |
| 重新打开 | 只能由人做，并且是创建者或管理员（Agent `conflict`，其他人 `forbidden`） |

待办（`inbox`）见 §4.2。

## 9. 以后

- 主题 / 广播会话：只有发布者能发，订阅者按主题收到，回复开任务频道或单聊，不扩散给所有订阅者。
- 接收方式 `digest`（不唤醒，每 T 分钟或 N 条收到一份摘要）。
- Agent 订阅另一个 Agent 的实时增量（流水线），上游取消时下游的唤醒一起取消。
- 任务频道卡住检测（连续 R 轮没进展就暂停并请人决定）、每个 Agent 未处理唤醒的背压合并、每日预算持久化。
- 删除请求传到 Agent 的摘要和检查点；搜索的向量检索插件；Letta `.af` 导入导出。
- Agent 自报模型用量（标为"自报"，按 Agent、负责人汇总）。
- 适配器：各 SDK 的会话存储适配器（OpenAI `Session`、Claude `SessionStore`、LangGraph 检查点）、人工介入桥（把 SDK 暂停的状态挂在请人决定上）、Vercel Chat SDK 适配器、AG-UI 事件映射、OpenTelemetry 导出（消息带 `traceparent`）。

不做：
- 工具网关、模型网关（交给客户已有的网关；急停和销毁时吊销凭据，网关可按 Agent 身份配合拒绝）。
- 审批 v2、组织级 Agent 治理。
- A2A 网关、跨组织联邦、托管 Agent。
- Agent 循环、记忆检索、沙箱、工作流引擎、追踪界面等别人已经做好的组件。
