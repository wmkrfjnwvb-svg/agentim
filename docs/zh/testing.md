# 测试

AgentIM 怎么测试、每套测试覆盖什么、怎么运行。每个[设计页](README.md#详细设计按板块)把它的功能点对应到具体的测试；这一页是总览。

## 1. 每个业务功能都要做到

这些来自 [CLAUDE.md](../../CLAUDE.md)，每个改动都适用：

1. **覆盖功能涉及的每个逻辑节点**，不只是它自己的操作。包括权限、推送、拉取、侧信道、流式、草稿、Agent、目录、审计、重启、多个数据库。
2. **边界**：上限、空值和最大值、未知的 id、重复和幂等的调用、用户对自己操作。
3. **错误码要符合业务含义**，并保持一致：
   - `forbidden`：不允许，或不是成员；
   - `not_found`：不存在或已删除；
   - `conflict`：当前状态不允许；
   - `bad_request`：输入无效，或不适用；
   - `blocked`、`rate_limited`。

   **被拒绝的请求不在日志里留任何痕迹。**
4. **并发**：让功能和所有与它冲突的操作竞争，跑很多轮，再检查不变量：只有一个赢家、回放日志等于状态、终态之后没有任何事情发生。
5. **证明竞争测试能失败**：故意改坏被守护的代码（变异），看测试能不能抓到。
6. **三种数据库**：SQLite、PostgreSQL、MySQL，CI 里在 Linux 和 Windows 上跑。

## 2. 测试套件

| 套件 | 位置 | 测什么 | 什么时候跑 |
|---|---|---|---|
| 服务端业务测试 | `server/crates/agentim-core/tests/*.rs`（约 500 个测试函数） | 每个操作和规则、竞争、重启、多个数据库。`backend_tests!` 里的测试每种数据库各跑一次 | 每次推送，三种数据库 |
| 集群测试 | `server/crates/agentim-cluster/tests/`（36 个） | 一个库上多个节点：转发、故障接管、栅栏、交接、失效通知、故障注入、变异检查 | 每次推送，PostgreSQL 和 MySQL |
| 崩溃测试 | `server/crates/agentim/tests/crash.rs` | 流量中用 `kill -9` 杀掉真实进程；每条已确认的消息都必须在 | 每次推送，Linux 和 Windows |
| 命令行测试 | `server/crates/agentim/tests/cli.rs` | 第一次运行、对运行中的服务端执行管理命令、演示、`verify` | 每次推送 |
| 从库切主 | `tests/replicas.rs` 加 `server.yml` | 真实的 PostgreSQL 和 MySQL 主从，在负载下切主 | 每次推送 |
| 业务校验器 | 测试套件之后跑 `agentim verify` | 对测试用过的每个库，按状态回放日志 | 每次推送 |
| 规则覆盖 | `tests/rules.rs` | 目录和代码一致；每次拒绝都是一条规则；每条规则都被某个测试触发 | 每次推送，在其他套件之后 |
| 规模测试 | `.github/workflows/scale.yml` | 2 万连接加重连风暴、2000 人大群、慢消费者 | 每晚 |
| 客户端核心 | `sdk/client-rust/core/tests/` | 假时钟、假网络、假存储的确定性测试；几千个随机种子的模拟；100% 行和分支覆盖率 | 每次改 SDK |
| SDK 一致性 | `sdk/client-rust/native/tests/conformance.rs` 和各平台封装的测试 | 同一组场景对着真实服务端跑：Rust、C、Swift、Kotlin、C#、Dart、React Native、浏览器 | 每次改 SDK |
| Agent SDK | `sdk/agent_e2e.sh`、`sdk/agent-node/e2e.mjs`、`sdk/agent-python/e2e.py` | 两个真实 Agent 和一个人：流式、卡片、表单、请求、状态、转人工、重新投递、重启 | 每次推送，Linux 和 Windows |
| 服务端 SDK | `sdk/node/smoke.mjs`、`sdk/python/smoke.py`、`sdk/go/smoke` | 每个端点对着真实服务端 | 每次推送 |
| 界面组件 | `sdk/react/test/`、`sdk/react-native-ui/test/` | 组件、聊天组件、格式化、对比度、对着真实服务端的端到端、和基准图比对的截图 | 每次改界面 |
| 控制台 | `server/ci/console_smoke.mjs` | 浏览器里对着真实服务端操作控制台 | 每次推送 |
| 示例 | `examples/test/e2e.mjs` | 浏览器里跑三个示例应用 | 每次推送 |
| 文档站 | `site/scripts/complete.mjs` | 每个端点、消息、错误、规则、SDK 方法都有页面 | 每次改文档站 |
| 压测 | `server/bench/` | 吞吐和延迟，同时检查正确性 | 手动；数字在 README |

## 3. 怎么运行

```bash
cd server
cargo fmt --check && cargo clippy --all-targets -- -D warnings
cargo test                                            # SQLite
AGENTIM_TEST_PG=postgres://user:pass@127.0.0.1 \
AGENTIM_TEST_MYSQL=mysql://user:pass@127.0.0.1 cargo test   # 三种数据库
AGENTIM_WRITE_RULES=1 cargo test --test rules         # 重新生成 docs/en/business-rules.md
bench/single-node.sh                                  # 性能
```

- PostgreSQL 和 MySQL 的地址不带库名：每个测试自己建库。因为没有数据库而跳过任何测试，CI 都会失败。
- 客户端 SDK：`cd sdk/client-rust && cargo test`；各平台封装有自己的 `test.sh`。
- 界面：`cd sdk/react && npm test`。
- 示例：`cd examples && npm test`。

## 4. 服务端测试在每个场景里都检查

- 每个成员看到的序号连续，推送按顺序到达。
- 不管重试多少次，每条消息正好一条。
- 被拒绝的请求什么都没改：没有事件、没有尝试的审计、没有副作用。
- 重启之后，所有已确认的东西都在，状态不变。
- 竞争测试的每一轮之后不变量都成立：一个赢家、回放等于状态、终态之后什么都不发生。

## 5. 故障注入

服务端有故障点（`failpoint.rs`，编译进测试版本），用来模拟：
- 提交失败；
- 批次暂停；
- 回复丢失；
- 数据库黑洞和连接重置；
- 时钟跳变；
- 节点之间丢帧。

集群套件用它们证明租约和栅栏的规则。每道防线都有一个对应的"变异"测试，去掉防线，期望测试失败。

## 6. 按板块的覆盖

| 板块 | 功能点 | 页面 |
|---|---|---|
| 消息 | MSG-01 … MSG-21 | [messaging](design/messaging.md#6-测试) |
| 同步和客户端 | SYN-01 … SYN-12、CLI-01 … CLI-11 | [sync-and-clients](design/sync-and-clients.md#6-测试) |
| Agent | AGT-01 … AGT-19 | [agents](design/agents.md#6-测试) |
| 审批 | APR-01 … APR-16 | [approvals](design/approvals.md#6-测试) |
| 内置助手 | AST-01 … AST-15 | [assistants](design/assistants.md#6-测试) |
| 聊天组件 | UI-01 … UI-14 | [chat-components](design/chat-components.md#6-测试) |
| 开放 API 和 Webhook | API-01 … API-15 | [open-api](design/open-api.md#6-测试) |
| 接入身份 | ACC-01 … ACC-17 | [access](design/access.md#6-测试) |
| 安全和合规 | SEC-01 … SEC-17 | [security](design/security.md#6-测试) |
| 链路和规则 | TRC-01 … TRC-10 | [tracing](design/tracing.md#6-测试) |
| 集群 | CLU-01 … CLU-16 | [cluster](design/cluster.md#6-测试) |
| 多租户 | TEN-01 … TEN-12 | [multi-tenancy](design/multi-tenancy.md#6-测试) |
| 开发者体验 | DX-01 … DX-14 | [developer-experience](design/developer-experience.md#6-测试) |
