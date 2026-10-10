# 06 单机版：设计、协议与测试结果

> 状态：v0.1（2026-10-10），对应代码 [`server/`](../server)
> 目标：用最小代价验证 [05 评审](05-review.md) 提出的 v0.2 消息模型和 AI 介入模型。**不验证规模。**

## 1. 运行

```bash
cd server
cargo run --release -- --demo          # 打开 http://127.0.0.1:8080，选 alice / bob / carol 登录
```

`--demo` 会写入演示用户和会话，启动 4 个脚本化 Agent（不需要大模型），并开放按用户名免密登录（仅用于演示）。

| 演示会话 | 能看到什么 |
|---|---|
| Alice · Aria | 流式回复；`/long` 后点 ■ 打断；`/secret` 触发流式过程中的 DLP；`/stall`、`/crash` 触发看门狗；注入式语句被标为低信任 |
| 产品讨论群 | 群内 Agent 默认 `trigger=mention`：不 @ 它就收不到消息 |
| 客服：退款工单 | RefundBot 处于审批模式：alice 先耳语，再审批草稿；bob 只在批准之后才看到回复 |
| Agent 循环实验室 | 两个 Agent 互相回复，在第 7 跳被熔断，全员可见熔断事件 |
| Bob · Aria | alice 不是成员，只能经 Aria 只读观察，观察动作写入审计 |

## 2. 与 05 评审的对应关系

| 评审项 | 实现 |
|---|---|
| P0-1 会话事件序号 | `events` 表：`msg.created / msg.updated / msg.recalled / member.* / agent.state / conv.*` 都占 `conv_seq`；消息另有不变的 `created_seq` |
| P0-2 侧信道 | `side_events` 表：`whisper / ephemeral / draft.* / guard.tripped / policy.blocked`，带 `audience` 和 `anchor_seq`；主日志对每个成员都无空洞 |
| P0-3 流式状态机 | `streaming → complete / cancelled / failed`；chunk 按 `idx` 幂等；60ms 合并推送；每秒写检查点；空闲 30 秒或断开即 `failed`；启动时把残留的 `streaming` 标为 `failed(server_restart)` |
| P0-4 信任模型 | 审批、撤回、接管、打断都在服务端强制；工具审批不在单机版范围内（需要托管工具网关） |
| P0-5 读取范围与 AI 标识 | 群内 Agent 默认 `mention`，私聊默认 `all`；`sender_type` 由服务端按认证身份写入；Agent 加入时全员可见系统事件 |
| P0-6 因果链 | `causation_id` 取显式回复，否则取最近一条他人消息；`hop` 由服务端计算 |
| P0-7 去重 | `dedup` 表与消息在同一事务写入 |
| P1-1 未读数 | 只统计别人发的、未撤回的新消息 |

存储是 SQLite（WAL，`synchronous=FULL`）。每个可见变化与宣布它的事件写在同一个事务里，事务提交后才 ACK。集群版的 Raft 选型见 05 评审的 ADR-2，存储接口可以在那时抽象出来。

## 3. 协议（JSON over WebSocket）

连接：`GET /ws?token=<token>`。客户端帧：`{"op": "...", "rid": 1, ...}`；服务端对请求回 `{"type": "res", "rid": 1, "ok": true, ...}`。

| op | 用途 |
|---|---|
| `hello`、`list_convs`、`create_conv`、`add_member` | 基础 |
| `send {conv_id, client_msg_id, text, reply_to?, causation_id?, on_behalf_of?, to_user?}` | 发消息；`to_user` 表示仅某人可见（走侧信道）；审批模式下的 Agent 返回 `draft_id` |
| `edit`、`recall` | 修订与撤回（发送者，或 Agent 的所有者） |
| `pull {conv_id, after_seq, as_agent?}` | 增量拉取；Agent 和观察者拿到的是过滤后的结果，用 `upto_seq` 推进游标 |
| `pull_side {conv_id, after_side_seq}` | 拉取自己可见的侧信道事件 |
| `read`、`focus {conv_id, as_agent?}` | 已读游标；前台会话（接收流式增量，进入时先收到快照） |
| `stream_start / stream_append {msg_id, idx, text} / stream_end / stream_cancel` | 流式；任何人类成员都可以 cancel（停止按钮） |
| `whisper`、`approve {draft_id, text?}`、`reject`、`takeover`、`release`、`set_agent_policy`、`reset_guard` | Agent 所有者的介入操作 |
| `list_drafts`、`stats` | 待审批草稿；运行状态和审计日志 |

服务端推送：`event`（主日志事件；投递给 Agent 时附带 `trust` 和 `flags`）、`side`（侧信道事件）、`delta`（合并后的流式增量）、`stream_cancelled`（通知 Agent 它的流被打断）。

外部 Agent 的接入示例见 [`server/examples/agent.mjs`](../server/examples/agent.mjs)（Node 22，无依赖）。

## 4. 测试

全部在 GitHub Actions 中运行，见 [`.github/workflows/server.yml`](../.github/workflows/server.yml)。

| 类别 | 内容 |
|---|---|
| 单元测试（5） | 护栏：跳数、重复、速率；过滤：注入、DLP |
| 验收测试（12，`tests/hub.rs`） | 05 评审 §9.1 的 7 条验收项，外加审批、DLP、注入、16 线程并发写入 |
| WebSocket 端到端（2，`tests/ws.rs`） | 鉴权、请求与响应、推送、错误帧；内置 Agent 的流式回复，增量拼起来等于最终内容 |
| 并发压测（`examples/loadtest.rs`，3 种负载形态） | 多客户端并发发送，并且同一 `client_msg_id` 重试；多个 Agent 并发流式（chunk 也会重传）。校验：主日志连续、每条消息恰好一次、发送方有序、推送有序且不缺、所有客户端的日志完全一致、流式内容完整 |
| 崩溃恢复（`ci/crash_test.sh`，3 个崩溃时间点） | 发送过程中 `kill -9`，重启后逐条核对 ACK 过的消息 |
| 浏览器测试（`ci/ui_smoke.mjs`，14 项断言） | 用 Playwright 跑完整个演示流程并截图 |

### 本地实测（云端开发容器，release 构建）

| 场景 | 结果 |
|---|---|
| 50 客户端 × 100 条（+1000 次重试）+ 10 路流式 × 60 块 | 全部校验通过；573 条/秒；ACK p50 64ms，p99 153ms |
| 200 客户端 × 20 条（+800 次重试）+ 20 路流式 × 40 块 | 全部校验通过；127 条/秒；ACK p50 995ms，p99 2.5s |
| 10 客户端 × 500 条（+1000 次重试）+ 2 路流式 × 200 块 | 全部校验通过；211 条/秒；ACK p50 44ms，p99 79ms |
| `kill -9` 崩溃恢复 | 崩溃前 ACK 的 1835 条消息全部保留，日志连续 |

**结论：正确性目标达成，性能还没有优化。** 瓶颈有两个，都在预期之内：

1. **每条消息一次 fsync**：低并发时吞吐的上限由它决定。改进：组提交，即把多条消息合并到一次 fsync。
2. **全局锁内逐个连接序列化并推送**：200 人的群里每条消息要扇出 200 次，这个场景共推送约 80 万个事件。改进：事件只序列化一次，在锁外推送，并按会话拆锁。

这两项是进入集群阶段之前的第一批优化。

## 5. 截图

| | |
|---|---|
| ![流式](images/01-streaming.png) 流式生成中 | ![打断、DLP、注入](images/02-interrupt-dlp-injection.png) 打断、流式 DLP、注入检测 |
| ![看门狗](images/03-stall-timeout.png) Agent 卡住后被看门狗标记为失败 | ![熔断](images/04-loop-breaker.png) Agent 互聊在第 7 跳被熔断 |
| ![审批](images/05-approval-pending.png) 耳语 + 待审批草稿（不占 seq） | ![批准后](images/07-customer-after-approval.png) 客户只在批准后看到回复 |
| ![观察](images/09-observe.png) 所有者经 Agent 只读观察 | |

## 6. 已知限制

- 演示登录不需要密码，仅供本地演示使用；没有 TLS。
- 预算按消息数和字符数计算，存在内存中，重启后清零；还没有 token 计量。
- 没有 MCP Server、A2A、文件、搜索、大群模式、举报和拉黑。
- 审批草稿还没有过期时间；接管锁也没有超时自动交还。
