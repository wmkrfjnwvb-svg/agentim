# 集群和数据库主从

多个 `agentim` 节点共用数据库。每个会话仍然只有一个写入者；节点故障不丢已确认的消息；数据库切主也不丢已确认的消息。除了数据库不需要任何组件：没有 ZooKeeper、etcd、Redis 或消息队列。

代码：`server/crates/agentim-cluster/`（`lib.rs` 节点、`assign.rs` 会合哈希和租约、`peer.rs` 节点间连接、`codec.rs` 帧、`ca.rs` 集群自己的证书、`sql.rs`、`admin.rs`）、`server/crates/agentim-core/src/shard.rs`、`cluster.rs`（核心里的钩子）、`replication.rs`；部署 `deploy/compose`、`deploy/helm`、`deploy/alerts.yml`。

## 1. 用途

| 解决 | 不解决 |
|---|---|
| 高可用：节点可以崩溃、升级、断网，消息不丢、不乱、不重复 | 数据库本身的高可用：那是客户的数据库或托管服务的事。AgentIM 保证数据库切主时已确认的消息还在 |
| 长连接分散到多个节点 | 跨地域多活 |
| 大群的推送分散到多个节点 | 单个数据库的写入上限（见[架构 §5](../architecture.md#5-写入路径)和多个数据库） |

## 2. 功能点

| 编号 | 功能 | 行为 |
|---|---|---|
| CLU-01 | 节点平等 | 同一个程序，每个节点角色一样。节点在数据库的节点表里互相发现。节点编号被活着的进程占用时拒绝启动 |
| CLU-02 | 分片和租约 | 每个库的会话分到 V 个虚拟分片（`hash(conv) mod V`，V = 256，初始化时定下）。另有控制分片（用户、令牌、Agent、密钥、目录）和维护分片。分片只能由在数据库里持有它租约的节点写 |
| CLU-03 | 每批写入都有栅栏 | 每批写入的最后一条语句，在同一个事务里校验批里每个分片的租约代号和持有者。旧的持有者什么都提交不了 |
| CLU-04 | 转发 | 任何节点接任何客户端。读在收到的节点做；写转给分片的持有者。分片在移动时请求最多跟随 4 秒。结果不确定时返回 `unavailable`，从不说"失败"，也从不重发：客户端用同一个 id 重试 |
| CLU-05 | 故障接管 | 崩溃节点的分片在租约到期（15 秒）后被接手。持有者 10 秒没续上租约就自己停写 |
| CLU-06 | 计划内的变化 | 加节点、下线、排空、滚动升级时，先排空写线程，再立即交出分片，不让任何请求失败。分片按首选节点（会合哈希）一次几个地重新分配 |
| CLU-07 | 事件通知 | 提交后，持有者按每个会话的序号顺序把事件发给其他所有节点。丢了一条通知只会变慢：客户端靠序号和 `heads` 位置校验发现缺口 |
| CLU-08 | 缓存失效 | 拉黑、吊销令牌和密钥、停用账号、急停和销毁 Agent、签名密钥、染色规则、设置，立即到达每个节点。和其他节点断开的节点会轮询数据库，3 秒内生效 |
| CLU-09 | 节点间连接 | 每个节点到每个其他节点一条 TLS 连接，用集群自己的 CA（加密存在数据库里）。握手检查集群、节点和版本。每 2 秒心跳；队列有界 |
| CLU-10 | 数据库时钟 | 所有租约时间都在 SQL 里取数据库的时钟；本地时钟只用来给节点自己的自停计时 |
| CLU-11 | 启动检查 | 拒绝不对的配置：时间参数、没有共享存储的文件、被占用的节点编号、集群里用 SQLite |
| CLU-12 | 数据库从库 | 启动时识别三种模式：没有从库、数据库端同步、应用等待异步从库。托管服务可以声明 `managed_sync`。写入要从库拿到才确认 |
| CLU-13 | 读取水位线 | 应用等待从库时，读只返回到 `confirmed_seq`，所以没人会看到一条切主后可能消失的消息 |
| CLU-14 | 数据库切主 | 记下主库身份；只读类错误和断开的连接让这一批按"结果不确定"结束；写线程重连并确认连到的是可写的主库；切主次数计入指标 |
| CLU-15 | 运维 | `agentim cluster status`、`agentim cluster drain --node`、按库的健康和就绪检查、Prometheus 指标和告警、三节点 Compose 文件和 Helm Chart |
| CLU-16 | 新老版本混跑 | 每条连接协商能力；不认识的帧和命令返回 `unsupported_op`，所以能滚动升级 |

## 3. 设计

### 3.1 正确性规则

| # | 规则 | 防住什么 |
|---|---|---|
| 1 | 写入提交（且复制到从库，§3.6）后才确认，然后才推送 | 任何时刻崩溃都不丢已确认的消息 |
| 2 | 每个会话任何时刻只有一个写入者；序号在事务里分配 | 乱序和重复的序号 |
| 3 | 每批写入在同一事务里校验租约代号 | 卡住的旧持有者醒来后还在写 |
| 4 | 幂等落在数据库（`client_msg_id` 唯一） | 结果不确定时产生重复 |
| 5 | 结果不确定的写入一律当作"可能已提交" | 误以为失败导致重复或丢失 |
| 6 | 节点之间只传请求和通知，不传数据 | 节点间网络出问题破坏正确性 |
| 7 | 读只返回已确认复制的部分 | 一条看到过的消息在数据库切主后消失 |
| 8 | `(conv, seq)` 唯一，作为第二道防线 | 分片逻辑有缺陷时写出重复的序号 |

### 3.2 租约

```sql
cluster_meta(k PK, v)
cluster_nodes(node_id PK, boot_id, addr, slot UNIQUE, version, state, heartbeat_at)
shard_lease(db, shard_id, PK(db, shard_id), owner /* node_id:boot_id */, epoch, expires_at, fence_count)
```

| 操作 | 怎么做 |
|---|---|
| 接手 | `UPDATE … SET owner, epoch=epoch+1, expires_at=db_now+D WHERE … AND (owner IS NULL OR expires_at < db_now)`。正好一行才算成功 |
| 续约 | 一条语句续本节点持有的全部分片（`expires_at > db_now`）。结果里少了的分片立即停写 |
| 交接 | 排空后：`UPDATE … SET owner=目标, epoch=epoch+1 … WHERE owner=我 AND epoch=当前`，再用 `CHECK` 帧通知目标，目标立即可以写 |
| 释放 | 下线时 `owner=NULL`，别人立即可以接手 |
| 栅栏（每批） | 提交前的最后一条语句：`UPDATE shard_lease SET fence_count=fence_count+1 WHERE db=? AND (shard_id, owner, epoch) IN (…)`。影响行数必须等于这批涉及的分片数，否则整批回滚，这些分片停写 |

栅栏持有租约行的锁直到提交，接手者必须等它。所以旧持有者的提交和新持有者的接手一定有先后，两个代号不可能都提交成功。代号用"等于"比较，所以切主后租约表回退时，节点会停写并重新接手。

| 时间 | 默认 | 约束 |
|---|---|---|
| 租约 D | 15 秒 | 最坏的接手时间约 D + 3 秒 |
| 续约间隔 R | 3 秒 | 到期前有多次重试 |
| 数据库调用超时 T | 2 秒 | 每次调用都有 |
| 自停 F | 从发出续约请求之前算起 10 秒（单调时钟） | `R + 2T ≤ F < D`，`D / F ≥ 1.5`；启动时检查 |
| 锁等待 | 2 秒（`lock_timeout`、`innodb_lock_wait_timeout`） | |

**进度检查**让卡住（不提交）的写线程交出它的分片。

### 3.3 路由

| 请求 | 在哪执行 |
|---|---|
| 读（同步、历史、搜索、上下文、列表、未读） | 收到请求的节点（不超过水位线） |
| 写某个会话 | 它所在分片的持有者 |
| 写控制库 | 控制分片的持有者 |
| 跨分片的操作（建会话、解散、销毁） | 每一步发给对应的持有者；修复逻辑和单机一样 |

被拒绝的请求经过转发后，码和规则不变，也不留痕迹。

### 3.4 节点间连接

```
帧头（16 字节）："AIMB" · 帧体长度（u32 大端）· 编码版本 · 消息类型 · 标志 · 保留
帧体：JSON（新字段可选，未知字段忽略）
HELLO · FORWARD_REQ · FORWARD_RESP（ok | 错误码 | not_owner | unavailable | unsupported_op）
PUBLISH（事件和失效通知）· CHECK · PING/PONG
```

| 方面 | 规则 |
|---|---|
| 存活 | 每 2 秒 PING/PONG；10 秒没有帧就断开 |
| 重连 | 退避 100 毫秒到 5 秒，带抖动 |
| 发送队列 | 每条连接 10 万帧。满了就断开，对端的客户端重新同步。写线程只往队列里放，从不等对端 |
| 帧上限 | 8 MB |
| 对端重启 | 启动编号变了：它待处理的转发按结果不确定处理，它迟到的帧丢掉 |

### 3.5 生命周期

- **加入**：新节点被看到两个续约间隔后才分到分片；之后每轮每个库最多接手 10 个、交出 4 个。
- **排空**（`agentim cluster drain` 或收到关闭信号）：
  1. 不再接新的客户端。
  2. 交出所有分片。
  3. 给客户端发带随机等待时间的 `goaway`。
  4. 退出。
- **崩溃**：租约到期后其他节点接手分片。首选节点先抢，其他节点晚一个间隔。

### 3.6 从库

| 模式 | 识别 | 提交后什么时候算已复制 |
|---|---|---|
| 没有从库 | 看不到从库，或 `mode = "none"` | 提交即算 |
| 数据库端同步 | PostgreSQL 配了 `synchronous_standby_names` 且 `synchronous_commit ≥ on`；MySQL 半同步 `AFTER_SYNC` | 提交即算。MySQL 每批检查，退回异步时扣住结果 |
| **应用等待**（异步从库时的默认） | 看得到从库但没有同步配置 | 所有正在复制的从库（或 `replicas_required` 台）都刷盘了这次提交的 LSN 或 GTID |
| 托管（声明） | `mode = "managed_sync"`（RDS Multi-AZ、Cloud SQL 高可用、Azure） | 提交即算；声明写审计 |

- **应该有从库时一台都没有，算"不满足"**，不算"不用等"。所以被隔离的旧主永远不能确认。
- **超时**（`wait_ms`，5 秒）：这批在主库已提交但未确认。它的事件、推送、唤醒都被扣住，客户端收到 `unavailable`；用同一个 id 重试会找到这条消息并等到确认。从不悄悄降级。
- **续约**不等从库，所以从库慢不会让节点丢掉租约。

## 4. 接口

- 配置：
  - `[cluster]`：`node_id`、`addr`、集群密钥、初始化时的分片数、时间参数；
  - `[storage.databases.replication]`：`mode`、`wait_ms`、`replicas_required`、`replica_urls`。
- 命令行：`agentim cluster status`、`agentim cluster drain --node <id>`。
- 管理 API：`/admin/v1/deploy/cluster`（节点、排空）、`/admin/v1/deploy/status`（数据库和从库模式）。
- 健康检查：`/healthz`、`/readyz`（按库）。
- 指标：
  - `agentim_cluster_*`（持有、转发、跟随超时）；
  - `agentim_db_failovers_total`、等从库的情况。

## 5. 规则和错误

- `unavailable` 表示结果不确定，用同一个 id 重试。
- `not_owner` 只在集群内部用。
- `unsupported_op` 来自更老的节点。
- 业务错误和单机完全一样。

## 6. 测试

`server/crates/agentim-cluster/tests/`：一个进程里起多个节点，PostgreSQL 和 MySQL，用故障点注入故障。

| 功能点 | 测试 |
|---|---|
| CLU-01, CLU-04, CLU-07 | `cluster.rs`: `requests_are_forwarded_and_pushes_reach_every_node`, `refusals_are_the_same_through_forwarding`, `one_message_per_client_msg_id_across_nodes`; `operations.rs`: `held_and_delayed_notifications_arrive_in_order`, `heads_from_any_node_show_where_conversations_stand` |
| CLU-02, CLU-03, CLU-05 | `cluster.rs`: `a_crashed_holder_is_replaced_without_loss_or_duplicates`, `a_stale_holder_commits_nothing`; `faults.rs`: `a_crash_under_concurrent_traffic_loses_nothing`, `a_batch_paused_before_its_check_leaves_no_trace`, `a_clock_jump_never_extends_holding`, `a_holder_in_a_database_blackhole_stops_writing`, `a_holder_whose_database_connections_reset_stops_writing`, `cut_off_from_the_database`, `nodes_cut_off_from_each_other_keep_one_holder`, `every_node_restarts`, `an_answer_lost_after_commit_is_found_by_the_retry`, `frames_lost_one_way_give_a_retryable_answer`; mutation checks: `mutation_without_the_check_a_stale_holder_writes`, `mutation_without_the_progress_gate_a_stuck_writer_keeps_its_shards` |
| CLU-06 | `cluster.rs`: `a_joining_node_gets_its_share_while_traffic_flows`, `leaving_hands_shards_over_at_once`; `operations.rs`: `nodes_joining_under_traffic_cost_no_request`, `a_node_leaving_under_traffic_costs_no_request`, `an_operator_drains_a_node_under_traffic`, `failing_renewals_stop_a_node_taking_clients` |
| CLU-08 | `cluster.rs`: `revocations_and_blocks_reach_every_node`, `signed_tokens_and_logouts_reach_every_node`, `dye_rules_reach_every_node_and_follow_the_request`; `faults.rs`: `revocations_reach_cut_off_nodes_within_3s`; `operations.rs`: `api_keys_created_or_revoked_on_one_node_change_at_once_on_the_others`, `mutation_without_the_cache_poll_a_cut_off_node_keeps_a_revoked_token` |
| CLU-11 | `cluster.rs`: `start_up_refuses_bad_setups`; `operations.rs`: `files_need_a_shared_store_in_a_cluster` |
| CLU-12–CLU-14 | `server/crates/agentim-core/tests/replicas.rs`: `postgres_replicas_keep_acknowledged_writes`, `mysql_replicas_keep_acknowledged_writes` (real primary/replica pairs, failover in CI); `.github/workflows/server.yml` "acknowledged writes survive a failover" |
| CLU-16 | `operations.rs`: `commands_this_version_does_not_know_are_unsupported`, `outputs_of_a_newer_node_are_skipped_one_by_one` |
| Tracing | `cluster.rs`: `the_trace_crosses_nodes` |
| Load | `server/bench/cluster.sh` (one and three nodes; numbers in the README) |


## 7. 限制和下一步

- 没做：
  - 防线被绕过时的检测和修复：切主时从库也没有的数据导致数据库回退；
  - 手动移动单个分片（加节点、排空时的自动再平衡已经覆盖运维需要）。
- 目前的集群数据都在一台机器上测的；下一步在多台机器上测。
