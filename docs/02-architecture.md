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

### 2.7 文件和图片（设计，待确认，2026-10-27）

负责人定的方向（2026-10-20、2026-10-27）：接业内标准的存储，部署者选**本机文件夹**或 **S3 兼容的对象存储**；图片发出后在聊天里显示一张很小的缩略图，点开看较大的预览图，再点"查看原图"下载原图；普通文件显示类型图标。原来的 D27–D30、D72（内容存进数据库）作废。

**业内对照**
- Mattermost：存储选 `local`（默认 `./data/`）或 `amazons3`（桶、前缀、区域、终端地址、密钥或 IAM 角色、只用 HTTPS、签名 v4），改了要重启；单个文件默认最大 100 MiB；图片上传时生成 120×100 的缩略图、宽 1920 的预览图和 16×16 的迷你图，PNG 出 PNG、其余出 JPEG（质量 90），先按 EXIF 方向转正；超过最大分辨率的图片拒绝；SVG 不生成；路径 `日期/teams/…/channels/…/users/…/文件ID/文件名`，缩略图和预览图放在原图旁边（[设置文档](https://docs.mattermost.com/administration-guide/configure/environment-configuration-settings.html)、[`server/channels/app/file.go`](https://github.com/mattermost/mattermost/blob/master/server/channels/app/file.go)）。他们解释预览图缩小是为了省流量、加载快，原图要下载才看得到（[论坛](https://forum.mattermost.com/t/show-pictures-in-original-size/4996)）。
- Slack：文件对象带 `thumb_64` 到 `thumb_1024` 八档缩略图（原图够大才有大的），`url_private` 和 `url_private_download`（强制下载）都要带令牌才能取（[文件对象](https://docs.slack.dev/reference/objects/file-object)）。
- S3 预签名地址：服务端签一个短时有效的地址，客户端直接传到对象存储或从那里下载，不经过 IM 服务器，存储的密钥不给客户端（[MinIO 文档](https://docs.min.io/community/minio-object-store/integrations/presigned-put-upload-via-browser.html)）。
- 没找到：OpenIM 上传设计的官方文档（之前记的"SDK 直传对象存储"出自它的源码，这次没有再核对）。

| 方面 | 做法 |
|---|---|
| 存储配置 | `[files] storage = "local"`（默认，`dir = "./data/files"`）或 `"s3"`（`bucket`、`region`、`endpoint`、`prefix`、`path_style`；密钥用环境变量 `AGENTIM_S3_ACCESS_KEY_ID` / `AGENTIM_S3_SECRET_ACCESS_KEY` 或机器角色）。用 `object_store` 库（Apache Arrow 项目，同时支持本机文件夹和 S3，0.14.2，2026-09-15 发布）。集群里多个节点必须用 S3，或者 `local` 加上 `shared = true`（大家挂同一个网络盘），否则启动时报错。启动时写一个探测对象再删掉，存储不可用就报错 |
| 存放路径 | `{prefix}/{conv_id}/{file_id}/original`、`…/thumb`、`…/preview`。`file_id` 是随机 ID，不含文件名（文件名只在数据库里），文件名里的怪字符不会进路径 |
| 上传 | `POST /api/files?conv_id=…`（客户端令牌放在 `Authorization` 头），开放 API `POST /v1/files`（`files:write`，以某个用户的身份），Agent 用 MCP 的 `upload_file` 或 Agent SDK 的 `uploadFile`。请求体就是文件内容，`Content-Type` 和 `X-File-Name` 只当参考。服务端流式写进存储，同时算 SHA-256。只有能在这个会话发言的人能上传（被移出、被拉黑、已解散、草稿模式的 Agent 都和发消息一样拒绝）。返回 `{file_id, name, size, mime, kind, width?, height?}` |
| 类型判断 | 看文件头，不信客户端声明。只有 JPEG、PNG、WebP、GIF 算图片（`kind: "image"`）；其余都是 `kind: "file"`，下载时带 `Content-Disposition: attachment`、`X-Content-Type-Options: nosniff`、`Content-Security-Policy: sandbox`。SVG 当普通文件（里面可以有脚本）。iPhone 的 HEIC 由客户端转成 JPEG 再传（iOS 选图时可以直接要 JPEG），服务端不解 HEIC |
| 缩略图和预览图 | 上传时生成，和原图一起放在存储里：**缩略图**长边 480 像素（聊天里约 240 点宽，2 倍屏上清楚），**预览图**长边 1920 像素（点开后看的，和 Mattermost 一样）。原图比这小就不生成，直接用原图。PNG 出 PNG（保留透明），其余出 JPEG（质量 82），先按 EXIF 方向转正。动图 GIF 的缩略图取第一帧，预览就是原图。在专门的线程池里做（不占写线程，同时最多 CPU 核数个）；超过 4000 万像素或解不开就不生成，文件照常能发，显示成普通文件的样子。另外记下宽高和主色：客户端在图片下载完之前按比例留出位置、填上主色，界面不跳动 |
| 位置信息 | 默认去掉原图里的 EXIF、XMP（含 GPS）。只删这些数据块、不重新编码，画质不变（JPEG 的 APP1、PNG 的 `eXIf`/`iTXt`、WebP 的 `EXIF`/`XMP`）；方向信息先用掉再删。部署可以关掉（`files.strip_metadata = false`） |
| 发消息 | 先上传，再发 `kind = "image"` 或 `"file"` 的消息，payload 里带 `file_id`（可以加 `caption`），服务端补上名字、大小、类型、宽高、主色、有没有缩略图；降级文本是 `[Image]` / `[File] 名字`。一个文件只能被一条消息引用，两条消息抢同一个文件只有一条成功（`conflict`）；别人的文件、别的会话的文件 `forbidden`，不存在或过期 `not_found` |
| 下载 | `file_url {file_id, variant: "thumb" | "preview" | "original"}`（客户端协议、开放 API、MCP 都有）每次都检查权限：上传者本人，或者引用它的消息你看得见；被移出后立刻不能再取。返回 5 分钟有效的地址：本机存储是服务端自己的 `/files/{file_id}/{variant}?exp=…&sig=…`（HMAC，绑定用户、文件和档位，支持 `Range`）；S3 且没开静态加密时直接给 S3 的预签名地址（流量不经过服务端，`response-content-disposition` 带上文件名）；开了静态加密时也走服务端（边解密边输出）。文件内容不会变，响应带 `Cache-Control: private, max-age=31536000, immutable`，客户端可以一直缓存 |
| 静态加密 | 开启时文件、缩略图、预览图都加密后再存：用会话的 DEK，按 64 KiB 分块 AES-256-GCM，每块的 AAD 是（文件 ID、档位、块序号、是否最后一块），块不能被调换、截断或拼接。解散会话删掉 DEK 后，存储里剩下的也解不开 |
| 不丢 | 内容完整写进存储之后，才通过写线程提交 `files` 记录；记录提交了，内容一定在。提交前失败留下的残留由后台清理（按存储里的对象和记录对账） |
| 删除 | 撤回消息、解散会话、删除账号、过了保留期：`files` 记录和"待删除"任务在同一个事务里提交，内容（三个档位）由后台删除，失败重试，重启后继续 |
| 过期 | 24 小时内没被消息引用的上传会删掉。部署者设文件保留期（`files.retention_days`，默认不过期，和消息保留期分开，和 Mattermost 一样）。文件过期后消息还在，显示"文件已过期" |
| 限制 | 单个文件默认最多 100 MiB（`files.max_bytes`），每人每天的上传总量（`files.daily_bytes_per_user`，默认 1 GiB）和 Agent 每分钟的上传次数有上限，超出 `rate_limited`；空文件 `bad_request`；文件名最长 255 字节，去掉路径和控制字符。前面有代理时要一起调大它的请求体上限（例如 Nginx 的 `client_max_body_size`） |
| 其他 | Agent 上传的文件标记 `ai_generated: true`（EU AI Act 第 50 条）。会话迁到另一个库时，`files` 记录和 DEK 随会话走（存储是共用的，内容不用搬）。可选的扫描钩子（和发送前拦截一样的 HTTP 回调，可以接 ClamAV）：开了之后扫描通过前不能被引用，扫描出错默认拒绝 |
| 以后 | 超过 100 MiB 的大文件让客户端用 S3 预签名地址直传（分片上传，断点续传）；视频的封面和时长 |

**客户端 SDK**：`send_file {conv_id, file, caption?}` 先显示一条本地消息（状态"上传中"，带进度），上传完成再发消息；上传失败和发送失败一样标记失败，可以重试或删掉。原生平台记下文件路径，重启后继续；浏览器把要传的文件放进 IndexedDB（单个超过 50 MiB 的不放，刷新后要重新选）。缩略图和预览图按 `file_id` 缓存。客户端核心不做网络请求，上传和下载由各平台的运行层做：核心发出 `upload` 效果，运行层回报进度和结果。

**界面（Web 和 React Native 组件）**
- 图片：聊天里显示缩略图，最大 240×320 点，按比例，圆角，加载前是主色的占位。点开后全屏看预览图（加载时先显示放大的缩略图），右上角有"View original"（取原图：浏览器在新标签打开，手机上保存或分享）、"Download"和关闭（Esc、点背景、手机上下滑）。连发的多张排成网格。
- 文件：一张卡片。左边是按类型的图标（PDF、文档、表格、演示、压缩包、音频、视频、代码、其他），用 Slack 和 Google Drive 那种按类型配色的风格，图标是我们自己画的 SVG，不用第三方图标库；中间是文件名（太长时省略中间、保留扩展名）和大小（`2.4 MB`）；右边是下载按钮。
- 发送：输入框左边有回形针按钮，网页上还能拖进来、粘贴截图；发之前可以写说明；上传时显示进度环，可以取消。

**测试要覆盖的逻辑节点**：
- 上传：权限（成员、非成员、被移出、被拉黑、已解散、草稿模式的 Agent）；大小（上限、正好上限、空文件、每日总量）；未知会话；类型（真图片、改了扩展名的、SVG、坏图片、超大分辨率、动图、带 EXIF 和 GPS 的）。
- 缩略图和预览图：尺寸、方向转正、比原图小才生成、PNG 透明保留。
- 发消息引用：别人的、别的会话的、已被引用、已过期、扫描未通过、两条消息抢一个文件。
- 读的路径：拉取、推送、侧信道、草稿（批准后才可见）、编辑（不能改附件）、撤回（下载地址立刻失效）。
- 下载：成员、非成员、被移出、加入前的消息、签名过期、签名被改、换档位、`Range` 边界、S3 预签名地址。
- 生命周期：解散（加密销毁）、删除账号、保留期、重启（没做完的删除继续）、多库迁移、审计。
- 入口和配置：开放 API 和 MCP 上传；静态加密开和关；本机存储和 S3（CI 里用 MinIO）；集群启动检查；三种数据库。
- 并发：两条消息抢同一个文件、清理和引用、撤回和下载、解散和上传。多轮后检查只有一个成功、存储里没有孤儿。
- 客户端：离线时选的文件、上传中断网、重启后继续、上传失败重试。
- 界面：截图测试加上图片、文件卡片和预览层。

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
