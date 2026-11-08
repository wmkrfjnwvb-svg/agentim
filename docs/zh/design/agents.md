# Agent

Agent 作为会话成员：身份和负责人、怎么接入、角色和配置、Agent 之间怎么协作、历史和状态、怎么搬走，以及它们周围的护栏。人对 Agent 的控制（草稿、请人决定、接管、急停、销毁）见 [approvals](approvals.md)；服务端自己跑的助手见 [assistants](assistants.md)。

代码：`server/crates/agentim-core/src/hub/agents.rs`、`hub/history.rs`、`hub/state.rs`、`hub/roles.rs`、`hub/lifecycle.rs`、`hub/ai.rs`、`hub/friends.rs`、`logic/routing.rs`、`logic/tasks.rs`、`logic/history.rs`、`logic/state.rs`、`logic/ai.rs`、`mcp.rs`、`guard.rs`、`role.rs`；SDK `sdk/agent-node`、`sdk/agent-python`。

## 1. 用途

任何框架写的 Agent 都能和人一起加入会话。服务端保证它能强制的事：
- Agent 能读什么；
- 它的消息标为 AI；
- 它不能冒充人；
- 它的负责人能看到、也能停下它做的事；
- Agent 之间对话不会失控。

Agent 内部的东西（模型、工具、它连的其他系统）在 AgentIM 之外。Agent 仍然可以请人批准一个操作（[approvals](approvals.md)）；对它自己的工具来说这是协作，不是强制。

## 2. 功能点

| 编号 | 功能 | 行为 |
|---|---|---|
| AGT-01 | 身份 | 发送者是 `human`、`agent` 或 `system`，由服务端写入。Agent 只能代表它的负责人（`on_behalf_of`）。AI 总是有标识（欧盟 AI 法第 50 条） |
| AGT-02 | 负责人：个人助理和团队 Agent | 每个 Agent 都有负责人。个人助理（默认）随负责人离开，负责人被停用时它也停下。团队 Agent 属于群或单元，负责人离开时它留下，可以转交 |
| AGT-03 | 负责人的权力只在他也在的会话里 | 代看、接管、耳语、改策略、重置熔断、审批草稿，只在负责人也是成员的会话里 |
| AGT-04 | 创建和密钥 | `create_agent`、开放 API、控制台或 `agentim admin` 创建 Agent，返回只显示一次的密钥和 MCP 配置块。密钥只存哈希，一把密钥一台设备。只有负责人能换和吊销。`agents.max_per_owner`（20） |
| AGT-05 | 读取范围和告知 | Agent 只能读加入以后的内容，加上拉它的人授权的历史。加入是一个所有人可见的事件，写明负责人和读取范围 |
| AGT-06 | Agent 开会话 | 只能找已经认识的人：负责人、负责人的其他 Agent、已经在同一个会话里的人。按小时限流 |
| AGT-07 | MCP 服务 | `POST /mcp`（无状态，当前 MCP 规范，也接受老版本）。只接受 Agent 密钥，人的令牌返回 `403`。工具覆盖消息、人、请求、任务、历史、状态、文件、卡片、配置 |
| AGT-08 | WebSocket 和 Agent SDK | Agent 用密钥连和客户端一样的 `/ws`。Node 和 Python SDK（没有依赖）提供至少一次的处理器、自动重连、流式、卡片、文件、请求、状态 |
| AGT-09 | 角色和配置 | 角色是一个 Markdown 文件（头部 + 指令，和 Claude Code 的 agents 格式一样）。配置通过 `welcome.config`、`get_config`、MCP 或 `GET /api/agent/config` 发给 Agent；改了推 `config.updated` |
| AGT-10 | 接收方式 | 每个成员在每个会话单独设：`all`、`mention`、`addressed`（群里默认）、`topics`、`coordinated`、`observe`。决定什么会送到、什么会唤醒 Agent |
| AGT-11 | 信封和唤醒 | 消息可以带 `to`、`topics`、`intent`、`refs`。唤醒集合在写入时算好并记在事件上；一个事件最多唤醒 5 个 Agent |
| AGT-12 | 抢单和发言权 | 对 `intent: request` 的消息 `claim`：只有一个成功。`floor_grant` 在协调模式的会话里把发言权给一个 Agent |
| AGT-13 | 任务频道 | `task` 类会话，状态 `submitted`、`working`、`input_required`、`completed`、`failed`、`canceled`。任务结束后 Agent 不再说话 |
| AGT-14 | 历史、上下文、搜索、摘要 | 只给 Agent，只能读它能看的：按序号翻页、从最近的摘要开始按 token 预算取上下文、带过滤的搜索、每个 Agent 自己的摘要。限流并审计 |
| AGT-15 | 检查点 | 每个 Agent 一个带版本的键值存储（比较后写入、保留 20 个旧版本、值 1 MiB、最多 1 万个键），用 Agent 自己的密钥加密 |
| AGT-16 | 搬走和导出 | 换一把新密钥就能在别处接着干。导出签名的包（状态、摘要，可选会话），在另一台服务器导入；验证不了来源的包要明确同意 |
| AGT-17 | 丰富的回复 | 流式、卡片、进度和步骤、来源、反馈、转人工（[chat-components](chat-components.md)、[approvals](approvals.md)） |
| AGT-18 | 护栏 | 每条链的根预算（50）、回复深度、重复内容熔断、每个会话的速率、每日预算、每个事件最多唤醒 5 个、输入的注入标记、输出的 DLP、流式过程中检查 |
| AGT-19 | 好友 | 双向好友，有加好友方式（`anyone`、`confirm`、`nobody`）、申请、删除；拉黑会解除好友。App Agent 从 `list_app_agents` 里加为好友。应用可以要求只有好友才能开单聊 |

## 3. 设计

### 3.1 负责人

| | 个人助理（默认） | 团队 Agent |
|---|---|---|
| 是什么 | 某个人的助理 | 群或单元的机器人（客服、发布、知识库）；负责人只是维护者 |
| 谁能建 | 被允许的任何人（`create_agent`） | 只有开发者：开放 API `kind: agent, team: true`、控制台、`agentim admin agent-create --team` |
| 负责人离开或被移出 | 一起离开（`owner_left`） | 留下；负责人对它的接管结束 |
| 负责人被停用 | 停下，重启后也停着 | 照常运行，标记出来由管理员转交 |
| 转交 | 不能 | 开放 API `POST /v1/agents/{id}:transfer`、控制台、命令行；可选同时换密钥 |

开着审批的 Agent，在负责人不在的会话里不能说话（`conflict`）：没人能审批。

### 3.2 接入

- **密钥**：Agent 密钥就是它的客户端令牌，只存哈希；每把密钥是一台设备，有自己的同步位置。
  - `agent_key {revoke_others}` 签发新密钥，并在同一次写入里吊销其他的。同时换密钥时，最后写入的那把是唯一有效的。
  - 别的进程（命令行）建的密钥，服务端第一次用到时载入，按地址限流；集群里各节点立即互相通知。
- **MCP**（`POST /mcp`）：无状态。每个请求带版本，不支持的版本返回支持的列表。
  - 业务错误作为工具结果返回（`isError`，`structuredContent.error` 里有 `code`、`message`、`rule`）。
  - `get_updates` 就是同步：一批由下一次调用确认，所以丢了的回复会再来一次。
  - 限流：每把密钥每秒 5 次、突发 20。
- **WebSocket**：用 Agent 密钥走客户端协议，同步、确认、发送、流式都能用。无状态的调用方用 MCP。
- **Agent SDK**（Node 22+、Python 3.9+，没有依赖）：
  - 先 `hello`，再分批同步。处理器做完一批才确认，所以崩溃后这批会重新给（至少一次；按序号去重）。
  - 自动重连（退避 0.5–30 秒），从服务端记住的位置继续。
  - 处理器：`on_message`（只给唤醒的消息）、`on_event`、`on_card_response`、`on_handback`、`on_config`。
  - `ctx` 提供 `reply`、`stream`、`progress`、`steps`、`showCard`、`askForm`、`sendFile`、`request_human` 和状态。

### 3.3 角色和配置

- 角色是 Markdown。头部必须有 `name` 和 `description`；`tags`、`tools`、`model`、`skills`、`mcp`、`apis` 照存；正文是指令。
  - 只接受简单的 YAML（`键: 值`、列表、一层映射），别的写法报 `role.header`。
  - 名字是 1–64 个 `a-z 0-9 -`。
- 角色由开放 API `/v1/roles/{name}` 和控制台管理；在用的角色不能删（`409 role.in_use`）。
- 给 Agent 角色：`PUT /v1/agents/{id}/role`、控制台，或负责人的 `set_agent_role`。
- 配置 `{version, role, skills, mcp, apis}` 发给 Agent 的方式：
  - `welcome.config`；
  - `get_config` 和 MCP；
  - `GET /api/agent/config`，带 ETag。

  改了以后，这个 Agent 在所有节点上的每个连接都收到 `config.updated`，SDK 自动重取。

### 3.4 Agent 之间的协作

Agent 能拿到什么由三层决定：
- **能看到**：成员关系、加入时间、侧信道的可见者；
- **收到**：接收方式；
- **被唤醒**：事件上的 `wake` 标记。

日志始终完整，所以成本跟唤醒次数走，不跟消息条数走。

| 方式 | 收到并被唤醒 |
|---|---|
| `all` | 全部。只能在单聊或 Agent 不超过 3 个的会话里设；单聊默认 |
| `mention` | @ 它、回复它的消息 |
| `addressed` | `mention`，加上信封 `to` 点了它的（群里默认） |
| `topics` | `addressed`，加上信封 `topics` 和它订阅的主题有交集的 |
| `coordinated` | 收到全部，只由发言权唤醒 |
| `observe` | 收到全部，从不唤醒 |

- **信封**：`to`（1–20 个成员）、`topics`（1–10 个）、`intent`（`request`、`proposal`、`answer`、`status`、`done`）、`refs`（1–20 个产出物的引用，不内联大段内容）。加密存放，随消息下发。
- **唤醒集合**：写入时算好并记在事件上，所以推送和同步看到的一样。流式消息只在结束时唤醒别的 Agent。最多唤醒 5 个（点名的优先），其余的记在 `wake_capped`。
- **抢单**：第一个 `claim` 成功，其余 `conflict`；`release_claim` 交出后别人可以再抢。日志事件 `msg.claimed`、`msg.released`。
- **发言权**：管理员把发言权给一个 Agent，10–3600 秒（默认 300）。没有发言权的协调模式 Agent 说话返回 `conflict`，回复 @ 它或 `to` 点了它的除外。

### 3.5 历史、状态和搬走

Agent 的历史和状态都在服务端，换机器、换框架都能接着干。

| 操作 | 规则 |
|---|---|
| `history {conv_id, after_seq?, before_seq?, limit?}` | 默认最新 50 条，最多 200。返回 `has_more`、`first_seq`、`last_seq`。一次最多看 5000 个事件 |
| `context {conv_id, max_tokens?}` | Agent 最近的摘要，加上之后的原始事件，从新到旧取到预算为止（默认 8000，256–200000；4 字节算一个 token） |
| `search {query, conv_id?, …过滤}` | 不区分大小写的子串，2–200 字节。过滤可以组合：发送者、发送者类型、消息类型、时间范围、intent、topic、`has_refs` |
| `put_summary {conv_id, from_seq, to_seq, text}` | 范围必须完全可见。最多 64 KB；每个 Agent 每个会话保留最近 20 条。随会话搬走、随会话销毁 |
| 限额和审计 | 每个 Agent 每分钟读 60 次、搜索 30 次。每次成功读取写一条带范围的审计 |
| 状态 | `state_get/put/list/delete`，比较版本后写入（新建传 `0`），保留 20 个旧版本。键 1–255 字节，值 ≤ 1 MiB，最多 1 万个键。存在控制库，用 Agent 自己的密钥加密，销毁时一起销毁。`archive/` 前缀只由导入写 |
| 导出 | `GET /v1/agents/{id}/export[?with_conversations=true]`：`agentim.agent/1` 包，含状态、摘要，可选它能看到的消息；带哈希，用签名密钥签名 |
| 导入 | `POST /v1/agents:import`：先全部检查再创建。改了内容又重算哈希的包签名对不上：`bad_request`。没签名或别处签名的包要 `allow_unverified` |

### 3.6 任务频道

- `create_conv {kind: "task", assignee?}` 建任务频道；MCP 有 `create_task`。
- 任何成员都能在未结束的状态之间改。结束（`completed`、`failed`、`canceled`）只能由创建者、管理员或负责人做。
- 结束后 Agent 不再被唤醒；它的发送、流式、抢单、草稿批准都返回 `conflict`；人照常说话。
- 只有人（创建者或管理员）能重新打开。

### 3.7 护栏

| 护栏 | 规则 | 触发后 |
|---|---|---|
| 根预算 | 每条消息有 `root_id`：人的消息开一个新根，Agent 的消息继承。每个根下 Agent 最多 50 条（`guards.root_messages`），存在 `root_budgets` | `rate_limited`，一条所有人可见的 `agent.state`（`root_budget`），通知负责人 |
| 深度 | 回复链最多 50 层 | 熔断 |
| 重复 | 同一 Agent 在一个会话最近 5 条里相同内容 ≥ 3 次 | 熔断，冷却 60 秒；负责人可以 `reset_guard` |
| 速率 | 每个 Agent 每个会话：突发 5、每秒 1 | `rate_limited` |
| 每日预算 | 每个 Agent 每天 500 条、20 万字符（含流式），跨会话和库合计 | `budget_exceeded` |
| 扩散 | 每个事件最多唤醒 5 个 | 其余只收到不唤醒 |
| 注入 | 给 Agent 的输入做检查，标记为低信任 | 标记随事件走 |
| DLP | Agent 的输出（消息、编辑、仅某人可见、payload、流式）检查卡号和密钥 | 拒绝，或取消流并替换内容 |

纯人类的会话不跑这些。

### 3.8 好友

- 好友是双向的，每对一行。`friend_request` 按对方的方式处理：
  - `anyone`：立即成为好友；
  - `confirm`（人的默认）：给对方一条申请，对方同意或拒绝；
  - `nobody`：`forbidden`。

  双方同时互加直接成为好友。
- 拉黑会解除好友并拒绝申请。
- `friend_remove` 解除好友，已有的单聊保留。
- 后端用 `PUT /v1/users/{id}/friends/{other}` 导入好友关系。
- App Agent 默认 `anyone`，由 `list_app_agents` 列出。
- `clients.allow.direct_to_non_friends = false` 时，单聊和拉人只能对好友。
- Webhook：`friend.requested`、`friend.accepted`、`friend.declined`、`friend.removed`。

## 4. 接口

| 接口 | 操作 |
|---|---|
| 客户端协议（Agent 和负责人） | `create_agent`、`agent_key`、`revoke_agent_key`、`list_agents`、`set_agent_role`、`list_roles`、`get_config`、`set_agent_policy`、`reset_guard`、`observe`、`history`、`context`、`search`、`put_summary`、`state_get`、`state_put`、`state_list`、`state_delete`、`claim`、`release_claim`、`floor_grant`、`floor_release`、`update_task`、`progress`、`feedback`、`list_feedback`、`friend_request`、`friend_accept`、`friend_decline`、`friend_remove`、`list_friends`、`set_friend_policy`、`list_app_agents` |
| MCP 工具 | `whoami`、`list_conversations`、`get_updates`、`send_message`、`edit_message`、`recall_message`、`send_card`、`progress`、`send_file`、`file_url`、`find_user`、`open_direct`、`create_group`、`add_member`、`request_human`、`get_request`、`cancel_request`、`handoff`、`create_task`、`update_task`、`claim`、`release_claim`、`floor_grant`、`floor_release`、`get_history`、`get_context`、`search`、`put_summary`、`state_get`、`state_put`、`state_list`、`state_delete`、`get_config` |
| 开放 API | `PUT /v1/users/{id}`（`kind: agent`）、`/v1/agents/{id}/keys`、`:transfer`、`/role`、`/export`、`/v1/agents:import`、`/v1/roles…`、`/v1/users/{id}/friends…` |

## 5. 规则和错误

规则前缀：`agent.*`、`role.*`、`config.*`、`task.*`、`history.*`、`state.*`、`friend.*`（[目录](../../en/business-rules.md)）。Agent 调只给人的操作（以及反过来）返回 `forbidden` 或 `bad_request`；被停下的 Agent 返回 `conflict`；已销毁的 `not_found`；超过护栏 `rate_limited` 或 `budget_exceeded`。

## 6. 测试

路径在 `server/crates/agentim-core/tests/` 下；三种数据库。

| 功能点 | 测试 |
|---|---|
| AGT-01 | `scenarios.rs`: `on_behalf_of_is_only_for_an_agent_speaking_for_its_owner`, `humans_are_not_rate_limited_but_agents_are`; `agents.rs`: `each_kind_is_refused_what_belongs_to_the_other` |
| AGT-02 | `team_agents.rs`: 全部 10 个测试; `audit.rs`: `a_persons_agents_leave_with_them`, `deactivating_an_owner_stops_their_agents`; `developer_api.rs`: `team_agents_are_transferred` |
| AGT-03 | `team_agents.rs`: `owners_act_on_an_agent_only_where_they_are_members`, `side_entries_skip_an_absent_owner`; `scenarios.rs`: `agent_policy_is_owner_only`; `coverage.rs`: `agent_policy_targets_only_your_own_agents`, `observing_through_an_agent_is_owner_only_and_audited`, `focus_through_an_agent_and_focus_errors` |
| AGT-04 | `agents.rs`: `a_person_creates_an_agent_with_a_working_key`, `keys_are_rotated_and_revoked_by_the_owner_only`, `key_limits_and_deactivated_agents`, `keys_and_rotations_survive_a_restart`, `keys_made_by_another_process_are_found`, `race_rotations_leave_exactly_one_key`, `race_creations_respect_the_cap`, `refused_creations_leave_no_trace`; `developer_api.rs`: `agents_are_handed_out_and_listed`; `permissions.rs`: `race_creating_agents_while_it_is_revoked` |
| AGT-05 | `scenarios.rs`: `late_agents_do_not_see_history_from_before_they_joined`; `membership.rs`: `a_re_added_agent_sees_only_what_follows_its_return`; `audit.rs`: `an_own_agent_sees_no_more_than_its_adder` |
| AGT-06 | `agents.rs`: `agents_start_conversations_only_with_known_contacts`, `agent_conversations_are_rate_limited`, `race_agents_open_one_direct_conversation` |
| AGT-07 | `mcp.rs`: 全部 16 个测试 (discovery and versions, keys only, tools follow the rules, updates once in order, lost answers come again, rate limits, files, history, checkpoints, requests, tasks, cards) |
| AGT-08 | `ws.rs`: `websocket_auth_request_response_and_push`; `sdk/agent-node/e2e.mjs`, `sdk/agent-python/e2e.py`, `sdk/agent_e2e.sh` (a real server, two agents, streaming, cards, requests, state conflicts, hand-off, handler errors redelivered, server restart), `sdk/agent-node/files.test.mjs` |
| AGT-09 | `roles.rs`: 全部 6 个测试 |
| AGT-10, AGT-11 | `routing.rs`: 全部 8 个测试; `hub.rs`: `group_agents_in_mention_mode_only_hear_what_is_addressed_to_them` |
| AGT-12, AGT-13 | `tasks.rs`: 全部 7 个测试 |
| AGT-14 | `history.rs`: 全部 11 个测试 |
| AGT-15 | `state.rs`: 全部 7 个测试 |
| AGT-16 | `migrate.rs`: 全部 7 个测试; `developer_api.rs`: `agents_move_by_export_and_import` |
| AGT-17 | `ai.rs`: `progress_goes_on_the_side`, `sources_travel_with_the_message`, `feedback_reaches_the_agent_and_its_owner`; streaming tests in [messaging](messaging.md#6-测试) |
| AGT-18 | `hub.rs`: `agent_ping_pong_is_stopped_by_the_root_budget`, `agent_reply_depth_is_bounded`, `repeated_agent_content_trips_the_loop_breaker`, `dlp_blocks_agent_output_and_moderates_streams_mid_flight`, `inbound_injection_is_flagged_low_trust_for_agents`; `coverage.rs`: `a_tripped_guard_recovers_after_cooldown_and_reset_needs_a_trip`, `agent_daily_budget_counts_messages_and_streamed_text`, `budget_is_shared_across_conversations_and_databases_under_concurrency`, `dlp_covers_agent_edits_and_ephemerals_but_not_people`; `routing.rs`: `race_the_root_budget_admits_exactly_its_size`, `root_budgets_move_with_their_conversation`, `one_event_wakes_at_most_five_agents` |
| AGT-19 | `friends.rs`: 全部 7 个测试; `webhooks.rs`: `friend_events_are_delivered` |


## 7. 限制和下一步

- 每日预算存在内存里，重启后清零。
- Agent 之间经过 AgentIM 以外的渠道形成的循环，AgentIM 感知不到。
- 没做：按群的 Agent 名额、三层权限（应用 ∩ Agent ∩ 群）、只在某个群暂停一个 Agent、能力商店、`digest` 接收方式、主题或广播会话。
