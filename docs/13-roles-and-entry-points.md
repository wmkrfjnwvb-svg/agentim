# 13 谁用哪个入口：开发者、最终用户、Agent、部署者

> 2026-10-20。起因：A4 把"删除账号""导出数据"同时放进了最终用户的客户端和开发者的开放 API。负责人指出这两类人的入口应该分清：**开发者**写客户端、做集成，API 怎么用由开发者决定；**最终用户**是开发者产品的用户，只通过开发者做的界面使用。本文排查了全部入口，列出混在一起的地方和改法。
>
> **状态**：§3 的 10 个问题和 §4 的 A–E **已确认**（2026-10-20，D116）；Agent 的授权按负责人的意见细化在 §4.1（D117）；登录方式 §4.2 只剩一个待定（D118）。

## 1. 四种角色，四类入口

| 角色 | 是谁 | 入口 | 身份凭据 |
|---|---|---|---|
| **最终用户** | 开发者产品里的人 | 客户端协议 `/ws`。开发者的 App 通过客户端 SDK 调用，最终用户从不直接接触 | 客户端令牌，由开发者后端签发（`POST /v1/users/{id}/tokens`） |
| **Agent** | 接进会话的 AI | 客户端协议 `/ws`、MCP `/mcp`、Agent SDK | Agent 密钥 |
| **开发者** | 用 AgentIM 搭产品的团队 | 开放 API `/v1`（后端调用）、Webhook、发送前拦截；以后加上控制台里的"应用"部分 | API Key（带权限范围） |
| **部署者** | 运行这套服务的人。自托管时往往就是开发者本人；托管云上是我们 | 命令行 `agentim …`、配置文件、`/metrics`；以后的管理 API `/admin/v1` 和控制台里的"部署"部分 | 服务器权限、管理员账号 |

**划分原则**

1. 客户端协议只放**一个人在聊天时自己会做的事**：收发消息、管理自己有权管的会话、已读、拉黑、举报、对自己的 Agent 做人工介入（审批、接管、急停）。
2. 下面这些都交给开发者，通过开放 API 完成：
   - 账号的生命周期：创建、停用、删除、导出；
   - 身份和登录：签发令牌，密码由开发者自己的账号系统管理；
   - 最终用户能做什么的开关；
   - Agent 的发放和处置；
   - 审核。

   开发者决定何时调用、怎样核实身份、怎样把结果交给最终用户。
3. 部署本身的事交给部署者：数据库、集群、加密密钥、全局统计、运维审计。开发者的 API Key 做不了这些，反过来也一样（[07 §1](07-open-api.md)）。

**业内的做法**（下面是我凭记忆的总结，接口名可能不准）

- **Stream Chat**：客户端能做什么，由开发者在控制台里按角色和频道类型配置权限策略。创建和删除用户、导出、封禁、签发令牌都只能在服务端用密钥调用。
- **Sendbird**：在应用设置里开关"用户能否建群、能否邀请"等。GDPR 的导出和删除、封禁、禁言走平台 API。
- **OpenIM**：IM 服务和账号服务是分开的两个程序。账号、注册、登录属于"应用"，不在 IM 核心里。

## 2. 现状逐项（✅ 归属对；⚠️ 混在一起或缺失）

### 2.1 客户端协议（`/ws`，最终用户和 Agent）

| 操作 | 现在谁能调用 | 判断 |
|---|---|---|
| `send` `edit` `recall` `read` `signal` `set_mute` `focus` `list_convs` `sync` `ack` `unread` `heads` `set_prefs` | 成员、本人 | ✅ |
| `create_conv` `add_member` `remove_member` `leave` `set_role` `transfer_owner` `dissolve` `update_conv` | 按群主、管理员、成员的规则 | ✅ 操作本身没问题，但开发者**没有开关**能关掉"用户自己建群、拉人"（§3 问题 7） |
| `block` `unblock` `list_blocks` `report` | 本人 | ✅（举报是 DSA 要求给最终用户的入口）。另外：Agent 也能调用 `block`，`report` 已经只限人 |
| `find_user` | 任何人，查同一单元、自己的 Agent、共享过会话的人 | ⚠️ 用户目录应该由开发者的产品决定，现在没有开关（问题 7）；Agent 也能调用 |
| `whisper` `approve` `reject` `takeover` `release` `set_agent_policy` `reset_guard` `observe` `list_drafts` `inbox` `decide` `handback` `list_feedback` | Agent 的负责人、被指定的人 | ✅ 这些是人对 AI 的介入，属于最终用户 |
| `stop_agent` `resume_agent` | 负责人 | ✅ 负责人随时能急停是应该有的。⚠️ 开放 API 没有对应接口，开发者和审核员停不了某个 Agent（问题 4） |
| `create_agent` `agent_key` `revoke_agent_key` `list_agents` `destroy_agent` | 任何人可以建自己的 Agent、拿密钥、销毁 | ⚠️ 最终用户在设备上拿到 Agent 密钥，开发者无法关闭（问题 3） |
| `stream_*` `request_human` `cancel_request` `get_request` `history` `context` `search` `put_summary` `state_*` `progress` `handoff` `claim` `release_claim` `floor_*` `respond` `feedback` | Agent 或成员 | ✅ |
| `list_devices` `logout_device` | 本人 | ✅ 管理自己的设备。Agent 也能调用（问题 9） |
| `set_password` `totp_setup` `totp_enable` `totp_disable` | 本人 | ⚠️ 这是内置账号体系（问题 2） |
| ~~`delete_account`~~ | — | ✅ 已删除，只走开放 API（问题 1） |
| `stats` | 任何令牌，只在 `--demo` 时开放 | ⚠️ 返回整个部署的统计、在线名单和最近的审计，属于部署者（问题 5） |

### 2.2 HTTP 路由

| 路由 | 用途 | 判断 |
|---|---|---|
| `/v1/...`（除下一行） | 开放 API，需要 API Key | ✅ |
| ~~`GET /v1/me/export`~~ | — | ✅ 已删除（问题 1） |
| `POST /api/auth/login`、`POST /api/auth/register` | 内置账号的密码登录和自助注册（注册默认关闭） | ⚠️ 问题 2 |
| `POST /api/login`、`POST /api/dev/users` | 演示用：免密码登录、随意建用户，只在 `--demo` 时可用 | ⚠️ 问题 6 |
| `GET /api/stats` | 同 `stats` | ⚠️ 问题 5 |
| `/mcp` | 只接受 Agent 密钥 | ✅ |
| `/metrics` `/healthz` `/readyz` | 部署者 | ✅ |

### 2.3 命令行（部署者）

`migrate-unit`、`encryption …`、`api-key …`、`cluster …` 都属于部署者 ✅。

下面这些开发者用开放 API 做不到：

- `user set-password`：问题 2；
- `agent create --team`、`agent transfer`、`agent export`、`agent import`：只能在服务器上执行。自托管时开发者就是部署者，还能用；到了托管云，开发者做不了（问题 3）。

### 2.4 开放 API 缺的开发者功能

- **Agent**：可以用 `PUT /v1/users/{id}`（`kind: agent`）创建，用 `/tokens` 签发和吊销密钥，但没有以下接口：
  - 团队 Agent、转交、急停和恢复、销毁、列表、导出和导入；
  - 而且 `DELETE /v1/users/{id}` 对 Agent 返回 400。
- **审核**：会话内禁言（[07 §3.1](07-open-api.md) 里 `moderation` 权限计划中的一项）没有做；查看某人的拉黑列表也没有。
- **设备**：只能吊销某人的全部令牌，看不到他的设备列表，也不能只下线其中一台。
- **服务端 SDK**（Node、Python、Go）：还没有加 A4 的接口（删除、删除进度、导出、举报队列），但 [07 §5](07-open-api.md) 写的是"覆盖全部接口"。

## 3. 问题和建议

| # | 问题 | 建议 |
|---|---|---|
| 1 | 删除账号、导出数据有最终用户入口 | 已确认：只保留 `DELETE /v1/users/{id}`、`GET …/erasure`、`GET /v1/users/{id}/export`；删除 `delete_account` 和 `/v1/me/export`，以及每人每小时 3 次的导出限制（开发者按会话分页续传时会被它卡住）；删除记录里写上是哪个 API Key 发起的。✅ 已完成 |
| 2 | **IM 里有一套最终用户的账号体系**：密码、TOTP、注册、改密码、`user set-password`。最终用户的账号应该在开发者的系统里，IM 只认开发者签发的令牌；现在两套并存，开发者的用户会多出一个"IM 密码" | 去掉最终用户的密码、TOTP、注册和对应的协议操作和路由。已有的代码（Argon2id、锁定、TOTP）挪给 A5 控制台的**管理员**登录用（[12 §4](12-developer-experience.md) 本来就要这些）。演示继续用 `--demo` 的免密码登录 |
| 3 | **最终用户可以自建 Agent、在设备上拿到 Agent 密钥**，开发者既关不掉、也管不了数量以外的东西；团队 Agent、转交、导出导入又只能在服务器上做 | Agent 的发放和处置放到开放 API（新权限 `agents:write`）：建（个人、团队）、列表、签发和吊销密钥、转交、销毁、导出、导入。客户端的 `create_agent`、`agent_key`、`revoke_agent_key`、`destroy_agent` 改成开发者可开的选项，**默认关**。需要"用户自带 Agent"的产品打开它（见 §4 决定 B） |
| 4 | 开发者、审核员不能急停或销毁某个 Agent | 开放 API 加急停、恢复、销毁。负责人在客户端的急停和恢复保留：这是人对 AI 的实时介入 |
| 5 | 整个部署的统计和审计（`stats`、`/api/stats`）挂在最终用户协议上 | 从客户端协议和 `/api` 删掉，挪到 A5 管理 API 的"概览"。演示页的统计面板改为调管理 API |
| 6 | 演示接口（免密码登录、随意建用户）编进了正式程序，`/api/dev/users` 也没有 Origin 校验 | 保留，只在 `--demo` 下可用，加上 Origin 校验；`--demo` 监听非本机地址时启动报错（除非显式加一个参数）；文档写清"仅限本机开发" |
| 7 | **开发者没有开关决定最终用户在客户端能做什么**（建群、建单聊、拉人、查用户目录、编辑或撤回的时限、自建 Agent） | 加一组应用级的客户端权限，默认值保持现在的行为（自建 Agent 除外，见问题 3）。先放在配置文件 `[clients.allow]`，A5 以后在控制台和管理 API 里改。拒绝时返回 `forbidden`，不留痕迹 |
| 8 | 开放 API 缺设备列表和单台下线、会话内禁言、某人的拉黑列表；服务端 SDK 没有 A4 接口 | 设备、禁言、SDK 补上；拉黑列表算个人数据，只在导出里出现，不单独开接口 |
| 9 | 角色检查有漏洞：Agent 能调用 `totp_*`、`list_devices`、`logout_device`、`block`、`feedback`（`find_user` 是 Agent 通过 MCP 找人用的，保留，受问题 7 的开关管）；"只给 Agent 用"的操作，拒绝人时有的返回 `bad_request`、有的返回 `forbidden` | ✅ 补上类型检查。凡是"你这种身份不能做"的统一返回 `forbidden`（CLAUDE.md 的错误码规则），每个都加测试，拒绝不留痕迹 |
| 10 | A5 控制台的设计把"应用管理"（API Key、用户、审核、Webhook、Agent）和"部署运维"（数据库、集群、加密、节点）放在同一个界面、同一套权限里。托管云上这是两拨人。应用级的设置（Webhook、发送前拦截、审核时限、Agent 上限）现在也写在部署的配置文件里 | 控制台和管理 API 分成两块：**应用**（开发者）和**部署**（部署者），各自的权限范围，托管云只给客户"应用"那块。应用设置先留在配置文件，A5 时搬到"应用"下，按 `tenant_id` 存 |

## 4. 决定（2026-10-20 已确认）

| | 问题 | 结果 |
|---|---|---|
| A | 最终用户的内置账号（密码、TOTP、注册）是去掉，还是留作"独立使用"模式？ | **去掉**。IM 没有账号体系，只有开发者给的用户 ID 和签发的令牌（§4.2）。独立使用的人用示例应用里的登录，示例自己签发令牌 |
| B | 最终用户自建 Agent：完全不提供，还是做成开发者可开的选项？ | **由开发者控制，默认不能自建**；开发者可以整体打开，也可以只授权给某些用户（§4.1） |
| C | 负责人在客户端销毁自己的 Agent，跟着 B 走（打开 B 时才能用），还是只留开放 API？ | 跟着 B 走（§4.1） |
| D | 问题 7 的客户端权限：先做成配置文件里全局一组，还是直接按角色和会话类型配置（Stream 的做法）？ | 全局一组默认值，加上按用户的授权（§4.1）；按会话类型细分以后再加 |
| E | 以上属于协议 v1 里删除操作，不符合"主版本内只做新增"（[02 §2.5](02-architecture.md)）。因为还没有正式发布（A9 未做），建议现在直接删、不升级版本号 | 发布前直接删 |

### 4.1 客户端权限和 Agent 授权（D117）

**两层**：部署的默认值，加上开发者对单个用户的授权。用户身上的设置优先。

| 层 | 在哪设 | 内容 |
|---|---|---|
| 默认值 | 配置文件 `[clients.allow]`；A5 以后在控制台和管理 API 的"应用设置"里改 | `create_groups`、`create_direct`、`add_members`、`find_users` 默认 `true`（保持现在的行为）；`create_agents` 默认 **`false`**；`edit_minutes`、`recall_minutes` 默认不限 |
| 单个用户 | 开放 API `PATCH /v1/users/{id}` 的 `permissions`（`users:write` 权限），例如 `{"permissions": {"create_agents": true}}`；`null` 表示跟随默认值。A5 的管理 API 和控制台可以做同样的事 | 同一组键 |

- **开发者的 App 怎么知道**：`hello` 返回的 `me` 带上算好的 `permissions`。改了以后，这个用户在线的设备收到 `user.updated`，App 据此显示或隐藏按钮。
- **被拒绝时**：返回 `forbidden`（"the app does not allow this"），不写任何东西。
- **`create_agents` 为 `true` 时**，用户在客户端上能对**自己的** Agent 做这些：
  - `create_agent`、`list_agents`；
  - `agent_key`、`revoke_agent_key`；
  - `destroy_agent`。
- **授权收回后**：
  - 已有的 Agent 照常运行，仍归这个用户；
  - 用户不能再新建、换密钥、销毁，这些交给开发者用开放 API 处理；
  - 急停、恢复、审批、接管、耳语、代看等**人对 AI 的介入始终保留**，因为这是负责人的安全责任，和能不能自建无关。
- **开发者自己发放 Agent**：开放 API 新权限 `agents:write`，A5 的管理 API 同样提供：
  - 创建个人或团队 Agent。现有的 `PUT /v1/users/{id}`（`kind: agent`）保留，加上 `team`；
  - 列出某人的 Agent：`GET /v1/users/{id}/agents`；
  - 签发和吊销密钥：现有的 `/tokens`；
  - 急停、恢复：`POST /v1/agents/{id}:stop`、`:resume`；
  - 销毁：`DELETE /v1/agents/{id}`；
  - 转交：`POST /v1/agents/{id}:transfer`；
  - 导出、导入：`GET /v1/agents/{id}/export`、`POST /v1/agents:import`。

  这些都走和客户端相同的业务逻辑，错误码一致，写审计。命令行的 `agent …` 改成调这套接口（A5 的 `agentim admin`）。
- **业内**：
  - Sendbird 的机器人只能用平台 API 创建（`POST /v3/bots`）；
  - Stream 的权限由开发者在服务端按角色授予，客户端不能改自己的角色。
- **测试**：
  - 默认值、用户覆盖、`null` 回到默认、收回后的每个操作，以及收回后急停、审批等仍然可用；
  - 每个开关对应的客户端操作都要被拦住，开放 API 以用户身份调用时同样遵守；
  - 授权和创建并发：收回授权之后，不会再多出新的 Agent；
  - 重启后保持，三种数据库。

### 4.2 登录：IM 没有账号，只有用户 ID 和令牌（D118 待定一项）

用户 ID 只来自开发者：开放 API 的 `PUT /v1/users/{id}`，或者下面方式 2、3 第一次见到的 ID。IM 不存最终用户的密码。业内有三种签发令牌的方式：

| 方式 | 谁在用 | 做法 | 我们 |
|---|---|---|---|
| 1. 开发者后端调接口签发 | Sendbird（`/users/{id}/token`，会话令牌默认 7 天）、OpenIM（`/auth/get_user_token`） | 用户登录开发者的产品后，开发者后端调 IM 的接口拿令牌，交给 App | ✅ `POST /v1/users/{id}/tokens` |
| 2. 开发者后端自己签名 | Stream（`createToken(user_id)`，JWT）、腾讯云 IM（UserSig，用密钥算 HMAC，默认 180 天） | 开发者后端用和 IM 共享的签名密钥离线生成令牌，不需要调 IM；IM 验签、读出用户 ID | 没有 |
| 3. IM 回调开发者 | Centrifugo 的 connect proxy | App 带着开发者自己的登录凭据连 IM，IM 把它转给开发者配置的回调地址，开发者返回用户 ID | 没有 |

客户端在令牌快过期时都向开发者的后端要新的（Ably 的 `authCallback`、Sendbird 的会话处理器）。我们的 TS SDK 已经有 `tokenProvider`，Rust 核心 SDK 照做。

**建议（D118，请你定）**：

- **加上方式 2**：签名密钥像 API Key 一样管理（可以有多把、可轮换、只存哈希或加密存放）。令牌是标准 JWT（HS256），`sub` 是用户 ID，带 `exp`。可选：第一次见到的 ID 自动创建用户，默认关。好处是开发者少一次网络调用，也是 Stream 和腾讯的主流做法。
- **方式 3 先不做**：每次连接和重连都要等开发者的后端，它慢了或宕机，IM 就连不上（影响 G1）；服务端重启后的大量重连也会全部打到开发者后端。真有需要，可以用 Webhook 加方式 1 或 2 代替。

## 5. 对核心保证（G1–G6）的影响

都是入口和权限上的调整，写入路径、单写者、seq、推送都不变：

- G1–G3、G5、G6：不受影响。
- G4 业务正确：会更严，补上了角色检查，最终用户能做的事改由开发者决定。

## 6. 顺序

1. ✅ 问题 1 和问题 9（`totp_*` 随问题 2 一起删除）。
2. 问题 2、5、6：删掉最终用户账号体系和部署统计，演示改用新入口。
3. 问题 3、4、8：开放 API 加 Agent 和设备接口，客户端选项；服务端 SDK 补齐。
4. 问题 7 和 §4.1：客户端权限和按用户授权。
5. 问题 10：并入 A5。

每一步都按 CLAUDE.md 的要求：覆盖每个相关逻辑节点、边界、错误码、并发、三种数据库，同步更新 04、05、07、03 的文档。
