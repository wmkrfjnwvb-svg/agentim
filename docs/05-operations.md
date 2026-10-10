# 05 运维、测试与客户端

> 版本 v0.3（2026-10-10）。§5.1 和 §7 描述的是已实现的部分；其余是**待确认的方案**。

## 1. 部署

- **服务端只运行在 Linux 上**（x86_64、aarch64），以单个二进制发布。提供 systemd unit 文件、Docker 镜像和配置文件模板。
- Windows 上的服务端只用于开发和测试，测试套件在 Windows 上同样要通过（§5）。
- 配置来源按优先级：命令行参数 > 环境变量（`AGENTIM_*`）> 配置文件（TOML）。启动时打印生效的配置，密钥类字段打码。
- systemd：`Restart=always`。fsync 失败时进程主动退出，由 systemd 拉起后从 WAL 恢复（见 [02](02-architecture.md) §4.4）。

## 2. 日志规范

| 规则 | 说明 |
|---|---|
| 格式 | 生产环境输出 JSON 行（`--log-format json`），开发环境输出易读格式。默认写 stdout，交给 journald 或 Docker 收集；可选按天滚动的日志文件 |
| 必带字段 | `ts`、`level`、`target`、`msg`、`conn_id`、`user_id`、`conv_id`、`op`、`rid`、`latency_ms`、`error.code` |
| 级别 | ERROR：需要人处理（存储故障、fsync 失败）；WARN：服务降级或异常拒绝（过载、熔断、推送被丢弃）；INFO：生命周期事件（启动、配置、连接汇总）；DEBUG：逐请求日志，默认关闭 |
| 隐私 | **默认不记录消息内容、令牌、耳语正文、工具参数。** 需要时用 `--log-content` 临时打开，打开这个开关本身会写一条审计记录 |
| 审计与日志分离 | 审计记录（谁对什么做了什么）存在数据库里，可导出为 JSONL 或发送到 SIEM。运维日志里不放审计内容 |
| 关联 | 每个请求带 `rid`，每个连接带 `conn_id`；之后接入 OpenTelemetry trace |

## 3. 指标（Prometheus `/metrics`）

方案：用 `metrics` 门面库加 Prometheus 导出器，部署简单。之后按需增加 OTLP 导出（OpenTelemetry 官方建议生产环境经 Collector 汇聚）。

指标命名遵循 Prometheus 惯例：计数器带 `_total` 后缀，耗时用 `_seconds`。**标签里不放 `user_id` 和 `conv_id`**，避免基数爆炸。

| 类别 | 指标 |
|---|---|
| 连接 | `agentim_ws_connections{kind}`、`agentim_ws_connects_total`、`agentim_ws_disconnects_total{reason}` |
| 请求 | `agentim_requests_total{op,result}`、`agentim_request_duration_seconds{op}` |
| 写入路径 | `agentim_ack_latency_seconds`、`agentim_commit_batch_size`、`agentim_commit_duration_seconds`、`agentim_write_queue_depth`、`agentim_write_rejected_total{reason}`、`agentim_messages_committed_total{sender_type}`、`agentim_events_committed_total{type}`、`agentim_dedup_hits_total` |
| 推送 | `agentim_push_total`、`agentim_push_dropped_total`、`agentim_resync_total`、`agentim_session_queue_depth` |
| 流式 | `agentim_streams_active`、`agentim_streams_finished_total{state,reason}`、`agentim_stream_first_delta_seconds`、`agentim_stream_deltas_total` |
| AI 治理 | `agentim_guard_trips_total{reason}`、`agentim_guard_rejects_total{code}`、`agentim_policy_blocks_total{hook}`、`agentim_inbound_flags_total{flag}`、`agentim_drafts_pending`、`agentim_drafts_total{state}`、`agentim_approval_latency_seconds`、`agentim_takeovers_active`、`agentim_tool_calls_total{tool,decision}` |
| 存储 | `agentim_db_pool_connections{state}`、`agentim_storage_errors_total{kind}`、`agentim_crypto_ops_total{op}`、`agentim_kms_calls_total{result}` |
| 进程 | CPU、内存、文件描述符、运行时长 |

## 4. 健康检查、告警、面板

- `/healthz`：进程存活。
- `/readyz`：数据库可写、写线程没有卡住、写入队列没有饱和、KMS 可用（BYOK 模式）。
- 随仓库提供 Prometheus 告警规则：
  - fsync p99 > 50ms；
  - 写入队列持续饱和；
  - 推送丢弃率过高；
  - 熔断次数突增；
  - 待审批草稿超过 SLA；
  - 存储错误。
- 随仓库提供 Grafana 面板 JSON。

## 5. 测试

### 5.1 现有测试（已在 Linux CI 上通过）

| 类别 | 内容 |
|---|---|
| 单元测试（5） | 护栏、过滤 |
| 验收测试（12） | seq 连续、去重、编辑和撤回能被同步、流式三种中断、熔断、接管、读取范围、注入、DLP、16 线程并发 |
| WebSocket 端到端（2） | 鉴权、请求与响应、推送；内置 Agent 的流式回复 |
| 并发压测（3 种负载） | seq 连续、每条消息恰好一次、发送方有序、推送有序、所有客户端视图一致、流式内容完整 |
| 崩溃恢复（3 个时间点） | `kill -9` 后已 ACK 的消息全部还在 |
| 浏览器测试（14 项断言） | 完整演示流程 |

### 5.2 跨平台方案（待确认）

| 测试 | Linux | Windows | 改动 |
|---|---|---|---|
| fmt、clippy、单元、验收、WebSocket | ✅ | 🔧 | CI 矩阵加上 `windows-latest` |
| 存储一致性测试 | SQLite、PostgreSQL、MySQL | SQLite | PostgreSQL 和 MySQL 用 Linux 服务容器（GitHub 的 Windows runner 不支持服务容器） |
| 崩溃恢复 | ✅ | 🔧 | 从 bash 脚本改写成 Rust 集成测试：测试用 `CARGO_BIN_EXE_agentim` 启动服务进程，再用 `Child::kill()` 强杀（Linux 上是 SIGKILL，Windows 上是 TerminateProcess），所以两个平台用同一份代码 |
| 并发压测 | ✅ | 🔧 | 步骤统一用 `shell: bash`（Windows runner 自带 Git Bash） |
| 浏览器测试 | ✅ | 🔧 | Playwright 支持 Windows |
| 故障注入（新） | 🔧 | 🔧 | 包装一层会出错的存储：模拟提交失败、fsync 报错，断言没有未落盘的 ACK |
| 加密开 / 关对比基准（新） | 🔧 | — | 吞吐和延迟都设门槛 |
| 依赖安全（新） | 🔧 | — | `cargo audit`、`cargo deny` |

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

### 7.2 当前性能（未优化）

| 场景（CI） | 吞吐 | ACK 延迟 |
|---|---|---|
| 200 客户端 × 20 条 + 20 路流式 | 284 条/秒 | p50 387ms，p99 1.03s |

所有一致性校验都通过。瓶颈是逐条 fsync 和全局锁内的扇出，改进方案见 [02](02-architecture.md) §4。

### 7.3 已知限制

演示登录不需要密码，令牌明文存储（见 [04 §1](04-security-compliance.md)，将修复）；没有 TLS；预算存在内存里；审批草稿没有过期时间；接管锁不会超时自动交还。
