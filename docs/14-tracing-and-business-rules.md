# 14 消息链路染色与业务规则

> 2026-10-29 负责人提出：做好 IM 链路的消息染色，让后续可以跟踪，特别是分布式（集群）场景；把所有业务逻辑梳理清晰，确保消息符合业务。2026-10-31 负责人确认 D138–D143（[06 §1.2](06-plan.md)），按 §6 的步骤实现。

## 1. 要解决的问题

| 问题 | 现在 | 目标 |
|---|---|---|
| 一条消息经过哪些地方 | 日志带 `conn_id`、`user_id`、`op`、`rid`，没有跨节点、跨环节的同一个编号；集群里转发到另一个节点后就接不上 | 每个请求一个 **trace_id**，从客户端、各个入口、写线程、提交、推送、别的节点、Webhook、Agent 的回复一路带着；任何一个环节的日志都能按它查到整条链路 |
| 某个人、某个会话"消息丢了""收不到" | 只能翻日志猜 | **染色**：运营者给某个用户、设备、会话、Agent 或 API 密钥打上标记，之后它们的请求在所有节点都完整记录（不受采样影响），并且有**消息轨迹**：输入消息 ID，看到它在哪一步、被谁、为什么处理成这样 |
| 被拒绝的请求为什么被拒绝 | 返回错误码和一句话，日志里没有是哪一条规则 | 每一条业务检查有固定的**规则编号**，拒绝时记在链路和指标里（不写进消息日志，符合"被拒绝的请求不留痕迹"） |
| 业务规则散在代码里 | 389 处检查分在 30 多个文件（§5.1 的统计） | 一份**规则目录**，从代码生成、CI 检查不过期；一个**业务校验器**，回放日志检查不变量，证明已经提交的消息都符合业务 |

## 2. 业界做法（调研）

- **W3C Trace Context**（[规范](https://www.w3.org/TR/trace-context/)）：`traceparent = 版本-trace_id(16 字节)-parent_id(8 字节)-flags`，`flags` 的最低位是"已采样"。值不合法时必须忽略并新开一条链路；字段里不能放个人信息或敏感信息，只用于关联；不应把链路头返回给链路之外的调用方；在信任边界可以重新开始链路（规范允许，但建议尽量少做）。这是 OpenTelemetry 默认的传播格式。
- **OpenTelemetry 消息系统约定**（[messaging spans](https://opentelemetry.io/docs/specs/semconv/messaging/messaging-spans/)，仍是实验状态）：生产者给每条消息一个"创建上下文"；批量处理时，处理的 span 用 **span link** 链接到每条消息的创建上下文，而不是把它们当父子关系。我们的写线程是组提交（一批几十到几百条一次提交，[02 §4](02-architecture.md)），正好是这种情况。
- **WebSocket**：没有标准的逐帧传播方式（只有握手是 HTTP，可以带头），各家都是在消息体里自己带（调研里看到的是厂商博客，[uptrace](https://uptrace.dev/opentelemetry/context-propagation)、[openobserve](https://openobserve.ai/blog/opentelemetry-context-propagation/)，没有找到标准）。
- **Matrix Synapse**（[OpenTracing 文档](https://element-hq.github.io/synapse/latest/opentracing.html)）：默认关闭；只把链路上下文传给白名单里的服务器；发送方标记"已采样"时接收方也会采样，所以多台服务器的采样策略会互相放大；不发送 baggage，但收到的 baggage 会记进日志。可借鉴的是"只在信任范围内传播"和"采样标志要能被接收方不理会"。文档里**没有找到**按用户强制追踪的选项。
- **RocketMQ 消息轨迹**（[官方说明](https://apache.googlesource.com/rocketmq/+/refs/heads/bazel/docs/en/Operations_Trace.md)）：按消息记录"发送—存储—消费"各环节，存到系统内置 Topic `RMQ_SYS_TRACE_TOPIC`（Broker 打开 `traceTopicEnable`），用 `mqadmin QueryMsgTraceById` 按消息 ID 查。这就是 §4 "消息轨迹"的原型：不依赖外部链路系统，按消息 ID 就能查。
- **"染色"的国内说法**（腾讯 TARS 的染色日志、各家的全链路灰度"流量染色"）：给特定用户或请求打标记，标记跟着调用链往下传，带标记的请求在每一跳都完整打日志。调研时**没有找到** TARS 的公开文档可以引用，这里只按通行的含义设计。

## 3. 链路全景：一条消息经过的环节

```
 客户端 SDK（核心生成 trace，跟着离线队列存下，重试同一个 trace）
   │ WebSocket 帧 {"op":"send", …, "trace":"00-…"}；HTTP 头 traceparent
 ① 入口：WS 客户端 / WS Agent / 开放 API / MCP / Agent 文件接口 / 管理 API / CLI
 ② 认证：令牌、签名密钥、API 密钥、Agent 密钥；失败计数和封禁（D130）
 ③ 会话层检查（hub.dispatch）：Agent 被停 → 只给人用的操作 → 应用的客户端权限 → 每人发送频率
 ④ before_send Webhook（可选）：成员检查、去重检查后调用，可以替换内容或拒绝
 ⑤ 路由：会话 → 数据库 → 分片；集群里分片不在本节点时转发到持有节点（ForwardReq）
 ⑥ 写线程排队（满了 overloaded）→ 组提交批次，每条一个保存点
 ⑦ 业务检查（logic，按顺序，见 §5.2）→ 写日志、去重、文件引用、草稿/审批
 ⑧ 提交（一次 fsync）→ 回复发送者（ACK 在提交之后）
 ⑨ 扇出：本节点的连接（推送或 resync）、侧信道、别的节点（集群发布）、实时增量
 ⑩ 接收方：推送 → 存入缓存 → ack（不存不 ack）；离线的下次 sync
 ⑪ 发件箱 → Webhook 投递（重试、丢弃）
 ⑫ Agent 被唤醒 → 回复（causation_id 指回触发它的消息）→ 又从 ① 开始
 ⑬ 后台：保留期清理、文件删除、草稿过期、会话迁移
```

每个环节都有会让消息"没到"的原因：② 令牌过期，③ 权限或限流，④ Webhook 拒绝，⑤ 会话正在迁移，⑥ 过载，⑦ 被禁言、被拉黑、策略拦截、不是成员，⑨ 推送队列满改为 resync，⑩ 客户端没 ack，⑪ Webhook 失败，⑫ Agent 熔断或被停。染色就是要让这些"在哪一步、为什么"一眼看出来。

## 4. 染色设计

### 4.1 链路编号（所有请求，默认开）

| 环节 | 怎么带 |
|---|---|
| 客户端 SDK | 核心给每个离线队列操作生成 `traceparent`（随机，和设备、用户无关），跟操作一起存下；重试、重启后续传都用同一个 trace_id，每次尝试一个新的 parent_id。帧里多一个可选字段 `trace` |
| HTTP 入口（开放 API、MCP、文件、管理 API） | 读 `traceparent` 头；没有或不合法就新开（按规范） |
| **信任边界**（D139） | 来自终端用户客户端的链路：服务端**另开自己的链路**，把客户端的 trace_id 记成属性 `client.trace_id` 和一个 span link，**不当父节点，也不理会它的"已采样"标志**（否则任何人都能让服务端为他全量记录，和 Synapse 文档提到的采样放大一样）。开发者后端（API 密钥）和 Agent（Agent 密钥）是可信的，直接接上 |
| 服务端内部 | `Op` 带上链路上下文（新字段，可序列化）；写线程的批次 span 用 link 指向批里每条命令（OTel 消息约定的批量做法）；集群转发 `ForwardReq` 和集群发布带上下文 |
| 存储 | 消息行多一列 `trace_id`（16 字节，可空），消息轨迹按它查；不存进事件的数据里、不推给别人 |
| 推送给人 | **不带**对方的 trace_id（规范建议不把链路头给链路外的人；也没有用处） |
| 推送给 Agent | 带 `trace`（触发它的消息的链路），Agent SDK 的 `ctx.trace` 可以接着传给模型调用和自己的服务；Agent 回复时带回来，回复的链路 link 到触发消息（加上已有的 `causation_id`），"用户消息 → Agent 被唤醒 → 调模型 → 回复"是一张图 |
| Webhook | 投递请求带 `traceparent` 头（`webhooks.trace = true`，默认开；不带 `tracestate`、不带 baggage） |
| 日志 | JSON 日志每行带 `trace_id`、`span_id`；按 trace_id 在任何日志系统里都能把多个节点的日志拼起来 |
| 导出（可选） | `[tracing] otlp_endpoint`、`sample_ratio`（默认 0，只有日志里的编号，不导出 span）；用 OpenTelemetry 的 OTLP 导出到 Jaeger、Tempo、Datadog 等 |

### 4.2 染色（按对象打标记，D140）

- 运营者在管理 API 或控制台加染色规则：对象是 `user`、`device`、`conv`、`agent`、`api_key` 之一；必须填原因；有效期最长 24 小时（默认 1 小时）；写进审计日志；数量有上限（默认 100 条）。规则存在控制库里，所有节点几秒内生效（复用 D130 的节点通知）。
- 命中规则的请求：链路标为已采样（不管 `sample_ratio`），在所有节点上这条链路的日志从 DEBUG 级别开始记录（按链路调日志级别，别的请求不受影响）；`tracestate` 里加 `agentim=dye:<规则编号>`，转发到别的节点时跟着走；由它引起的后续工作（扇出、Webhook、Agent 被唤醒后的回复）**继承染色**。
- 染色只在服务端内部传播：不传给 Webhook 接收方、不推给客户端（规则编号是内部信息）。
- 隐私：染色记录的是元数据（编号、环节、耗时、结果、规则编号），**和平时一样不记消息内容、令牌、耳语正文**（[05 §2](05-operations.md)）。染色规则本身是对一个人的特别关注，所以要原因、审计、自动过期，符合 GDPR 的目的限制和最少必要。

### 4.3 消息轨迹（不依赖链路系统，D141）

`GET /admin/v1/messages/{msg_id}/trace`（控制台"消息轨迹"页，读会话内容时每条消息旁有 trace 链接；只给 owner、operator、moderator 角色，每次查看审计为 `admin.messages.trace`）。从已经存在的持久数据拼出来，所以没有开导出、没有染色也能用；只有元数据，不含内容：

| 环节 | 来源（已实现） |
|---|---|
| 接收 | `trace_id`（消息行）、`client_msg_id`（去重记录，去重窗口内） |
| 提交 | 数据库、seq、提交时间、状态和版本、`causation_id` |
| 可见的人 | 现在能看到它的成员（`visible_from` ≤ seq，含之后加入但共享了历史的人）加上之后被移出、当时看得到的人（`past_members`），按人和 Agent 计数；挂在这个 seq 上的侧信道事件数 |
| 送达 | 这些人的每台设备（令牌表里的设备，加上令牌已删但有同步位置的设备）的确认位置是否过了这个 seq（`device_cursors`，[08](08-sync-and-client.md)）：已送达 N 台、未送达 M 台，未送达的逐台列出位置和最后在线时间（最多 100 台） |
| Webhook | 这条消息的每个事件 × 每个订阅了它的 Webhook：发件箱还有这行且该 Webhook 的位置没过它 → `pending`（带前面还排着几条）；放弃的有审计记录 `webhook.dropped`（现在带会话和 seq）→ `dropped`；其余 → `delivered`；发件箱已清掉而消息被撤回过，创建事件可能是被撤回收回的（D91）→ `delivered_or_withdrawn` |
| Agent | 能看到它的 Agent（触发方式、是否被停、设备送达情况），`causation_id` 指向它的回复和待审批草稿 |
| 之后 | 事件历史（创建、修改、撤回……谁、什么时候）、举报（控制库）、引用的文件状态、保留期到期时间 |

没有存下来、所以不显示的：哪个入口、哪个节点接的请求，before_send 是否改写过——按 `trace_id` 查日志。熔断状态只在内存里，也不显示。

配置了 `[tracing] trace_ui_url`（例如 `https://jaeger.example.com/trace/{trace_id}`）时附上链路的链接。

### 4.4 被拒绝的请求

不写消息日志（CLAUDE.md 的规则不变），但：链路 span 记 `error.code` 和**规则编号**（§5.1）；指标 `agentim_refusals_total{op, code, rule}`；染色的对象在 DEBUG 日志里能看到是哪一条规则拒绝的、当时的参数（不含内容）。

### 4.5 开销

默认配置下每个请求多：生成 16+8 字节随机数、日志多两个字段、消息行多 16 字节。验收：`bench/single-node.sh` 吞吐下降不超过 2%，p99 不超过 +1 ms；`sample_ratio = 1` 时另测一组写进 README。

## 5. 业务规则梳理

### 5.1 规则目录（D142）

- 每一处业务检查改成用一个宏声明：`rule!(SEND_SILENCED, "forbidden", "silenced in this conversation until {}")`，规则编号形如 `send.silenced`、`member.add.not_admin`、`file.attach.other_conv`。错误码和现在一样（`forbidden`、`not_found`、`conflict`、`bad_request`、`blocked`、`rate_limited`……），返回给客户端的错误多一个 `rule` 字段，方便开发者按规则处理和查文档。
- `docs/15-business-rules.md` 由测试从代码生成：按领域分组，每条有编号、错误码、说明、在哪个环节检查（③ 会话层 / ④ Webhook / ⑦ 写线程）。CI 检查文档和代码一致（像 openapi.json 和 MCP 工具清单那样）。
- 现在的分布（从代码统计，2026-10-29）：

| 领域 | 检查数 | 主要文件 |
|---|---|---|
| 会话和成员（建、加人、移人、角色、转让、解散、拉黑、禁言） | 约 60 | `logic.rs`、`hub.rs` |
| 消息（发送、编辑、撤回、信封、卡片、反馈、举报） | 约 45 | `logic.rs`、`logic/cards.rs`、`logic/ai.rs` |
| 流式回复、草稿、审批、接管 | 约 25 | `logic.rs` |
| Agent（创建、密钥、停止、销毁、迁移、熔断、转人工） | 约 35 | `hub/agents.rs`、`hub/lifecycle.rs`、`logic/ai.rs` |
| 请人决定、任务、抢单、发言权 | 约 30 | `logic/requests.rs`、`logic/tasks.rs` |
| 文件 | 约 25 | `logic/files.rs`、`hub/files.rs` |
| 历史、上下文、检查点、同步 | 约 40 | `hub/history.rs`、`hub/state.rs`、`hub/sync.rs` |
| 账号、设备、令牌、签名密钥、登录 | 约 30 | `hub/login.rs`、`hub.rs` |
| 合规和审核（导出、删除、举报处理） | 约 25 | `hub/compliance.rs`、`logic/compliance.rs` |
| 控制台和管理 | 约 35 | `hub/admin.rs`、`logic.rs` |
| 参数格式（长度、范围、类型） | 约 40 | 各处 |

### 5.2 例子：发送一条消息的检查顺序（现在的代码）

| 顺序 | 环节 | 检查 | 结果 |
|---|---|---|---|
| 1 | ② | 令牌有效、未吊销、设备未注销 | 握手 401 |
| 2 | ③ | Agent 被主人停了 | `conflict` |
| 3 | ③ | 应用的客户端权限（`[clients.allow]` 和个人权限） | `forbidden` |
| 4 | ③ | 每人发送频率（所有连接合计，D95） | `rate_limited` |
| 5 | ④ | 有 before_send 时：去重命中直接放行；不是成员 `forbidden`；文本、种类格式；Webhook 改写或拒绝 | 各码 |
| 6 | ⑤ | 会话在迁移 / 已迁走 | `conv_migrating` / `conv_moved` |
| 7 | ⑦ | **去重最先**：已经提交过的重试直接返回原结果（之后成员、禁言可能变了，M1） | 原结果 |
| 8 | ⑦ | 账号停用或删除（在写线程里再查一次，删除后不能再提交） | `forbidden` |
| 9 | ⑦ | 是成员、未被禁言 | `forbidden` |
| 10 | ⑦ | 代表主人发言只能是 Agent 的主人 | `forbidden` |
| 11 | ⑦ | 内容策略（DLP、出站策略） | `policy_blocked` |
| 12 | ⑦ | 单聊里被对方拉黑 | `blocked` |
| 13 | ⑦ | 定向可见（`to_user`）：对方拉黑；不能带信封和文件 | `blocked` / `bad_request` |
| 14 | ⑦ | Agent：护栏（循环、频率、每日额度）、任务状态、发言权、接管中 | `loop_detected` / `rate_limited` / `conflict` / `taken_over` |
| 15 | ⑦ | 文件：同一会话、自己的、已完成、没被别的消息用过、图片才能发成图片 | `forbidden` / `not_found` / `conflict` / `bad_request` |
| 16 | ⑦ | 需要审批的 Agent：变成草稿，等人批准 | 草稿 |
| 17 | ⑧⑨ | 提交后回复、扇出、发件箱 | — |

其他每个操作在规则目录里都有同样的一张表。

### 5.3 业务校验器（D143）

`agentim verify [--conv ID] [--since T]`：只读，回放每个会话的日志和当前状态，检查不变量，输出不符合的会话、seq 和规则：

- 顺序：seq 从 1 连续；`dissolved` 之后没有事件；每个会话最多一个 `conv.created`。
- 成员：每条 `msg.created` 的发送者在那个 seq 时是成员、没被禁言；离开或被移出之后没有他发的消息；单聊始终两个人。
- 修改：编辑和撤回的人有权限（发送者、Agent 的主人、撤回时的群主和管理员、系统）；撤回之后没有编辑；版本只增不减。
- 去重：同一 `(会话, 发送者, client_msg_id)` 只有一条消息。
- Agent：审批过的草稿只发一次；被停期间没有它的消息；流式回复都结束了（没有永远在写的）。
- 文件：每个文件最多被一条消息引用；引用的文件属于同一会话、发送者是上传者；被扔掉的文件没有消息还在引用（或消息已撤回）。
- 侧信道：受众都是当时的成员。
- 日志回放的结果和保存的状态一致（成员表、消息当前版本、未读）。

CI：三种数据库上的场景测试和并发测试跑完后都跑一遍校验器，要求零问题（现在测试里分散的不变量检查合到这里）；运营者可以随时对生产库跑（只读，限速）。

## 6. 计划和测试

| 步骤 | 内容 | 测试 | 状态 |
|---|---|---|---|
| T1 | 链路上下文：`Op` 字段、集群转发和发布、写线程批次 link、日志字段、消息行 `trace_id`（schema v10）；客户端核心生成并持久化 `trace` | 单节点和三节点：一条消息的所有日志行 trace_id 相同；转发后的节点日志也有；重启后续传同一个 trace；不合法的 traceparent 新开；客户端的已采样标志被忽略 | 完成（`tests/tracing.rs`、集群 `the_trace_crosses_nodes`） |
| T2 | 染色规则：管理 API、控制台、过期、审计、节点通知、按链路调日志级别、继承到 Webhook 和 Agent 回复 | 规则生效和过期；别的用户不受影响；跨节点继承；规则编号不出现在推送和 Webhook 里 | 完成（`tests/dye.rs`、集群 `dye_rules_reach_every_node_and_follow_the_request`） |
| T3 | 消息轨迹接口和控制台页面 | 各种情况：已送达、部分设备离线、Webhook 失败、被 Agent 回复、撤回；无权限的角色 403；审计记录 | 完成（`tests/msgtrace.rs`、控制台冒烟测试） |
| T4 | 规则宏、规则目录生成、错误里的 `rule` 字段、拒绝指标 | 目录和代码一致；每个规则至少一个测试触发它（覆盖检查） | 进行中 |
| T5 | 业务校验器 | 对故意改坏的数据库（变异）能找出每一类问题；场景和并发测试后零问题 |  |
| T6 | OTLP 导出（可选）、Agent SDK 的 `ctx.trace`、Webhook `traceparent` | 导出到本地收集器看到完整链路；性能对比写进 README | Webhook `traceparent` 随 T2 完成 |

依赖：`opentelemetry`、`opentelemetry-otlp`、`tracing-opentelemetry`（发布满两周的版本），只在开了导出时初始化。

## 7. 待确认

| # | 事项 | 建议 |
|---|---|---|
| D138 | 链路编号用 W3C `traceparent`，WebSocket 帧里用可选字段 `trace` | 是 |
| D139 | 终端用户客户端的链路在服务端另开、只做 link，不理会它的采样标志；开发者后端和 Agent 直接接上 | 是 |
| D140 | 染色按 用户 / 设备 / 会话 / Agent / API 密钥，要原因、审计、最长 24 小时；只在服务端内部传播 | 是 |
| D141 | 消息轨迹从持久数据拼出来，不依赖链路系统；消息行加 `trace_id` | 是 |
| D142 | 规则编号和规则目录（错误里加 `rule` 字段，文档从代码生成并在 CI 检查） | 是 |
| D143 | 业务校验器 `agentim verify`，CI 在三种数据库上跑 | 是 |
