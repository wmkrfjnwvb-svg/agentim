# 安全和合规

传输加密和静态加密、密钥、安全工程，以及每个部署都需要的合规功能：AI 标识、数据导出和删除、保留期、举报和审核、禁言、组织单元。

代码：`server/crates/agentim-core/src/crypto.rs`（静态加密）、`storage.rs`（加密的列、`secure_delete`）、`hub/compliance.rs` 和 `logic/compliance.rs`（导出、删除、举报、审核）、`hub/export.rs`、`tls.rs`、`guard.rs`（内容过滤、DLP、注入标记）。

## 1. 用途

大多数用户在海外，所以 GDPR、DSA、欧盟 AI 法、英国网络安全法和应用商店的规则是要求，不是加分项。AgentIM 给每个部署提供满足它们的工具；即使备份或磁盘泄露，内容也是安全的。

## 2. 功能点

| 编号 | 功能 | 行为 |
|---|---|---|
| SEC-01 | TLS | 服务端自己提供 TLS（启动时检查证书），或者放在代理后面（`behind_proxy`） |
| SEC-02 | 静态加密 | 默认开启。AES-256-GCM，每个会话每 30 天一把数据密钥（DEK），由主密钥（KEK）包装。加密内容、payload、事件快照、侧信道事件、草稿、审计详情、Webhook 请求体 |
| SEC-03 | 主密钥 | 来自 `key_file` 或 `AGENTIM_MASTER_KEY`。单个 SQLite 库的部署在库旁边自动生成 `<库>.key`（0600）。主密钥不对时拒绝启动（库里存了它的指纹） |
| SEC-04 | 篡改检测 | AAD 是（表、列、行）：被挪动或改过的密文解不开，直接拒绝（fail closed） |
| SEC-05 | 加密销毁 | 解散会话时删除它的数据密钥，备份里的密文从此读不出来。销毁 Agent 时删除它自己的密钥 |
| SEC-06 | 开启、关闭、轮换 | 从关到开：新内容立即加密，`agentim encryption encrypt-existing` 加密老数据。从开到关：老内容照常可读。`agentim encryption rotate-key` 只重新包装数据密钥 |
| SEC-07 | 静态保存的密钥 | API、Agent、管理密钥只存哈希；签名密钥和连接凭据加密保存。密钥从不出现在日志和错误里 |
| SEC-08 | 日志不含内容 | 日志和指标从不包含消息内容、令牌、耳语。审计只有谁、做了什么、什么时候，不含内容 |
| SEC-09 | AI 标识 | `sender_type` 由服务端写入；消息带 `ai_generated`；Agent 加入是一条所有人可见的通知，写明负责人和读取范围（欧盟 AI 法第 50 条、GDPR 告知） |
| SEC-10 | 导出一个人的数据 | `GET /v1/users/{id}/export`（`compliance`）：边读边发的 NDJSON，可以续传。包括资料、设备、会话、本人的消息、反馈、卡片回应、举报、Agent，不包括别人的消息 |
| SEC-11 | 删除账号 | `DELETE /v1/users/{id}`：这个人立即被封住，然后每个会话的写线程清除他的内容（和撤回一样，另加 `user.erased`），他退出会话（群主转给别人），最后删除资料、设备和 Agent。重启后接着做；id 不复用 |
| SEC-12 | 保留期 | 按部署设置；旧事件连同它的衍生数据按顺序从头删除（[sync-and-clients](sync-and-clients.md)） |
| SEC-13 | 举报 | `report {target_kind, target_id, category, text?}`，对象是消息、用户、会话，保存证据快照（加密，撤回后仍在）；重复的合并；每人每小时 20 次 |
| SEC-14 | 审核队列 | 按严重度和截止时间（默认 24 小时）排序。处置：`none`、`remove_content`、`suspend_user`（天数）、`ban_user`，带原因和依据（DSA 第 17 条），同一对象所有未处理的举报一起处理。审核员只决定一次 |
| SEC-15 | 禁言 | 在一个会话里禁言一个成员 1 秒到 1 年；他能看不能说，期间 Agent 的草稿也不能被批准 |
| SEC-16 | 内容过滤 | Agent 输出的 DLP；Agent 输入的注入标记；流式过程中检查（[agents](agents.md#37-护栏)） |
| SEC-17 | 组织单元 | 可选的组织单元，有自己的库、目录可见性和策略；下层只能收紧（[messaging](messaging.md#36-存储)） |

## 3. 设计

### 3.1 静态加密

| 方面 | 做法 |
|---|---|
| 算法 | AES-256-GCM（`ring`），96 位随机 nonce。DEK 用 KEK 包装，AAD 是 DEK 的 id |
| DEK | 每个会话每 30 天一把（`data_keys`），缓存在内存里。共用的内容（审计、Webhook 发件箱）每个库每月一把。新 DEK 和用它的数据在同一个事务里提交 |
| 加密的列 | `messages.content`、`messages.payload`、`events.payload`、`side_events.data`、`drafts.content`、`drafts.payload`、`audit.detail`、`webhook_outbox.body`、举报快照、请求的 context、来源、信封、反馈原因、摘要、Agent 状态 |
| 不加密的 | ID、序号、时间、类型、成员关系、角色、发送者 ID、会话标题和资料（路由、排序、权限检查要用） |
| 格式 | `enc:v1:<key_id>:<base64(nonce‖密文‖tag)>`。没有这个前缀的值是开启加密前写的明文，照常读取 |
| 搬到另一个库 | DEK 随会话一起搬，密文原样复制 |
| 开销 | 写入吞吐约 2%（[README](../../../README.md#performance)） |

### 3.2 删除账号和并发

- 会话的写线程先把这个人标为已删除，再清除他的内容。之后排到的他的发送一律被拒，所以清除后不会再出现他的内容。
- 进度记在 `erasures`，重启后接着做完；重复请求返回同一个进度。
- 审计记录保留：它们不含内容，保留是法定义务。备份按自己的保留期自然过期。

### 3.3 合规对照

| 法规 | 要求 | 功能 |
|---|---|---|
| 欧盟 AI 法第 50 条 | 让人知道对方是 AI；机器可读的标记 | SEC-09 |
| 欧盟 AI 法第 14 条 | 人的监督 | [approvals](approvals.md) |
| GDPR 第 15、20 条 | 访问权和可携带权 | SEC-10 |
| GDPR 第 17 条 | 删除权 | SEC-11 |
| GDPR 第 5 条 | 数据最小化和存储期限 | SEC-08、SEC-12 |
| DSA 第 16、17 条 | 通知和处理；处置理由 | SEC-13、SEC-14 |
| App Store 1.2、Google Play UGC | 过滤、举报、拉黑、24 小时内处理 | SEC-13、SEC-14、SEC-16、拉黑（[messaging](messaging.md)） |
| App Store 5.1.1(v) | App 里能删除账号 | SEC-11（App 调自己的后端，后端调 API） |
| CCPA / CPRA | 访问和删除 | SEC-10、SEC-11 |
| 数据驻留 | 数据存在指定地域 | 自托管自己决定；组织单元可以有自己的库 |

### 3.4 安全工程

- **输入限制**：帧大小、JSON 深度、请求频率。
- **权限矩阵测试**：角色 × 操作 × 会话类型，正反用例都有。
- **依赖检查**：每次改依赖时、每周一次检查漏洞和许可证（`cargo-deny`、npm audit）。
- **发布物**有签名（Sigstore）和构建来源证明，镜像带 SBOM（[developer-experience](developer-experience.md)）。
- **漏洞报告**见 [`SECURITY.md`](../../../SECURITY.md)。

## 4. 接口

- 开放 API：`/v1/users/{id}/export`、`DELETE /v1/users/{id}`、`/v1/users/{id}/erasure`、`/v1/reports…`、`…/members/{user}:silence`。
- 客户端协议：`report`、`block`。
- 配置：`[encryption]`（`key_file`、模式）、`[retention]`、`[moderation]`（时限）。
- 命令行：`agentim encryption encrypt-existing|decrypt-existing|rotate-key`。

## 5. 规则和错误

规则前缀：`report.*`、`account.*`、`member.*`（禁言）（[目录](../../en/business-rules.md)）。
- `not_found`：举报自己看不到的东西。
- `bad_request`：举报自己。
- `rate_limited`：举报太多。
- `conflict`：处置已处置的举报。
- `forbidden`：已删除或被禁言时发送。

## 6. 测试

路径在 `server/crates/agentim-core/tests/` 下；三种数据库。

| 功能点 | 测试 |
|---|---|
| SEC-01 | `transport.rs`: `the_server_speaks_tls_itself`, `certificates_are_checked_at_startup` |
| SEC-02–SEC-06 | `encryption.rs`: 全部 10 个测试 (nothing readable at rest in any table, generated key for a single SQLite database, master key checks, tamper and move detection, one data key per conversation under races, shredding on dissolve, encrypting an old database in place, turning it off, rotating the master key, moving conversations with their keys); `audit.rs`: `text_that_looks_encrypted_is_kept_as_text` |
| SEC-07 | `login.rs`: `signing_keys_are_kept_encrypted_and_listed_without_secrets`; `agents.rs` (keys as hashes) |
| SEC-08 | `coverage.rs` (DLP and logs: `dlp_covers_agent_edits_and_ephemerals_but_not_people`); `dye.rs`: `only_dyed_requests_are_logged_in_full` |
| SEC-10, SEC-11 | `compliance.rs`: `export_has_the_persons_own_data`, `deleting_an_account_erases_what_the_person_wrote`, `an_interrupted_deletion_finishes_after_a_restart`, `deletion_refusals_and_repeats`, `race_sends_while_the_account_is_deleted` |
| SEC-12 | see [sync-and-clients](sync-and-clients.md#6-测试) (SYN-12) |
| SEC-13, SEC-14 | `compliance.rs`: `reports_follow_the_rules`, `the_queue_and_decisions`, `race_moderators_decide_once` |
| SEC-15 | `developer_api.rs`: `silenced_members_cannot_speak`, `silences_end_and_survive_a_restart`, `race_sends_while_a_member_is_silenced`, `a_silenced_agents_drafts_wait` |
| SEC-16 | see [agents](agents.md#6-测试) (AGT-18) |
| SEC-17 | `multidb.rs`, `membership.rs`: `the_directory_follows_membership_and_units` |


## 7. 限制和下一步

- 没做：
  - 本机磁盘上文件的应用层加密（先用磁盘或存储桶加密）；
  - 端到端加密；
  - 媒体的 C2PA 标记；
  - 年龄门槛的钩子（COPPA）；
  - DPIA 模板；
  - 被举报内容延后到保留期之后再删。
