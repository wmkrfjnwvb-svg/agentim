# 架构

这一页给出全貌：程序、数据模型、写入路径、协议、存储和集群。每个板块另有详细设计页，这里给出链接。

## 1. 原则

1. **顺序只在会话内。** 会话是分片的单位，没有全局顺序。
2. **序号和数据在同一个事务里提交，持久化（fsync）之后才确认。**
3. **推送只是提示，拉取才是真相。** 推送可以丢，同步一定能补回。
4. **只有存储是强一致的。** 路由、在线状态、流式缓冲、缓存都是可以重建的软状态。
5. **Agent 在消息核心之外运行。** 模型慢了或出错不会拖慢核心。内置助手虽然在同一个进程里，也通过和普通 Agent 一样的协议和核心通信。
6. **过载时拒绝请求，不用无界队列。** 稳定优先于吞吐。
7. **扩展点是 trait、Webhook 和钩子**，不改服务端代码就能定制。

## 2. 程序

`agentim` 是一个 Rust 程序。同一个程序服务客户端、跑集群，也提供运维和管理命令。

```
server/crates/
  agentim/          程序入口（main.rs）
  agentim-core/     其余全部
    src/server.rs        HTTP 和 WebSocket（axum）、TLS、Origin 检查
    src/hub.rs, hub/     会话层：认证、权限、路由、扇出、副作用、Agent、助手、管理
    src/logic.rs, logic/ 在数据库写线程里运行的业务逻辑
    src/db.rs, db/       所有 SQL（别处没有 SQL）
    src/storage.rs       方言、连接、表结构和迁移
    src/protocol.rs      客户端协议（操作、帧、版本）
    src/open_api.rs      开放 API /v1
    src/mcp.rs           MCP 服务
    src/admin_api.rs     控制台和 `agentim admin` 背后的管理 API
    src/ai/              内置助手的模型厂商和连接
    src/crypto.rs        静态加密
    src/trace.rs, otel.rs  链路
  agentim-cluster/  租约、节点间转发、交接
server/console/     管理后台网页，嵌进程序里
```

请求从客户端到数据库的路径：

```
客户端 ──WebSocket──▶ server.rs ──▶ hub（会话层：认证、权限、规则检查 ③）
                                      │  读 ──▶ 读连接池 ──▶ 数据库
                                      │  写 ──▶ 会话的写线程（每个库或每个分片一个）
                                      │           logic（规则检查 ⑦，同一事务）──▶ 提交 + fsync
                                      │           提交后：确认 + 已提交的事件
                                      ▼
                                   扇出 ──▶ 每个连接一个有界发送队列
                                   Webhook ◀── 同一事务里写进的发件箱
```

## 3. 数据模型

### 3.1 会话日志

每个会话一条事件日志。`conv_seq` 是事件序号：每个事件加一、没有空洞、所有成员看到的都一样。

| 事件 | 含义 |
|---|---|
| `conv.created`、`conv.updated`、`conv.dissolved` | 会话创建、修改、解散 |
| `member.joined`、`members.joined`、`member.left`、`member.removed`、`member.policy` | 成员变化。建群时一起加入的成员每 200 人一条 `members.joined` |
| `msg.created` | 新消息。流式消息在开始时创建，`state=streaming` |
| `msg.updated` | 编辑、流式结束、审核替换（`version+1`） |
| `msg.recalled` | 撤回（内容清空） |
| `agent.state` | 接管、交还、熔断、护栏重置。所有人可见 |

每条消息另有一个不变的 `created_seq`，表示它在会话里的位置。**展示按 `created_seq`，同步按 `conv_seq`。** 所以一次增量同步就能拿到新消息和旧消息的变化。

### 3.2 侧信道

每个会话还有一条侧信道，序号是 `side_seq`，放只有部分成员能看的内容：
- 耳语、只给某个成员看的消息；
- 等待审批的草稿；
- 护栏和过滤的私下通知；
- Agent 的进度和步骤。

每个侧信道事件带 `audience`（谁能看）和 `anchor_seq`（产生时日志的最新序号）。Agent 和审批人按 `(anchor_seq, side_seq)` 合并两条日志。普通成员永远拿不到给别人的侧信道事件。所以主日志对谁都没有空洞，也不会泄露"这里有一条你看不到的消息"。

### 3.3 消息

```
Message {
  conv_id, msg_id, created_seq,
  sender_id, sender_type,   // human | agent | system —— 服务端按认证身份写入
  on_behalf_of,             // Agent 代表谁说话（它的负责人或委托人）
  kind,                     // text、image、file、agentim.card、带命名空间的自定义类型
  text, payload, version,
  state,                    // committed | streaming | complete | cancelled | failed | recalled
  state_reason,
  hop, causation_id,        // 服务端计算，用于防循环
  reply_to, mentions,
  ai_generated,             // 机器可读的 AI 标识（欧盟 AI 法第 50 条）
  trace_id,
  created_at, updated_at    // 写线程赋值
}
```

每条消息都带纯文本 `text`。自定义类型的 `text` 就是老客户端显示的降级文字。

## 4. 可靠和有序

| 机制 | 做法 |
|---|---|
| 持久化后确认 | 写线程在 `COMMIT` 返回之后才确认 |
| 去重 | `(conv_id, sender_id, client_msg_id)` 和消息写在同一个事务；重试拿到原来的结果 |
| 推拉结合 | 推送带序号；接不上的客户端收到 `resync`，用 `sync` 补齐 |
| 同一发送者的顺序 | 同一连接上的请求按顺序一个接一个处理 |
| 流式 | 开始时占序号；增量不占序号，按 `(msg_id, idx)` 幂等追加；每秒写一次检查点；结束是一条 `msg.updated` |
| 崩溃恢复 | 启动时把残留的 `streaming` 消息标为 `failed(server_restart)`，保留到最后一个检查点的内容 |

## 5. 写入路径

每个数据库（集群里是每个分片）一个写线程，组提交，提交后扇出：

```
连接（按到达顺序）                      写线程（每个库或分片一个）
──命令──▶ 有界命令队列 ──▶ ┌──────────────────────────────────────────┐
                           │ 取一条，再取走队列里已有的                  │
                           │ （最多 512 条或 1 MB；不额外等待）          │
                           │ BEGIN                                     │
                           │   SAVEPOINT; 命令1; RELEASE（失败只回滚它） │
                           │   SAVEPOINT; 命令2; RELEASE                │
                           │ COMMIT  ← 整批一次 fsync                   │
                           └──────┬────────────────────────────────────┘
                                  │ 提交后：按顺序确认和发事件
                                  ▼
                  扇出：每个事件只序列化一次，共享字节
                  → 每个连接一个有界发送队列
                  → 队列满：丢掉推送，标记 resync
```

为什么正确：

| 风险 | 处理 |
|---|---|
| 落盘前就确认 | `COMMIT` 返回后才确认和推送 |
| 一条命令失败拖垮整批 | 每条命令一个 SAVEPOINT，失败只回滚它自己 |
| 回滚的命令在序号里留下空洞 | 序号计数器是数据库里的一行，在 SAVEPOINT 里递增，会一起回滚 |
| 整批提交失败（磁盘满、fsync 报错） | 整批每条都返回错误，客户端用同一个 `client_msg_id` 重试。fsync 报错是致命的：停止写入、`/readyz` 失败、进程退出（退出码 70），靠数据库日志恢复 |
| 慢客户端耗尽内存 | 发送队列有界；满了丢推送，通知客户端重新同步 |
| 写入过载 | 命令队列有界；满了返回 `overloaded`，客户端重试（幂等） |
| 被拒绝的请求也要留一条记录（熔断事件、DLP 审计） | 命令可以标记"出错也提交" |

**瓶颈在哪。** fsync 已经不是瓶颈，瓶颈是每条 SQL 语句的固定开销：每条消息约 9 条语句，加上 SAVEPOINT。所以每个库每秒约 1,300 条。下一步：
- 减少每条命令的语句数；
- 把扇出移出写线程；
- PostgreSQL 和 MySQL 按会话开多个写线程。

目标是每节点每秒 1 万条。

## 6. 存储

- **数据库**：SQLite（内嵌，默认）、PostgreSQL（生产推荐）、MySQL/MariaDB，行为完全一样，同一套测试在三种库上都跑。
- **分层**：`storage.rs`（方言、连接、表结构、持久化检查）和 `db.rs`（全部业务 SQL）。业务逻辑看不到 SQL。
- **启动时检查持久化配置**：
  - SQLite 要求 `synchronous=FULL`；
  - PostgreSQL 要求 `fsync=on`，且 `synchronous_commit` 不是 `off`；
  - MySQL 要求 `innodb_flush_log_at_trx_commit=1`，开了 binlog 时还要 `sync_binlog=1`。

  不满足就拒绝启动，除非设了 `unsafe_durability = true`。
- **表结构迁移**：`meta.schema_version` 和 `meta.schema_compat`。新库直接建成最新结构；老库启动时逐步迁移，每一步可重入。同一时间只有一个节点迁移（advisory lock 或 `GET_LOCK`）。主版本内只做加法，所以滚动升级时新老节点可以同时在线。
- **多个数据库**（高级配置）：一个组织单元可以有自己的库。
  - 一个会话只在一个库里，控制库存目录。
  - 会话可以在线搬到另一个库，搬时校验内容哈希。
  - 一个库出问题只影响它上面的会话。

详见 [messaging](design/messaging.md)。

## 7. 协议

- **客户端协议**：WebSocket 上的 JSON，`GET /ws?v=2,1`。
  - 令牌放在请求头里（浏览器放在 WebSocket 子协议里），不放在地址里。
  - 服务端回 `welcome`：协议版本、功能、限制和 `me`。
  - 请求是带 `id` 的 `{op, …}`，回复带同样的 `id`；推送是事件。
  - 详见 [sync-and-clients](design/sync-and-clients.md)。
- **版本**：整数主版本。主版本内只做加法：新的可选字段、事件、操作、错误码、功能。客户端忽略不认识的字段，不认识的消息类型显示降级文字。
- **开放 API** `/v1` 给后端（[open-api](design/open-api.md)）。**MCP** 在 `POST /mcp` 给 Agent（[agents](design/agents.md)）。**管理 API** 给控制台和命令行（[access](design/access.md)）。
- **错误**用一组固定的码：`forbidden`、`not_found`、`conflict`、`bad_request`、`blocked`、`rate_limited`、`policy_blocked`、`budget_exceeded`、`overloaded`。每次拒绝还带上业务规则编号（[业务规则目录](../en/business-rules.md)）。

## 8. Agent 和助手在架构里的位置

- **Agent** 是带负责人的 `agent` 类用户。它像客户端一样连接（WebSocket、Agent SDK），或者通过 MCP 连接。它是会话成员，所有规则都对它生效。
- 服务端在此之上给 Agent 加了：
  - 放草稿和步骤的侧信道；
  - Agent 之间协作用的唤醒和预算；
  - 护栏（跳数上限、熔断、限流、过滤、DLP、注入标记）；
  - 接管和急停。
- **内置助手**是服务端自己跑的 Agent。每个启用的助手有一个进程内会话，走同一套协议。被唤醒时，它拼上下文、调模型厂商、流式回复、调工具；会改数据的工具等人批准。集群里每个助手只由一个节点跑（会合哈希选出），按 `client_msg_id` 去重，偶尔跑两次也无害。

详见 [agents](design/agents.md)、[approvals](design/approvals.md)、[assistants](design/assistants.md)。

## 9. 集群

集群除了数据库不需要任何组件：
- 节点平等，没有主节点，也没有选举。
- 会话分到固定数量的虚拟分片。分片由在数据库里持有租约的节点写，每批写入在同一个事务里校验租约代号。
- 任何节点都能接任何客户端，写请求转发给分片的节点。节点之间直接用 TLS 互连。
- 节点崩溃后，租约到期，分片被别的节点接手。计划内的变化（加节点、下线、滚动升级）立即交接分片。
- 数据库有从库时，等从库收到才确认，所以数据库切主不丢已确认的消息。

详见 [cluster](design/cluster.md)。

## 10. 怎么守住核心保证

| 保证 | 主要机制 |
|---|---|
| G1 稳定 | 到处都是有界队列；过载就拒绝；慢客户端重新同步；按库隔离故障 |
| G2 不丢 | 持久化后确认；去重；fsync 报错就退出；等从库；租约栅栏 |
| G3 有序 | 每个会话一个写线程；序号在事务里分配；按顺序扇出 |
| G4 正确 | 会话层查一次规则，写线程的事务里再查一次；拒绝不留痕迹；校验器回放日志 |
| G5 并发 | 组提交；事件只序列化一次；没有全局锁和序号器 |
| G6 集群 | 同一个程序；分片和租约；转发让协议不变 |
