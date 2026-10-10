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
- ⏳ 随仓库提供 Prometheus 告警规则（提交耗时 p99 > 50ms、写入队列持续饱和、推送丢弃率过高、熔断突增、待审批草稿超过 SLA、存储错误）和 Grafana 面板 JSON。

## 5. 测试

所有测试都在 GitHub Actions 中运行：每次推送跑 `.github/workflows/server.yml`；规模测试在 `.github/workflows/scale.yml`，每晚跑一次，也可以手动触发（§5.1）。

| 测试 | Linux | Windows | 内容 |
|---|---|---|---|
| fmt、clippy（警告即失败） | ✅ | ✅ | |
| 单元测试（10） | ✅ | ✅ | 护栏、过滤、方言（占位符改写、MySQL 建表语句、Windows 路径）、推送队列满时的 resync、队列排空后补发欠下的 resync |
| 验收测试（`tests/hub.rs`，13 项 × 3 种数据库） | SQLite、PostgreSQL 16、MySQL 8.4 | SQLite | seq 连续、去重、编辑和撤回能被已同步的客户端拿到、流式三种中断、打断、熔断、重复检测、接管、读取范围、注入、DLP、并发且组提交生效、**故障注入：提交失败时不发 ACK** |
| 多库（`tests/multidb.rs`，6 项） | ✅ | ✅ | 单元的内容和会话标题不会出现在共享库文件里（直接读文件字节确认）；跨库的成员、列表、侧信道；迁移后事件完全一致、seq 连续、旧库不残留内容；迁移期间并发写入不丢不重；崩溃后目录修复；单元放在另一种数据库上（SQLite 控制库 + PostgreSQL 单元库） |
| WebSocket 端到端（2） | ✅ | ✅ | 鉴权、请求与响应、推送；内置 Agent 的流式回复 |
| 崩溃恢复（`tests/crash.rs`） | ✅ | ✅ | 启动真实的二进制，发送过程中强杀进程（Linux 上是 SIGKILL，Windows 上是 TerminateProcess），在 3 个时间点各做一轮，重启后逐条核对已 ACK 的消息 |
| 并发压测（`examples/loadtest.rs`） | ✅ + 性能门槛 | ✅ | 扇出场景（同群）与纯写入场景（`--solo`）。校验 seq 连续、恰好一次、发送方有序、推送有序且不缺、所有客户端视图一致、流式内容完整 |
| 浏览器（`ci/ui_smoke.mjs`，14 项断言） | ✅ | ✅ | 完整演示流程 + 截图 |
| 规模测试（`examples/scale.rs`，每晚） | ✅ | ✅ | 见 §5.1 |
| ⏳ | | | 依赖安全扫描（`cargo audit`、`cargo deny`）、加密开 / 关的对比基准（P2） |

### 5.1 规模测试

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

## 6. 客户端方案（待确认）

| 阶段 | 方案 | 说明 |
|---|---|---|
| 现在 | Web 客户端（由服务端直接提供） | Windows 用户用浏览器打开即可 |
| v0.3 | 把 Web 客户端做成 PWA | Edge 或 Chrome 里可以"安装为应用"；支持桌面通知（审批提醒）；改动很小 |
| v0.4 | **Windows 桌面端：Tauri 2** | 用系统自带的 WebView2（Windows 10/11 已预装），复用 Web 界面；安装包约 10MB，Electron 通常要 150MB 以上；支持原生通知、托盘、自动更新。代价是 WebView 在不同系统上的行为有差异，需要单独测试。macOS 和 Linux 的桌面端可以顺带支持 |
| 并行 | TypeScript SDK、Rust SDK | 第三方客户端和 Agent 都能用 |

不选 Electron 的原因：安装包和内存占用都大得多；我们只需要 Windows 和 Web 两个渲染目标，不需要 Chromium 带来的跨平台像素级一致性。

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
| `hello`、`list_convs`、`create_conv`、`add_member` | 基础 |
| `send {conv_id, client_msg_id, text, reply_to?, causation_id?, on_behalf_of?, to_user?}` | 发消息；`to_user` 表示仅某人可见；审批模式下返回 `draft_id` |
| `edit`、`recall` | 修订、撤回 |
| `pull {conv_id, after_seq, as_agent?}`、`pull_side` | 增量拉取；过滤后的结果用 `upto_seq` 推进游标 |
| `read`、`focus` | 已读游标；前台会话（接收流式增量） |
| `stream_start / stream_append / stream_end / stream_cancel` | 流式 |
| `whisper`、`approve`、`reject`、`takeover`、`release`、`set_agent_policy`、`reset_guard` | 所有者的介入操作 |
| `list_drafts`、`stats` | 待审批草稿；运行状态和审计 |

服务端推送：`event`（发给 Agent 时附带 `trust` 和 `flags`）、`side`、`delta`、`stream_cancelled`。外部 Agent 的接入示例见 `server/examples/agent.mjs`。

### 7.2 当前性能

见 [02 §4.7–§4.8](02-architecture.md)：组提交已经把 fsync 从瓶颈中去掉；目前单个 SQLite 库每秒约 1,100–1,300 条，瓶颈是每条 SQL 语句的固定开销。下一步的优化方案待确认。

### 7.3 已知限制

演示登录不需要密码，令牌明文存储（P2 修复，见 [04 §1](04-security-compliance.md)）；没有内置 TLS（P2）；预算存在内存里；审批草稿没有过期时间（P3）；接管锁不会超时自动交还；在线迁移还没有管理接口。建大群时，创建者会收到每个成员的加入事件（2000 人群就是 2000 条），超过推送队列的部分会被丢弃并改发 resync，数据不丢，但更合理的做法是合并成一条事件（放在大群模式里做，P6）。
