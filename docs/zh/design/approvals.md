# 审批和人的控制

人怎么始终掌控 Agent：
- Agent 的消息可以先作为草稿等待；
- Agent 可以请人决定；
- 会改数据的操作按操作和金额等人批准；
- 等这个人处理的事都在一个待办里；
- 人可以接管、交还、打断、急停、销毁 Agent。

代码：`server/crates/agentim-core/src/logic.rs`（草稿、审批规则、接管）、`policy.rs`（审批规则）、`logic/requests.rs` 和 `hub/requests.rs`（请求和待办）、`hub/lifecycle.rs` 和 `logic/lifecycle.rs`（急停、销毁）、`hub/assistants.rs`（内置助手的操作审批）。界面：`sdk/react/src/inbox.tsx`、`components.tsx`（`ApprovalCards`）。

## 1. 用途

欧盟 AI 法第 14 条要求：用在重要事情上的 AI 要有人监督，人能决定不用它、能否决它的输出。AgentIM 把这一点做进协议：
- 规则要求时，Agent 说什么由人批；
- 会改数据时，助手做什么由人批；
- 人随时可以接管会话。

## 2. 功能点

| 编号 | 功能 | 行为 |
|---|---|---|
| APR-01 | 草稿 | 需要审批时，Agent 的 `send` 返回 `draft_id`。草稿在侧信道、不占序号；只有批准的草稿才进日志。只有一个决定生效 |
| APR-02 | 两层审批规则 | `approval` 由 Agent 的负责人设；`required_approval` 由会话管理者设，负责人改不了也绕不过。哪层要求审批就要审批 |
| APR-03 | 什么情况要审批 | `mode`：`off`、`all` 或 `rules`。规则：`files`、`links`、`mentions`、`cards`、`first_message`，以及最多 20 个 `topics` |
| APR-04 | 谁来批 | `approvers`：`owner`（默认）、`managers`，或具体的人（必须还在会话里）。任何一个人决定就算数 |
| APR-05 | 审核 Agent | 会话里的一个 Agent 先审。它只能 `approve` 或 `escalate`，不能拒绝、不能改。`human_required` 时通过后仍要人批。审核超时（`review_secs`）转给人 |
| APR-06 | 草稿过期 | `ttl_secs`（60 秒到 7 天，默认 24 小时）。过期的草稿作废，并告诉 Agent |
| APR-07 | 批准的消息写明谁批的 | 批准的消息记下 `approval: {by, by_type, edited}` |
| APR-08 | 请人决定 | `request_human`，`kind` 是 `approve`、`answer` 或 `choose`。发给负责人或指定的成员，第一个决定生效；会过期（60 秒到 30 天），可以取消；Agent 通过事件或等待拿到结果 |
| APR-09 | 操作审批 | 连接的写工具分 `read_only`、`confirm`、`allow`，再加最多 20 条按操作和金额的规则，例如：退款超过 30 美元要 Sam 批，改地址直接执行。批准后用存下的参数执行，最多一次（[assistants](assistants.md)） |
| APR-10 | 操作卡片 | 请求带 `context.operation`：做什么、参数、金额、规则。批准人在聊天里和待办里看到一张卡片 |
| APR-11 | 待办 | `inbox {limit?}` 跨会话列出等这个人处理的事：要决定的请求、自己 Agent 的草稿、转给他的会话。指定了决定人的请求只给这些人看 |
| APR-12 | 接管和交还 | `takeover` 把 Agent 锁在会话外：它发消息返回 `taken_over`，进行中的流被取消。`release` 交还；接手的人 `agents.takeover_idle_minutes`（30）分钟不说话也会自动交还。所有人看到 `agent.state` |
| APR-13 | Agent 转人工 | `handoff {to?, note}`：Agent 把会话交给一个人（默认负责人）。`handback` 交回。转出期间 Agent 不被唤醒 |
| APR-14 | 打断、耳语、代看 | 任何人都能停掉一个流（Agent 会收到通知）。负责人可以给 Agent 耳语、代看它能看到的内容（写审计） |
| APR-15 | 急停和恢复 | `stop_agent`：Agent 的每个操作都返回 `conflict`，连接断开；重启后和每个写线程里都有效。`resume_agent` 恢复 |
| APR-16 | 销毁 | `destroy_agent {confirm: 名字}`：不可撤销。吊销密钥，连同数据密钥删掉状态，取消流、请求、草稿、抢单、发言权，从每个会话移出。它的消息保留，名字不能再用。中途崩溃，启动时做完 |

## 3. 设计

### 3.1 草稿和规则

- 两层的格式一样：`{mode, rules, topics, approvers, reviewer, human_required, ttl_secs, review_secs}`。旧的布尔值 `approval: true/false` 仍然可以用，等于 `all` / `off`。
- 一条消息两层都命中时，用 `required_approval` 的审批人和审核 Agent；`human_required` 任一层要求就算。
- **流式**：开始时还不知道内容。以下情况整个流都变成草稿：
  - 模式是 `all`；
  - 规则里有 `links` 或 `mentions`；
  - 或者这是 Agent 的第一条消息。

  `files`、`cards` 和话题的规则不作用于流。
- **谁能看到草稿**：Agent 自己、还在会话里的审批人、审核 Agent。一个能批的人都没有时，发送返回 `conflict`（`draft.no_approver`）。
- **审核 Agent**：
  - 必须是成员；
  - 不能审自己，两个 Agent 不能互相审；
  - `escalate {draft_id, note?}` 只有它能用。
- **定时器**：草稿过期、审核超时、请求过期用同一个定时器。截止时间存在库里，所以重启后照常生效。
- 批准的草稿保留类型、payload 和文件，文件在批准后才可见。

### 3.2 请人决定

| 方面 | 规则 |
|---|---|
| 发起 | `request_human {conv_id, kind, title, options?, context?, assignees?, ttl_secs?, client_req_id}`。`title` 最多 4 KB；`context` 任意 JSON，最多 32 KB（加密）。同一个 `client_req_id` 返回同一个请求 |
| 谁来决定 | 默认负责人。指定的人（1–20 个成员，只能是人）代替负责人。任何一个人决定即可 |
| 谁能看到 | Agent、负责人、指定的人（侧信道） |
| 决定 | `decide {request_id, decision, text?, choice?}`。第一个决定生效，之后 `conflict`；决定和类型不匹配 `bad_request` |
| 结束 | 到时间过期；Agent 可以 `cancel_request`；Agent 被移出时待决的请求取消 |
| 结果 | 侧信道事件 `request.decided`、`request.expired`、`request.cancelled`；或 `get_request {wait_secs}` 最多等 25 秒 |
| 上限 | 每个 Agent 同时最多 100 个待决请求 |

### 3.3 操作审批

给内置助手用（任何在 `request_human` 里带 `context.operation` 的 Agent 也一样）：
- **谁来批**：`confirm` 的写操作由正在被回答的人批；这个人不是成员时由 Agent 的负责人批。规则对上时由规则里的审批人批；他们都不在会话里时由负责人批。
- **不卡住**：模型收到"还没执行，在等某某批准"，并在回复里这样告诉对方。调用存在请求里，重启不丢。
- **批准后**：用存下的参数执行存下的调用，不让模型重新生成参数。执行前再查一次：
  - 连接还在、仍允许写；
  - 工具仍在角色里。

  先占住 `ai-q-<请求号>`，保证最多执行一次；调 API 时带 `Idempotency-Key: <请求号>`。
- **拒绝**：把理由交给模型。**过期**或**取消**：回一句固定的话"没有批准，所以没有执行"。
- **规则**：每个连接最多 20 条，第一条对上的生效。每条有：
  - `tool`；
  - 可选的 `amount_field` 加 `over`（值缺失或不是数字算"超过"）；
  - `approvers`；
  - `confirm: false`，表示这个写操作直接执行。

  保存连接时不带 `approvals` 就保留原来的规则，带 `[]` 才清空。控制台有规则编辑器。

### 3.4 待办

- `inbox {limit?}`（1–500，默认 100，带 `more`）从新到旧列出：
  - 这个人能决定的请求，带 `conv_title`、`agent_name`、`asker_name`；
  - 他的 Agent 的草稿；
  - 转给他的会话。
- 新请求实时推给在线的指定人。
- React 和 React Native 的 `<Inbox>`：分类标签和数量、搜索、列表和详情（手机上分两页）。操作有决定、批准、改后发出草稿、丢弃、打开、交还（[chat-components](chat-components.md)）。

### 3.5 接管和转人工

- 接管和转人工是同一条记录，是存在数据库里的一把锁：
  - Agent 的发送和流式返回 `taken_over`；
  - 进行中的流被取消；
  - 它不被唤醒。
- `agent.state` 显示谁接手了、为什么。转人工记下 `handoff: true` 和说明。
- 接手的人 `takeover_idle_minutes` 内没说话，接管自动结束，`reason: idle_timeout`。
- 内置助手的 `handoff_to_person` 工具在回复结束后才转人工，这样解释的那句话不会被截断。

### 3.6 急停和销毁

- **急停**：
  1. 先写控制库，所以重启后仍然有效。
  2. 之后每条路径（WebSocket、MCP、代它调用的开放 API）都拒绝，只有 `hello` 和 MCP 的 `whoami` 还能用。
  3. 每个库的写线程再查一次，所以急停事件之后不会再出现它的任何内容。
  - 只有负责人的客户端能急停。命令行是另一个进程，服务端不能立即知道。
- **销毁**：
  1. 一次写入标记为已销毁。
  2. 删除检查点和数据密钥（加密销毁），吊销密钥。
  3. 然后逐个会话：取消流和请求，删除草稿、抢单、摘要，收回发言权，把它移出。
  - 用户记录作为墓碑保留。中途中断的销毁在启动时修复。

## 4. 接口

- 客户端协议：
  - `set_agent_policy {approval, required_approval, trigger, topics}`、`list_drafts`、`approve {draft_id, text?}`、`reject`、`escalate`；
  - `request_human`、`get_request`、`cancel_request`、`decide`、`inbox`；
  - `takeover`、`release`、`handoff`、`handback`、`stream_cancel`、`whisper`、`observe`；
  - `stop_agent`、`resume_agent`、`destroy_agent`。
- 开放 API：`/v1/agents/{id}` 上的急停、恢复、销毁。连接和它的 `approvals` 在管理 API 里。
- 事件：侧信道上的 `draft.*`、`request.*`；日志里的 `agent.state`。

## 5. 规则和错误

规则前缀：`draft.*`、`approval.*`、`request.*`、`agent.*`（[目录](../../en/business-rules.md)）。
- `conflict`：已决定、已过期、被接管、被急停、没有审批人。
- `forbidden`：不是审批人、不是负责人，或审核 Agent 做了批准和转人之外的事。
- `bad_request`：决定和类型不匹配，或规则写错。

## 6. 测试

路径在 `server/crates/agentim-core/tests/` 下；三种数据库。

| 功能点 | 测试 |
|---|---|
| APR-01 | `scenarios.rs`: `approval_drafts_are_private_until_decided`; `coverage.rs`: `streamed_drafts_stay_private_until_approved`; `concurrency.rs`: `approve_and_reject_race_has_one_outcome`; `edges.rs`: `a_policy_hit_inside_a_streamed_draft_discards_it`; `files.rs`: `files_of_drafts_wait_for_approval` |
| APR-02–APR-06 | `approvals.rs`: `rules_pick_the_messages`, `managers_require_approval`, `a_reviewer_goes_first`, `deadlines`, `one_decision_wins`; `team_agents.rs`: `an_agent_needing_approval_cannot_speak_without_its_owner` |
| APR-07 | `requests.rs`: `approved_drafts_carry_the_approval`; `encryption.rs` (audit keeps a hash of the approved draft) |
| APR-08 | `requests.rs`: `every_kind_is_asked_and_decided`, `only_the_right_people_decide_once`, `race_decisions_have_one_winner`, `requests_expire`, `removal_cancels_and_limits_hold`, `requests_are_seen_by_their_parties_and_can_be_awaited`, `requests_survive_a_restart`, `requests_live_with_their_conversation_across_databases`, `refused_requests_leave_no_trace`; `mcp.rs`: `agents_ask_people_to_decide` |
| APR-09, APR-10 | `assistant_tools.rs`: `writes_wait_for_approval`, `approvals_by_operation_and_amount`; `server/ci/console_smoke.mjs` (the rule editor); `examples/test/e2e.mjs` (after-sales refunds) |
| APR-11 | `requests.rs`: `the_inbox_lists_what_waits_for_me`; `sdk/react/test/chat-components.test.tsx`, `sdk/react-native-ui/test/chat-components.test.tsx` |
| APR-12 | `scenarios.rs`: `takeover_rules`; `hub.rs`: `takeover_blocks_the_agent_until_released`, `a_silent_takeover_goes_back_to_the_agent`; `concurrency.rs`: `takeover_racing_agent_messages_is_consistent`; `coverage.rs`: `approving_while_taken_over_is_refused`, `race_approval_against_takeover` |
| APR-13 | `ai.rs`: `an_agent_hands_off_and_gets_it_back`, `a_handoff_survives_a_restart`, `handoff_refusals_leave_no_trace`, `race_one_handback`, `race_nothing_from_the_agent_while_handed_off` |
| APR-14 | `hub.rs`: `humans_can_interrupt_and_the_agent_is_told`; `scenarios.rs`: `whisper_is_only_between_an_owner_and_its_agent`; `coverage.rs`: `observing_through_an_agent_is_owner_only_and_audited` |
| APR-15, APR-16 | `lifecycle.rs`: 全部 7 个测试; `developer_api.rs`: `stopping_resuming_and_destroying_agents`; `mcp.rs`: `stopped_and_destroyed_agents_are_refused` |


## 7. 限制和下一步

- 多级审批链没做。
- 外部 Agent（用 SDK 自己跑的）在连接配置里看到 `writes`，要自己用 `request_human` 请人；AgentIM 管不到它们在外面的调用。
