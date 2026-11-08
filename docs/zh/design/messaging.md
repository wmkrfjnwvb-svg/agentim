# 消息

会话、事件日志、消息、成员、文件、扩展和存储。

代码：`server/crates/agentim-core/src/logic.rs` 和 `logic/`（写线程里的业务逻辑）、`hub.rs`（会话层）、`db.rs`（SQL）、`storage.rs`（数据库和表结构）、`filestore.rs` 和 `hub/files.rs`（文件）。

## 1. 用途

消息是其他所有板块的基础。它给每个会话保存一份持久、连续、所有成员看到都一样的记录，并强制谁能读、谁能写。人、Agent、系统都走同一条写入路径。

## 2. 功能点

| 编号 | 功能 | 行为 |
|---|---|---|
| MSG-01 | 单聊 | 每对用户只有一个：再建一次返回已有的。成员固定 |
| MSG-02 | 群 | 建群时带标题和成员；一起加入的成员每 200 人一条 `members.joined`。有人数上限（`clients.max_group_members`） |
| MSG-03 | 发消息 | 文字最多 64 KB，`reply_to`（只能指向同一会话），@ 按完整名字匹配。按 `(conv_id, 发送者, client_msg_id)` 去重 |
| MSG-04 | 编辑 | 带版本（`version+1`）。可以要求指定版本，旧版本被拒。重新计算 @。可以配置编辑时限 |
| MSG-05 | 撤回 | 所有地方的内容都清掉，包括之前的事件快照、搜索和草稿。只有一次撤回生效。可以配置撤回时限 |
| MSG-06 | 流式 | 回复开始时占序号；增量不占序号且幂等；结束是一条 `msg.updated`。上限：256 KB、10 分钟（看门狗结束超时的流）、每个 Agent 每个会话 2 路并发 |
| MSG-07 | 侧信道 | 耳语（负责人和自己的 Agent 之间）、只给某个成员的消息（`to_user`）、草稿、私下通知。主日志对谁都没有空洞 |
| MSG-08 | 已读和未读 | 每个成员一个已读位置，多端共享。未读只算别人发的、自己能看到的消息。自己发消息算已读 |
| MSG-09 | 成员和角色 | 群主、管理员、成员。管理员能拉人、移除普通成员；成员可以拉自己的 Agent。被移出、退出后立刻没有访问权 |
| MSG-10 | 转让和解散 | 群主转让群主或解散会话；解散删除内容并停掉进行中的工作 |
| MSG-11 | 拉黑 | 被拉黑的人不能和你开单聊、不能给你发仅你可见的消息、@ 你不会通知你。共同的群不受影响 |
| MSG-12 | 用户目录 | 只能看到有共同会话的人、自己的 Agent、单元允许发现的人。`find_user` 按精确的名字或邮箱查 |
| MSG-13 | 自定义消息类型 | 带命名空间的 `kind` 加 JSON `payload`（≤ 32 KiB），和文字一样存储、推送、同步、编辑、撤回。`text` 是老客户端显示的降级文字 |
| MSG-14 | 信号 | `typing` 或自定义类型，只发给此刻在线的成员，不落库。每人每秒 20 个 |
| MSG-15 | 文件和图片 | 分片直传 S3 或服务端磁盘；缩略图在设备上生成；去掉位置信息；一个文件只属于一条消息；每次下载都签名并检查权限 |
| MSG-16 | 保留期 | 按应用设置保留期：旧事件被删除，落在后面的设备重新开始 |
| MSG-17 | 多个数据库 | 一个组织单元可以有自己的库。会话可以在线在库之间搬 |
| MSG-18 | 持久性 | 持久化提交后才确认；提交失败什么都不确认并停止写入；启动时检查数据库的持久化配置 |

## 3. 设计

### 3.1 日志、侧信道和消息

模型见[架构 §3](../architecture.md#3-数据模型)：每个会话一条事件日志（`conv_seq`）、一条侧信道（`side_seq`、`audience`、`anchor_seq`），消息有固定的 `created_seq`。`sender_type` 由服务端按认证身份写入；`on_behalf_of` 只允许 Agent 代表它的负责人说话。

### 3.2 成员

| 操作 | 谁能做 | 结果 |
|---|---|---|
| `add_member` | 群主和管理员；成员可以拉自己的 Agent | `member.joined` |
| `remove_member` | 群主和管理员；成员可以移除自己拉进来的 Agent | `member.removed`。被移出的人收不到推送，也不能再读这个会话 |
| `leave` | 任何成员（群主要先转让） | `member.left` |
| `set_role` | 群主 | 群主、管理员、成员 |
| `transfer_owner` | 群主 | 换群主 |
| `dissolve` | 群主 | `conv.dissolved`。所有成员失去访问；内容删除；审计里留一条"这个会话存在过" |
| `block`、`unblock` | 任何人 | 见 MSG-11。应用商店要求有拉黑 |

Agent 被移出时：取消它进行中的流，丢弃它待审批的草稿，结束它的任务（原因"已移除"）。

**并发不变量**：`member.removed` 的序号是 N 时，被移出的人不会有序号大于 N 的消息。移除和这个人的发送同时发生时只有两种结果：消息在移除之前提交，或者发送被拒。这由单写线程保证：两个操作在同一个写线程里先后执行。

### 3.3 流式

- 流以 `msg.created`（`state=streaming`）开始，占住序号。
- 增量推给正在看（focus）的成员，按 `(msg_id, idx)` 追加，重复的增量什么都不做。
- 每秒写一次检查点；晚进入的成员拿到当前已生成的内容。
- 以一条 `msg.updated` 结束（`complete`、`cancelled` 或 `failed`）。崩溃留下的流变成 `failed(server_restart)`，保留最后一个检查点的内容。
- 能打断流的人：发送者、会话的群主和管理员、Agent 的负责人。

### 3.4 扩展

| 扩展 | 规则 |
|---|---|
| 自定义类型 | `kind` 至少两段、用点分隔，每段以小写字母开头、只含 `a-z0-9_-`，最多 64 字节；`agentim.` 保留。`payload` 是最多 32 KiB 的 JSON 对象。内容策略（DLP、注入标记）也检查 payload。草稿保留类型和 payload |
| 信号 | `signal {conv_id, kind, data?}`，`data` ≤ 4 KiB。不落库、不占序号。非成员 `forbidden`；单聊被拉黑 `blocked`；每秒超过 20 个 `rate_limited` |
| 自定义字段 | 用户和会话上各一个最多 4 KiB 的 JSON 对象 |
| Webhook 和发送前拦截 | 见 [open-api](open-api.md) |

### 3.5 文件

本机磁盘和 S3 是同一套流程，只是地址不同。

1. `upload_start {conv_id, name, size, mime?, thumb?}`。服务端检查：
   - 发送者能不能在这个会话发言（被移出、被拉黑、已解散、草稿模式的 Agent 和发消息一样被拒）；
   - 大小（最多 `files.max_bytes`，默认 4 GiB；空文件拒绝）；
   - 文件名（255 字节；去掉路径和控制字符）。

   返回 `file_id`、片大小和签名地址。S3 是预签名的 PUT（64 MiB 以上每片 16 MiB）；磁盘是服务端自己的、HMAC 签名的 `PUT /files/upload/{file_id}/{n}`。地址 1 小时有效。
2. 客户端 PUT 各片，最多同时 4 片，换网络后断点续传。
3. `upload_complete {file_id, parts}`。服务端检查：
   - 大小；
   - 看开头 64 字节判断的真实类型；
   - 缩略图（JPEG 或 PNG，最多 320 像素）。

   都对才提交 `files` 记录；不对就 `bad_request`，并删掉已经传上去的内容。
4. 类型为 `image` 或 `file` 的消息带上 `file_id`。一个文件只属于一条消息：两条消息抢同一个文件只有一条成功（`conflict`）。

Agent、MCP 和后端也可以一次传整个文件（最多 64 MiB），由服务端去掉 EXIF 和 GPS、生成缩略图。

| 方面 | 规则 |
|---|---|
| 存储 | `[files] storage = "local"`（默认 `./data/files`）或 `"s3"`（任何兼容 S3 的存储）。集群要用 S3 或共享磁盘 |
| 图片 | JPEG、PNG、WebP、GIF 是图片；其余都是文件，下载时作为附件，带 `nosniff` 和 sandbox CSP。SVG 当文件 |
| 缩略图 | 在设备上生成：长边 320 像素，按 EXIF 转正，JPEG 或 PNG。原生平台由共用的 Rust 运行层生成，浏览器用 `createImageBitmap` |
| 位置信息 | 默认上传前去掉 EXIF 和 XMP，不重新编码。部署可以禁止保留 |
| 下载 | `file_url {file_id, variant}` 每次都检查权限：上传者本人，或能看到这条消息的人。返回 5 分钟有效的地址（预签名 GET，或服务端支持 `Range` 的签名地址） |
| 删除 | 撤回、拒绝草稿、解散、删除账号、保留期：记录和 `file_trash` 在同一事务里提交；后台任务删内容，失败退避重试 |
| 过期 | 24 小时内没被消息引用的文件删除。`files.retention_days` 单独设文件保留期 |
| 限制 | `files.max_bytes`、`files.daily_bytes_per_user`、`files.max_open_per_user`（同时 10 个上传） |

### 3.6 存储

- **持久化检查和表结构迁移**：见[架构 §6](../architecture.md#6-存储)。
- **多个数据库**：控制库存目录（`conv_id` → 单元 → 数据库）和成员索引，不存内容。
  - 一个会话只在一个库里，元数据和内容从不分开。
  - 跨库的列表在服务端合并。
  - **搬会话**：先只读，复制数据并用内容哈希校验，切换目录，再删旧副本。搬的过程中写入返回 `conv_migrating`，客户端用同一个 `client_msg_id` 重试。
  - 搬到一半崩溃，启动时修复：目录指向完整的那份，删掉残留的另一份。
- **SQLite `secure_delete`**：删除的内容不留在空闲页里。

## 4. 接口

| 接口 | 操作 |
|---|---|
| 客户端协议 | `create_conv`、`update_conv`、`list_convs`、`send`、`whisper`、`edit`、`recall`、`stream_start`、`stream_append`、`stream_end`、`stream_cancel`、`focus`、`read`、`unread`、`pull`、`pull_side`、`add_member`、`remove_member`、`leave`、`set_role`、`transfer_owner`、`dissolve`、`block`、`unblock`、`list_blocks`、`find_user`、`signal`、`upload_start`、`upload_parts`、`upload_complete`、`file_url` |
| 开放 API | `/v1/conversations…`、`/v1/conversations/{id}/messages…`、`/v1/uploads`、`/v1/conversations/{id}/files`（[open-api](open-api.md)） |
| 事件 | 日志里的 `conv.*`、`member(s).*`、`msg.*`；侧信道上的 `side` 事件 |

完整的协议参考由代码生成到文档站（`site/` 的 "Client protocol"）。

## 5. 规则和错误

[业务规则目录](../../en/business-rules.md)里的前缀：
- `conv.*` 会话；
- `member.*` 成员；
- `block.*` 拉黑；
- `msg.*` 消息；
- `stream.*` 流式；
- `file.*` 文件；
- `sync.*` 同步。

常见的码：
- `forbidden`：不是成员，或角色没有权限；
- `not_found`：会话、消息、文件不存在；
- `conflict`：版本过期、已经撤回、文件已被使用；
- `bad_request`：超出限制或输入不对；
- `blocked`；
- `rate_limited`。

被拒绝的请求不在日志里留任何东西。

## 6. 测试

服务端测试除特别说明外都在 SQLite、PostgreSQL、MySQL 上跑。路径在 `server/crates/agentim-core/tests/` 下。

| 功能点 | 测试 |
|---|---|
| MSG-01 | `membership.rs`: `one_direct_conversation_per_pair`, `direct_conversations_have_fixed_members`, `race_direct_conversation_creation` |
| MSG-02 | `scenarios.rs`: `conversation_creation_is_validated`; `membership.rs`: `a_large_group_starts_with_a_few_events`, `race_members_act_while_a_large_group_is_created`; `audit.rs`: `groups_have_a_size_limit`; `edges.rs`: `conversation_input_limits`, `a_failed_conversation_creation_leaves_no_trace` |
| MSG-03 | `scenarios.rs`: `message_size_is_limited_in_bytes_and_content_round_trips_exactly`, `retries_are_deduplicated_per_sender_and_conversation`, `replies_must_point_into_the_same_conversation`, `mentions_match_whole_names`; `concurrency.rs`: `a_retry_from_many_devices_creates_one_message`; `edges.rs`: `client_msg_id_limits`; `audit.rs`: `a_retry_gets_its_original_result_whatever_changed`; `hub.rs`: `concurrent_senders_get_unique_contiguous_seqs_and_share_commits` |
| MSG-04 | `scenarios.rs`: `edits_are_versioned_and_restricted`; `audit.rs`: `edits_can_require_a_version`, `edits_recompute_mentions`, `edits_of_older_messages_stay_hidden_from_new_members`; `coverage.rs`: `edit_boundaries`; `concurrency.rs`: `concurrent_edits_keep_a_linear_version_history`; `permissions.rs`: `edit_and_recall_windows` |
| MSG-05 | `scenarios.rs`: `recall_is_restricted_and_not_repeatable`, `recall_removes_the_content_from_history`, `recalling_a_live_stream_cancels_it`; `concurrency.rs`: `one_recall_wins`, `recall_racing_edits_leaves_no_content_and_nothing_after_it`; `audit.rs`: `a_recalled_message_leaves_no_draft_copy` |
| MSG-06 | `scenarios.rs`: `stream_permissions_and_chunk_rules`, `stream_size_limit_is_enforced`, `overlong_streams_are_closed_by_the_watchdog`, `streams_can_be_interrupted_by_the_right_people`, `late_focus_gets_the_stream_so_far`; `hub.rs`: `interrupted_streams_end_as_failed_and_keep_partial_content`, `edits_recalls_and_stream_completion_reach_already_synced_clients`; `concurrency.rs`: `duplicate_chunks_from_two_connections_are_applied_once`, `stream_end_racing_cancel_finishes_once`; `audit.rs`: `streamed_replies_mention_like_messages`; `ws.rs`: `demo_agent_streams_a_reply_with_coalesced_deltas` |
| MSG-07 | `hub.rs`: `main_log_is_gap_free_for_members_despite_whispers_ephemerals_and_drafts`; `scenarios.rs`: `ephemeral_messages_reach_only_their_audience`, `whisper_is_only_between_an_owner_and_its_agent`; `coverage.rs`: `side_channel_paging_and_read_errors`, `mentions_ignore_non_members_and_ephemerals_to_yourself`; `audit.rs`: `targeted_messages_and_drafts_are_deduplicated` |
| MSG-08 | `scenarios.rs`: `read_cursors_drive_unread_counts`; `audit.rs`: `unread_counts_start_at_what_a_member_can_see`, `writing_counts_as_reading`; `concurrency.rs`: `concurrent_read_cursors_keep_the_maximum`; `sync.rs`: `unread_counts`, `read_and_mute_state_reach_every_device` |
| MSG-09 | `scenarios.rs`: `only_members_can_read_or_write_a_conversation`, `only_the_owner_adds_members_and_members_may_add_their_own_agents`; `membership.rs`: `roles_decide_who_can_manage_members`, `role_rules_at_the_edges`, `removed_members_lose_access_and_are_told`, `removed_members_cannot_act_on_anything_in_the_conversation`, `removing_an_agent_stops_its_work`, `race_add_and_remove_keep_log_and_state_consistent`, `race_remove_and_leave`, `race_sends_against_removal`, `membership_actions_are_audited`; `concurrency.rs`: `adding_the_same_member_concurrently_adds_them_once`, `members_added_during_traffic_get_every_later_event`; `audit.rs`: `deactivated_people_cannot_be_added`, `race_remove_and_add_back_keep_the_index_true` |
| MSG-10 | `membership.rs`: `leaving_and_transferring_ownership`, `race_ownership_transfers`, `dissolving_deletes_the_conversation_for_everyone`, `dissolving_stops_live_work`, `race_membership_changes_against_dissolve`, `race_sends_against_dissolve` |
| MSG-11 | `membership.rs`: `blocking_stops_direct_contact`, `blocking_edge_cases`; `edges.rs`: `blocking_only_closes_direct_conversations` |
| MSG-12 | `membership.rs`: `the_directory_shows_only_people_you_know`, `the_directory_follows_membership_and_units`; `coverage.rs`: `user_and_conversation_listing_edges` |
| MSG-13 | `extensions.rs`: `custom_kinds_are_stored_pushed_and_pulled`, `custom_kind_and_payload_limits`, `editing_and_recalling_custom_messages`, `custom_kinds_through_side_channel_and_approval`, `content_policies_see_the_payload`, `race_edit_and_recall_of_a_custom_message` |
| MSG-14 | `extensions.rs`: `signals_reach_online_members_only`, `signal_permissions_and_limits` |
| MSG-15 | `files.rs`: 全部 13 个测试 (upload, parts, completion checks, one message per file, races, sweeps, drafts, restart, open API, deletion); `files_s3.rs`: `files_go_straight_to_the_store` (MinIO); `mcp.rs`: `agents_send_and_read_files`; cluster `operations.rs`: `files_need_a_shared_store_in_a_cluster` |
| MSG-16 | `audit.rs`: `retention_takes_a_prefix_and_what_derives_from_it`; `sync.rs`: `retention_deletes_old_events_and_resets_devices`; `pages.rs`: `retention_and_the_switch` |
| MSG-17 | `multidb.rs`: 全部 9 个测试 (moves, failed moves, writes during a move, start-up repair, listing across databases); `membership.rs`: `membership_changes_in_a_unit_database_update_the_directory`; `concurrency.rs`: `many_conversations_across_databases_stay_independent` |
| MSG-18 | `hub.rs`: `acked_messages_survive_restart_and_retries_are_deduplicated`, `failed_commit_acks_nothing_and_stops_writes`; `coverage.rs`: `one_failing_command_does_not_spoil_its_batch`, `a_full_write_queue_refuses_cleanly_and_retries_succeed`, `unsafe_postgres_durability_is_refused_unless_allowed`, `writes_wait_for_a_concurrent_sqlite_writer`, `startup_waits_for_a_concurrent_sqlite_writer`; `audit.rs`: `a_lock_held_elsewhere_fails_the_batch_not_the_database`; `schema.rs`: 全部 7 个测试; `server/crates/agentim/tests/crash.rs` (`kill -9` mid-traffic) |

覆盖整个板块的其他测试：
- 各种格式不对的请求: `scenarios.rs`: `malformed_requests_are_rejected_without_side_effects`;
- 重启后的状态: `scenarios.rs`: `state_survives_a_restart`.


## 7. 限制和下一步

- 写入吞吐每个库约每秒 1,300 条。计划：减少每条命令的语句、扇出移出写线程、PostgreSQL 和 MySQL 每个库多个写线程。
- 大群模式（2000 到 10 万人）没做：每个会话的日志只读一份，每个用户一个"会话有更新"的收件箱，推送按节点合并。
- 没做：话题（thread）和表情回应；本机磁盘上文件的应用层加密（先用磁盘加密）。
