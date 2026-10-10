# 05 运维、测试与客户端

> 版本 v0.3（2026-10-10）。§1–§5、§7 已实现（标 ⏳ 的条目除外）；§6 客户端是**待确认的方案**。

## 1. 部署

- **服务端只运行在 Linux 上**（x86_64、aarch64），以单个二进制发布。Windows 上的服务端只用于开发和测试，测试套件在 Windows 上同样通过（§5）。
- 配置：命令行参数 + 配置文件（`--config agentim.toml`，示例见 `server/agentim.example.toml`）。只用一个 SQLite 文件时，用 `--db` 即可。
- 存储故障时进程主动退出（退出码 70），由 systemd（`Restart=always`）拉起后从日志恢复，见 [02 §4.4](02-architecture.md)。调试时可以用 `--no-exit-on-storage-failure` 关闭这个行为。
- ⏳ systemd unit 文件、Docker 镜像、发布流水线。

## 2. 日志规范

| 规则 | 说明 |
|---|---|
| 格式 | `--log-format json` 输出 JSON 行（生产环境），默认是易读格式。写到 stdout，由 journald 或 Docker 收集。级别用 `RUST_LOG` 控制，默认 `info` |
| 字段 | 请求日志带 `conn_id`、`user_id`、`op`、`rid`、`result`、`latency_ms`；出错时带 `error.code`、`error` |
| 级别 | ERROR：需要人处理（存储故障、启动失败）；WARN：存储类请求错误、一致性修复、迁移问题；INFO：启动、写线程启动、迁移完成；DEBUG：逐请求日志、连接建立 |
| 隐私 | **日志里不记录消息内容、令牌、耳语正文**，只记录 ID 和错误码 |
| 审计与日志分离 | 审计记录（谁对什么做了什么）存在数据库里，可以通过 `stats` 查看。⏳ 导出为 JSONL 或发送到 SIEM |
| ⏳ | 按天滚动的日志文件；OpenTelemetry trace |

## 3. 指标（Prometheus `/metrics`）

用 `metrics` 门面库加 Prometheus 导出器。设置 `--metrics-token` 或环境变量 `AGENTIM_METRICS_TOKEN` 后，访问需要带 `Authorization: Bearer <token>`。命名遵循 Prometheus 惯例：计数器带 `_total` 后缀，耗时用 `_seconds`。**标签里不放 `user_id` 和 `conv_id`**。

| 类别 | 已实现 | ⏳ 后续 |
|---|---|---|
| 连接 | `agentim_ws_connections{kind}`、`agentim_ws_connects_total{kind}`、`agentim_ws_disconnects_total{kind}` | |
| 请求 | `agentim_requests_total{op,result}`、`agentim_request_duration_seconds{op}`、`agentim_ack_latency_seconds` | |
| 写入路径 | `agentim_commit_batch_size`、`agentim_commit_duration_seconds{database}`、`agentim_write_queue_depth{database}`、`agentim_write_rejected_total{reason}`、`agentim_messages_committed_total{sender_type}`、`agentim_events_committed_total{type}`、`agentim_dedup_hits_total` | |
| 推送 | `agentim_push_total`、`agentim_push_dropped_total`、`agentim_resync_total`、`agentim_stream_deltas_total` | 每个连接的队列深度 |
| 流式 | `agentim_streams_active`、`agentim_streams_finished_total{state,reason}` | 首个增量的延迟 |
| AI 治理 | `agentim_guard_trips_total{reason}`、`agentim_guard_rejects_total{code}`、`agentim_policy_blocks_total{hook}`、`agentim_inbound_flags_total{flag}`、`agentim_drafts_total{state}`、`agentim_approval_latency_seconds`、`agentim_takeovers_total{action}` | 待审批数量（gauge）、工具调用（P3） |
| 存储 | `agentim_storage_healthy{database}`、`agentim_storage_errors_total{kind}`、`agentim_conversation_migrations_total{result}` | 连接池、加密与 KMS（P2） |
| 进程 | | CPU、内存、文件描述符 |

## 4. 健康检查、告警、面板

- `/healthz`：进程存活。
- `/readyz`：每个数据库都可写（没有处于存储故障状态），写入队列没有超过容量的 90%。返回每个库的状态；不就绪时返回 503。
- ⏳ 管理系统（§8）展示健康状态；随仓库提供 Prometheus 告警规则（提交耗时 p99 > 50ms、写入队列持续饱和、推送丢弃率过高、熔断突增、待审批草稿超过 SLA、存储错误）和 Grafana 面板 JSON。

## 5. 测试

**测试规则**（每个业务功能都必须做到，见仓库根目录的 `CLAUDE.md`）：
- 覆盖这个功能影响到的每个逻辑节点，不只是功能自己的操作；
- 覆盖边界：上限、空值、未知 ID、重复调用、对自己操作；
- 错误码符合业务含义且前后一致：`forbidden`（不允许或不是成员）、`not_found`（不存在或已删除）、`conflict`（当前状态不允许）、`bad_request`（输入无效或不适用）、`blocked`、`rate_limited`；被拒绝的请求不能在日志里留下任何痕迹；
- 和所有可能冲突的操作做并发竞争，多轮重复后检查不变量；守护竞争的测试要做"故意改坏能否抓到"的验证。

所有测试都在 GitHub Actions 中运行：每次推送跑 `.github/workflows/server.yml`；规模测试在 `.github/workflows/scale.yml`，每晚跑一次，也可以手动触发（§5.4）。

| 测试 | Linux | Windows | 内容 |
|---|---|---|---|
| fmt、clippy（警告即失败） | ✅ | ✅ | |
| 单元测试（10） | ✅ | ✅ | 护栏、过滤、方言（占位符改写、MySQL 建表语句、Windows 路径）、推送队列满时的 resync、队列排空后补发欠下的 resync |
| 验收测试（`tests/hub.rs`，13 项 × 3 种数据库） | SQLite、PostgreSQL 16、MySQL 8.4 | SQLite | seq 连续、去重、编辑和撤回能被已同步的客户端拿到、流式三种中断、打断、熔断、重复检测、接管、读取范围、注入、DLP、并发且组提交生效、**故障注入：提交失败时不发 ACK** |
| 业务场景（`tests/scenarios.rs`，29 项 × 3 种数据库） | SQLite、PostgreSQL 16、MySQL 8.4 | SQLite | 每个业务领域一项，覆盖允许的路径、拒绝的路径（谁不能做、返回什么错误码）以及事后状态（主日志、侧信道、推送、重启后），见 §5.2 |
| 并发场景（`tests/concurrency.rs`，12 项 × 3 种数据库） | SQLite、PostgreSQL 16、MySQL 8.4 | SQLite | 同一业务操作在多个会话里同时发生、互相冲突的操作竞争，每种重复 20 轮后检查不变量，见 §5.3 |
| 成员生命周期（`tests/membership.rs`，17 个场景 + 7 个竞争 × 3 种数据库） | SQLite、PostgreSQL 16、MySQL 8.4 | SQLite | 见 §5.3.1 的覆盖矩阵 |
| 覆盖排查补充（`tests/coverage.rs`，19 项 × 3 种数据库 + 1 项 PostgreSQL + 2 项 SQLite） | SQLite、PostgreSQL 16、MySQL 8.4 | SQLite | 见 §5.5 |
| HTTP 与 WebSocket 接口（`tests/http.rs`，5 项） | ✅ | ✅ | 见 §5.5 |
| 配置校验（`src/config.rs` 单元测试，3 项） | ✅ | ✅ | 见 §5.5 |
| 多库（`tests/multidb.rs`，6 项） | ✅ | ✅ | 单元的内容和会话标题不会出现在共享库文件里（直接读文件字节确认）；跨库的成员、列表、侧信道；迁移后事件完全一致、seq 连续、旧库不残留内容；迁移期间并发写入不丢不重；崩溃后目录修复；单元放在另一种数据库上（SQLite 控制库 + PostgreSQL 单元库） |
| WebSocket 端到端（2） | ✅ | ✅ | 鉴权、请求与响应、推送；内置 Agent 的流式回复 |
| 崩溃恢复（`tests/crash.rs`） | ✅ | ✅ | 启动真实的二进制，发送过程中强杀进程（Linux 上是 SIGKILL，Windows 上是 TerminateProcess），在 3 个时间点各做一轮，重启后逐条核对已 ACK 的消息 |
| 并发压测（`examples/loadtest.rs`） | ✅ + 性能门槛 | ✅ | 扇出场景（同群）与纯写入场景（`--solo`）。校验 seq 连续、恰好一次、发送方有序、推送有序且不缺、所有客户端视图一致、流式内容完整 |
| 浏览器（`ci/ui_smoke.mjs`，14 项断言） | ✅ | ✅ | 完整演示流程 + 截图 |
| 规模测试（`examples/scale.rs`，每晚） | ✅ | ✅ | 见 §5.4 |
| ⏳ | | | 依赖安全扫描（`cargo audit`、`cargo deny`）、加密开 / 关的对比基准（P2） |

### 5.2 业务场景测试

| 领域 | 覆盖的情况 |
|---|---|
| 会话与成员 | 创建参数校验（类型、标题长度、未知成员、Agent 不能建会话、单聊只能有一个对方、成员重复或包含自己）；非成员的 14 种读写操作全部拒绝且不进日志；只有群主能拉人，成员可以拉自己的 Agent；重复拉人返回冲突；新成员能看历史并收到推送；后加入的 Agent 看不到加入前的内容，所有者代看也一样 |
| 发送 | 长度按字节算（64KB 边界、中文边界）；特殊字符和 emoji 原样往返，重启后不变；NUL 字节被拒绝；去重按发送者和会话区分，重试改了文本也保持原文，重启后仍去重；引用必须在同一会话；@ 按完整名字匹配；代发只允许 Agent 代表自己的所有者；仅某人可见的消息只到目标、发送者和 Agent 所有者，不占主日志 seq |
| 编辑与撤回 | 只能改自己的（或自己 Agent 的），版本号递增；撤回后不能再编辑；撤回后历史里原文和编辑过的版本都不留（重启后、SQLite 文件里也没有）；群主可撤回他人消息（管理），普通成员不行；重复撤回返回冲突且不加事件；撤回的消息不算未读 |
| 同步 | 分页连续无重叠、越界、limit 边界；已读游标只进不退，超过末尾会被收回到末尾；自己的消息不算未读；游标重启后保留；同一用户多个设备都收到推送；离线设备按 seq 补拉 |
| 流式 | 只有成员 Agent 能流式；每个 Agent 在每个会话最多 2 路；分块重复忽略、跳号拒绝；别人不能追加或结束；结束后不能再追加；成员和所有者可以打断，非成员和其他 Agent 不行；大小上限不会被超过；超时由看门狗关闭；撤回进行中的流；中途打开会话的人看到的文本不重复 |
| 审批 | 草稿只有所有者看得到，不占 seq；只有所有者能批准或拒绝；批准时可以改文本，作者仍是 Agent；重复批准、批准后拒绝、拒绝后批准都返回冲突；未知草稿 |
| 介入 | 耳语只在所有者和自己的 Agent 之间；接管时取消进行中的流，Agent 发消息和开流都被拒绝；接管重启后仍有效；只有所有者能改 Agent 策略，重启后保留 |
| 护栏与协议 | 人类不限速，Agent 突发会被限速但不熔断；格式错误、未知操作、缺字段、类型错误都返回 bad_request 并带回 rid，不产生任何写入 |
| 重启 | 日志、成员策略、未读、待审批草稿、侧信道都保留；重启后可以批准之前的草稿；seq 接着增长 |

### 5.3 并发场景测试

| 场景 | 不变量 |
|---|---|
| 8 个设备同时重试同一条消息 | 只创建一条，所有设备拿到同一个 msg_id |
| Agent 和所有者同时编辑同一条消息 | 版本号逐个递增，每次成功的编辑恰好一个事件，最后一个事件就是当前状态 |
| 撤回和多个编辑同时发生 | 撤回总会成功；撤回之后这条消息不再有任何事件；历史中不留任何版本的内容 |
| 6 个会话同时撤回同一条 | 恰好一个成功 |
| 所有者两台设备同时批准、两台同时拒绝同一草稿 | 恰好一个决定生效；批准的草稿只发布一次，拒绝的永不发布 |
| 5 个会话同时拉同一个人 | 只加一次；同时拉不同的人全部成功 |
| 持续发送中拉新成员 | 新成员从加入那一刻起每个事件都按序收到，一个不缺 |
| Agent 结束流、两个人同时打断、Agent 同时追加 | 只结束一次，最终状态是完成或取消之一 |
| 两条连接同时发送相同的分块 | 每块只生效一次，内容完全正确 |
| 接管、释放和 Agent 发消息同时发生 | 日志里接管和释放交替出现；被接管期间不会有 Agent 消息落库 |
| 多个设备同时更新已读游标 | 结果是最大值 |
| 8 对用户分属两个库同时建会话、发消息 | 每个会话 seq 连续、只有自己的消息；每个人只看到自己的会话 |

测试本身也验证过：故意删掉"已撤回不能再撤回"的检查后，并发撤回测试会失败。

#### 5.3.1 成员生命周期的覆盖矩阵

| 受影响的逻辑节点 | 覆盖的情况 |
|---|---|
| 推送 | 被移除者收到移除事件，之后收不到任何推送（多台设备都是）；其他成员看到移除 |
| 拉取、侧信道、已读、正在看的会话 | 被移除后全部 `forbidden`；正在看的会话立即停止推送流式增量；通过自己仍在会话里的 Agent 只能看到发给自己的侧信道事件；Agent 也被移除后全部拒绝 |
| 发送、仅某人可见的消息 | 被移除者不能发；别人不能再给被移除者发仅他可见的消息 |
| 编辑、撤回 | 被移除者不能再改或撤回自己以前的消息（以前的消息保持原样）；Agent 被移除后，所有者也不能再改它的消息 |
| 流式 | 被移除的人类不能再打断；被移除的 Agent 的流以"取消（removed）"结束，Agent 收到通知，之后追加返回 `not_found` |
| 审批 | 被移除 Agent 的待审批草稿被丢弃，批准返回 `conflict`，待审批列表清空 |
| 介入（耳语、接管、策略、重置护栏、代看） | Agent 被移除后全部 `forbidden` |
| 会话列表与用户目录 | 被移除后不再列出（直接成员和通过 Agent 代看都是）；离开后对方不再出现在 `hello` 里；`find_user` 只在同一单元内或已有共同会话时能找到 |
| 角色 | 普通成员不能管理；管理员能拉人、移除普通成员和别人的 Agent、撤回他人消息，不能动群主和其他管理员；只有群主能改角色；Agent 不能当管理员或群主；重新加入后是普通成员 |
| 群主 | 群主不能退出、不能移除自己、不能转让给自己或 Agent；转让后原群主变为管理员；重启后保持 |
| 单聊 | 不能拉人、移除、退出、改角色、转让；双方都能删除，Agent 不能；每对用户只有一个；拉黑后不再返回已有的单聊 |
| 解散 | 只有群主（单聊是任一方）；全员收到通知；之后所有操作返回 `not_found`；进行中的流取消并通知 Agent；草稿、侧信道、待审批列表都清空；重启后仍然不存在；**审计记录保留**（包括解散前的移除、转让记录） |
| 拉黑 | 双向阻止单聊、仅某人可见的消息；被拉黑者不能拉对方进群或建群；拉黑的一方仍然可以拉对方进群；共同群照常；重复拉黑和取消都是幂等的；未知用户 `not_found`；不能拉黑自己；重启后保留 |
| 多个数据库 | 单元数据库里的移除、退出、解散都同步到控制库的成员索引和会话目录，重启后不会复活 |
| 边界与错误码 | 未知用户、未知会话、非成员、空角色、重复移除，都有对应的错误码 |
| 日志与状态一致 | 从日志重放成员变化，结果必须等于实际成员列表；seq 始终连续 |
| 并发竞争 | 发送和移除（移除之后不会有被移除者的消息）；多端同时建单聊（只有一个）；移除和退出同时发生（只成功一次）；同时转让给不同的人（只有一个群主）；发送和解散；加人、移人、改角色同时发生（日志重放等于状态）；解散和加人、移人、改角色同时发生（解散总会成功，之后谁都看不到） |

故意改坏验证：去掉"移除后刷新成员缓存"这一行，有 4 个测试失败（包括发送与移除的竞争测试）。

场景测试发现并已修复的问题：

1. **撤回后原文仍在历史里**。日志的每个事件都带着消息快照，撤回只改了最新状态，从头拉取还能看到原文和编辑过的版本。现在撤回会清空这条消息在所有事件里的内容。
2. **任何拥有 Agent 的人都能往任意会话写事件**（`reset_guard` 没检查 Agent 是否在会话里）。现在必须是成员。
3. 重复撤回会再加一个事件；现在返回冲突。
4. 已读游标可以设到末尾之后，导致之后的新消息都不算未读；现在收回到末尾。
5. 单聊可以没有对方或有多个对方；成员列表里有重复或包含自己时整个创建失败。
6. 流式消息存下来的内容可能超过大小上限；现在超限的那一块不保留。
7. 中途打开会话的人会看到一段文字出现两次。
8. `@bob` 会把 `@bobby` 也算成提到 bob；现在取最长匹配。
9. 文本里的 NUL 字节在 PostgreSQL 上导致存储错误；现在直接拒绝。

### 5.5 全功能覆盖排查（2026-10-10）

按测试规则把已有的全部功能逐个排查：列出每个功能影响到的逻辑节点，对照测试找缺口，补测试；补测试时发现的问题当场修复，并做"故意改坏能否抓到"的验证。

| 功能 | 影响到的逻辑节点 | 覆盖情况 |
|---|---|---|
| 发送 | 权限、长度和 NUL、去重（按发送者和会话、重启后、并发、**过期清理**）、引用和因果链、@（**非成员不算**）、代发、仅某人可见（**发给自己**）、推送、未读、审批分支、接管、护栏、DLP、**同批里一条失败不影响其他**、**写队列满时拒绝且重试成功** | 全部有测试；粗体为本次补充 |
| 编辑、撤回 | 权限（含被移除者）、状态冲突、**长度和 NUL 边界**、**补全提前结束的流式消息**、Agent 编辑的 **DLP**、撤回清除历史、重复撤回、并发 | 全部有测试 |
| 拉取、侧信道、已读、正在看的会话 | 分页和边界、**侧信道分页**、**通过 Agent 代看只限所有者且写审计**、**代看的会话列表标注和未读**、已读游标边界、**未知会话**、**通过 Agent 看流式增量** | 全部有测试 |
| 流式 | 权限、分块规则、打断权限、大小和时长上限、超时、断线、重启恢复、撤回进行中的流、中途打开、**审批模式的流式草稿（只有所有者看得到增量，取消或为空时丢弃）**、**预算耗尽时取消** | 全部有测试 |
| 审批 | 可见性、权限、冲突、重启后保留、并发决定、**接管期间不能批准（新规则）**、**批准与接管并发** | 全部有测试 |
| 介入 | 耳语、接管（含重启）、策略（**只能针对自己的 Agent；非法取值**）、**重置护栏只在熔断时可用（新规则）** | 全部有测试 |
| 护栏 | 跳数、重复、速率（人不限速）、**每日消息和字符预算**、**预算跨会话、跨数据库并发时精确**、**熔断冷却后自动恢复** | 全部有测试 |
| 策略过滤 | 注入标记（推送和拉取）、DLP（发送、流式、**编辑、仅某人可见**）、**人类消息不过滤** | 全部有测试 |
| 成员生命周期 | 见 §5.3.1 | 全部有测试 |
| 多库与迁移、崩溃恢复、推送队列 | 已有测试（§5 表格） | 已覆盖 |
| HTTP 接口 | **非演示模式下登录、建用户、统计都关闭**、**演示模式下可用及错误码**、**`/metrics` 令牌**、**存储故障时 `/readyz` 返回 503**、**WebSocket 拒绝无效令牌、非法请求带回 rid、超过 1MB 的帧断开连接且服务不受影响** | 本次补充 |
| 配置与启动 | **启动和写入时遇到别的连接在写（SQLite）会等待而不是失败**、**配置校验的每种错误**、**单元默认库**、**配置文件解析**、**数据库表结构版本更新时拒绝启动**、**PostgreSQL 持久性设置不安全时拒绝启动，显式允许时放行** | 本次补充 |

**本次排查发现并修复的问题**：

1. **任何登录用户都能看全站统计**（审计日志、在线用户、计数），WebSocket 的 `stats` 和 `/api/stats` 都是。现在只在演示模式下开放，正式环境由之后的管理 API 提供。
2. **接管期间所有者可以批准 Agent 的草稿**，于是会在"已被人接管"时冒出一条 Agent 消息，和接管的含义矛盾。现在返回 `conflict`，要先释放。并发测试证明这个竞争原来确实存在。
3. **没有熔断时也能"重置护栏"**，每次都往日志里写一条无意义的事件。现在返回 `conflict`。
4. **SQLite 上，别的连接正在写时，服务端的写入会直接失败**（`database is locked`），而不是等待。原因是批量事务先读后写，读完以后别的连接提交了，升级为写锁时 SQLite 直接报错、不等待。重启时旧进程还没完全退出就会遇到（CI 上偶发了一次）。现在 SQLite 的批量事务和启动建表都在开始时就拿写锁（`BEGIN IMMEDIATE`），遇到锁会按忙等待超时排队。新增的测试能稳定复现原来的问题；改动后单库写入吞吐不变（约 1,150 条/秒）。

**故意改坏验证**：分别撤掉上面前三处修复；第 4 项的测试在修复前稳定失败，对应的测试都会失败（第 2 项有两个测试失败，包括并发测试）。

**暂时无法自动测试的项**：MySQL 的持久性设置（`innodb_flush_log_at_trx_commit`、`sync_binlog`）只能全局修改，会影响同时运行的其他测试，所以只测了 PostgreSQL；MySQL 这部分靠代码审查和启动日志。

### 5.4 规模测试

压测程序和服务端在同一台 runner 上（4 核），所以这里的数字用来发现退化，不代表容量上限。万级消息/秒和百万连接要在集群阶段用独立的压测机器验证。

| 场景 | Linux 规模 | Windows 规模 | 校验什么 |
|---|---|---|---|
| `connections --storm` 海量长连接 + 重连风暴 | 2 万连接（4 个本地源地址），每组 100 人，1% 的人在发，200 条/秒；中途全部连接同时断开再重连 | 5000 连接，100 条/秒 | 每个连接的服务端内存；重连风暴期间发送不失败；重连的客户端靠拉取补齐，数据不丢 |
| `biggroup` 大群 | 2000 人一个群，50 条/秒 | 1000 人 | 端到端投递延迟（发送时间戳到每个接收端） |
| `slow` 慢消费者 | 500 人一个群，10% 的人停止读取，300 条/秒，每条 2KB | 200 人 | 慢的人收到 resync 并能拉取补齐；其他人一条不缺、延迟不受影响 |

每个场景都检查：每个接收端最终拿到所在会话的全部事件（推送收到，或在 resync 通知、重连之后拉取补齐）；同一连接上的推送按 seq 顺序到达。**客户端没被通知就缺事件**、拉取也补不齐、推送乱序，都会让测试失败。性能门槛只在 Linux 上设了宽松的 p99 ≤ 2 秒，用来发现严重退化。

GitHub Actions 结果（标准 runner，release，SQLite，每个场景发送 120 秒）：

| 场景 | Linux | Windows |
|---|---|---|
| 海量长连接 + 重连风暴 | 2 万连接 2.5 秒建好，每个连接约 13KB 服务端内存；2 万连接同时断开，2.3 秒内全部重连，期间发送零失败；端到端 p50 3ms、p99 7ms；240 万次投递无丢失、无乱序 | 5000 连接，重连 1.6 秒；p99 37ms；无丢失、无乱序 |
| 2000 人大群，50 条/秒（每秒 10 万次推送） | 1204 万次投递全部按序到达；端到端 p50 14ms、p99 43ms；服务端内存 54MB | 通过 |
| 慢消费者（500 人，50 人不读，300 条/秒、每条 2KB） | 50 个慢客户端全部收到 resync 并拉取补齐；其余 450 人一条不缺，p99 30ms；服务端内存 34MB，丢弃的 169 万次推送没有占用内存 | 通过 |

规模测试发现并已修复的问题：

1. **慢消费者可能不知道自己漏了消息**。推送队列满时欠下的 resync 通知，原来要等下一次推送才会发出；如果之后没有新消息，客户端永远不知道有缺口。现在队列一排空就补发。
2. **每个空闲连接占约 140KB 内存**（WebSocket 库默认 128KB 读缓冲 + 128KB 写缓冲），2 万连接就是 2.7GB。改小缓冲后约 14KB，降到原来的十分之一。
3. **大群的推送接收列表按事件重复构建**。建一个 2000 人的群会产生 2000 个成员事件，每个事件都复制一份 2000 人的接收列表，服务端内存涨到 600MB 以上。现在接收列表按成员列表缓存、各事件共享，只有群里有 Agent 时才按事件复制。同一场景下 2000 连接加 2000 人群的服务端内存从 619MB 降到 46MB。

本地跑 PostgreSQL 和 MySQL 的测试：设置 `AGENTIM_TEST_PG=postgres://user@host` 和 `AGENTIM_TEST_MYSQL=mysql://user:pass@host`（URL 里不带库名），每个测试会自动新建一个库。

## 6. SDK 与客户端（D8）

### 6.1 方案：一个 Rust 核心，各语言绑定

连接管理、断线重连、`client_msg_id` 重试去重、按 seq 的同步与补拉、resync 处理、本地存储，以及 E2EE（MLS）都比较难写对。七种语言各写一遍，行为一定会出现差异，密码学代码也无法逐一审计。业内成熟的做法是**一个 Rust 核心 + 自动生成的各语言绑定**：

| 参考 | 做法 |
|---|---|
| matrix-rust-sdk | Rust 核心，UniFFI 生成 Kotlin / Swift 绑定，Element X 的 iOS、Android 客户端都基于它 |
| Wire core-crypto | OpenMLS + UniFFI（移动端）+ wasm（Web） |
| libsignal | Rust 核心，提供 Java、Swift、TypeScript 绑定 |
| Mozilla application-services | Rust 核心，UniFFI 生成 Firefox 移动端用的绑定 |

| 目标 | 绑定方式 | 产物 |
|---|---|---|
| Rust | 直接使用 `agentim-sdk` crate | crates.io |
| Android | UniFFI → Kotlin | AAR（arm64-v8a、armeabi-v7a、x86_64） |
| iOS / macOS | UniFFI → Swift | XCFramework、Swift Package |
| Python | UniFFI → Python（PyO3 备选） | wheel（Linux、Windows、macOS） |
| TypeScript | 浏览器：wasm-bindgen；Node：napi-rs | npm 包 |
| C / C++ | cbindgen 生成 C 头文件 + C++ 封装 | 静态库和动态库；附 **CMake 配置**，可以 `find_package(agentim)` 或用 `add_subdirectory` 从源码编译 |
| Go | cgo 调用 C 接口 | Go module，附预编译库 |

另外提供**纯 Go、纯 Python 的轻量 Agent 客户端**（不含 E2EE）：服务端 Agent 大多用 Go 或 Python 编写，交叉编译时 cgo 很麻烦；而协议本身是 WebSocket 上的 JSON，轻量客户端几百行就能写好。

**构建脚本**：`sdk/build.sh` 和 `sdk/build.ps1`，一条命令编译指定目标（例如 `./build.sh android`、`./build.ps1 cpp -Release`）；CI 在 Linux、Windows、macOS 上构建全部产物，并对每种语言跑同一套协议一致性测试（复用服务端的场景测试，用 SDK 重新实现客户端一侧）。

### 6.2 Windows 客户端

| 项目 | 选择 | 理由 |
|---|---|---|
| 框架 | **Tauri 2** | 用系统自带的 WebView2（Windows 10/11 已预装）；安装包约 10MB，Electron 通常要 150MB 以上；后端直接调用 Rust SDK，不用绑定 |
| 界面 | React，和 Web 客户端共用 | 一套界面同时用于 Web、Windows，之后是 macOS、Linux |
| 本地数据 | SQLite（SDK 自带）；E2EE 私钥用 Windows DPAPI / 凭据管理器保护 | |
| 系统集成 | 原生通知、托盘、开机启动、自动更新（签名安装包） | |

Web 客户端继续由服务端直接提供，也可以安装为 PWA；手机端在 SDK 完成后再做。

## 7. 单机版运行与演示（已实现）

```bash
cd server
cargo run --release -- --demo          # 打开 http://127.0.0.1:8080，选 alice / bob / carol 登录
```

| 演示会话 | 能看到什么 |
|---|---|
| Alice · Aria | 流式回复；`/long` 后点 ■ 打断；`/secret` 触发流式过程中的 DLP；`/stall`、`/crash` 触发看门狗；注入式语句被标为低信任 |
| 产品讨论群 | 群内 Agent 默认只在被 @ 时回应 |
| 客服：退款工单 | alice 先耳语，再审批 RefundBot 的草稿；bob 只在批准后才看到回复 |
| Agent 循环实验室 | 两个 Agent 互相回复，在第 7 跳被熔断 |
| Bob · Aria | alice 经 Aria 只读观察，观察动作写入审计 |

| | |
|---|---|
| ![流式](images/01-streaming.png) | ![打断、DLP、注入](images/02-interrupt-dlp-injection.png) |
| ![看门狗](images/03-stall-timeout.png) | ![熔断](images/04-loop-breaker.png) |
| ![审批](images/05-approval-pending.png) | ![批准后](images/07-customer-after-approval.png) |
| ![观察](images/09-observe.png) | |

### 7.1 协议摘要（JSON over WebSocket）

连接：`GET /ws?token=<token>`。客户端帧：`{"op": "...", "rid": 1, ...}`；服务端对请求回 `{"type": "res", "rid": 1, "ok": true | false, ...}`。

| op | 用途 |
|---|---|
| `hello`、`list_convs`、`create_conv`、`add_member` | 基础；`hello` 只返回和你有共同会话的人和你的 Agent；单聊每对用户只有一个，重复创建返回已有的（`existing: true`） |
| `remove_member`、`leave`、`set_role`、`transfer_owner`、`dissolve` | 成员生命周期：群主和管理员管理成员；被移除或退出后看不到会话；解散后全员失去访问、内容删除，只保留审计记录 |
| `block`、`unblock`、`list_blocks`、`find_user {name}` | 拉黑（双方都不能单聊、不能拉对方进群、不能发仅某人可见的消息）；按准确用户名找人 |
| `send {conv_id, client_msg_id, text, reply_to?, causation_id?, on_behalf_of?, to_user?}` | 发消息；`to_user` 表示仅某人可见；审批模式下返回 `draft_id` |
| `edit`、`recall` | 修订、撤回 |
| `pull {conv_id, after_seq, as_agent?}`、`pull_side` | 增量拉取；过滤后的结果用 `upto_seq` 推进游标 |
| `read`、`focus` | 已读游标；前台会话（接收流式增量） |
| `stream_start / stream_append / stream_end / stream_cancel` | 流式 |
| `whisper`、`approve`、`reject`、`takeover`、`release`、`set_agent_policy`、`reset_guard` | 所有者的介入操作 |
| `list_drafts`、`stats` | 待审批草稿；运行状态和审计 |

服务端推送：`event`（发给 Agent 时附带 `trust` 和 `flags`）、`side`、`delta`、`stream_cancelled`。外部 Agent 的接入示例见 `server/examples/agent.mjs`。

### 7.2 当前性能

三种数据库的单机完整数据见根目录 [README](../README.md#single-node-performance)，用 `server/bench/single-node.sh` 生成，用户可以在自己的机器上重跑。要点：单库写入约 1,000–1,300 条/秒（瓶颈是每条 SQL 语句的固定开销，不是 fsync，见 [02 §4.8](02-architecture.md)）；1 万连接时端到端 p99 在 11–21ms；每个连接约 13.5KB 服务端内存；`kill -9` 后 130–240ms 恢复就绪。

### 7.3 已知限制

演示登录不需要密码，令牌明文存储（P2 修复，见 [04 §1](04-security-compliance.md)）；没有内置 TLS（P2）；预算存在内存里；审批草稿没有过期时间（P3）；接管锁不会超时自动交还；在线迁移还没有管理接口。撤回会清空数据库中的内容，但 SQLite 的 WAL 文件在下次检查点之前可能还留着旧页面，是否在撤回后主动做检查点待定。建大群时，创建者会收到每个成员的加入事件（2000 人群就是 2000 条），超过推送队列的部分会被丢弃并改发 resync，数据不丢，但更合理的做法是合并成一条事件（放在大群模式里做，P6）。

## 8. 管理系统（D14）

### 8.1 结构

```
浏览器 ──► agentim-console（Go 单二进制，内嵌 React 前端）
               │ 管理员登录、RBAC、审计
               ▼ HTTP /admin/v1（OpenAPI）
           agentim（Rust 服务端）──► 数据库
```

- **Rust 服务端**提供管理 API（`/admin/v1`，默认只监听内网地址，用单独的管理令牌或 mTLS 认证）。所有修改都经过服务端的业务逻辑和写线程，所以管理操作同样有序、有审计，不会绕过权限和不变量。**管理系统不直接读写数据库**。
- **Go 管理端**（`agentim-console`）：用 `go:embed` 把 React 前端打包进二进制；负责管理员账号（密码 + TOTP，企业版接 SSO）、角色（超级管理员、单元管理员、审计员）、调用管理 API。接口用 OpenAPI 描述，Go 和 TypeScript 的客户端代码从 OpenAPI 生成，两边不会对不上。
- **一键启动**：`agentim-console --with-server` 会顺带启动并守护本机的 `agentim` 进程，用户只需要运行一个命令；也提供 Docker Compose 和 systemd 单元。

### 8.2 功能

| 模块 | 内容 |
|---|---|
| 概览 | 在线连接数、消息量、写入延迟、存储健康、告警（读 `/metrics` 和 `/readyz`） |
| 用户与设备 | 创建、禁用、重置密码；设备列表、强制下线；按单元筛选 |
| 组织单元 | 单元、隔离等级、所在数据库、单元迁移（调用现有的迁移流程） |
| 会话 | 只显示元数据（成员、消息数、所在库）；**看内容需要合规权限，并写审计** |
| Agent（[03 §9.4](03-ai-native.md)） | 注册、凭据、权限、预算与成本、护栏配置、急停、任务和记事本的查看 |
| 合规（D7） | 举报与审核队列、处置记录、导出、删除请求、保留策略 |
| 审计 | 按人、按 Agent、按会话、按操作检索，可导出 |
| 系统 | 加密状态与密钥轮换、数据库配置、版本与升级 |

前端组件库待定（D17），建议 Ant Design。

