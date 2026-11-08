# 开放 API、Webhook 和发送前拦截

给开发者后端用的 HTTPS 接口、AgentIM 发给后端的事件，以及让后端在消息存储前检查它的钩子。

代码：`server/crates/agentim-core/src/open_api.rs`（`/v1` 路由、OpenAPI 3.1 文档）、`webhook.rs`（投递、签名、发送前拦截）、`hub/developer.rs`（开发者操作）、`hub/export.rs`、`hub/compliance.rs`。服务端 SDK：`sdk/node`、`sdk/python`、`sdk/go`。

## 1. 用途

有四类接口，各用各的凭据：

| 接口 | 谁用 | 凭据 |
|---|---|---|
| 客户端协议（`/ws`） | 终端用户的设备和 Agent | 用户令牌或 Agent 密钥，每台设备一个 |
| **开放 API**（`/v1`） | 开发者的后端 | 带权限范围的应用 API 密钥 |
| 管理 API（`/admin/v1`） | 控制台和 `agentim admin` | 管理员账号或管理密钥（[access](access.md)） |
| **Webhook**、MCP | 后端收事件；Agent 用 MCP | 签名回调；Agent 密钥 |

开放 API 操作 IM 里的数据和行为，管理 API 操作部署本身，两种密钥都做不了对方的事。

## 2. 功能点

| 编号 | 功能 | 行为 |
|---|---|---|
| API-01 | API 密钥和权限范围 | 每个应用可以有多把（便于轮换），每把有权限范围、可选的 IP 允许列表和过期时间，只存哈希。`Authorization: Bearer`，要求 HTTPS（本机回环除外） |
| API-02 | 以用户或系统的身份 | `as: <用户>` 完全遵守这个用户的业务规则。`as: system` 能管理会话、发系统通知，但不能以用户的身份说话。没有"绕过规则"的模式 |
| API-03 | 开发者决定用户 ID | `PUT /v1/users/{id}` 按开发者自己的 ID 创建或更新（1–64 个字母、数字和 `_-.@:`），幂等 |
| API-04 | 用户 | 创建、读取、修改（资料、自定义字段、权限）、批量（100）、停用、恢复、设备、全部设备退出 |
| API-05 | Agent | 创建（`kind: agent`）、密钥、急停和恢复、转交、销毁、列表、导出和导入（[agents](agents.md)） |
| API-06 | 会话 | 创建（可用开发者自己的 ID）、读取、修改、解散；批量拉人（可带历史）、移除、角色、转让；禁言成员 1 秒到 1 年 |
| API-07 | 消息 | 以用户或系统身份发（带信封和来源）；"给某人发消息"（自动建单聊）；批量发给最多 500 人；编辑、撤回；按序号读历史（`messages:read`，写审计）；标记已读 |
| API-08 | 文件 | 以成员或系统身份分片上传或整个上传（64 MiB）；签名的读取地址 |
| API-09 | 合规和审核 | 导出一个人的数据（NDJSON，可续传）、删除账号（后台进行，可查进度）、举报和处置 |
| API-10 | 通用约定 | RFC 9457 错误格式，码和协议相同，另带 `rule`；所有 `POST` 支持 `Idempotency-Key`；游标分页（≤ 200）；按密钥限流，带 `RateLimit-Policy`、`RateLimit`、`Retry-After`；批量逐项返回结果 |
| API-11 | 认证失败 | 一直发未知密钥的地址会被拒绝一段时间 |
| API-12 | OpenAPI 文档 | `GET /v1/openapi.json`（OpenAPI 3.1）；文档站的参考由它生成 |
| API-13 | Webhook | 签名（Standard Webhooks）、有序、至少一次。事件和改动在同一个事务里写进发件箱，所以被拒绝或没提交的写入什么都不会发 |
| API-14 | 发送前拦截 | 每次 `send` 之前同步调用，可以放行、拒绝（`policy_blocked`）或替换消息 |
| API-15 | 服务端 SDK | Node、Python、Go：所有端点、`createUserToken`（本地签用户令牌）、Webhook 验签 |

## 3. 设计

### 3.1 权限范围

| 权限范围 | 允许 |
|---|---|
| `users:write` / `users:read` | 创建、导入、修改、停用用户；读资料和设备 |
| `tokens:revoke` | 让一个人在所有设备上退出（用户令牌由开发者用自己的签名密钥签，不经过 API） |
| `agents:write` / `agents:read` | 创建和管理 Agent；列出和导出（导出会话还要 `messages:read`） |
| `conversations:write` / `conversations:read` | 会话、成员、角色、解散 |
| `messages:write` | 发送、编辑、撤回、标记已读、上传文件 |
| `messages:read` | 读历史和文件。单独的权限，因为能读私聊；每次读取写审计 |
| `moderation` | 举报、禁言成员 |
| `compliance` | 导出和删除一个人的数据（GDPR） |

### 3.2 同一条路径，同一套规则

- 通过 API 的写入和客户端一样走写线程和组提交，持久化之后才返回成功。
- 成员和收到别的消息一样实时收到 API 发的消息；以用户身份发的消息同步到这个用户的所有设备。
- 去重靠 `Idempotency-Key`，消息还可以用 `client_msg_id`。
- 每次调用写审计：哪把密钥、以谁的身份、做了什么、对象、结果。日志里从不出现密钥和消息内容。

### 3.3 Webhook

| 方面 | 规则 |
|---|---|
| 配置 | `[[webhooks]]`：`name`、`url`、`secret`（`whsec_…` base64，和 Standard Webhooks 一样；至少 16 字节）、`events`（精确类型、`msg.*` 前缀或 `*`）、`max_attempts`（8）、`timeout_ms`、`backoff_ms`。租户管理自己的（[multi-tenancy](multi-tenancy.md)） |
| 不丢 | 订阅的事件和事件本身在同一个事务里写进 `webhook_outbox` |
| 有序 | 每个 Webhook 在每个库一个投递任务，按提交顺序发；前一条没成功不发下一条 |
| 至少一次 | 成功后通过写线程移动游标，重启后从游标继续。接收方按 `webhook-id` 去重 |
| 签名 | `webhook-id`、`webhook-timestamp`、`webhook-signature: v1,<base64 HMAC-SHA256(id.timestamp.body)>` |
| 请求体 | `{type, timestamp, database, data: {conv_id, seq, event}}` |
| 重试 | 指数退避，最长 5 分钟一次。超过 `max_attempts` 就放弃这一条，写审计并计数。接收方可以用历史接口补 |
| 之后被删的内容 | Webhook 断着的时候被撤回或解散的内容，之后不再投递 |
| 新的 Webhook | 从当前位置开始，不补发历史 |
| 事件类型 | `conv.*`、`member.*`、`members.joined`、`msg.*`、`card.response`、`friend.*`，以及日志里的其他事件 |

### 3.4 发送前拦截

- `[before_send]` 设 `url`、`secret`、`timeout_ms`（默认 2000）和 `on_error`（`allow` 或 `reject`）。
- 在进写线程之前调用，所以不拖慢组提交。请求和 Webhook 一样签名：`{type: "message.before_send", conv_id, client_msg_id, kind, text, payload, reply_to, to_user, sender}`。
- 回复：
  - `{action: "allow"}`；
  - `{action: "reject", reason}`，返回 `policy_blocked` 和这个原因；
  - `{action: "replace", text, payload}`，替换后照常再检查一遍。
- 超时、非 2xx、不是 JSON、未知的 action，按 `on_error` 处理。
- 格式不对的请求和非成员在调用钩子之前就被拒绝，已发出消息的重试也不经过钩子。编辑和流式不经过它。

## 4. 接口

完整的端点列表在 `GET /v1/openapi.json` 和文档站。资源有：
- `/v1/users…`、`/v1/agents…`、`/v1/roles…`；
- `/v1/conversations…`（含成员、消息、已读、上传、文件）；
- `/v1/messages…`、`/v1/reports…`。

## 5. 规则和错误

码和规则与客户端相同，对应到 HTTP 状态：

| 码 | HTTP 状态 |
|---|---|
| `bad_request` | 400 |
| `forbidden` | 403 |
| `not_found` | 404 |
| `conflict` | 409 |
| `rate_limited` | 429 |

规则前缀：`apikey.*`，以及各资源的前缀（[目录](../../en/business-rules.md)）。缺权限范围是 403。错误详情带 `code`、`rule`、`detail`。

## 6. 测试

路径在 `server/crates/agentim-core/tests/` 下；三种数据库。

| 功能点 | 测试 |
|---|---|
| API-01, API-10, API-11 | `open_api.rs`: `authentication_and_scopes`, `errors_are_problem_details`, `idempotency_keys`, `input_limits`, `rate_limits_per_key`, `failed_authentications_block_one_address`, `race_key_reloads_against_new_and_revoked_keys`, `batches_report_each_item`, `address_rules`; cluster `operations.rs`: `api_keys_created_or_revoked_on_one_node_change_at_once_on_the_others` |
| API-02, API-06, API-07 | `open_api.rs`: `conversations_as_users_and_the_system`, `messages_as_users_and_the_system`; `developer_api.rs`: `silenced_members_cannot_speak`, `silences_end_and_survive_a_restart`, `race_sends_while_a_member_is_silenced`, `a_silenced_agents_drafts_wait` |
| API-03, API-04 | `open_api.rs`: `users_lifecycle`, `tokens_for_users`; `developer_api.rs`: `devices_are_listed_and_logged_out` |
| API-05 | `developer_api.rs`: `agents_are_handed_out_and_listed`, `stopping_resuming_and_destroying_agents`, `agents_move_by_export_and_import`, `team_agents_are_transferred` |
| API-08 | `files.rs`: `the_open_api_uploads_and_reads_files` |
| API-09 | `compliance.rs`: 全部 8 个测试 |
| API-12 | `site/scripts/complete.mjs` (CI: every endpoint, error and protocol message has a docs page) |
| API-13 | `webhooks.rs`: `events_are_delivered_in_order_and_signed`, `failed_deliveries_are_retried_in_order`, `an_event_is_dropped_after_the_last_attempt`, `a_restart_resumes_without_loss`, `refused_and_uncommitted_writes_send_nothing`, `concurrent_senders_are_all_delivered_once`, `delivered_events_leave_the_outbox`, `events_of_every_database_are_delivered`, `filters_choose_the_event_types`, `a_new_webhook_does_not_replay_history`, `recalled_and_dissolved_content_is_not_delivered_later`, `race_recall_and_dissolve_against_writes_in_one_batch`, `friend_events_are_delivered` |
| API-14 | `webhooks.rs`: `before_send_allows_rejects_and_replaces`, `before_send_failures_follow_the_configuration`, `before_send_skips_retries_and_non_members` |
| API-15 | `sdk/node/smoke.mjs`, `sdk/python/smoke.py`, `sdk/go/smoke` (every endpoint against a real server, in CI) |


## 7. 限制和下一步

- 没做：API 请求的 HMAC 签名、给全部用户的广播、从别家导入历史消息、异步任务资源。
