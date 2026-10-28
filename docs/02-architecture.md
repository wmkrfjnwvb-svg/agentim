# 02 架构

> §1–§5 已实现并通过测试（代码在 `server/`），未实现的部分单独注明；§4.8 是计划中的写入优化，§6 是计划中的大群模式；集群见 [11](11-cluster.md)。

## 1. 设计原则

1. 按会话分片，只保证会话内有序。
2. seq 分配和持久化在同一个事务里完成；**持久化（fsync）之后才 ACK**。
3. 推送只是通知，拉取才是真相。推送可以丢，正确性由同步保证。
4. 只有存储层是强一致的；路由、在线状态、流式缓冲都是软状态，丢失后可以重建。
5. Agent 在 IM 外部运行，模型的延迟和故障不会传导到消息核心。
6. 过载时拒绝请求，不排无界队列（稳定性优先于吞吐）。
7. 扩展点落在 trait、Webhook 和钩子上（§2.6），不改服务端代码就能定制。

## 2. 消息模型

### 2.1 主日志：会话事件序号

每个会话一条事件日志。`conv_seq` 是**事件序号**，单调递增、没有空洞，所有成员看到的都一样。

| 事件 | 含义 |
|---|---|
| `conv.created`、`conv.updated` | 会话创建、属性变化 |
| `member.joined`、`members.joined`、`member.left`、`member.policy` | 成员变更、Agent 策略变化。建群时一起加入的成员每 200 人一条 `members.joined`（`data.members` 是成员数组，顺序同成员列表：群主、管理员、成员，再按 id；D128），单人加入仍是 `member.joined` |
| `msg.created` | 新消息。流式消息在开始时创建，`state=streaming` |
| `msg.updated` | 编辑、流式结束、审核替换（`version+1`） |
| `msg.recalled` | 撤回（内容清空） |
| `agent.state` | 接管、交还、熔断、护栏重置（对全员可见，保证透明） |

消息另有一个不变的 `created_seq`，表示它在会话中的位置。**展示按 `created_seq` 排序，同步按事件 `conv_seq` 进行。** 增量同步就能拿到"新消息 + 旧消息的变化"。

### 2.2 侧信道：定向可见内容

每个会话还有一条侧信道，序号是 `side_seq`。存放只有部分人可见的内容：耳语、仅某人可见的消息、审批草稿、护栏和过滤的私有通知。每条侧信道事件带两个字段：

- `audience`：可见者列表。
- `anchor_seq`：产生时主日志的最新 seq。

Agent 和审批人按 `(anchor_seq, side_seq)` 把两条日志归并，得到完整上下文。普通成员拿不到侧信道，所以**主日志对每个成员都没有空洞，也不泄露"这里有一条你看不到的消息"**。

### 2.3 消息字段

```
Message {
  tenant_id, conv_id, msg_id, created_seq,
  sender_id, sender_type,      // human | agent | system，由服务端按认证身份写入
  on_behalf_of,                // Agent 代表谁发言（只能是它的所有者或委托人）
  kind,                        // text | interactive | tool_call | tool_result | thinking | ...
  content, version,
  state,                       // committed | streaming | complete | cancelled | failed | recalled
  state_reason,                // timeout | disconnected | cancelled_by:<name> | policy:... | taken_over
  hop, causation_id,           // 由服务端计算，用于循环熔断
  reply_to, mentions,
  ai_generated,                // 机器可读的 AI 标记（EU AI Act 第 50 条）🔧
  created_at, updated_at       // 由写入方（写线程）赋值
}
```

### 2.4 成员生命周期与用户目录

测试见 [05 §5](05-operations.md)（`tests/membership.rs`）。

| 操作 | 谁能做 | 结果 |
|---|---|---|
| `remove_member` | 群主和管理员；成员可以移除自己拉进来的 Agent | 主日志写 `member.removed`；被移除的人立刻收不到推送，也不能再读取这个会话 |
| `leave` | 任何成员（群主要先转让） | `member.left`；同上 |
| `set_role` | 群主 | 角色：群主、管理员、成员；管理员可以拉人、移除普通成员、撤回他人消息 |
| `transfer_owner` | 群主 | 群主变更事件 |
| `dissolve` | 群主 | `conv.dissolved`；所有成员失去访问；默认立即删除正文，保留"这个会话存在过"的审计记录 |
| `block` / `unblock` | 任何人 | 被拉黑的人不能和你建单聊，不能给你发仅你可见的消息，@你不会通知你；共同群里的消息照常（由客户端选择折叠）。App Store 要求有拉黑功能 |
| 单聊 | — | 同一对用户只有一个单聊：再次创建会返回已有的单聊 |

**Agent 被移除时**：取消它进行中的流，丢弃它待审批的草稿，结束它的任务（标为取消，原因"已移除"）。

**用户目录**：`hello` 只返回和你有共同会话的人、你的 Agent，以及同一单元里允许互相发现的人。找新的人用 `find_user`，按精确的用户名或邮箱查，并受单元的可见性策略限制。

**并发不变量**：`member.removed` 的 seq 为 N 时，被移除者不会出现 seq 大于 N 的消息；移除与该成员的发送同时发生时，结果只有两种：消息在移除之前落库，或者发送被拒绝。由单写者保证：移除和发送在同一个写线程里串行执行。

### 2.5 协议版本管理

**规则**

| 方面 | 规则 |
|---|---|
| 版本号 | 协议用整数主版本（`v1`、`v2`），主版本内只做向后兼容的新增；服务端软件版本用语义化版本号，和协议版本分开 |
| 功能声明 | 新功能用带命名空间的功能名声明（`files`、`reactions`、`tasks`）；实验功能用 `x.<域名>.<名字>`。客户端只使用服务端声明过的功能；服务端也按每个连接声明的功能决定怎么推送 |
| 主版本内允许 | 新增可选字段、新事件类型、新操作、新错误码、新的功能名 |
| 主版本内禁止 | 删除或改名字段、改变字段含义或类型、把可选改成必填、改变已有错误码的含义 |
| 客户端必须遵守 | 忽略不认识的字段；不认识的事件或消息类型显示降级文本；不认识的错误码按通用错误处理。SDK 默认就这样做 |
| 降级显示 | 每条消息都必须带纯文本（`text` / `fallback`，缺失时服务端拒绝）；新消息类型在老客户端上显示它，例如"[投票] 午饭吃什么？请升级客户端查看" |
| 弃用（计划） | 宣布后至少保留 12 个月且跨一个主版本；服务端同时支持当前和上一个主版本；弃用的用法在响应里带提示并计入指标 |
| 存储兼容 | 表结构有版本号，启动时自动迁移（§5.3）；集群滚动升级时新旧节点读写兼容（[11](11-cluster.md)） |
| 兼容测试（计划） | 协议用 AsyncAPI 描述；CI 里保留上一个版本的 SDK 和场景测试，每次用它们测新服务端 |

**握手与行为**（`server/crates/agentim-core/src/protocol.rs`，测试 `tests/protocol.rs`）

| 环节 | 行为 |
|---|---|
| 连接 | `GET /ws?v=2,1`，令牌放在请求头里、不放地址里（地址会被代理和负载均衡的访问日志记下，D129）：原生客户端和 Agent 用 `Authorization: Bearer <令牌>`；浏览器的 WebSocket 不能设这个头，改为提供子协议 `agentim, agentim.bearer.<令牌的 base64url>`，服务端回 `agentim`（Kubernetes 的做法，[`k8s.io/apiserver` websocket](https://godocs.io/k8s.io/apiserver/pkg/authentication/request/websocket)）；只给了 bearer 子协议没给 `agentim`、或者不是 base64url，返回 400。三者都有时 `Authorization` 优先。旧的 `?token=` 还接受一个版本，然后删除。可选带上客户端支持的协议版本（最多 16 个）；没有共同版本时在升级前返回 HTTP 400 `unsupported_protocol`，格式错误返回 400 `bad_request`，两者都附带服务端支持的 `protocols` |
| `welcome` | `protocol`（选定版本）、`protocols`、`server {name, version}`、`features`、`limits {message_bytes, frame_bytes}`、`me` |
| `hello` | 可选 `protocol: [..]`、`client {name, version, platform}`、`features: [..]`；回复同样的服务端信息加上 `me`、`users`、`my_agents`。不带这些字段的老客户端照常工作（得到当前版本）。重复 `hello` 以最后一次为准；被拒绝的 `hello` 不改变连接的状态 |
| 最低版本 | 配置 `[clients.min_versions]`（如 `agentim-js = "1.2.0"`，启动时校验格式）；低于最低版本返回 `client_too_old`（带升级说明），随后以关闭码 1008 断开。配置了最低版本的客户端必须带可比较的版本号，否则 `bad_request`；没配置的客户端名不检查 |
| 限制 | 名称、版本、平台各不超过 64 字节；功能最多 100 个，每个 1–64 字节 |
| 未知操作 | 不认识的 `op` 返回 `unknown_op`（客户端能区分"服务端太老"和"请求写错了"）；认识的操作缺字段或类型错误仍是 `bad_request`。`OPS` 列表和请求解析器由单元测试保证一致 |
| 未知字段 | 忽略 |
| 指标 | `agentim_clients_total{protocol, client}`；客户端名只有小写字母、数字和 `._-`、不超过 32 字节时才原样作为标签，否则记为 `other` |

### 2.6 扩展机制

开发者不改服务端代码就能加入自己的消息类型和功能，而且不破坏顺序、权限、审计、加密。

| 扩展点 | 状态 |
|---|---|
| 自定义消息类型、信号、Webhook、发送前拦截、自定义字段 | ✅ 见下 |
| 类型注册：为自定义类型注册 JSON Schema，服务端据此校验，标注要过 DLP、Agent 可见的字段；可配置"只允许已注册的类型" | 计划 |
| 交互消息：卡片带按钮或表单，点击后动作发给发消息的 Agent 或 Webhook，结果作为新消息或更新回到会话 | 见 [03](03-agents.md) |
| 服务端插件：现在是编译进服务端的 Rust trait；以后是 WASM 插件（Wasmtime / Extism，沙箱限制内存、CPU 时间和可调用的接口，运行时加载） | trait ✅；WASM 计划 |
| Agent：用 SDK 或 MCP 接入（[03 §1](03-agents.md)） | ✅ |

**自定义消息类型**（测试 `tests/extensions.rs`）

| 环节 | 行为 |
|---|---|
| 发送 | `send` 可选 `kind` 和 `payload`。`kind` 缺省或为 `text` 时不能带 `payload`。自定义类型必须带命名空间：至少两段、以点分隔，每段以小写字母开头、只含 `a-z0-9_-`，总长不超过 64 字节，`agentim.` 保留给内置类型。`payload` 必须是 JSON 对象，序列化后不超过 32 KiB，也可以不带。`text` 照常必填，就是老客户端显示的降级文本 |
| 存储与读取 | `payload` 和消息写在同一个事务（`messages.payload`），推送、同步、重启后都原样返回；去重、seq、权限和普通消息完全一样 |
| 编辑 | `edit` 可以带新的 `payload` 替换旧的；不带就保留。纯文本消息带 `payload` 返回 `bad_request` |
| 撤回 | 数据库里的 `payload` 清空，此前所有事件快照里的 `payload` 也清空 |
| 仅某人可见 | 侧信道事件里带上 `kind` 和 `payload` |
| 审批 | 草稿保存 `kind` 和 `payload`（`drafts.kind`、`drafts.payload`），批准后照原样发布；批准时改的只是降级文本 |
| 内容策略 | Agent 发送和编辑时的 DLP、Agent 读取时的注入检测，都同时检查文本和 `payload` |

**信号**：`signal {conv_id, kind, data?}`：`kind` 是内置的 `typing` 或带命名空间的自定义类型；`data` 是不超过 4 KiB 的 JSON 对象。只推给此刻在线的其他成员（同一用户的所有连接，包括 Agent），不推回发送者；不落库、不占 seq、不进侧信道，之后连上的人也收不到。非成员 `forbidden`，会话不存在 `not_found`，单聊里任一方拉黑了对方 `blocked`（群聊不受影响）。每个用户每秒最多 20 个（所有连接合计），超出 `rate_limited`。推送帧：`{"type": "signal", "conv_id", "kind", "from", "data"}`。

**Webhook**（测试 `tests/webhooks.rs`；签名和 Standard Webhooks 一致）

| 方面 | 行为 |
|---|---|
| 配置 | `[[webhooks]]`：`name`、`url`、`secret`（推荐 Standard Webhooks 的格式 `whsec_` 加 base64，HMAC 密钥是解码后的字节，和官方验签库一致；也可以是任意文本，用它本身的字节；密钥至少 16 字节）、`events`（`msg.created` 这样的精确类型、`msg.*` 前缀或 `*`；订阅 `member.joined` 或 `member.*` 的也收到 `members.joined`）、`max_attempts`（默认 8）、`timeout_ms`、`backoff_ms` |
| 不丢 | 订阅的事件和事件本身在**同一个事务**里写进 `webhook_outbox`（每批一条多行 INSERT）。没提交的写入、被拒绝的请求都不会发出去 |
| 有序 | 每个 Webhook 在每个数据库上有一个投递任务，按提交顺序一条一条发；前一条没成功不会发下一条 |
| 至少一次 | 成功后通过写线程移动游标（`meta` 里的 `webhook:<name>`）。重启后从游标继续；接收方用 `webhook-id` 去重 |
| 签名 | 请求头 `webhook-id`、`webhook-timestamp`、`webhook-signature: v1,<base64(HMAC-SHA256(id.timestamp.body))>` |
| 请求体 | `{"type", "timestamp", "database", "data": {"conv_id", "seq", "event"}}` |
| 重试 | 非 2xx 或超时就按指数退避重试（最长 5 分钟一次）；超过 `max_attempts` 次后放弃这一条，写审计 `webhook.dropped`，计入 `agentim_webhook_dropped_total`，继续发后面的。接收方可以用开放 API 的历史接口按 seq 补齐 |
| 清理 | 所有 Webhook 都处理过的行立即删除 |
| 新增 Webhook | 从当前位置开始，不重放历史 |
| 多库 | 每个库有自己的 outbox 和投递任务，`database` 字段标明来源 |

**发送前拦截**：`[before_send]`：`url`、`secret`、`timeout_ms`（默认 2000）、`on_error`（`allow` 或 `reject`）。每条 `send` 在进入写线程**之前**同步调用（不占写线程，组提交不受影响），请求带同样的签名头，内容为 `{"type": "message.before_send", "conv_id", "client_msg_id", "kind", "text", "payload", "reply_to", "to_user", "sender"}`。回复 `{"action": "allow"}`、`{"action": "reject", "reason"}`（返回 `policy_blocked` 和这个原因）或 `{"action": "replace", "text", "payload"}`（替换后照常校验，非法就 `bad_request`）。超时、非 2xx、不是 JSON、未知的 action 都按 `on_error` 处理。格式不对的请求先被拒绝，不会发给钩子；成员、拉黑等规则在钩子之后照常检查。编辑和流式消息不经过钩子。

**自定义字段**：用户和会话都可以挂一个不超过 4 KiB 的 JSON 对象，通过开放 API（`PUT/PATCH /v1/users/{id}`、`PATCH /v1/conversations/{id}`）或客户端的 `update_conv` 修改，会话的修改以 `conv.updated` 事件通知成员。

**扩展也守住核心保证**：自定义消息走同一条写入路径（单写者、组提交），同样不丢、有序；内容大小和信号频率都有上限；扩展类型的版本放在类型的内容里（例如 `schema_version`），兼容规则和 §2.5 相同。

### 2.7 文件和图片（2026-10-27 负责人确认，D132–D134；2026-10-28 服务端已做）

负责人定的方向：
- 部署者选**本机文件夹**或 **S3 兼容的对象存储**。用 S3 时客户端直接传到 S3，用本机文件夹时经过服务端。
- 图片由客户端先生成缩略图，缩略图和原图各传一次。缩略图长边 320 像素。聊天里显示缩略图；点开先看放大的缩略图，点"View original"再下载原图看。
- 单个文件默认不限制到很小，上限 4 GiB。默认去掉位置信息。
- 普通文件显示类型图标。
- 原来的 D27–D30、D72（内容存进数据库）作废。

**业内对照**
- Mattermost：存储选 `local`（默认 `./data/`）或 `amazons3`（桶、前缀、区域、终端地址、密钥或 IAM 角色）。上传时服务端生成 120×100 的缩略图和宽 1920 的预览图，先按 EXIF 方向转正。原图要下载才看得到（[设置文档](https://docs.mattermost.com/administration-guide/configure/environment-configuration-settings.html)、[`file.go`](https://github.com/mattermost/mattermost/blob/master/server/channels/app/file.go)、[论坛](https://forum.mattermost.com/t/show-pictures-in-original-size/4996)）。
- Slack：缩略图有八档，`url_private` 要带令牌才能取（[文件对象](https://docs.slack.dev/reference/objects/file-object)）。
- S3 预签名地址：服务端签一个短时有效的地址，客户端直接上传或下载，存储的密钥不给客户端（[MinIO](https://docs.min.io/community/minio-object-store/integrations/presigned-put-upload-via-browser.html)）。
- S3 的限制：一次 PUT 最大 5 GiB；分片上传每片 5 MiB 到 5 GiB，最多 10000 片（[S3 限制](https://docs.aws.amazon.com/AmazonS3/latest/userguide/qfacts.html)）。
- 没核对到：OpenIM 上传设计的官方文档。

**上传流程**：本机和 S3 是同一套流程，只是地址指向不同。

1. `upload_start {conv_id, name, size, mime?, thumb?: {size, width, height}}`，客户端协议和开放 API `POST /v1/uploads` 都有。服务端检查：
   - 能不能在这个会话发言：被移出、被拉黑、已解散、草稿模式的 Agent 都和发消息一样拒绝；
   - 大小：不超过 `files.max_bytes`，默认 4 GiB；空文件 `bad_request`；
   - 文件名：最长 255 字节，去掉路径和控制字符。

   通过后记下一条待完成的上传，返回 `{file_id, part_size, parts: [{n, url, headers}], thumb: {url, headers}?, expires_at}`：
   - S3：小于 64 MiB 的是一个预签名 PUT 地址；更大的先建分片上传，每片 16 MiB，地址分批给，用 `upload_parts {file_id, from}` 再要；
   - 本机：地址是服务端自己的 `PUT /files/upload/{file_id}/{n}?exp=…&sig=…`（HMAC，绑定用户、文件和片号）。
   - 地址 1 小时有效。
2. 客户端按地址 PUT。最多同时传 4 片；单片失败重试，换网络后从没传完的片继续（断点续传）。S3 返回的 `ETag` 记下来。用 S3 时，桶要配置 CORS，允许网页 PUT 并暴露 `ETag`，文档里给出配置。
3. `upload_complete {file_id, parts: [{n, etag}]}`。服务端：
   - S3 上完成分片上传，确认对象大小和声明的一样；
   - 读开头 64 字节判断类型（不信客户端声明）；
   - 有缩略图时取回来，确认是不超过 320 像素的 JPEG 或 PNG；
   - 然后通过写线程提交 `files` 记录，返回 `{file_id, name, size, mime, kind, width?, height?, thumb}`。

   大小对不上、类型和声明的图片不符、缩略图不合格，都 `bad_request` 并删掉已经传上去的内容。
**整个文件一次传上来**（Agent、MCP、开发者的后端）：
- 入口：
  - Agent 用 `POST /api/files?conv_id=…&name=…`，密钥放在 `Authorization` 头里；
  - MCP 用 `send_file`，内容是 base64，最多 16 MiB，传完直接发出；
  - 后端用开放 API 的 `POST /v1/conversations/{id}/files?as=…&name=…`。
- 每个请求最多 64 MiB。更大的文件走上面的分片流程。
- 服务端先去掉 EXIF 和 GPS，再生成缩略图，然后存起来，和上面一样检查。
- `/api/files` 先校验密钥再读请求体：没有密钥的人不能让服务端缓存大请求。

4. 发 `kind = "image"` 或 `"file"` 的消息，payload 带 `file_id`（可以加 `caption`）。服务端补上名字、大小、类型、宽高、有没有缩略图；降级文本是 `[Image]` / `[File] 名字`。
   - 一个文件只能被一条消息引用，两条消息抢同一个文件只有一条成功（`conflict`）。
   - 别人的、别的会话的文件 `forbidden`；没完成、不存在、过期的 `not_found`。

| 方面 | 做法 |
|---|---|
| 存储配置 | `[files] storage = "local"`（默认，`dir = "./data/files"`）或 `"s3"`。S3 要配：`bucket`、`region`、`endpoint`（MinIO、R2、阿里云 OSS、腾讯云 COS 这些兼容 S3 的服务都填自己的地址）、`prefix`、`path_style`；密钥用环境变量 `AGENTIM_S3_ACCESS_KEY_ID` / `AGENTIM_S3_SECRET_ACCESS_KEY`。签名用 `rusty-s3`（纯 Rust、只负责签名不发请求，0.10.2，2026-08-01），请求由服务端已有的 HTTP 客户端发。集群里多个节点必须用 S3，或者 `local` 加 `shared = true`（大家挂同一个网络盘）；都没有时照常运行，但上传返回 `conflict` 并说明要怎么配，启动时也打一条警告（不让已有的集群因为升级起不来）。用 S3 时启动先写一个探测对象再删掉，桶不可用就报错；本机文件夹在第一次写入时才建 |
| 存放路径 | `{prefix}/{conv_id}/{file_id}/original` 和 `…/thumb`。`file_id` 是随机 ID，文件名只在数据库里，不进路径 |
| 类型 | 只有 JPEG、PNG、WebP、GIF 算图片（`kind: "image"`），其余都是 `kind: "file"`。普通文件下载时带 `Content-Disposition: attachment`。本机存储由服务端输出，还带 `X-Content-Type-Options: nosniff` 和 `Content-Security-Policy: sandbox`；S3 的预签名地址里用 `response-content-disposition` 和 `response-content-type` 指定这两项。SVG 当普通文件（里面可以有脚本）。iPhone 的 HEIC 由客户端转成 JPEG 再传 |
| 缩略图 | **客户端生成**：长边 320 像素（聊天里约 160–240 点宽），先按 EXIF 方向转正。不透明的出 JPEG（质量 75），透明的出 PNG，一般 15–40 KB。原图长边不到 320 就不传缩略图，直接用原图。生成的位置：浏览器用 `createImageBitmap` 和 `OffscreenCanvas`；其他平台都在共用的 Rust 原生运行层里用 `image` 库（0.25.10，2026-03-10），Swift、Kotlin、C#、Dart、React Native 都不用自己写。图片解不开或超过 4000 万像素，就不生成缩略图，当成普通文件发。另外记下原图转正后的宽高（服务端也按 EXIF 方向算，方向 5–8 宽高互换），客户端下载前按比例留出位置，界面不跳动 |
| 位置信息 | 客户端上传前默认去掉原图里的 EXIF 和 XMP（含 GPS）：JPEG 的 APP1、PNG 的 `eXIf`/`iTXt`、WebP 的 `EXIF`/`XMP`。只删这些数据块、不重新编码，画质不变；方向信息先用掉再删。这段放在 Rust 核心里，所有平台共用。应用可以关掉（`send_file {keep_metadata: true}`），部署可以禁止关掉（`files.allow_metadata = false`，默认禁止）。服务端 SDK 和 Agent SDK 上传时也做同样的处理 |
| 下载 | `file_url {file_id, variant: "thumb" | "original"}`（客户端协议、开放 API、MCP 都有）每次都检查权限：上传者本人，或者引用它的消息你看得见；被移出后立刻不能再取。返回 5 分钟有效的地址：S3 是预签名 GET，流量不经过服务端；本机是服务端的 `/files/{file_id}/{variant}?exp=…&sig=…`，支持 `Range`。文件内容不会变，客户端按 `file_id` 一直缓存 |
| 静态加密 | S3：内容不经过服务端，用桶的服务端加密（SSE-S3 或 SSE-KMS）；解散会话、删除文件时删掉对象。本机文件夹：**还没做**应用层加密，先用磁盘加密（LUKS、BitLocker、云盘加密）。计划的做法：用会话的 DEK 按 64 KiB 分块 AES-256-GCM，AAD 是（文件 ID、档位、块序号、是否最后一块）；排在 [06 §2](06-plan.md) |
| 不丢 | 内容完整传上去、`upload_complete` 检查过，才提交 `files` 记录：记录提交了，内容一定在。没完成的上传 24 小时后清理：S3 上中止分片上传；文档建议桶加上"中止未完成的分片上传"的生命周期规则兜底 |
| 删除 | 以下情况文件会被扔掉：撤回消息、拒绝草稿、解散会话、删除账号（删他上传的）、消息过了保留期、文件过了保留期、草稿随 Agent 离开而消失。`files` 记录删除和 `file_trash` 记录在同一个事务里提交。原图和缩略图由后台删除，失败按 1 秒起翻倍、最长 1 小时重试，重启后继续 |
| 过期 | 完成后 24 小时内没被消息引用的文件会删掉。部署者设文件保留期（`files.retention_days`，默认不过期，和消息保留期分开）。过期后消息还在，显示"File expired" |
| 限制 | 单个文件默认最多 4 GiB（`files.max_bytes`，可以改大）。每人每天的上传总量默认不限（`files.daily_bytes_per_user` 可以设）。同时进行中的上传每人最多 10 个（`files.max_open_per_user`），超出 `rate_limited`。前面有代理时，用本机存储要调大它的请求体上限（例如 Nginx 的 `client_max_body_size`，按片大小 16 MiB 就够） |
| 其他 | Agent 上传的文件标记 `ai_generated: true`（EU AI Act 第 50 条）。会话迁到另一个库时，`files` 记录和 DEK 跟着会话走，存储是共用的，内容不用搬 |
| 以后 | 可选的病毒扫描钩子（接 ClamAV，扫描通过前不能引用）；视频的封面和时长 |

**客户端 SDK**（核心 `core/src/upload.rs`；原生运行层 `native/src/files.rs`）
- 应用发 `send_file {conv_id, path, name?, mime?, caption?, reply_to?, client_msg_id?, keep_metadata?}`（原生；浏览器传 `File`）。运行层先做完准备：读开头判断是不是图片；图片（256 MiB 以内，要整张读进内存）去掉 EXIF 和 XMP 写一份副本、按方向转正生成缩略图、记下转正后的宽高；其他文件直接用原路径。然后交给核心的 `send_file {conv_id, file: {source, name, size, mime?, width?, height?}, thumb?: {source, size, width, height}, caption?}`。
- 核心马上显示一条本地消息：`status: "uploading"`，`payload` 带名字、大小、宽高和本地来源（`local_source`、`local_thumb`，界面先用它显示），`upload: {sent, size}` 是进度。说明文字可以在发出前改（`edit` 改的是说明）。
- 一个文件就是离线队列里的一条发送，前面多一段上传：`upload_start` → 运行层按 `Put` 效果 PUT（每个文件最多同时 4 片，缩略图也是一次 `Put`），回报 `PutDone`（带 ETag）和 `PutProgress` → 全部传完 `upload_complete` → 按普通消息发出，种类以服务端判断为准（声称是图片但不是的按文件发）。传好的片和 ETag 跟着操作一起存下，重启后只传没传完的。
- **上传不挡住会话**：上传期间后面写的消息照常发出，文件消息在上传完成时排进发送顺序。Matrix 的 Rust SDK 把上传放进房间的顺序队列，上传期间后面的消息都要等；它的提案 MSC2246 把"大文件挡住后面所有消息"列为要解决的问题（[send_queue](https://docs.rs/matrix-sdk/0.18.0/matrix_sdk/send_queue/)、[MSC2246](https://github.com/tulir/matrix-spec-proposals/blob/9a395ed0ab19172cc77eaf648b52f8a8ff5a0ac7/proposals/2246-asynchronous-uploads.md)）。待负责人确认（[06 §1.1](06-plan.md)）。
- PUT 不需要 WebSocket：断线时进行中的 PUT 照常完成。失败处理：网络错误和 5xx 退避重试（和消息一样一直重试）；400/401/403/404/410（地址过期、上传已关）先重新要地址（`upload_parts`），快过期（剩 1 分钟）的地址也重新要；413（代理的请求体上限）和来源读不到直接失败；`upload_parts` 或 `upload_complete` 说 `not_found`（上传过期被清理）就从头再传，最多 3 次；`upload_parts` 说 `conflict`（其实已经完成、只是回答丢了）就再 `upload_complete`；`upload_complete` 说 `bad_request`（内容和声明的不符，服务端已扔掉）标记失败，`retry` 从头再传。
- 删掉（`discard`）或撤回还没发出的文件：核心发 `ReleaseUpload`，运行层停掉它的 PUT；上传完成时也发，运行层删掉自己做的副本和缩略图。启动时删掉 `uploads/` 里队列不再引用的文件。来源被删掉的标记失败。
- `file_url {conv_id, file_id, variant?}` 返回绝对地址（本机存储的相对地址按 `url` 补全），在有效期前 30 秒内重复问直接用缓存，离线时也能用缓存。
- 浏览器（还没做）：缩略图用 `createImageBitmap` 和 `OffscreenCanvas`，去位置信息用 wasm 里同一份 `media.rs`，文件放进 IndexedDB（单个超过 200 MiB 的不放，刷新后要重新选）。

**界面（Web 和 React Native 组件）**
- 图片：聊天里显示缩略图，按比例，最大 240×240 点，圆角，加载前是浅灰占位。点开后全屏显示放大的缩略图，底部（手机）或右上角（电脑）有"View original"，点了下载原图、加载时显示进度，下载完替换显示。还有"Download"和关闭：Esc、点背景、手机上往下滑都能关。连发的多张排成网格。
- 文件：一张卡片。
  - 左边是按类型的图标：PDF、文档、表格、演示、压缩包、音频、视频、代码、其他，按类型配色，图标是我们自己画的 SVG；
  - 中间是文件名（太长时省略中间、保留扩展名）和大小（`2.4 MB`）；
  - 右边是下载按钮。
- 发送：输入框左边有回形针按钮；网页上还能拖进来、粘贴截图；发之前可以写说明；上传时显示进度环，可以取消。

**测试要覆盖的逻辑节点**
- 上传开始：权限（成员、非成员、被移出、被拉黑、已解散、草稿模式的 Agent）；大小（上限、正好上限、空文件、每日总量）；未知会话；文件名；同时进行中的上限；重复的 `upload_start`。
- 上传过程：片的签名（过期、被改、换片号、换用户）；断点续传；片乱序到达；本机存储的 `Range` 和重写。
- 上传完成：大小不符、类型不符（改了扩展名的、SVG、坏图片）、缩略图不合格或太大、少片、重复完成。
- 客户端处理：缩略图的尺寸、方向转正、透明保留；去掉 EXIF 和 GPS 后图像字节不变。
- 发消息引用：别人的、别的会话的、没完成的、已被引用的、已过期的、两条消息抢一个文件。
- 读的路径：拉取、推送、侧信道、草稿（批准后才可见）、编辑（不能改附件）、撤回（下载地址立刻失效）、搜索（文件名）、Webhook、MCP、开放 API。
- 下载：成员、非成员、被移出、加入前的消息、签名过期、签名被改、换档位、S3 预签名地址。
- 生命周期：解散（本机加密销毁、S3 删除对象）、删除账号、保留期、24 小时清理、重启（没做完的删除继续）、多库迁移、审计。
- 配置：本机存储和 S3（CI 里用 MinIO）；静态加密开和关；集群启动检查；三种数据库。
- 并发：两条消息抢同一个文件、清理和引用、撤回和下载、解散和上传、同一文件重复完成。多轮后检查只有一个成功、存储里没有孤儿。
- 客户端 SDK：离线时选的文件、上传中断网、重启后继续、失败重试。
- 界面：截图测试加上图片、文件卡片和查看大图的层。

## 3. 可靠性与顺序

| 机制 | 说明 |
|---|---|
| 持久化后 ACK | 写事务提交后才回 ACK |
| 去重 | `(conv_id, sender_id, client_msg_id)` 与消息写在同一个事务；重试返回原 seq |
| 推拉结合 | 推送携带 seq；接不上就发 `resync`，客户端用 `sync` 补齐（[08](08-sync-and-client.md)） |
| 发送方顺序 | 同一连接的请求按到达顺序串行处理 |
| 流式 | 开始即占位（分配 seq）；增量不占 seq，按 `(msg_id, idx)` 幂等追加；每秒写一次检查点；结束时 `msg.updated` |
| 崩溃恢复 | 启动时把残留的 `streaming` 消息标为 `failed(server_restart)`，内容保留到最后一个检查点 |
| 未读数 | 只统计别人发的、未撤回的新消息，不受编辑和系统事件影响 |

已验证：`kill -9` 后已 ACK 的消息全部保留；并发压测中 seq 连续、每条消息恰好一次、推送有序。详见 [05 §5](05-operations.md)。

## 4. 写入路径与组提交

### 4.1 目标

每个写操作单独一个事务、一次 fsync 太慢；把同一时刻排队的命令合并进一个物理事务，一次 fsync，同时不放松"落盘后才 ACK"。

### 4.2 依据

做法和 PostgreSQL、MySQL 的领头者式组提交一致；SQLite 是单写者，所以在应用层用单个写线程合并。PostgreSQL 的 fsyncgate 说明 **fsync 失败后不能重试**，只能崩溃后从日志恢复（§4.4）。

### 4.3 方案：单写者 + 组提交 + 锁外扇出

```
 连接（按到达顺序）                写线程（每个数据库一个）
 ──cmd──▶ 有界命令队列 ──▶ ┌────────────────────────────────────────┐
                           │ 取出第 1 条，再取走队列里已有的命令      │
                           │ （最多 512 条或 1MB；默认不额外等待）    │
                           │ BEGIN                                  │
                           │   SAVEPOINT; cmd1; RELEASE（失败只回滚它）│
                           │   SAVEPOINT; cmd2; RELEASE              │
                           │   …                                     │
                           │ COMMIT  ← 整批一次 fsync                 │
                           └──────┬─────────────────────────────────┘
                                  │ 提交成功后：按顺序回 ACK + 已提交事件
                                  ▼
                          扇出任务：每个事件只序列化一次（共享字节）
                          → 每个连接一个有界发送队列
                          → 队列满：丢弃推送，发 resync 标记（客户端去同步）

 读请求 ──▶ 读连接池（不经过写线程）
 流式增量 ──▶ 直接进扇出（不落盘）；检查点作为普通写命令批量提交
```

**批量策略**：写线程取到第一条命令后，只拿走"此刻已经在队列里"的命令，不额外等待。低负载时延迟等于一次 fsync；高负载时一批自然变大，吞吐约等于"批大小 / fsync 延迟"。`commit_delay_us` 可选，默认 0。

### 4.4 正确性论证（G2 不丢、G3 有序、G4 正确）

| 风险 | 处理 |
|---|---|
| ACK 早于落盘 | 只有在 `COMMIT` 返回之后才回 ACK，并推送事件 |
| 一条命令失败拖垮整批 | 每条命令一个 SAVEPOINT，失败只回滚它自己，返回错误 |
| 回滚的命令占用了 seq，留下空洞 | seq 计数器存在数据库行里，在 SAVEPOINT 内递增，回滚会一起撤销；内存里的缓存值也随之回滚 |
| 批内重复的 `client_msg_id` | 命令按顺序执行，后一条能看到前一条写入的去重记录 |
| 整批提交失败（磁盘满、fsync 报错） | 整批命令都返回错误，客户端用同一个 `client_msg_id` 重试。fsync 报错视为致命：停止接受写入，`/readyz` 失败，进程退出（退出码 70）后由 systemd 拉起，再从 WAL 恢复 |
| 同一连接的消息乱序 | 每个连接串行把命令放进队列；写线程按 FIFO 执行；ACK 按顺序返回 |
| 推送乱序 | 扇出任务按提交顺序处理事件；每个连接的发送队列是 FIFO |
| 慢客户端拖垮内存（G1） | 发送队列有界；满了就丢弃推送，发 `resync` |
| 写入过载（G1） | 命令队列有界；满了就返回 `overloaded`，客户端退避后重试（幂等） |
| 被拒绝的请求，有些记录必须保留（熔断事件、DLP 拦截的审计、流式过程中的内容替换） | 命令可以标记"出错也提交"：返回错误，但它的写入随批次一起提交 |
| 迁移切换前按旧路由排队的写请求在切换后才执行，写进即将删除的旧副本 | 写线程执行任何会话操作前，先确认会话当前归属本库，否则返回 `conv_moved`，由 hub 按新目录重新路由 |

运行时状态（流式缓冲、护栏计数、成员缓存）由各自的写线程独占；在线连接注册表单独加锁，只在提交后的扇出时使用。

### 4.5 不采用的方案

`synchronous=NORMAL` 和"先 ACK 后 fsync"都会丢已 ACK 的数据（违反 G2）；多线程同时写 SQLite 只会带来锁竞争；固定等待 N 毫秒再提交只会增加低负载延迟（作为可选参数，默认关闭）。

### 4.6 与 PostgreSQL / MySQL、集群的关系

- **PostgreSQL / MySQL**：数据库本身也做组提交，我们的批量还能减少网络往返。它们支持并发写，以后可以按会话哈希开 N 个写线程（§4.8 C）；SQLite 固定为 1 个。
- **集群**：每个节点的写线程负责它持有租约的分片，每批写入在同一事务里校验租约（[11](11-cluster.md)），命令接口不变。

### 4.7 验收结果

崩溃恢复（`kill -9`，Linux 和 Windows）、并发一致性、故障注入（提交失败时不发 ACK、不推送、该库停止写入、`/readyz` 失败、重启后恢复）在 SQLite、PostgreSQL、MariaDB / MySQL 上全部通过；并发发送时提交次数明显少于命令数。服务端和客户端都开启 `TCP_NODELAY`。性能数字见根目录 README（`server/bench/single-node.sh`）。

### 4.8 剩余瓶颈与下一步（计划）

fsync 已经不是瓶颈（本机约 0.1ms，平均每批约 15 条命令）。瓶颈是**每条 SQL 语句的固定开销**：通过 sqlx 访问时 SQLite 每条约 31µs（驱动在独立线程上执行，每条都要跨线程往返），PostgreSQL 约 100µs，MySQL 约 66µs；一次发送约 9 条语句加 SAVEPOINT，每条命令在写线程里约 0.7ms，单库上限约每秒 1,300 条。目标是每节点 1 万条/秒。

| 方案 | 预期收益 | 代价 |
|---|---|---|
| A. 减少每条命令的语句数（`RETURNING` 取 seq、人类消息跳过因果推断查询、合并插入） | 约 1.3–1.5 倍 | 小 |
| B. SQLite 写线程改用原生驱动（rusqlite） | 约 5–10 倍（单 SQLite 库） | 三种数据库不再共用同一套写入代码 |
| C. PostgreSQL / MySQL 按会话哈希开多个写线程 | 随 CPU 核数增长 | 中；同一会话固定在同一个写线程，顺序不变 |
| D. 扇出移出写线程，交给独立任务 | 降低大群场景的 ACK 延迟 | 中 |

建议顺序：先 A 和 D，生产推荐 PostgreSQL 并配合 C；B 等有明确的单机 SQLite 高吞吐需求时再做。

## 5. 存储抽象

### 5.1 目标

SQLite 用于单机和小规模部署，PostgreSQL 推荐用于生产，MySQL / MariaDB 也支持。三者语义完全一致。

### 5.2 选型

sqlx（异步、纯 Rust、直接写 SQL）。没有用 ORM 或 sea-query：业务 SQL 都很简单，方言差异只有占位符（PostgreSQL 用 `$n`）、列类型（MySQL 用 `LONGTEXT`，主键列用 `VARCHAR`）和建索引写法（MySQL 在建表语句里声明），由一个很小的方言层处理；需要 upsert 的地方在写线程里"先查后写"（写线程唯一，没有竞争）。

### 5.3 方案

```
hub（业务逻辑） ──▶ 领域接口（不出现 SQL）
                     ├─ 写：begin_batch() → WriteTx
                     │      next_seq / append_event / insert_message / update_message /
                     │      append_side / dedup / drafts / takeovers / audit / savepoint / commit
                     └─ 读：pull / pull_side / members / convs_for / unread / ...
                            │
                 SqlStorage（sqlx 连接池 + 方言层）
                     ├─ SQLite（默认，单二进制内嵌）
                     ├─ PostgreSQL（生产推荐）
                     └─ MySQL / MariaDB（InnoDB、utf8mb4）
```

- **代码分层**：`storage.rs`（方言、连接、建表、持久化检查）和 `db.rs`（全部业务 SQL）。业务逻辑（`logic.rs`）只调用 `db.rs`，看不到 SQL。
- **持久化配置检查**（G2）：启动时检查。SQLite 要求 `synchronous=FULL`；PostgreSQL 要求 `fsync=on` 且 `synchronous_commit` 不为 `off`；MySQL 要求 `innodb_flush_log_at_trx_commit=1`，开启 binlog 时还要求 `sync_binlog=1`。不满足就拒绝启动；`unsafe_durability = true` 可以显式放行并打印警告。
- **SQLite `secure_delete`**：删除的内容被覆盖，不留在空闲页里；会话迁移到专属库之后，旧库文件里不残留它的内容（测试直接读数据库文件确认）。
- **一致性测试**：同一套验收测试分别跑三种后端（`tests/hub.rs`）。CI 在 Linux 上用服务容器跑 PostgreSQL 和 MySQL，并检查没有测试被跳过；SQLite 在 Linux 和 Windows 上都跑。
- **加密字段**：见 [04 §2.6](04-security-compliance.md)。
- **迁移工具**（计划）：SQLite → PostgreSQL 的导出导入。

**表结构版本与自动迁移**

| 方面 | 规则 |
|---|---|
| 版本 | `meta` 表记录 `schema_version`（库的版本）和 `schema_compat`（还能使用这个库的最老程序对应的版本）。程序里的 `TABLES` 描述最新表结构，`MIGRATIONS` 按版本列出每一步的变化 |
| 新库 | 直接按最新结构建表，写入最新版本，不重放迁移 |
| 老库 | 启动时按顺序执行缺少的迁移，每做完一个就更新版本号。每个库（控制库和各单元库）各自迁移 |
| 可重入 | 每一步先检查再执行（例如加列前先查列是否存在）。MySQL 的 DDL 会隐式提交，所以中途崩溃后重启必须能接着做完 |
| 并发启动 | 多个节点同时启动时只能有一个在迁移：PostgreSQL 用事务级 advisory lock，MySQL 用 `GET_LOCK`，SQLite 用 `BEGIN IMMEDIATE` 的写锁 |
| 只做加法 | 主版本内的迁移只新增表、可为空的列和索引，老程序在新库上照样能读写，不提高 `schema_compat`，滚动升级时新老节点可以同时在线 |
| 破坏性变化 | 删除或改名列、改类型、改含义时提高 `schema_compat`；老程序发现库的 `schema_compat` 比自己新就拒绝启动（`schema_mismatch`）。分两步发布：先发能同时处理新旧结构的版本，再发真正的变化 |
| 启动检查 | 库的版本比程序新、但 `schema_compat` 不比程序新：照常启动并打印警告；否则拒绝启动 |

### 5.4 多库路由：组织单元的物理隔离（高级配置）

免费的高级配置，没有授权开关（D101）；大多数部署用不到。某个组织单元（[04 §5](04-security-compliance.md)）的数据要求和其他单元物理分开存放时使用。

```
                 ┌─ 控制库：租户、单元、用户、Agent、
                 │          会话目录（conv_id → 所属单元 → 所在数据库）、成员索引
                 │          （只有 ID 和关系，没有消息内容和会话标题）
agentim 进程 ────┼─ 数据库 shared：普通单元的会话          ← 写线程 1
                 ├─ 数据库 unit-b：单元 B 的全部会话        ← 写线程 2（可以在另一台服务器或另一个地区）
                 └─ 数据库 unit-c：……                     ← 写线程 3
```

**规则**

1. **一个会话只属于一个数据库**，由创建时的所属单元决定。该会话的全部事件（seq、元数据、正文、侧信道、草稿、审计里的内容快照）都写进这个库，由这个库的写线程按 §4 写入。
2. **不拆分单条消息。** 元数据和正文写进同一个库的同一个事务；拆开需要跨库原子写入，否则会出现"有 seq 没内容"；元数据本身也敏感。
3. **跨库查询走控制库。** 会话列表、未读汇总先查控制库的会话目录和成员索引，再并发访问各个数据库，在服务端合并。
4. **跨单元的会话**：属于发起人的单元，存放在该单元的库里，其他单元的成员可以加入。严格单元的成员加入宽松单元的会话时，按策略二选一：禁止，或者把会话迁移到更严格的库。
5. **迁移**：单元改用另一个库时按会话逐个迁移：会话先只读，复制数据并校验内容哈希，再切换控制库里的目录，最后删除旧副本。迁移期间这个会话的写入返回 `conv_migrating`（客户端用同一个 `client_msg_id` 重试），其他会话不受影响。
   - 用法：修改配置把单元指向新库，然后运行 `agentim migrate-unit --config agentim.toml --unit <名称>`，要求先停服务。在线迁移已在库里实现并有测试，但还没有管理接口。
   - 崩溃安全：目录只在新副本校验通过后才切换。任何一步中断，重启时的一致性修复会把目录指回完整的副本，并删除残留的另一份。
6. **独立的运维边界**：每个库有各自的连接配置、备份策略和数据库账号。共享库的数据库管理员没有专属库的访问权限。
7. **故障隔离**：一个库不可用时，只有它上面的会话写入失败（客户端重试，去重保证不重复），其他库照常。`/readyz` 和指标按库分别报告。

**对核心保证的影响**：G2 / G3 不变（一个会话只在一个库、一个写线程，没有跨库事务）；G4 读写先查控制库目录，目录与数据库不一致时以目录为准并告警；G1、G5 增强（故障隔离、每库一个写线程并行）；G6 一个库相当于一组专属分片。

不采用"元数据放共享库、正文放另一个内容服务"（需要跨服务原子写入），也不用"同库多 schema"做物理隔离（备份、账号、物理位置都分不开）。

## 6. 大群模式（计划）

消息体一律读扩散（会话日志只存一份），只对"会话有更新"这个信号做有限写扩散，在线推送在节点层聚合。

| | 普通群（≤ 2000 人） | 大群（2000–10 万人） |
|---|---|---|
| 用户同步信号 | 写入成员的"用户收件箱"，同一会话合并为一条 | 不写；成员本地标记为大群 |
| 重连补齐 | 读自己的收件箱 | 批量查询所在大群的 `max_seq` |
| 在线推送 | 节点聚合，逐条推送 | 节点聚合，按时间窗口（如 200ms）合并 seq 通知 |
| 流式增量 | 只推给正在看该会话的人 | 同左 |
| 成员事件 | — | 一次拉很多人合并成一条事件 |

用户收件箱放在同一个数据库里，按 `user_id` 分片，可以从会话提交事件重放重建。

## 7. 集群

已实现，完整设计见 [11 集群](11-cluster.md)。要点：免费；除数据库外不依赖任何组件；节点平等，会话按固定数量的虚拟分片由数据库租约决定归属（没有选举），每批写入在同一事务里校验租约代号；节点之间全互连，只传转发请求和通知，不传数据；数据库有异步从库时等从库确认后才回复成功。

## 8. 对核心保证的影响

每个设计都附一张"对 G1–G6 的影响"表。写入路径的现状：

| 机制 | G1 稳定 | G2 不丢 | G3 有序 | G4 正确 | G5 并发 | G6 集群 |
|---|---|---|---|---|---|---|
| §4 组提交 | 有界队列和背压；fsync 失败快速失败 | 落盘后才 ACK；故障注入测试 | 单写者 FIFO；SAVEPOINT 不留空洞 | = | 一次 fsync 服务整批 | 写线程就是分片的写入归属者 |
| §5 存储抽象 | 一致性测试；启动时检查持久化配置 | 拒绝不安全的数据库配置 | 三种后端语义一致 | = | PostgreSQL / MySQL 可以开多个写线程 | 存储接口与分片边界对齐 |
| 锁外扇出 | 慢客户端隔离 | 推送只是提示 | 按提交顺序 | = | 每个事件只序列化一次 | = |
