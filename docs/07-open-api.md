# 07 开放 API（服务端 API）

> 开放 API 是给**开发者的后端**调用、用来操作 IM 的 HTTPS 接口：以某个用户或系统的身份发消息、撤回、建群、拉人、导入用户、签发登录令牌等。✅ 表示已实现，其余是计划。

## 1. 四类接口，各管各的

| 接口 | 谁在用 | 做什么 | 身份 | 位置 |
|---|---|---|---|---|
| **客户端协议** | 最终用户的设备（App、网页、Agent） | 收发自己的消息、同步、在线推送 | 用户或 Agent 的令牌，每台设备一个 | WebSocket `/ws`（[05 §7.1](05-operations.md)） |
| **开放 API**（本文） | 开发者的后端服务 | 以用户或系统的身份操作 IM，批量和导入 | 应用的 API Key | HTTPS `/v1/...` |
| **管理接口** | 部署者（开发者控制台，[05 §8](05-operations.md)、[12](12-developer-experience.md)） | 整个部署的运维：数据库与组织单元、迁移、管理员账号、审计、加密密钥、系统健康 | 管理员账号 | 默认只在内网 |
| **Webhook / MCP** | 开发者的后端、Agent 框架 | 接收事件、发送前拦截（[02 §2.6](02-architecture.md)）；Agent 通过 MCP 使用 IM（[03 §2.3](03-agents.md)） | 签名回调；Agent 凭据 | 服务端主动调用 / MCP 端点 |

**开放 API 操作"IM 里的数据和行为"，管理接口操作"这套 IM 本身"。** 开放 API 的 Key 不能做任何管理的事，反之亦然。

## 2. 通行规范

错误格式 RFC 9457（problem+json）；`Idempotency-Key` 防重复；游标分页；IETF `RateLimit-Policy` / `RateLimit` 限流响应头（草案）。

## 3. 方案

### 3.1 应用与凭据

- **应用（App）**：数据模型里的 `tenant_id`。现在每个部署一个应用（`default`）；多应用之间的完全隔离和管理界面是计划。
- **API Key**：一个应用可以有多个 Key，用于轮换（新旧同时有效）。每个 Key 有**权限范围**、可选的 **IP 允许列表**和过期时间，可以随时吊销。数据库里只存哈希。
- **认证**（D24）：`Authorization: Bearer <key>`，强制 HTTPS（例外见 §5）✅。可选的请求签名（计划）：`HMAC-SHA256(密钥, 时间戳 + 随机数 + 方法 + 路径 + 请求体哈希)`，5 分钟内有效，随机数防重放。

| 权限范围 | 允许 | 状态 |
|---|---|---|
| `tokens:issue` | 给用户签发、吊销客户端登录令牌 | ✅ |
| `users:write` / `users:read` | 创建、导入、修改、停用用户；读取用户资料 | ✅ |
| `conversations:write` / `conversations:read` | 建会话、改资料、管理成员和角色、解散；读取会话和成员 | ✅ |
| `messages:write` | 以用户或系统身份发消息、编辑、撤回、标记已读 | ✅ |
| `messages:read` | 读取消息历史（**单独的权限**，因为能读到私聊内容；每次读取写审计） | ✅ |
| `moderation` | 处理举报（禁言计划中） | ✅ |
| `compliance` | 导出、删除用户数据（GDPR） | ✅ |
| `agents:write` | 注册 Agent、管理凭据和策略（[03 §1](03-agents.md)） | 计划 |

### 3.2 以谁的身份操作（D25）

| 方式 | 写法 | 规则 |
|---|---|---|
| **以用户身份** | 请求体里 `"as": "<user_id>"` | **完全遵守这个用户的业务规则**：必须是会话成员、受拉黑和单聊规则约束、去重按这个用户算。结果和用户在客户端上操作一样（同步到这个用户的所有设备） |
| **以系统身份** | `"as": "system"` | 每个应用一个系统身份。可以建会话、管理成员和角色、发系统通知（标明来自系统）、撤回任何消息、解散会话；**不能**冒充用户说话，也不受"必须是成员"的限制。细节见 §5 |

没有"以任意用户身份、绕过规则"的超级模式：那会破坏"业务正确"这条核心保证，也让审计失去意义。每次调用都写审计：哪个 Key、以谁的身份、做了什么。

### 3.3 用户 ID 由开发者决定（D23）

开发者直接用自己系统里的用户 ID（例如 `user_12345`）。`PUT /v1/users/{id}` 按这个 ID 创建或更新（幂等，适合同步和导入）。ID 规则：1–64 个字符，字母、数字和 `_-.@:`，应用内唯一。服务端内部 ID 和它一一对应。

### 3.4 接口一览（v1）

| 资源 | 接口 | 说明 | 状态 |
|---|---|---|---|
| 登录令牌 | `POST /v1/users/{id}/tokens` | **开发者后端给用户签发客户端令牌**（可设有效期、设备名、平台） | ✅ |
| | `DELETE /v1/users/{id}/tokens` | 吊销这个用户的全部令牌，并立即断开 | ✅ |
| 用户 | `PUT /v1/users/{id}`、`GET`、`PATCH` | 创建或更新、读取、修改资料（昵称、头像、自定义字段） | ✅ |
| | `POST /v1/users:batch` | 一次最多 100 个，逐个返回结果 | ✅ |
| | `POST /v1/users/{id}:deactivate`、`:reactivate` | 停用后不能登录，已有的消息保留 | ✅ |
| | `DELETE /v1/users/{id}`、`GET …/erasure` | 删除账号（GDPR 第 17 条），后台完成，查进度；`compliance` 权限（[04 §4.2](04-security-compliance.md)） | ✅ |
| | `GET /v1/users/{id}/export` | 导出这个用户本人的数据（NDJSON，可续传），开发者交给用户；`compliance` 权限。客户端没有导出和删除账号（[13](13-roles-and-entry-points.md)） | ✅ |
| 会话 | `POST /v1/conversations` | 建群或单聊；可以指定开发者自己的会话 ID；单聊每对用户只有一个 | ✅ |
| | `GET`、`PATCH`、`DELETE /v1/conversations/{id}` | 读取、修改资料、解散 | ✅ |
| | `POST /v1/conversations/{id}/members`、`DELETE …/members/{user}`、`PATCH …/members/{user}` | 批量拉人（可带 `history`，[08 §2.4](08-sync-and-client.md)）、移除、改角色；转让群主 | ✅ |
| | `GET /v1/users/{id}/conversations` | 某个用户的会话列表 | ✅ |
| 消息 | `POST /v1/conversations/{id}/messages` | 在会话里发消息（以用户或系统身份）；可带路由信封 `to`、`topics`、`intent`、`refs` 和来源 `sources`（[03 §6.2](03-agents.md)、[03 §3](03-agents.md)） | ✅ |
| | `POST /v1/users/{id}/messages` | **"给某人发消息"**：没有单聊就自动建一个 | ✅ |
| | `POST /v1/messages:batch-send` | 同一个发送者发给多人（一次最多 500 人），逐人返回结果 | ✅ |
| | `PATCH /v1/messages/{id}`、`POST /v1/messages/{id}:recall` | 编辑、撤回 | ✅ |
| | `GET /v1/conversations/{id}/messages` | 历史消息，按 seq 分页（`messages:read`），只能查到保留期内的内容 | ✅ |
| | `POST /v1/conversations/{id}/read` | 替某个用户标记已读 | ✅ |
| 文档 | `GET /v1/openapi.json` | OpenAPI 3.1 文档 | ✅ |
| 文件 | `POST /v1/files` | 以某个用户的身份上传（[02 §2.7](02-architecture.md)） | 计划 |
| 全员通知 | `POST /v1/broadcasts` | 发给全部用户或某个单元，异步任务；可选"只发给在线用户"；单独的队列和限流，不挤占实时消息 | 计划 |
| 导入 | `POST /v1/conversations/{id}/messages:import` | 迁移历史消息：保留原时间和发送者，不推送、不算未读 | 计划 |
| 审核 | `GET /v1/reports`、`GET /v1/reports/{id}`、`POST /v1/reports/{id}:resolve` | 举报队列和处置（删除内容、停用几天、永久停用，处置记录按 DSA 第 17 条）；`moderation` 权限 | ✅ |
| 异步任务 | `GET /v1/jobs/{id}` | 导出、删除、全员通知、导入的进度和结果；分批执行、可重试 | 计划 |

### 3.5 通用约定

| 方面 | 约定 |
|---|---|
| 版本 | 路径带主版本 `/v1`，规则和协议版本管理一致（[02 §2.5](02-architecture.md)）：主版本内只做向后兼容的新增 |
| 防重复 | 所有 `POST` 支持 `Idempotency-Key`（细节见 §5）。发消息还可以用 `client_msg_id`，和客户端的去重一致 |
| 错误 | `application/problem+json`，带稳定的 `code` 字段，**和 WebSocket 协议的错误码相同**（`forbidden`、`not_found`、`conflict`、`bad_request`、`blocked`、`rate_limited`……），HTTP 状态码按含义对应（§5） |
| 批量 | 逐项返回结果（`{"results": [{"ok": true, …}, {"ok": false, "code": "blocked"}]}`），一项失败不影响其他项 |
| 分页 | 游标分页（`cursor` + `limit`，最大 200）；消息历史的游标就是 seq |
| 限流 | 按 Key 限流；响应带 `RateLimit-Policy` / `RateLimit` 头；超出返回 `429` 和 `Retry-After` |
| 时间与 ID | 时间用毫秒时间戳；ID 都是字符串 |

### 3.6 和核心保证的关系

- **同一条写入路径**：写入和客户端一样经过单写者和组提交，fsync 之后才返回成功，所以同样不丢、有序、去重。
- **同样的推送**：通过开放 API 发的消息，会话成员和客户端发的一样实时收到；以用户身份发的消息同步到这个用户的所有设备。
- **同样的业务规则**：以用户身份调用时，规则和客户端完全一致（§3.2）。

### 3.7 安全与合规

- Key 只存哈希；日志里不出现密钥和消息内容；每次调用写审计（Key、身份、操作、目标、结果）。
- `messages:read` 和 `compliance` 是单独的权限，默认不给；读取历史写审计，可以追溯"谁看过谁的消息"。
- GDPR 的导出和删除只走 `compliance` 权限的开放 API（[04 §4.2](04-security-compliance.md)）。

### 3.8 测试（按 [CLAUDE.md](../CLAUDE.md) 的测试规则）

| 范围 | 覆盖 |
|---|---|
| 每个接口 | 正常路径；每种权限范围缺失时返回 403；以用户身份时用户的每条业务规则（非成员、拉黑、单聊、被移除）；以系统身份时允许和不允许的操作；未知 ID、超限、非法参数 |
| 一致性 | 开放 API 发的消息与客户端发的在日志、推送、未读、去重上完全一致；以用户身份发的消息同步到该用户的所有设备 |
| 防重复 | 同一个 `Idempotency-Key` 并发重试只执行一次；同 Key 不同请求体返回 422 |
| 批量 | 部分失败逐项返回；500 人批量发送和客户端发送同时进行时 seq 连续、无丢失 |
| 鉴权 | 吊销、过期、IP 不在允许列表 |
| 限流 | 超出返回 429，其他 Key 不受影响 |
| 故障 | 批量发送中途 `kill -9`：已确认的项全部存在，未确认的项重试后不重复 |
| 跨平台与数据库 | 三种数据库；Linux 和 Windows |

## 4. 后续

请求签名（HMAC）、文件上传、全员通知、历史导入、异步任务、Agent 和设备接口、客户端权限（[13 §4.1](13-roles-and-entry-points.md)）、多应用及其管理界面。

## 5. 实现细节

| 方面 | 做法 |
|---|---|
| API Key | 格式 `ak_<key_id>_<secret>`，数据库只存整个 Key 的 SHA-256。命令行管理：`agentim api-key create --scopes …`（只在创建时显示一次）、`api-key list`、`api-key revoke <key_id>`。可选过期时间和 IP 允许列表。服务端运行时新建的 Key 立即可用；吊销在 10 秒内生效（每 10 秒重新读取，未知的 Key 最多每秒查一次库） |
| 明文 HTTP | 只有来自本机地址的请求允许；部署在 TLS 反向代理之后时设 `open_api.allow_http = true` |
| 客户端令牌 | 格式 `atk_<43 位 base64url>`（256 位随机数），只存 SHA-256；每个令牌就是一台设备（`device_id`、名称、平台、签发时间、过期时间、最近使用时间） |
| 设备 | 客户端协议的 `list_devices`、`logout_device {device_id}`（吊销并立即断开那台设备）；开放 API 吊销用户的全部令牌时也立即断开。新设备登录时，这个用户已在线的设备收到 `device.added`；已读在其他设备上同步（`read` 推送） |
| 以系统身份 | 内置的系统用户（`kind = system`，名字 `system` 保留）。可以发系统通知（`sender_type = system`，不需要是成员）、拉人、移除非群主的成员（群主要先转让）、改角色、转让群主、撤回任何消息、解散会话、读取任何会话的历史。建会话必须指定 `owner`，按这个用户的身份建，拉黑等规则照常检查 |
| 以用户身份 | 内部用一个不登记在线状态的会话执行和客户端完全相同的操作，所以规则、去重、推送、审计一致 |
| 审计 | 每个写操作和每次读取历史都写审计：`api.<操作>`，带 Key ID 和身份 |
| 防重复 | `Idempotency-Key`（1–255 个可见字符）保存 24 小时：同 Key 同请求体返回第一次的结果（响应头 `Idempotent-Replayed: true`）；不同请求体 `422`；第一次还在执行时 `409` |
| 限流 | 每个 Key 一个令牌桶，默认每秒 100 次、突发 200 次（`open_api.rate_per_second`、`open_api.burst`）；响应头 `RateLimit-Policy`、`RateLimit`；超出 `429` + `Retry-After` |
| 错误 | `application/problem+json`：`type`（`https://agentim.dev/errors/<code>`）、`title`、`status`、`detail`、`code`；状态码：`bad_request` 400、未认证 401、`forbidden` / 缺权限 403、`not_found` 404、`conflict` 409、`idempotency_mismatch` 422、`rate_limited` 429、`blocked` 403、`policy_blocked` 403、`overloaded` / `storage_unavailable` 503 |
| OpenAPI | 手写的 OpenAPI 3.1 文档 `server/crates/agentim-core/openapi.json`，`GET /v1/openapi.json` 返回它；测试保证文档里的每个路径和方法都存在于路由里，反之亦然 |
| 服务端 SDK | Node（TypeScript）、Python、Go 三个轻量客户端（`sdk/node`、`sdk/python`、`sdk/go`），手写，覆盖全部接口，带重试（同一个 `Idempotency-Key`）和错误类型；CI 里各跑一遍对真实服务端的冒烟测试 |
