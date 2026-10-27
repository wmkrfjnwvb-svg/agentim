# 13 谁用哪个入口：开发者、最终用户、Agent、部署者

> 2026-10-20。起因：A4 把"删除账号""导出数据"同时放进了最终用户的客户端和开发者的开放 API。负责人指出这两类人的入口应该分清：**开发者**写客户端、做集成，API 怎么用由开发者决定；**最终用户**是开发者产品的用户，只通过开发者做的界面使用。本文排查了全部入口，列出混在一起的地方和改法。
>
> **状态**：§3 的 10 个问题和 §4 的 A–E **已确认**（2026-10-20，D116）并**全部完成**（2026-10-21）；Agent 的授权按负责人的意见细化在 §4.1（D117）；登录方式按 §4.2 只用开发者签名的 JWT（D118）。

## 1. 四种角色，四类入口

| 角色 | 是谁 | 入口 | 身份凭据 |
|---|---|---|---|
| **最终用户** | 开发者产品里的人 | 客户端协议 `/ws`。开发者的 App 通过客户端 SDK 调用，最终用户从不直接接触 | 客户端令牌：开发者后端用签名密钥自己签的 JWT（§4.2） |
| **Agent** | 接进会话的 AI | 客户端协议 `/ws`、MCP `/mcp`、Agent SDK | Agent 密钥 |
| **开发者** | 用 AgentIM 搭产品的团队 | 开放 API `/v1`（后端调用）、Webhook、发送前拦截；控制台 `/console` 和管理 API `/admin/v1/app` 的"应用"部分 | API Key（带权限范围）；控制台的管理员账号或管理密钥 |
| **部署者** | 运行这套服务的人。自托管时往往就是开发者本人；托管云上是我们 | 命令行 `agentim …`、配置文件、`/metrics`；管理 API `/admin/v1/deploy` 和控制台里的"部署"部分 | 服务器权限、管理员账号或管理密钥 |

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
| ~~`set_password` `totp_setup` `totp_enable` `totp_disable`~~ | — | ✅ 已删除（问题 2） |
| ~~`delete_account`~~ | — | ✅ 已删除，只走开放 API（问题 1） |
| ~~`stats`~~ | — | ✅ 已删除，改到管理 API 的概览（问题 5） |

### 2.2 HTTP 路由

| 路由 | 用途 | 判断 |
|---|---|---|
| `/v1/...`（除下一行） | 开放 API，需要 API Key | ✅ |
| ~~`GET /v1/me/export`~~ | — | ✅ 已删除（问题 1） |
| ~~`POST /api/auth/login`、`POST /api/auth/register`~~ | — | ✅ 已删除（问题 2） |
| `POST /api/login`、`POST /api/dev/users` | 演示用：免密码登录、随意建用户，只在 `--demo` 时可用 | ✅ 两个都校验 Origin，`--demo` 只监听本机（问题 6） |
| ~~`GET /api/stats`~~ | — | ✅ 已删除（问题 5） |
| `/mcp` | 只接受 Agent 密钥 | ✅ |
| `/metrics` `/healthz` `/readyz` | 部署者 | ✅ |

### 2.3 命令行（部署者）

`migrate-unit`、`encryption …`、`api-key …`、`cluster …` 都属于部署者 ✅。

下面这些开发者用开放 API 做不到：

- ~~`user set-password`~~：✅ 已删除（问题 2）；
- ~~`agent create/transfer/key/list/export/import`~~：✅ 已删除。开发者用开放 API、管理 API 或控制台；命令行是 `agentim admin agent-create …`，它调管理 API（问题 3、10）。

### 2.4 开放 API 缺的开发者功能（✅ 已补上）

- **Agent**：建（个人、团队，`PUT /v1/users/{id}` 带 `team`，返回第一把密钥）、列表、签发和吊销密钥、急停和恢复、转交、销毁、导出和导入，权限 `agents:write` / `agents:read`（[07 §3.4](07-open-api.md)）。`DELETE /v1/users/{id}` 仍然只删人，Agent 用 `DELETE /v1/agents/{id}` 销毁。
- **审核**：会话内禁言（`:silence`、`:unsilence`，`moderation`）。某人的拉黑列表算个人数据，只在导出里出现。
- **设备**：设备列表和单台下线（`GET`/`DELETE /v1/users/{id}/devices[/{device}]`）。
- **服务端 SDK**（Node、Python、Go）：覆盖全部接口，包括 A4 的删除、删除进度、导出、举报队列。

## 3. 问题和建议

| # | 问题 | 建议 |
|---|---|---|
| 1 | 删除账号、导出数据有最终用户入口 | 已确认：只保留 `DELETE /v1/users/{id}`、`GET …/erasure`、`GET /v1/users/{id}/export`；删除 `delete_account` 和 `/v1/me/export`，以及每人每小时 3 次的导出限制（开发者按会话分页续传时会被它卡住）；删除记录里写上是哪个 API Key 发起的。✅ 已完成 |
| 2 | **IM 里有一套最终用户的账号体系**：密码、TOTP、注册、改密码、`user set-password`。最终用户的账号应该在开发者的系统里，IM 只认开发者签发的令牌；现在两套并存，开发者的用户会多出一个"IM 密码" | ✅ 去掉最终用户的密码、TOTP、注册和对应的协议操作和路由。已有的代码（Argon2id、锁定、TOTP）挪给 A5 控制台的**管理员**登录用（[12 §4](12-developer-experience.md) 本来就要这些）。演示继续用 `--demo` 的免密码登录 |
| 3 | **最终用户可以自建 Agent、在设备上拿到 Agent 密钥**，开发者既关不掉、也管不了数量以外的东西；团队 Agent、转交、导出导入又只能在服务器上做 | ✅ Agent 的发放和处置放到开放 API（新权限 `agents:write`）：建（个人、团队）、列表、签发和吊销密钥、转交、销毁、导出、导入。客户端的 `create_agent`、`agent_key`、`revoke_agent_key`、`destroy_agent` 改成开发者可开的选项，**默认关**。需要"用户自带 Agent"的产品打开它（见 §4 决定 B） |
| 4 | 开发者、审核员不能急停或销毁某个 Agent | ✅ 开放 API 加急停、恢复、销毁。负责人在客户端的急停和恢复保留：这是人对 AI 的实时介入 |
| 5 | 整个部署的统计和审计（`stats`、`/api/stats`）挂在最终用户协议上 | ✅ 从客户端协议和 `/api` 删掉，挪到管理 API 的"概览"（`GET /admin/v1/app/overview`）。演示页的统计面板删掉 |
| 6 | 演示接口（免密码登录、随意建用户）编进了正式程序，`/api/dev/users` 也没有 Origin 校验 | ✅ 保留，只在 `--demo` 下可用，加上 Origin 校验；`--demo` 监听非本机地址时启动报错（除非显式加一个参数）；文档写清"仅限本机开发" |
| 7 | **开发者没有开关决定最终用户在客户端能做什么**（建群、建单聊、拉人、查用户目录、编辑或撤回的时限、自建 Agent） | ✅ 加一组应用级的客户端权限，默认值保持现在的行为（自建 Agent 除外，见问题 3）。默认值在配置文件 `[clients.allow]` 和控制台的"应用设置"里改。拒绝时返回 `forbidden`，不留痕迹 |
| 8 | 开放 API 缺设备列表和单台下线、会话内禁言、某人的拉黑列表；服务端 SDK 没有 A4 接口 | ✅ 设备、禁言、SDK 补上；拉黑列表算个人数据，只在导出里出现，不单独开接口 |
| 9 | 角色检查有漏洞：Agent 能调用 `totp_*`、`list_devices`、`logout_device`、`block`、`feedback`（`find_user` 是 Agent 通过 MCP 找人用的，保留，受问题 7 的开关管）；"只给 Agent 用"的操作，拒绝人时有的返回 `bad_request`、有的返回 `forbidden` | ✅ 补上类型检查。凡是"你这种身份不能做"的统一返回 `forbidden`（CLAUDE.md 的错误码规则），每个都加测试，拒绝不留痕迹 |
| 10 | A5 控制台的设计把"应用管理"（API Key、用户、审核、Webhook、Agent）和"部署运维"（数据库、集群、加密、节点）放在同一个界面、同一套权限里。托管云上这是两拨人。应用级的设置（Webhook、发送前拦截、审核时限、Agent 上限）现在也写在部署的配置文件里 | ✅ 控制台和管理 API 分成两块：**应用**（`/admin/v1/app`，`owner`、`developer`，`moderator` 只有审核和会话）和**部署**（`/admin/v1/deploy`，`owner`、`operator`），托管云只给客户"应用"那块（[12 §4](12-developer-experience.md)）。客户端权限的默认值已搬到"应用设置"，按 `tenant_id` 存在控制库；Webhook、发送前拦截、审核时限、Agent 上限这一版仍在配置文件，控制台只读显示 |

## 4. 决定（2026-10-20 已确认）

| | 问题 | 结果 |
|---|---|---|
| A | 最终用户的内置账号（密码、TOTP、注册）是去掉，还是留作"独立使用"模式？ | **去掉**。IM 没有账号体系，只有开发者给的用户 ID 和签发的令牌（§4.2）。独立使用的人用示例应用里的登录，示例自己签发令牌 |
| B | 最终用户自建 Agent：完全不提供，还是做成开发者可开的选项？ | **由开发者控制，默认不能自建**；开发者可以整体打开，也可以只授权给某些用户（§4.1） |
| C | 负责人在客户端销毁自己的 Agent，跟着 B 走（打开 B 时才能用），还是只留开放 API？ | 跟着 B 走（§4.1） |
| D | 问题 7 的客户端权限：先做成配置文件里全局一组，还是直接按角色和会话类型配置（Stream 的做法）？ | 全局一组默认值，加上按用户的授权（§4.1）；按会话类型细分以后再加 |
| E | 以上属于协议 v1 里删除操作，不符合"主版本内只做新增"（[02 §2.5](02-architecture.md)）。因为还没有正式发布（A9 未做），建议现在直接删、不升级版本号 | 发布前直接删 |

### 4.1 客户端权限和 Agent 授权（D117，✅ 已实现）

**两层**：部署的默认值，加上开发者对单个用户的授权。用户身上的设置优先。

| 层 | 在哪设 | 内容 |
|---|---|---|
| 默认值 | 配置文件 `[clients.allow]` 是第一次启动的初始值；在控制台或管理 API 的"应用设置"（`PUT /admin/v1/app/settings/clients`）里改过以后，以控制库里存的为准，所有节点立即生效 | `create_groups`、`create_direct`、`add_members`、`find_users` 默认 `true`（保持现在的行为）；`create_agents` 默认 **`false`**；`edit_minutes`、`recall_minutes` 默认不限 |
| 单个用户 | 开放 API `PATCH /v1/users/{id}` 的 `permissions`（`users:write` 权限），例如 `{"permissions": {"create_agents": true}}`；`null` 表示跟随默认值。管理 API（`PATCH /admin/v1/app/users/{id}`）和控制台的"人员"页做同样的事 | 同一组键 |

- **开发者的 App 怎么知道**：`hello` 返回的 `me` 带上算好的 `permissions`。改了以后，这个用户在线的设备收到 `user.updated`，App 据此显示或隐藏按钮。
- **被拒绝时**：返回 `forbidden`（"the app does not allow this: …"），不写任何东西。开放 API 以这个用户身份（`"as": "<id>"`）调用时同样遵守；以 `"as": "system"` 调用（包括替用户建会话）不受限制。Agent 受它负责人的权限约束（比如 `find_users` 关了，负责人的 Agent 也不能查人）。
- **时限**：`edit_minutes`、`recall_minutes` 只限制一个人编辑或撤回**自己**的消息；群主、管理员撤回别人的消息不受限制。
- **自建 Agent 的检查在控制库的写线程里再做一次**：收回授权一旦返回，之后不会再建出这个人的 Agent（和建 Agent 在同一个写线程里排队）。
- **`create_agents` 为 `true` 时**，用户在客户端上能对**自己的** Agent 做这些：
  - `create_agent`；
  - `agent_key`、`revoke_agent_key`；
  - `destroy_agent`。

  `list_agents` 始终可用：负责人要看到自己的 Agent 才能急停、审批（实现时从上面的清单里拿出来了）。
- **授权收回后**：
  - 已有的 Agent 照常运行，仍归这个用户；
  - 用户不能再新建、换密钥、销毁，这些交给开发者用开放 API 处理；
  - 急停、恢复、审批、接管、耳语、代看等**人对 AI 的介入始终保留**，因为这是负责人的安全责任，和能不能自建无关。
- **开发者自己发放 Agent**：开放 API 新权限 `agents:write`，管理 API 和控制台同样提供：
  - 创建个人或团队 Agent。现有的 `PUT /v1/users/{id}`（`kind: agent`）保留，加上 `team`；
  - 列出某人的 Agent：`GET /v1/users/{id}/agents`；
  - 签发和吊销密钥：现有的 `/tokens`；
  - 急停、恢复：`POST /v1/agents/{id}:stop`、`:resume`；
  - 销毁：`DELETE /v1/agents/{id}`；
  - 转交：`POST /v1/agents/{id}:transfer`；
  - 导出、导入：`GET /v1/agents/{id}/export`、`POST /v1/agents:import`。

  这些都走和客户端相同的业务逻辑，错误码一致，写审计。命令行的 `agent …` 已删除，改用 `agentim admin agent-create|agent-key|agent-stop|agent-resume|agent-transfer|agent-destroy`（调管理 API）。
- **业内**：
  - Sendbird 的机器人只能用平台 API 创建（`POST /v3/bots`）；
  - Stream 的权限由开发者在服务端按角色授予，客户端不能改自己的角色。
- **测试**：
  - 默认值、用户覆盖、`null` 回到默认、收回后的每个操作，以及收回后急停、审批等仍然可用；
  - 每个开关对应的客户端操作都要被拦住，开放 API 以用户身份调用时同样遵守；
  - 授权和创建并发：收回授权之后，不会再多出新的 Agent；
  - 重启后保持，三种数据库。

### 4.2 登录：开发者用签名密钥自己签发令牌（D118，✅ 已实现）

IM 没有账号体系，不存最终用户的密码。用户 ID 只来自开发者：开放 API 的 `PUT /v1/users/{id}`，或者第一次带着有效令牌连上来的 ID。

**只用一种方式：开发者后端用签名密钥离线签发 JWT**（2026-10-20 负责人确认，只保留一种，避免两套并存）。这是 Stream（`createToken`，JWT）、腾讯云 IM（UserSig，HMAC）、声网 Chat 的做法：开发者后端不用调我们的接口，签令牌不依赖 IM 是否在线。没有采用的：

- 开发者后端调接口换令牌（Sendbird、OpenIM）：现有的 `POST /v1/users/{id}/tokens` 删除，发布前直接删（§4 E）；
- IM 回调开发者验证（Centrifugo 的 connect proxy）：每次连接和重连都要等开发者后端，它慢了或宕机 IM 就连不上（影响 G1），服务端重启后的大量重连也会全部打到开发者后端。

**令牌格式**（标准 JWT，任何语言的 JWT 库都能签）：

```
头部  {"alg": "HS256", "typ": "JWT", "kid": "sk_…"}
内容  {"sub": "u123", "iat": 1792500000, "exp": 1792503600}
签名  HMAC-SHA256(签名密钥, base64url(头部) + "." + base64url(内容))
```

| 字段 | 规则 |
|---|---|
| `alg` | 只接受 `HS256`；`none` 和其他算法一律拒绝（防止已知的 JWT 算法混淆攻击） |
| `kid` | 必填，指明用哪一把签名密钥 |
| `sub` | 必填，用户 ID，规则和 `PUT /v1/users/{id}` 一样 |
| `iat` | 必填，签发时间；比服务器时间晚超过 60 秒的拒绝 |
| `exp` | 必填；`exp - iat` 不能超过 `max_token_days`（默认 30 天）。不提供永久令牌。快过期时 SDK 的 `tokenProvider` 向开发者后端要新的（Ably 的 `authCallback`、Sendbird 的会话处理器也是这样） |
| 其他字段 | 忽略 |

**签名密钥**：

- 部署者用本机命令 `agentim signing-key create|list|revoke`（还没有管理员时也能用），开发者在控制台的"密钥"页或管理 API（`/admin/v1/app/signing-keys`）管理。密钥只在创建时显示一次。
- 可以同时有多把，用于轮换：先建新的，开发者切换，再吊销旧的。吊销后用它签的令牌立即失效，已连接的断开。
- 验签需要原始密钥，不能只存哈希：用已有的静态加密（[04](04-security-compliance.md)）加密存放。集群里每个节点在本地验签，密钥变化走已有的缓存失效通知。

**用户和设备**：

- **第一次见到的用户 ID 自动创建**（Stream 的客户端连接也会建用户）：有效令牌本身就是开发者的担保。已删除的 ID（[04 §4.2](04-security-compliance.md)）、已停用的用户、Agent 的 ID 都拒绝；`system` 是保留字（开放 API 的 `"as": "system"`），任何地方都不能用作用户 ID。资料由开发者用 `PUT`/`PATCH /v1/users/{id}` 补上。
- **设备**：令牌里不带设备。App 第一次运行时生成一个设备 ID 存在本地，连接时带上（`/ws?device=…`，可以加 `device_name`、`platform`；令牌放在 `Authorization: Bearer` 头里，浏览器放在 `agentim.bearer.` 子协议里，见 [02 §2.5](02-architecture.md)），服务端第一次见到时登记这台设备，欢迎帧里返回服务端的 `device_id`。按设备记同步位置（[08 §2](08-sync-and-client.md)）、`list_devices`、新设备通知都不变；30 天不用就忘掉（`auth.token_idle_days`）。现在没有每人的设备上限。
- **下线一台设备**（`logout_device`、开放 API）：断开它、删掉它的同步位置，并记下时刻：这台设备上签发时间不晚于这一秒的令牌都被拒绝。App 需要向开发者后端要一个新令牌才能再连上，那时它是一台新设备。令牌本身不是按设备签的，所以拿到令牌的人仍可以换一个设备 ID 连上，直到令牌过期；真正怀疑泄露时用"让这个人在所有设备上退出"。业内同样如此：Stream 只能按用户吊销（`revoke_tokens_issued_before`）；Firebase 的 `revokeRefreshTokens` 也只按用户，文档里"设备丢失"的例子同样是整个用户退出；腾讯云 IM、声网 Chat 能把某台设备踢下线，但那是断开连接，不是让令牌失效。补救办法是令牌有效期短，靠 `tokenProvider` 自动续签。
- **让一个人在所有设备上退出**：`DELETE /v1/users/{id}/tokens`（权限 `tokens:revoke`），记下吊销时刻，`iat` 不晚于这一秒的令牌一律失效，已连接的立即断开。
- **令牌过期不断开已连接的连接**：只在连接时检查，和 Stream、Sendbird 一样；断线重连时 SDK 用 `tokenProvider` 拿新令牌。
- **Agent 不走这里**：Agent 继续用 Agent 密钥。开放 API 里给 Agent 签发和吊销密钥是 `POST /v1/agents/{id}/keys`、`DELETE /v1/agents/{id}/keys[/{device}]`（`agents:write`）。

**开发和测试**：命令行 `agentim token sign --user u1 [--ttl 3600]` 用最新的签名密钥签一个令牌，给本地调试用；`--demo` 自己建一把签名密钥，`/api/login` 返回签好的令牌；服务端 SDK（Node、Python、Go）提供 `createUserToken`。

**测试**：

- 格式：缺字段、`alg` 是 `none` 或 `RS256`、未知 `kid`、签名错、过期、`iat` 在未来、有效期超过上限、`sub` 不合法，都拒绝且不留痕迹（不建用户、不登记设备）。
- 用户：第一次见到自动创建；已删除、已停用、`system`、Agent 的 ID 拒绝；并发用同一个新 ID 连接只建一个用户。
- 吊销：吊销密钥、吊销用户全部令牌后，旧令牌立即失效、已连接的断开、新签的可用；吊销和连接并发，吊销之后不会有旧令牌连上；集群里每个节点都生效。
- 设备：新设备登记、同一 App 设备 ID 在不同人之间互不影响、参数校验、下线后旧令牌在这台设备上不能再连而新令牌可以、30 天不用被忘掉、同步位置照常。
- 重启后保持，三种数据库。

## 5. 对核心保证（G1–G6）的影响

都是入口和权限上的调整，写入路径、单写者、seq、推送都不变：

- G1–G3、G5、G6：不受影响。
- G4 业务正确：会更严，补上了角色检查，最终用户能做的事改由开发者决定。

## 6. 顺序

1. ✅ 问题 1 和问题 9（`totp_*` 随问题 2 一起删除）。
2. ✅ 问题 2、5、6：删掉最终用户账号体系和部署统计，演示改用新入口。
3. ✅ §4.2 的签名令牌（替换 `POST /v1/users/{id}/tokens`）；✅ 问题 3、4、8：开放 API 加 Agent、设备和禁言接口，服务端 SDK 补齐（客户端选项在第 4 步）。
4. ✅ 问题 7 和 §4.1：客户端权限和按用户授权。
5. ✅ 问题 10：随 A5 完成（[12 §4](12-developer-experience.md)）。

每一步都按 CLAUDE.md 的要求：覆盖每个相关逻辑节点、边界、错误码、并发、三种数据库，同步更新 04、05、07、03 的文档。
