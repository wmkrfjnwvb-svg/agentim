# 链路和业务规则

怎么在每个节点上跟踪一条消息经过的每一步：链路编号、染色规则、消息轨迹、带编号的业务规则，以及对照日志和状态的校验器。

代码：`server/crates/agentim-core/src/trace.rs`（链路上下文、染色匹配）、`otel.rs`（OTLP 导出）、`hub/dye.rs`、`hub/msgtrace.rs` 和 `db/msgtrace.rs`（消息轨迹）、`hub/verify.rs` 和 `db/verify.rs`（校验器）、`metrics.rs`、`lib.rs` 里的 `rule!` 宏；客户端的链路在 `sdk/client-rust/core`。

## 1. 用途

"我的消息没到"可能有十几种原因，分散在很多节点上：
- 令牌过期；
- 权限或限流；
- 发送前拦截；
- 会话正在搬；
- 过载；
- 被禁言或被拉黑；
- 推送队列满了；
- 客户端没确认；
- Webhook 失败；
- Agent 熔断。

链路让这些原因都看得见，又不记录内容；带编号的规则让每次拒绝都说出原因。

## 2. 功能点

| 编号 | 功能 | 行为 |
|---|---|---|
| TRC-01 | 到处都有链路编号 | 每个请求一个 W3C 链路上下文，从客户端 SDK（重试、重启都是同一条链路）经过会话层、写线程批次、集群转发、扇出、Webhook（`traceparent`）和 Agent（`ctx.trace`） |
| TRC-02 | 信任边界 | 来自终端用户客户端的链路只做关联，不接着用：服务端开自己的链路，记下 `client.trace_id`，所以谁也不能逼服务端全量记录。Agent 和后端的链路接着用 |
| TRC-03 | 日志带链路 | 每行日志带 `trace_id` 和 `span_id`，在任何日志系统里都能把多个节点的日志拼起来 |
| TRC-04 | OTLP 导出 | `[tracing] otlp_endpoint`、`sample_ratio`（默认 0：日志里只有编号）。染色的链路总是导出 |
| TRC-05 | 染色规则 | 运维给一个 `user`、`device`、`conv`、`agent` 或 `api_key` 打标记，要写原因，最长 24 小时（默认 1 小时），最多 100 条。命中的请求被采样，并在每个节点上从 DEBUG 级别开始记录；它引起的后续工作也被染色。规则编号只在集群内部的 `tracestate` 里传 |
| TRC-06 | 消息轨迹 | `GET /admin/v1/messages/{msg_id}/trace` 只用存下来的数据拼出路径：收到、提交、谁能看到、哪些设备拿到了哪些没有、每个 Webhook 的状态、Agent 和它们的回复、之后的历史。只有元数据，每次查看都写审计 |
| TRC-07 | 带编号的业务规则 | 每处业务检查都是 `rule!("conv.not_member", "forbidden", …)`。拒绝在协议、开放 API、MCP 里都带 `rule`。`agentim_refusals_total{op, code, rule}` 按规则计数。目录由代码生成 |
| TRC-08 | 每次拒绝都是一条规则 | 业务代码拒绝时没有用带编号的规则，测试就失败。有规则从没被测试触发，CI 就失败（除了一个只减不增的短名单） |
| TRC-09 | 校验器 | `agentim verify [--conv] [--since]` 按当前状态回放每个会话的日志，检查不变量：序号连续、成员关系、消息状态、草稿、文件、同步位置。只读，发现问题时以非零码退出 |
| TRC-10 | 开销小 | 带链路编号，写入吞吐约少 2%；全量导出再多约 8% |

## 3. 设计

### 3.1 一条消息的路径

```
 客户端 SDK（核心生成链路，和排队的操作一起存下，重试用同一条链路）
 ① 入口：WS 客户端 / WS Agent / 开放 API / MCP / Agent 文件接口 / 管理 API / 命令行
 ② 认证：令牌、签名密钥、API 密钥、Agent 密钥；失败计数
 ③ 会话层：Agent 被停 → 只给人用的操作 → 应用权限 → 每人的频率
 ④ 发送前拦截（可选）
 ⑤ 路由：会话 → 数据库 → 分片；转发给分片的节点
 ⑥ 写线程的队列（满了 overloaded）→ 组提交的一批，每条命令一个保存点
 ⑦ 写线程里的业务检查 → 写日志、去重、文件引用、草稿和审批
 ⑧ 提交（一次 fsync）→ 回复发送者
 ⑨ 扇出：本机连接（推送或 resync）、侧信道、其他节点、实时增量
 ⑩ 接收方：推送 → 缓存 → 确认；离线设备下次同步
 ⑪ 发件箱 → Webhook 投递（重试、放弃）
 ⑫ Agent 被唤醒 → 回复（causation_id 指回去）→ 又从 ① 开始
 ⑬ 后台：保留期、文件删除、草稿过期、会话搬迁
```

业务规则标着它们的环节：③ 会话层、⑦ 写线程，以及文件存储。

### 3.2 链路经过哪里

| 位置 | 怎么带 |
|---|---|
| 客户端 SDK | 核心给每个排队的操作生成 `traceparent` 并存下。每次尝试一个新的 parent id，链路编号不变。帧里有一个可选的 `trace` |
| HTTP 入口 | 读 `traceparent`；没有或不合法就新开一条 |
| 服务端内部 | `Op` 带着上下文。写线程的批次 span 链接到批里的每条命令。集群转发和发布带着上下文 |
| 存储 | `messages.trace_id`（16 字节，可空），消息轨迹靠它查；不复制进事件，不推给人 |
| 推给 Agent | 带触发消息的链路；Agent 回复时带回来 |
| Webhook | `traceparent` 头（`webhooks.trace = true`），不带 `tracestate` 和 baggage |

### 3.3 染色规则

- 规则存在控制库里，每个节点几秒内生效，走和吊销密钥一样的通知路径。
- 按链路调高日志级别，别的请求保持原来的级别。
- 染色只记元数据（编号、环节、耗时、结果、规则编号），从不记内容、令牌、耳语。染色规则是对一个人的特别关注，所以要写原因、写审计、自动过期。

### 3.4 消息轨迹

| 部分 | 来源 |
|---|---|
| 收到 | `trace_id`、`client_msg_id`（去重窗口内） |
| 提交 | 数据库、序号、时间、状态和版本、`causation_id` |
| 谁能看到 | `visible_from ≤ seq` 的成员，加上之后被移出、当时能看到的人；挂在这个序号上的侧信道事件 |
| 送达 | 这些人的每台设备：确认位置有没有过这个序号。最多列出 100 台没送达的设备，带位置和最后在线时间 |
| Webhook | 每个事件 × 每个 Webhook：`pending`（带前面还排着几条）、`dropped`（来自审计）、`delivered` |
| Agent | 能看到它的 Agent、怎么被触发、是否被停、它们的回复和草稿 |
| 之后 | 事件历史（编辑、撤回）、举报、文件、保留期到期日 |

没有存下来、所以不显示的：哪个入口、哪个节点接的请求，发送前拦截有没有改过它。这些用链路编号去日志里查。设了 `[tracing] trace_ui_url` 时，会附上 Jaeger、Tempo 等链路界面的链接。

### 3.5 规则

- 规则编号是 `<领域>.<名字>`，每个编号只对应一个错误码。领域见[目录](../../en/business-rules.md)。
- `AGENTIM_WRITE_RULES=1 cargo test --test rules` 重新生成目录。目录过期、一个编号有两个码、业务拒绝不是规则，测试都会失败。
- `server/ci/rules-untested.txt` 列出还没有测试触发的规则。名单里的规则被测到了 CI 会失败，提醒把它移出名单；不在名单里的规则没被测到 CI 也会失败。

## 4. 接口

- 管理 API：
  - `/admin/v1/deploy/dye-rules`（列出、新增、删除；`operator` 或 `owner`）；
  - `/admin/v1/messages/{id}/trace`。
- 控制台页面：染色规则、消息轨迹。
- 命令行：`agentim verify`。
- 指标：`agentim_refusals_total`。
- 配置：`[tracing] otlp_endpoint`、`sample_ratio`、`trace_ui_url`；`[webhooks] trace`。

## 5. 规则和错误

规则前缀：`dye.*`（必须写原因、最长 24 小时、最多 100 条；[目录](../../en/business-rules.md)）。看消息轨迹需要 `owner`、`operator` 或 `moderator`，并写审计。

## 6. 测试

路径在 `server/crates/agentim-core/tests/` 下；三种数据库。

| 功能点 | 测试 |
|---|---|
| TRC-01–TRC-03 | `tracing.rs`: 全部 5 个测试 (a retry keeps the first trace; agents and backends are continued; agents get the trace by sync; invalid or missing headers start a new trace; people get a new trace that links theirs); cluster `cluster.rs`: `the_trace_crosses_nodes`; client `sdk/client-rust/core/tests/rules.rs`: `every_attempt_of_an_operation_carries_its_trace`, `a_queued_operation_keeps_its_trace_across_a_restart` |
| TRC-04 | `otlp.rs`: `sampled_and_dyed_traces_reach_the_collector` |
| TRC-05 | `dye.rs`: 全部 5 个测试; cluster `cluster.rs`: `dye_rules_reach_every_node_and_follow_the_request` |
| TRC-06 | `msgtrace.rs`: 全部 6 个测试 |
| TRC-07, TRC-08 | `rules.rs`: `the_catalog_matches_the_code`, `business_refusals_are_named_rules`, `every_rule_is_triggered_by_a_test` (CI, after every other suite); `http.rs`: `refusals_name_their_rule_and_are_counted` |
| TRC-09 | `verify.rs`: `a_clean_database_has_nothing_to_report`, `each_kind_of_damage_is_found` (mutation: each kind of damage is planted and must be found); `server/crates/agentim/tests/cli.rs`: `verify_reports_and_exits_by_what_it_finds`; CI runs `agentim verify` after the scenario and concurrency suites on all three databases |
| TRC-10 | `server/bench/single-node.sh` (paired runs; numbers in the README) |


## 7. 限制和下一步

- 链路审计没做：每晚检查每条由消息、唤醒、回复组成的链都有终点和原因。
- 还有少数规则没有被任何测试触发（`server/ci/rules-untested.txt`）。
