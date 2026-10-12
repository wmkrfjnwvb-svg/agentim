# 调研：集群的成熟做法、异常处理和社区踩过的坑

> 2026-10-12。目的：写集群之前，先看成熟组件的源码和社区问题单，把别人踩过的坑变成我们的规则和测试用例。设计见 [11 集群](../11-cluster.md)。
> 方法：克隆源码（只读）逐段阅读，再查 GitHub 问题单、官方文档和事故分析。每条结论都附出处；"推断"表示由源码或文档推出，原文没有直说；"未核实"表示没找到可靠来源。

| 方向 | 研究对象（源码版本） |
|---|---|
| 分片归属 | Temporal `30c6256`（及 Cadence） |
| 租约和切主 | Patroni `dbee9c3`、Kubernetes client-go `2a34061` 的 leaderelection、Kleppmann《How to do distributed locking》 |
| 只靠数据库协调 | Quartz `741ccc3`、Airflow `92340cf`、db-scheduler `466ff4b`、ShedLock `127dde8` |
| 节点之间通信和推送 | Redis Cluster `38087a9`、Mattermost `5dec0b8`、Matrix Synapse `c0805ea`、Centrifuge `35f9f3f` |
| 数据库切主 | PostgreSQL、MySQL 官方文档和源码（xact.c、syncrep.c），AWS / GCP / Azure 托管数据库文档，GitHub、GitLab 的事故报告 |
| 测试方法 | Jepsen 的分析报告、Elle、FoundationDB、TigerBeetle VOPR、madsim、turmoil、fail-rs、TiKV、CockroachDB roachtest、Temporal 的测试 |

## 1. 最重要的十条结论

1. **租约本身挡不住"两个主"，必须在存数据的地方校验代号。** client-go 源码原话："does not guarantee that only one client is acting as a leader (a.k.a. fencing)"（`tools/leaderelection/leaderelection.go:17-20`）。Patroni 要靠看门狗。Jepsen 测 etcd 3.4.3：健康集群里 1–3 秒的租约锁几分钟内就出现两个持有者，每 5 秒暂停一次时，被锁保护的更新丢了约 18%。Kleppmann：进程可能在"检查租约"和"写"之间暂停，所以只有存储端拒绝旧代号才可靠。**我们的优势**：锁和数据在同一个数据库，代号校验能和写入放在同一个事务里，正确性完全不依赖时间。
2. **校验必须真的和接手操作互相等待。** 普通 `SELECT epoch` 不会和接手的 `UPDATE` 冲突：PostgreSQL 默认读已提交，MySQL 可重复读下 `SELECT` 读的是旧快照。Temporal 的做法：业务事务对分片行加共享锁（`FOR SHARE` / `LOCK IN SHARE MODE`）并校验 rangeID，接手和续约加排他锁（`common/persistence/sql/execution.go:39-57`、`sql/shard.go:82-175`）。db-scheduler 用 `UPDATE … WHERE version = ?` 并要求影响行数正好是 1（`ExecutionUpdate.java:140-168`）。
3. **写入结果不确定时（超时、连接断在提交时），一律当作"可能已提交"。** Temporal 的处理（`service/history/shard/context_impl.go:1501-1550`）：停写这个分片，丢掉内存状态，自己代号加一重新接手（这一步会等旧事务结束），从数据库重新加载，再返回可重试错误。客户端用同一个幂等编号重试。MongoDB、CockroachDB、TiDB、jetcd 都因为"重试一个其实已经成功的请求"出过重复数据（Jepsen 报告）。
4. **每一次数据库调用都必须有硬超时，而且要在数据库那一侧也设超时。** Temporal 至今未修的问题 [#12246](https://github.com/temporalio/temporal/issues/12246)：PostgreSQL 切主后一条卡死的连接一直占着分片的写入资格，等待它的请求只算"排队超时"、不算数据库错误，分片永远不会被重新接手。我们用的 sqlx 没有 TCP 保活、没有读超时，也不支持多主机地址（读 sqlx 源码）。
5. **只"等从库确认再回复"还不够：从库确认之前，别人可能已经读到了。** PostgreSQL 先在本地提交、再等从库；等待被取消或连接被终止时，只给一个警告，事务已经本地可见（`syncrep.c`；[pgsql-hackers 讨论](https://www.postgresql.org/message-id/CALj2ACUrOB59QaE6%3DjF2cFAyv1MR7fzD8tr4YM5%2BOwEYG1SNzA%40mail.gmail.com)；[Patroni 文档](https://patroni.readthedocs.io/en/latest/replication_modes.html)）。应用自己等从库时更严重：提交一返回，所有人都能读到。所以读取路径要有"已确认复制的水位线"。
6. **MySQL 半同步复制超时后会悄悄退回异步**，提交照样返回成功（默认 10 秒，[官方文档](https://dev.mysql.com/doc/refman/8.4/en/replication-semisync.html)）。GitLab 的 Patroni 配置"会在切主时静默丢掉最多 1 MB 的写入"（[GitLab issue 7282](https://gitlab.com/gitlab-com/gl-infra/infrastructure/-/issues/7282)）。GitHub 2018-10-21 事故：43 秒网络分区后，旧主上有一段写入没复制过去，服务降级 24 小时 11 分钟（[事故报告](https://github.blog/news-insights/company-news/oct21-post-incident-analysis/)）。
7. **节点之间的推送只能当"加速器"。** Redis 发布订阅、Synapse 复制流、Centrifuge、Mattermost 都承认会丢。而且只靠"序号跳号"发现不了**最后一条丢了、之后又没有新消息**的情况：Centrifuge 每 40 秒比对一次流顶位置（`client.go:1492-1540`），Synapse 定期广播位置。
8. **发送队列必须有上限，超了就断开。** Redis 默认不限，非对称分区下一条连接 10 分钟涨到 16 GB 导致内存耗尽（[redis#9688](https://github.com/redis/redis/issues/9688)，修复 [PR #9774](https://github.com/redis/redis/pull/9774)）。而且要**每个方向单独判断存活**：那次事故里，健康的入站连接让坏掉的出站连接一直"活着"。
9. **时间要用数据库的，而且要用对函数。** PostgreSQL `now()` 是**事务开始**的时间，要用 `clock_timestamp()`；MySQL 默认只精确到秒，要用 `UTC_TIMESTAMP(6)` 和 `DATETIME(6)`，`TIMESTAMP` 类型有 2038 年上限。节点本地"我还是不是负责人"的截止时间，用单调时钟、从**发出续约请求之前**算起（client-go 从收到回复后算，留下了误差）。Quartz 用应用时钟，官方要求各机器时钟差在 1 秒内。
10. **很多问题不需要任何故障就会出现。** Jepsen 在 PostgreSQL 12.3、MySQL 8.0.34、RDS 上发现的隔离问题，大多在健康集群里就能复现。所以第一层就要在**无故障的高并发**下跑总校验。

## 2. 分项结论和出处

### 2.1 分片归属（Temporal）

| 做法 | 出处 | 我们 |
|---|---|---|
| 分片数在集群生命周期内固定，不能改 | [社区说明](https://community.temporal.io/t/understanding-temporal-internals/569)；`controller_impl.go:159-165` | 采用：每个数据库的分片数在初始化时固定 |
| 归属有两层："该归谁"由成员环建议，"能不能写"只由数据库条件写决定 | `ownership.go:135-149`；`context_impl.go:1164-1208` | 采用：首选节点按哈希算，写入只认数据库代号 |
| rangeID 每次加一，同时预留一段任务编号（2^20 个） | `task_key_generator.go:151-185` | 我们的序号按会话在事务里分配，不需要号段 |
| 只有带当前代号的错误才能改变分片状态，旧请求的迟到错误不能把新接手的分片弄掉 | `context_impl.go:1506-1508` | 采用 |
| 续约前先排空在途写入 | `context_impl.go:1165-1171` | 采用：续约只在两批之间执行 |
| 失去归属（被别人接手）不重试；超时等错误重试，指数退避，初始 1 秒、总共 5 分钟 | `context_impl.go:2048-2137`；修复 [PR #3584](https://github.com/temporalio/temporal/pull/3584)（之前超时会让接手直接放弃）、[PR #3625](https://github.com/temporalio/temporal/pull/3625)（测试） | 采用，并写测试 |
| 优雅下线：先退出成员环，再报不健康，再等一段时间，期间对迟到请求回"已不归我"，最后停分片 | `service/history/service.go:117-170`；[PR #4587](https://github.com/temporalio/temporal/pull/4587) | 采用；我们另外主动交出租约，新节点不用等租约到期 |
| 启动后所有首选分片接手完、再稳定 5 秒才报就绪 | `service.go:79-90` | 采用 |
| 每分钟全量对一次账，因为成员变化事件可能丢 | `ownership.go:79-108` | 采用 |
| 接手并发上限 10，从随机分片开始 | `controller_impl.go:375-474` | 采用 |
| 调用方缓存的归属在成员变化后 30 秒过期 | [PR #7556](https://github.com/temporalio/temporal/pull/7556) | 采用；连接出错时立即删除缓存 |
| 写入不受调用方取消影响，至少给 2 秒 | `context_impl.go:2410-2433` | 采用：提交不能被中途取消 |
| 第二层保护：每行还有版本号 | `execution_util.go:630-666` | 采用：`(会话, 序号)` 唯一约束兜底 |
| 社区：滚动发布时每次约 1100 条"更新分片失败"，10 分钟内平息，至今未关闭 | [#1428](https://github.com/temporalio/temporal/issues/1428) | 交接期间的"已不归我"记为警告并计数，不报错误 |
| 社区：数据库连接池关了不重建，成员心跳只打日志，节点悄悄掉出集群 29 小时 | [#11691](https://github.com/temporalio/temporal/issues/11691) | 连续失败要升级处理：退出集群或退出进程 |
| 社区：节点能回心跳但服务不了分片，路由照样发过去 | [#11108](https://github.com/temporalio/temporal/issues/11108) | 自检：写线程卡住就主动放弃分片 |
| 社区：两个集群共用一个数据库，代号来回跳 | [Cadence 排障文档](https://cadenceworkflow.io/docs/operation-guide/troubleshooting) | 租约表带集群编号，启动时检查 |

### 2.2 租约和时间（Patroni、client-go）

| 做法 | 出处 | 我们 |
|---|---|---|
| 时间参数有硬约束并在启动时检查：Patroni `loop_wait + 2 × retry_timeout ≤ ttl`（默认 10/10/30）；client-go `LeaseDuration > RenewDeadline > 1.2 × RetryPeriod`（默认 15/10/2） | `patroni/config.py:293-331`；`leaderelection.go:77-91` | 采用：`续约间隔 + 2 × 单次超时 ≤ 自停时间 < 租约时长`，启动时检查 |
| 租约时长 / 自停时间 = 能容忍的时钟速率差（15/10 = 1.5 倍） | `leaderelection.go:28-37` | 采用 1.5 |
| 续约要和业务隔离：续约和业务共用队列时，高负载下续约被饿死，进程退出 | [kueue#15537](https://github.com/kubernetes-sigs/kueue/issues/15537)、[kardinal#1592](https://github.com/pnz1990/kardinal-promoter/issues/1592) | 续约用专用连接，一条 SQL 续本节点的全部分片 |
| 错误分类必须是白名单：一个没识别的 etcd 错误绕过了保护分支，把健康的主库降级了 | [patroni#3572](https://github.com/patroni/patroni/issues/3572) | 只有"影响 0 行"立即自停；其他一切错误重试到自停时间 |
| 硬编码的 2 秒超时在节点延迟 5 秒时导致误切换 | [patroni#3550](https://github.com/patroni/patroni/issues/3550) | 所有超时都由租约时长推导 |
| 旧主卡住后醒来、发现锁空着就抢回来，覆盖了新主 | Patroni 3.1.0 修复（`ha.py:1652-1662`） | 重新接手一律代号加一 |
| 成员名重复导致两个主 | [patroni#1804](https://github.com/patroni/patroni/issues/1804) | 节点身份 = 节点编号 + 每次启动随机生成的启动编号 |
| 失去领导权只是取消一个上下文，业务停不停取决于应用；有项目失去一次后再也不参选 | `leaderelection.go:218-221`；[kuberhealthy#1533](https://github.com/kuberhealthy/kuberhealthy/issues/1533) | 失去分片要真正停写，并回到待命，可以再接手 |
| 正常退出时不释放租约，导致长时间无人负责 | [kubernetes#132599](https://github.com/kubernetes/kubernetes/issues/132599) | 优雅下线时主动交出 |
| 系统时钟往前拨 1 小时，约 2 秒内两个调度器同时工作，官方未修，只建议保持时钟同步 | [kubernetes#125761](https://github.com/kubernetes/kubernetes/issues/125761) | 本地只用单调时钟；数据库切主后数据库时钟可能跳变，加监控 |
| 计划内交接：现任停止续约、标记首选继任者（client-go）；先做检查点、确认副本追上再释放锁（Patroni） | `leaderelection.go:407-418`；`ha.py:1757-1770` | 交接时直接把租约改给目标节点，代号加一 |
| 防抖：刚释放锁的节点一段时间内不参选 | `ha.py:1635-1637`、`1910-1925` | 刚失去分片的节点一个租约时长内不抢回；非首选节点晚一个续约间隔再抢 |

### 2.3 只靠数据库协调（Quartz、Airflow、db-scheduler、ShedLock）

| 做法或坑 | 出处 | 我们 |
|---|---|---|
| Quartz、Airflow、ShedLock 都没有代号保护。Quartz 误判活节点已死后，那个节点继续执行任务，只打一行警告，代码里留着"以后再修" | `JobStoreSupport.java:3344-3349, 3406` | 我们有代号，旧节点写不进去 |
| db-scheduler 的 `version` 列就是代号，所有写都带 `AND version = ?` 并要求正好 1 行 | `ExecutionUpdate.java:140-168` | 同样做法 |
| 条件必须写进 `UPDATE` 的 `WHERE`：Quartz 在 MySQL 上"先查再写"走错了分支 | commit `8b1ea35` | 采用 |
| ShedLock 漏写 `WHERE name = ?`，一次抢锁改写了所有过期的锁 | [ShedLock#244](https://github.com/lukas-krecan/ShedLock/issues/244) | 测试：抢分片 A 不能改动分片 B（变异检查） |
| MySQL 默认返回"改变的行数"而不是"匹配的行数"；sqlx 0.9 固定打开了"匹配的行数"（`sqlx-mysql/src/connection/stream.rs:38`） | [MySQL 文档](https://dev.mysql.com/doc/c-api/8.0/en/mysql-affected-rows.html) | 校验语句让值一定改变（计数加一），并用测试钉住驱动行为 |
| 锁不存在的行再插入，MySQL 的间隙锁会死锁 | [InnoDB 锁](https://dev.mysql.com/doc/refman/8.0/en/innodb-locking.html) | 租约行在初始化时预先建好 |
| MySQL 死锁回滚**整个事务**（保存点也没了）；锁等待超时默认只回滚**那一条语句**且不释放锁；默认锁等待 50 秒 | [InnoDB 错误处理](https://dev.mysql.com/doc/refman/8.0/en/innodb-error-handling.html) | 写连接把锁等待设为 2–5 秒；死锁时整批失败；**要验证现有的按命令保存点在 MySQL 死锁时的行为**（§3） |
| Airflow：固定加锁顺序，一个 PR 实测未排序 658 次死锁、排序后 0 次；`FOR UPDATE` 会连带锁住 JOIN 进来的行 | [PR #70128](https://github.com/apache/airflow/pull/70128)、[PR #67264](https://github.com/apache/airflow/pull/67264) | 固定顺序：节点表 → 租约行（按分片号升序）→ 业务表；锁定语句不 JOIN |
| Airflow：心跳正常但进程不干活 | `RELEASE_NOTES.rst:1016-1031` | 续约由写线程自己的循环驱动，写线程卡住就不续 |
| db-scheduler：关闭时心跳先停，执行中的任务被误判死 | commit `5b6166a` | 关闭顺序：停收 → 排空提交 → 交出租约 → 最后停心跳 |
| Quartz：提交时出错，先在新事务里查数据库确认到底提交没有 | `JobStoreSupport.java:3790-3818` | 结果不确定时按幂等编号查询 |
| 本项目写线程用的是单独的连接，不经过连接池，没有连接检查 | `server/crates/agentim-core/src/storage.rs:82-99` | 重连后先重新确认租约再写 |

### 2.4 节点之间通信和推送（Redis Cluster、Mattermost、Synapse、Centrifuge）

| 做法或坑 | 出处 | 我们 |
|---|---|---|
| Redis 用 `static_assert` 冻结帧头字段的位置，留保留字段；新功能做成扩展，**只有见过对方发来带扩展标记的包，才给它发扩展**；协议版本号从没升过 | `cluster_legacy.h:229-288`；`cluster_legacy.c:2954-2956`；[PR #9530](https://github.com/redis/redis/pull/9530) | 节点间协议按能力协商；不支持的操作返回明确的 `unsupported_op` |
| Redis 7.0/7.2 混跑：新字段在旧节点上缺失，推断出的值不一致，崩溃且重启失败 | [PR #12805](https://github.com/redis/redis/pull/12805) | 缺失字段的默认值必须确定，且不能持久化成矛盾状态 |
| 未知消息类型：Redis 和 Centrifuge 忽略并记日志；Synapse 回错误并断开 | `cluster_legacy.c:3468-3470`；`node.go:871-873`；`protocol.py:254-257` | 忽略并计数，不断开 |
| IP 被另一个节点复用：用发送方编号校验，不一致就清掉地址、断开 | `cluster_legacy.c:3094-3112` | 握手校验节点编号和启动编号 |
| 被移除的节点被别的节点重新传播回来：60 秒黑名单 | `cluster_legacy.c:1837-1900` | 退役节点留墓碑 |
| Synapse 只认**同一条连接上**位置声明之后的数据；用 `prev ≤ 当前 ≤ new` 判断缺口，也能识别写入方重启后的"流倒退" | `handler.py:582-595, 700-707` | 节点间每次（重新）建连，先交换位置再收增量 |
| Synapse：先订阅成功再请求复制，否则回复丢失；连上后主动广播自己的位置 | `redis.py:152-173` | 采用 |
| Synapse 转发写请求带事务编号，超时用**同一编号**重试；回复带回流位置，调用方先等自己追上（最多 5 秒）。但它的去重缓存在内存里，目标进程重启就没了 | `replication/http/_base.py:96-134, 369-377` | 幂等必须落在数据库（`client_msg_id` 唯一约束） |
| Synapse 把缓存失效做成可补拉的流；Mattermost 失效后重建缓存失败时直接断开客户端 | `docs/tcp_replication.md:231-258`；`web_hub.go:705-720` | 安全相关的失效（拉黑、吊销、急停）写进数据库的失效日志，节点按编号拉取；和某节点断连时清掉相关缓存；加硬过期兜底 |
| Synapse：多个写入方时流位置卡在最慢的那个 | [PR #8499](https://github.com/matrix-org/synapse/pull/8499) | 我们的序号按会话在事务里用行锁分配，提交顺序等于序号顺序，不会卡 |
| Centrifuge：同一频道并发发布导致误报缺口，后来要求按频道串行 | `changelog.md:1086` | 同一会话的通知按序号顺序发出 |
| Centrifuge：被过滤（无权限）的消息用占位保留位置，否则被当成缺口 | `client.go:4840-4851` | 设备不可见的事件也要推进游标（现有同步协议已经这样做，集群下要保持） |
| Centrifuge：流在存储里丢失后重建，用"流纪元"防止新流的位置恰好接上旧位置 | `broker.go:97-108` | 会话游标带纪元；数据库从备份恢复时更换 |
| Mattermost：节点间端口不通时"看起来正常"，只是消息要刷新才出现 | [mattermost-operator#333](https://github.com/mattermost/mattermost-operator/issues/333) | 每对节点的连接状态、队列、断连次数都要有指标和告警 |
| Mattermost 节点心跳 60 秒、30 分钟才判离线；只支持相邻小版本混跑 | `cluster_discovery.go:12-14`；[HA 文档](https://docs.mattermost.com/deployment-guide/server/high-availability-cluster-based-deployment) | 心跳 2 秒、10 秒判离线；支持相邻两个版本混跑 |

### 2.5 数据库切主

| 结论 | 出处 |
|---|---|
| PostgreSQL 的 `synchronous_commit` 只有在配置了 `synchronous_standby_names` 时才会等从库；任何用户都可以按会话改它 | [runtime-config-wal](https://www.postgresql.org/docs/current/runtime-config-wal.html) |
| 没有 SQL 函数能直接返回自己这次提交的位置；提交后在同一连接查 `pg_current_wal_insert_lsn()`，得到的位置一定不小于这次提交；`pg_walfile_name()` 的前 8 位是当前时间线 | [functions-admin](https://www.postgresql.org/docs/current/functions-admin.html)；`xlogfuncs.c` |
| `pg_stat_replication` 的位置列需要 `pg_read_all_stats` 或 `pg_monitor` 权限，否则是空值（不能当作"没有从库"） | [monitoring-stats](https://www.postgresql.org/docs/current/monitoring-stats.html) |
| 从库报告的写入、落盘位置几乎是实时的 | [runtime-config-replication](https://www.postgresql.org/docs/current/runtime-config-replication.html) |
| 被提升的从库上已有的连接不会断，会从只读变成可写；`system_identifier` 主从相同，只能识别"同一个集群" | [runtime-config-preset](https://www.postgresql.org/docs/current/runtime-config-preset.html) |
| MySQL 的主库看不到每个从库执行到了哪里，不用半同步时必须直连从库；sqlx 会丢弃 MySQL 返回的 GTID 跟踪信息，只能提交后查 `@@GLOBAL.gtid_executed` | [GTID 函数](https://dev.mysql.com/doc/refman/8.4/en/gtid-functions.html)；sqlx `ok.rs` |
| MySQL 只有 `AFTER_SYNC` 等待点是无损的；账号有 `SUPER` 或 `CONNECTION_ADMIN` 时，`read_only` 挡不住写入 | [半同步](https://dev.mysql.com/doc/refman/8.4/en/replication-semisync.html)；[系统变量](https://dev.mysql.com/doc/refman/8.4/en/server-system-variables.html) |
| 主流托管高可用（RDS Multi-AZ 实例、Cloud SQL、Azure）的备库对应用不可见，切主 60–120 秒，靠 DNS 或固定 IP 切换；RDS Multi-AZ 集群是半同步；Aurora 存储层共享 | [RDS](https://docs.aws.amazon.com/AmazonRDS/latest/UserGuide/Concepts.MultiAZSingleStandby.html)、[Cloud SQL](https://docs.cloud.google.com/sql/docs/postgres/high-availability)、[Azure PG](https://learn.microsoft.com/vi-vn/azure/postgresql/flexible-server/concepts-high-availability)、[Aurora](https://docs.aws.amazon.com/AmazonRDS/latest/AuroraUserGuide/Concepts.AuroraHighAvailability.html) |
| 连接可能卡住几个小时直到操作系统的 TCP 超时；DNS 缓存会返回旧地址 | [HikariCP Rapid Recovery](https://github.com/brettwooldridge/HikariCP/wiki/Rapid-Recovery) |
| 结果不确定（PostgreSQL 有专门的错误码 `08007`）时不能用事务号去查，切主后事务号不可靠；要用和数据写在同一事务里的业务幂等编号 | [errcodes](https://github.com/postgres/postgres/blob/master/src/backend/utils/errcodes.txt)；[AWS JDBC 包装器](https://github.com/aws/aws-advanced-jdbc-wrapper/blob/main/docs/using-the-jdbc-driver/using-plugins/UsingTheFailoverPlugin.md) |
| Jepsen：RDS PostgreSQL 的从库读违反快照隔离；MySQL `innodb_flush_log_at_trx_commit=0` 会丢已提交数据 | [RDS PG 17.4](https://jepsen.io/analyses/amazon-rds-for-postgresql-17.4)、[MySQL 8.0.34](https://jepsen.io/analyses/mysql-8.0.34) |

### 2.6 测试方法

| 结论 | 出处 |
|---|---|
| Jepsen：客户端没收到确认的操作要记为"结果未知"，不能记为失败 | [Knossos](https://github.com/jepsen-io/knossos) |
| 暴露租约问题的是**进程暂停**；暴露重复的是**分区加超时**和**杀进程**；暴露"复制确认不是共识"的是**分区后再切主** | [etcd 3.4.3](https://jepsen.io/analyses/etcd-3.4.3)、[jetcd](https://jepsen.io/analyses/jetcd-0.8.2)、[CockroachDB](https://jepsen.io/analyses/cockroachdb-beta-20160829)、[Kafka](https://aphyr.com/posts/293-call-me-maybe-kafka)、[Redis](https://aphyr.com/posts/307-call-me-maybe-redis-redux) |
| 会话消息日志天然对应 Elle 的 `list-append` 模型，可以交给成熟的检查器判断"不丢、不重、顺序一致" | [Elle](https://github.com/jepsen-io/elle)、[elle-cli](https://github.com/ligurio/elle-cli) |
| Rust 的确定性模拟工具和真实数据库配合不好：turmoil 的时间按刻度前进，等真实数据库时会跑偏，同一种子也复现不了，切不断到数据库的连接；madsim 不支持 sqlx 和 Windows | turmoil `rt.rs`、`builder.rs`；[madsim](https://github.com/madsim-rs/madsim) |
| 业内做法：进程内多节点 + 真实存储 + 可注入故障的传输层（TiKV 的 `SimulateTransport`）+ 预埋故障点（fail-rs） | [TiKV transport_simulate.rs](https://github.com/tikv/tikv/blob/master/components/test_raftstore/src/transport_simulate.rs)、[fail-rs](https://github.com/tikv/fail-rs) |
| fail-rs 的 `pause` 会阻塞整个系统线程，在 tokio 里要自己写异步版本 | fail-rs `src/lib.rs` |
| Temporal 的 `ExecuteAndTimeout` 故障：操作其实成功了，但调用方收到超时 | [fault.go](https://github.com/temporalio/temporal/blob/main/common/persistence/faultinjection/fault.go) |
| CockroachDB 区分四种挂法（黑洞、崩溃、暂停、死锁），测**部分分区**，用**客户端看到的最长不可用时间**衡量切换，每种故障做 9 次 | [failover.go](https://github.com/cockroachdb/cockroach/blob/master/pkg/cmd/roachtest/tests/failover.go) |
| 自动变异工具 cargo-mutants 看不到 SQL 字符串里的条件，关键保护要留测试专用开关做手工变异 | [cargo-mutants 限制](https://github.com/sourcefrog/cargo-mutants/blob/main/book/src/limitations.md) |
| GitHub Actions 的服务容器只能在 Linux 上用；Windows 镜像预装 PostgreSQL 14 和 MySQL 8.0（默认停用）；toxiproxy 有 Windows 版 | [服务容器](https://docs.github.com/en/actions/use-cases-and-examples/using-containerized-services/about-service-containers)、[Windows 镜像](https://github.com/actions/runner-images/blob/main/images/windows/Windows2022-Readme.md)、[toxiproxy](https://github.com/Shopify/toxiproxy) |
| Bitnami 的免费镜像 2025 年起停止更新，主从测试用官方镜像加 `pg_basebackup -R` 搭建 | [Docker Hub](https://hub.docker.com/r/bitnami/postgresql) |

## 3. 调研中发现的现有代码问题（单机版也受影响）

| # | 问题 | 依据 | 处理 |
|---|---|---|---|
| E1 | **写线程的数据库连接没有超时保护**：sqlx 不设 TCP 保活和读超时，连接卡死时写线程可能一直挂住 | sqlx `net/socket/mod.rs`；Temporal #12246 | 每次数据库调用加超时；PostgreSQL 写连接设 `statement_timeout`、`idle_in_transaction_session_timeout`，MySQL 设锁等待超时；先写测试复现 |
| E2 | **MySQL 死锁时按命令保存点的批量提交行为未验证**：死锁回滚整个事务，之后的命令是否会在自动提交模式下逐条生效，需要测试确认 | §2.3；`server/crates/agentim-core/src/shard.rs` 的批量提交 | 写测试：批中途注入死锁，断言整批报错、没有部分提交、后续批正常 |
| E3 | **数据库自己做了异步主从时，单机版同样可能在切主时丢"已发送成功"的消息** | §2.5 | 和集群的"等从库确认"一起解决，单机版也启用 |
| E4 | **Windows CI 只跑 SQLite**，没有按 CLAUDE.md 跑 PostgreSQL 和 MySQL | `.github/workflows/server.yml` | 用 Windows 镜像预装的数据库或安装动作补上；主从切换测试只在 Linux 跑（服务容器限制） |

## 4. 待决事项

都已写进 [06 §1.1](../06-plan.md)。
