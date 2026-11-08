# 多租户

一个部署可以服务很多互相独立的项目（租户）。每个租户在同一个进程里有自己的数据库和自己的 AgentIM 实例：自己的地址、用户、密钥、设置、Webhook 和限额。托管云用的就是这个；自托管不开 `[tenancy]` 就是一个租户，和以前完全一样。

代码：`server/crates/agentim-core/src/tenant.rs`、`hub/silos.rs`（每个租户一个实例）、`hub/tenants.rs`（部署的租户接口）、`hub/tenant_webhooks.rs`、`server.rs`（按域名路由）。

## 1. 用途

托管的项目之间要像不同的部署一样隔离，但又不能每个项目一个进程，也不能依赖只有某种数据库才有的功能（例如 PostgreSQL 的行级安全）。所以每个租户一个数据库：最简单、隔离最强，三种数据库都能用，以后搬一个租户就是搬一个库。

业内常见的做法有三种（见 AWS SaaS Lens 和 Azure SQL 的多租户模式）：
- silo：每个租户一套存储，隔离最强，成本最高；
- pool：共用表，最便宜，隔离最弱；
- bridge：按套餐混用。

AgentIM 用 silo。

## 2. 功能点

| 编号 | 功能 | 行为 |
|---|---|---|
| TEN-01 | 租户是一个完整的实例 | 自己的 `Hub`、数据库、写线程、缓存、签名密钥、API 密钥、Webhook、限额。实例内部走的就是单租户的代码路径 |
| TEN-02 | 每个租户一个库 | `[[tenancy.clusters]] name, url`，地址里的 `{tenant}` 换成租户 id：SQLite 每个租户一个文件，PostgreSQL 和 MySQL 每个租户一个库。免费和付费租户可以放在不同的数据库集群 |
| TEN-03 | 按域名路由 | `<租户>.<host_suffix>` 交给租户的实例。其他域名是部署自己（它的管理 API 和控制台）。未知或暂停的租户返回 404 |
| TEN-04 | 密钥只在自己的租户有效 | 密钥和令牌只在它所属租户的域名上有效 |
| TEN-05 | 打开实例 | 第一次用到时打开（同时来的第一批请求只打开一个实例），启动时打开所有在用的租户 |
| TEN-06 | 开通 | `POST /admin/v1/deploy/tenants {id, name, limits, cluster?}` 先建库（建目录或 `CREATE DATABASE`），再写租户记录。未知的集群什么都不留。每个租户单独建密钥和签名密钥 |
| TEN-07 | 限额在实例里算 | `connections`、`mau`（每月新的人数，超了拒绝新人第一次登录）、`requests_per_second`（WebSocket 和开放 API 合计，突发两倍）、托管的 AI 额度。改了立即生效 |
| TEN-08 | 用量 | `GET …/tenants/{id}/usage?day=`：本月去重的人数（不算 Agent）、当天的消息和 Agent 消息、文件存储量、当前连接数 |
| TEN-09 | 暂停和恢复 | 暂停时关闭实例（连接、写线程、连接池），域名返回 404，数据保留 |
| TEN-10 | 删除 | 关闭实例，删除它的库和本地文件；S3 上 `tenants/<id>/` 下的文件靠存储桶的生命周期规则删除 |
| TEN-11 | 租户的 Webhook | 存在租户自己的库里，密钥加密，每个租户最多 10 个。每个 Webhook 有自己的投递任务和游标，所以一个接收方失败只拖慢它自己的租户 |
| TEN-12 | Webhook 的安全 | 只许 `https`。内网、回环、链路本地等地址在设置时拒绝，每次投递时解析 DNS 后再查一次。连续失败 5 天，或接收方回 `webhook-delivery: disable`，就停用 |

## 3. 设计

- 部署自己的控制库只有 `tenants`（id、名字、限额、是否暂停、集群），以及部署的管理员、密钥和审计，没有任何租户的数据。
- 开了 `[tenancy]` 就必须至少配一个 `[[tenancy.clusters]]`，否则不启动。
- 每个打开的租户占一个写连接和一个小的读连接池（默认 2 个）。PostgreSQL 上有几千个活跃租户时，前面加 PgBouncer。SQLite 每个租户一个文件。
- 多租户部署目前跑在一个节点上。下一步是把租户分到多个节点（每个租户只在一个节点上打开，其他节点转发）。自托管的集群不变。

## 4. 接口

- 部署管理 API（`owner`、`operator`）：
  - `/admin/v1/deploy/tenants` 和 `/admin/v1/deploy/tenants/{id}`：创建、修改、暂停、删除；
  - `…/{id}/usage`、`…/{id}/api-keys`、`…/{id}/signing-keys`、`…/{id}/webhooks/{name}`。
- 配置：`[tenancy] host_suffix, webhooks_per_tenant, webhook_disable_after_ms`、`[[tenancy.clusters]]`。

## 5. 规则和错误

规则前缀：`tenant.*`（id、名字、集群、限额、`tenant.mau`、`tenant.rate`、`tenant.day`）和 `webhook.*`（地址、数量）（[目录](../../en/business-rules.md)）。被拒绝的修改不留痕迹。

## 6. 测试

路径在 `server/crates/agentim-core/tests/` 下；三种数据库。

| 功能点 | 测试 |
|---|---|
| TEN-01–TEN-05, TEN-09, TEN-10 | `silos.rs`: `each_tenant_is_its_own_instance`, `keys_work_only_on_their_tenants_host`, `race_first_uses_open_one_instance`, `suspension_closes_the_instance`, `tenants_survive_a_restart`, `deleting_a_tenant_drops_its_database` |
| TEN-06, TEN-08 | `tenancy.rs`: `the_deployment_api_provisions_tenants`, `tenant_ids_names_and_changes_are_checked`, `a_single_tenant_server_has_no_tenants_api` |
| TEN-07 | `silos.rs`: `plan_limits_hold_in_the_tenants_instance`; `assistants.rs`: `hosted_projects_spend_their_ai_credit` |
| TEN-11, TEN-12 | `silos.rs`: `webhooks_live_in_the_tenants_database`; `tenancy.rs`: `the_deployment_api_manages_tenant_webhooks`; `tenant_webhooks.rs`: 全部 10 个测试 (only own events, a failing receiver delays only its tenant, starts at the end, suspended tenants get nothing, turned off after failing too long or by the receiver, private addresses refused, the last places under races, refusals leave no trace, the outbox cleaned per tenant) |


## 7. 限制和下一步

- 没做：
  - 按租户的管理 API 和控制台（每个项目自己的后台）；
  - 改套餐时把租户的库搬到另一个集群；
  - 多租户部署的多节点；
  - 长时间不用的实例自动关闭。
