# 接入身份

谁连接 AgentIM、各自怎么证明身份、各自能做什么：用开发者签发的令牌的终端用户、用密钥的 Agent、用 API 密钥的后端、用账号和管理密钥的管理员。也包括客户端权限、设备、管理 API、控制台和命令行。

代码：`server/crates/agentim-core/src/auth.rs`（令牌、签名密钥）、`auth_limit.rs`（认证失败的限制）、`hub/login.rs`、`registry.rs`（密钥和设备，内存里和跨节点）、`admin_api.rs`、`admin_cli.rs`、`console.rs`（内嵌的控制台）、`server.rs`（传输、TLS、Origin）；控制台界面 `server/console/`。

## 1. 用途

AgentIM 没有自己的终端用户账号，也从不存他们的密码。开发者的后端签发令牌，为自己的用户担保。

| 谁 | 入口 | 凭据 |
|---|---|---|
| 终端用户（人） | App 里的 `/ws` | 开发者后端用签名密钥签的 JWT |
| Agent | `/ws`、`/mcp`、`/api/…` | Agent 密钥 |
| 开发者的后端 | `/v1` | 带权限范围的 API 密钥 |
| 开发者和运维 | `/console`、`/admin/v1`、`agentim admin` | 管理员账号（密码 + TOTP）或管理密钥 |
| 机器上的部署者 | `agentim` 本机命令 | 能登录这台机器、能访问数据库 |

## 2. 功能点

| 编号 | 功能 | 行为 |
|---|---|---|
| ACC-01 | 签名的用户令牌 | HS256 JWT，带 `kid`、`sub`、`iat`、`exp`，后端离线签（服务端 SDK 的 `createUserToken`）。拒绝 `alg: none` 和其他算法；`iat` 比服务器时间晚超过 60 秒拒绝；`exp - iat` ≤ `max_token_days`（30） |
| ACC-02 | 签名密钥 | 可以有多把便于轮换；只显示一次；加密保存（验签要原文）；吊销一把，它签的令牌和连接在所有节点立即失效 |
| ACC-03 | 第一次登录建用户 | 没见过的 id 带着有效令牌连上，就建这个人，只建一次。已删除的 id、停用的用户、Agent 的 id 和 `system` 都拒绝 |
| ACC-04 | 设备 | App 带上自己的设备 id（`device=…`，可带名字和平台）。服务端登记设备、列出、下线一台（这台设备上截至那一秒签发的令牌失效），长期不用的设备被遗忘 |
| ACC-05 | 全部设备退出 | `DELETE /v1/users/{id}/tokens`：截至那一秒签发的令牌失效，连接断开；对 Agent 是吊销全部密钥 |
| ACC-06 | 令牌放在请求头里 | `Authorization: Bearer`，浏览器用子协议 `agentim.bearer.<base64url>`；从不放在地址里 |
| ACC-07 | 客户端权限 | 应用默认值（`create_groups`、`create_direct`、`add_members`、`find_users`、`create_agents`（默认关）、`edit_minutes`、`recall_minutes`），在控制台里设并存下；开放 API 可以按人覆盖。`hello` 返回生效的权限，改了推 `user.updated` |
| ACC-08 | 人对 AI 的控制永远保留 | 没有 `create_agents` 时，已有的 Agent 照常运行，负责人仍然能急停、审批、接管、耳语、代看 |
| ACC-09 | API 密钥 | 有权限范围、可选的 IP 允许列表和过期时间，只存哈希；在控制台、管理 API 或 `agentim api-key` 里建和吊销；变化立即到达所有节点 |
| ACC-10 | 认证失败的限制 | 未知的密钥和令牌，每个地址每秒最多查一次库（合计每秒 20 次）；一直失败的地址被拒绝一段时间 |
| ACC-11 | 管理员账号 | 角色 `owner`、`developer`、`moderator`、`operator`；Argon2id 密码、TOTP、连续失败锁定。最后一个 owner 不能被降级或停用 |
| ACC-12 | 第一个 owner | 还没有管理员时，服务端打印一个一次性的初始化链接，只能从本机打开；或者在本机运行 `agentim admin-key create --role owner` |
| ACC-13 | 控制台会话 | `HttpOnly`、`SameSite=Strict` 的 Cookie（12 小时不用失效，最长 7 天），每个修改都要 CSRF 令牌，只接受同源；会话里乱猜也算登录失败 |
| ACC-14 | 管理 API 的两块 | `/admin/v1/app/…` 给开发者（密钥、用户、会话、Agent、助手、连接、角色、举报、Webhook、审计、设置），`/admin/v1/deploy/…` 给运维（数据库、集群、管理员）。每个路由都检查角色 |
| ACC-15 | 看内容要写原因 | 会话列表只有元数据；看消息要带 `reason`（1–500 字节），并写审计 |
| ACC-16 | 管理命令行和管理密钥 | `agentim admin --url --key` 调同一套管理 API；管理密钥（`adk…`）带角色，只存哈希 |
| ACC-17 | 传输 | 内置 TLS（启动时检查证书）、浏览器的 Origin 检查（`allowed_origins`）、每人连接数上限、关掉空闲连接 |

## 3. 设计

### 3.1 令牌和设备

- **只有一种登录方式**：后端离线签令牌。没有采用的做法：
  - 调接口换令牌：多一次调用、多一个依赖；
  - 每次连接回调后端（connect proxy）：重连风暴会全部打到后端。
- **验签**：每个节点在本地验令牌。签名密钥有缓存，变化通知其他节点。
- **按时间吊销**：下线一台设备、全部退出，都记下一个截止时间，`iat` 不晚于它的令牌一律拒绝。令牌不绑设备，所以拿到令牌的人在它过期前仍能以新设备连上。
- **设备 id 属于 App**：第一次运行时生成并保存，每台设备因此有自己的同步位置（[sync-and-clients](sync-and-clients.md)）。

### 3.2 权限

```
生效的权限 = 这个人的覆盖值（设了的话）否则应用默认值
```

- 被拒绝的操作返回 `forbidden`（"the app does not allow this: …"），什么都不写。
- 开放 API 的 `as: <用户>` 同样遵守，`as: system` 不受限制。
- Agent 受它负责人的权限约束。
- `create_agents` 在控制库的写线程里再查一次，所以收回授权一返回，这个人就不会再多出新的 Agent。

### 3.3 管理

| 块 | 给谁 | 角色 |
|---|---|---|
| 应用 `/admin/v1/app/…` | 开发者 | `owner`、`developer`（可写）；`moderator`（举报，其他只读） |
| 部署 `/admin/v1/deploy/…` | 运维 | `owner`、`operator`；管理员账号和管理密钥只有 `owner` |

- 每个修改都走和客户端、开放 API 同一套业务逻辑和错误码。
- 每个修改都写审计 `admin.<动作>`，操作者是 `admin:<名字>` 或 `admin:key:<名字>`。
- 被拒绝的请求不留痕迹。
- 托管云上，客户只拿到应用那块。
- 控制台是 React 单页应用，构建时嵌进程序。没有构建好的前端时，嵌一个占位页，所以 Rust 的构建从不依赖 Node。

## 4. 接口

- 客户端协议：`hello`（带 `me.permissions`）、`list_devices`、`logout_device`。
- 开放 API：`PATCH /v1/users/{id}`（`permissions`）、`/v1/users/{id}/devices…`、`DELETE /v1/users/{id}/tokens`。
- 管理 API：`/admin/v1/setup`、`/admin/v1/login`、`/admin/v1/logout`、`/admin/v1/app/*`、`/admin/v1/deploy/*`。
- 命令行：
  - `agentim signing-key create|list|revoke`、`agentim api-key create|list|revoke`、`agentim admin-key create`、`agentim token sign`；
  - `agentim admin <命令>`（`overview`、`users`、`user`、`permissions`、`deactivate`、`reactivate`、`agents`、`agent-create`、`agent-key`、`agent-stop`、`agent-resume`、`agent-transfer`、`agent-destroy`、`admin-create` 等）。

## 5. 规则和错误

规则前缀：`account.*`、`apikey.*`、`signing.*`、`admin.*`、`session.*`（[目录](../../en/business-rules.md)）。
- 401：令牌未知或过期，客户端要一次新令牌。
- 403：这个人已停用或已删除，客户端停止重试。
- `forbidden`：缺权限或角色不够。
- `conflict`：最后一个 owner、验证码重复使用、初始化链接重复使用。

## 6. 测试

路径在 `server/crates/agentim-core/tests/` 下；三种数据库。

| 功能点 | 测试 |
|---|---|
| ACC-01–ACC-06 | `login.rs`: 全部 16 个测试 (signed tokens, forged and malformed tokens, first login once, racing first logins, devices, logout one or all, revoking a signing key, keys stored encrypted, header transport, the token over the WebSocket, cut-offs across a restart, races of revocation and connection); `audit.rs`: `idle_and_old_tokens_expire`, `revocations_by_another_process_take_effect`; cluster `cluster.rs`: `signed_tokens_and_logouts_reach_every_node`, `revocations_and_blocks_reach_every_node`; `faults.rs`: `revocations_reach_cut_off_nodes_within_3s` |
| ACC-07, ACC-08 | `permissions.rs`: 全部 7 个测试 |
| ACC-09, ACC-10 | `open_api.rs`: `authentication_and_scopes`, `failed_authentications_block_one_address`, `race_key_reloads_against_new_and_revoked_keys`; `http.rs`: `websocket_rejects_bad_tokens_oversized_frames_and_bad_requests`; `transport.rs`: `a_logout_before_registration_is_caught`, `race_logout_during_the_handshake` |
| ACC-11–ACC-16 | `admin_api.rs`: 全部 12 个测试 (setup once and only locally, roles on every route, the last owner, lockout and TOTP, CSRF and same origin, guesses in a session, reading content needs a reason, the app area follows the business rules, defaults apply everywhere, restart, another process); `server/crates/agentim/tests/cli.rs`: `admin_commands_reach_a_running_server`; `server/ci/console_smoke.mjs` (the console in a browser) |
| ACC-17 | `transport.rs`: `the_server_speaks_tls_itself`, `certificates_are_checked_at_startup`, `cross_site_browser_pages_are_refused`, `origin_settings_are_checked`, `connections_are_capped_and_idle_ones_closed`; `audit.rs`: `a_persons_limits_count_all_their_connections` |
| Closed demo endpoints | `http.rs`: `outside_the_demo_login_user_creation_and_stats_are_closed`, `demo_endpoints_work_in_the_demo`; `coverage.rs`: `account_and_stats_ops_are_gone`; `edges.rs`: `refused_user_creation_and_tokens` |


## 7. 限制和下一步

- 没做：OIDC 登录、管理员的 SSO/SCIM、带 HMAC 签名的开放 API 请求。
- 客户端权限以外的大多数应用设置（Webhook、发送前拦截、审核时限）在控制台里只读，在配置文件里设。
