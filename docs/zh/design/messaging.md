# 消息

会话、事件日志、消息、成员、话题、表情回应、置顶消息、文件、扩展和存储。

代码：`server/crates/agentim-core/src/logic.rs` 和 `logic/`（写线程里的业务逻辑）、`hub.rs`（会话层）、`db.rs`（SQL）、`storage.rs`（数据库和表结构）、`filestore.rs` 和 `hub/files.rs`（文件），`logic/social.rs`、`hub/social.rs`、`db/social.rs`（话题、回应、置顶）。

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
| MSG-19 | 话题 | 回复用 `thread_id` 指向根消息，它就是日志里一条普通消息。只有一层（话题里不能再开话题）。根消息带汇总：回复数、最后一条、参与的人。成员可以关注话题；只通知关注者和被提到的人；每个话题有自己的已读位置和未读数；会话的未读数不算话题回复，除非这条回复也显示在会话里（`also_in_conv`） |
| MSG-20 | 表情回应 | 每条消息、每个人、每个 key 一个；key 是一个表情，或应用定义的 `:name:`。加两次、删一个没有的，都不改变任何东西。每条消息最多 50 种、每人最多 20 个；`single` 模式下每人只留一个。不算未读、不发通知；Agent 只收到对它自己消息的回应；可配置让 👍/👎 同时算作对 Agent 的反馈 |
| MSG-21 | 置顶消息 | 每个会话最多 100 条；置顶两次不改变任何东西。群里所有成员都能置顶（`pin_policy = members`，默认），或只有群主和管理员（`admins`）；别人的置顶只有群主和管理员能取消。应用允许时 Agent 才能置顶。撤回会取消置顶 |

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

### 3.6 话题

话题是一条根消息，加上用 `thread_id` 指向它的回复。回复是同一条日志里的消息：占 `conv_seq`，所以顺序、同步、重试、重启、集群、校验器都和普通消息一样处理。

| 方面 | 规则 |
|---|---|
| 根 | 同一会话主日志里的消息：不是侧信道消息，不是系统消息，没有撤回，本身不是回复（只有一层）。在根之后才加入的成员能看到之后的回复，也能回复；根本身对他们仍然不可见 |
| 回复 | `send`、`stream_start` 和 Agent 的草稿都接受 `thread_id`；`reply_to` 仍可以引用任何消息。`also_in_conv: true` 让回复同时显示在会话里（消息上带 `thread_shown`） |
| 汇总 | `threads`（回复数、最后一条的 seq、时间和发送者）和每条回复在同一个事务里写入；撤回的回复会被减掉。客户端用 `msg_summary`、`pull_thread`、`list_threads` 取：回复数、最后一条、最多 5 个参与者和总人数、调用者是否关注、他的未读数 |
| 关注 | `thread_members`：根的发送者、回复过的人、被提到的人都会关注。再次回复会重新关注；被提到不会撤销"取消关注"。已经是这个状态时 `follow_thread`、`unfollow_thread` 不改变任何东西 |
| 已读和未读 | `read_thread` 移动调用者在所有设备上的位置（只前进，不超过最后一条回复；其他设备收到 `thread_read`）。在话题里发言算读了这个话题，不算读了会话。会话的未读数不算没有显示在会话里的话题回复 |
| 通知 | 只在话题里的回复，通知关注者和被提到的人（受免打扰影响）；其他人安静地收到事件 |
| Agent | Agent 参与的话题（根和回复），在任何触发模式下它都能看到；唤醒规则不变（被提到、`to`、触发模式）。`context` 带 `thread_id` 时只给根和回复。内置助手回答话题里的消息时回到这个话题，卡片和表单也一样。草稿保留话题；根被撤回后不能再批准 |
| 撤回 | 撤回回复：从汇总里减掉。撤回根：回复保留，不能再回复（`thread.root_recalled`） |
| 保留期、删除账号、导出 | 保留期删除消息时连同它们的话题。被删除的人的回复从汇总里减掉，关注记录删除。导出列出这个人关注的话题 |

### 3.7 表情回应

| 方面 | 规则 |
|---|---|
| Key | 一个表情（一个 Unicode 表情序列：带肤色、旗帜、键帽、ZWJ 组合），或应用定义的短名 `:name:`（`messages.reaction_names`，由客户端画） |
| 只一次 | `(msg_id, user_id, key)` 唯一。对已有的 `react`、对没有的 `unreact`，回答 `changed: false`，不写任何东西 |
| 上限 | 每条消息 `messages.reaction_keys`（50）种、每人每条 `messages.reactions_per_user`（20）个，超过是 `conflict`。消息上已有的 key 还能有更多人加。每人每秒 `messages.reactions_per_second`（20）次 |
| 单选模式 | `messages.reaction_mode = "single"`：一个人的新回应替换旧的（先一条 `reaction.removed`，再一条 `reaction.added`） |
| 事件 | `reaction.added`、`reaction.removed` 占 seq，带消息（`msg_id`、`msg_seq`、`msg_sender`）、key 和人。不算未读，不发通知。看不到那条消息的成员收不到 |
| Agent | Agent 只收到对它自己消息的回应，不会因此被唤醒。Agent 也能回应（比如在请求上点 👀） |
| 反馈 | 打开 `messages.reactions_as_feedback` 时，一个人在 Agent 消息上的 👍 或 👎（任何肤色）也算他对 Agent 的反馈（[agents](agents.md)）；以最新的那个为准，删掉就收回反馈 |
| 汇总 | `msg_summary`：每个 key 的数量、调用者是否在其中、最近 5 个人；`list_reactions` 分页列出某个 key 的人 |
| 撤回、删除账号、导出 | 撤回删除这条消息的回应（撤回事件本身表示这一点）。被删除的人的回应、以及他被删除的消息上的回应都删除。导出列出这个人加过的回应 |

### 3.8 置顶消息

| 方面 | 规则 |
|---|---|
| 谁能置顶 | 单聊：两个人都能。群和任务频道：`pin_policy = members`（默认）时所有成员，`admins` 时只有群主和管理员；群主或管理员用 `update_conv` 设置。Agent：只有 `messages.agents_pin` 打开时。被禁言的成员不能置顶 |
| 取消 | 置顶的人、群主、管理员；单聊两个人都能 |
| 什么能置顶 | 调用者能看到的主日志消息：不是系统消息，没有撤回。话题回复也可以 |
| 只一次 | 对已置顶的 `pin`、对没置顶的 `unpin`，回答 `changed: false`，不写任何东西 |
| 上限 | 每个会话 `messages.pins_per_conv`（100）条，超过是 `conflict`；先取消一条 |
| 事件 | `msg.pinned`、`msg.unpinned` 占 seq（客户端显示一条提示）；不算未读、不发通知；审计记录为 `msg.pinned`、`msg.unpinned` |
| 列表 | `list_pins`：最新置顶在前，带消息、数量和权限设置；后加入的成员只看到自己能看到的消息的置顶 |
| 撤回 | 撤回会取消置顶（撤回事件本身表示这一点） |

### 3.9 存储

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
| 客户端协议 | `create_conv`、`update_conv`（含 `pin_policy`）、`list_convs`、`send`（含 `thread_id`、`also_in_conv`）、`whisper`、`edit`、`recall`、`stream_start`（含 `thread_id`）、`stream_append`、`stream_end`、`stream_cancel`、`focus`、`read`、`unread`、`pull`、`pull_side`、`add_member`、`remove_member`、`leave`、`set_role`、`transfer_owner`、`dissolve`、`block`、`unblock`、`list_blocks`、`find_user`、`signal`、`upload_start`、`upload_parts`、`upload_complete`、`file_url`；话题：`read_thread`、`follow_thread`、`unfollow_thread`、`list_threads`、`pull_thread`；回应：`react`、`unreact`、`list_reactions`；置顶：`pin`、`unpin`、`list_pins`；`msg_summary`（最多 100 条消息的话题、回应和置顶） |
| 开放 API | `/v1/conversations…`、`/v1/conversations/{id}/messages…`（发送可带 `thread_id`）、`/v1/conversations/{id}/threads[/{thread}]`、`/v1/conversations/{id}/messages/{msg}/reactions/{key}`（GET、PUT、DELETE）、`/v1/conversations/{id}/pins[/{msg}]`、`/v1/uploads`、`/v1/conversations/{id}/files`（[open-api](open-api.md)） |
| MCP | `send_message` 可带 `thread_id`；`react`、`unreact`、`get_thread`、`list_pins` |
| 事件 | 日志里的 `conv.*`、`member(s).*`、`msg.*`（含 `msg.pinned`、`msg.unpinned`）、`reaction.added`、`reaction.removed`；侧信道上的 `side` 事件；发给用户其他设备的 `thread_read`、`thread_follow` |
| 配置 | `[messages]`：`reaction_keys`、`reactions_per_user`、`reaction_mode`、`reaction_names`、`reactions_as_feedback`、`reactions_per_second`、`pins_per_conv`、`agents_pin` |

完整的协议参考由代码生成到文档站（`site/` 的 "Client protocol"）。

## 5. 规则和错误

[业务规则目录](../../en/business-rules.md)里的前缀：
- `conv.*` 会话；
- `member.*` 成员；
- `block.*` 拉黑；
- `msg.*` 消息；
- `thread.*` 话题；
- `reaction.*` 表情回应；
- `pin.*` 置顶消息；
- `social.*` 读取话题、回应和置顶；
- `stream.*` 流式；
- `file.*` 文件；
- `sync.*` 同步。

常见的码：
- `forbidden`：不是成员，或角色没有权限；
- `not_found`：会话、消息、文件不存在；
- `conflict`：版本过期、已经撤回（也包括作为话题的根、被回应或被置顶）、文件已被使用、回应或置顶到了上限；
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
| MSG-19 | `threads.rs`: `replies_go_into_a_thread_with_a_summary`, `the_root_of_a_thread_is_checked`, `thread_replies_count_and_notify_only_followers`, `reading_and_following_threads`, `recall_and_retries_in_threads`, `agents_answer_in_the_thread`, `new_members_see_replies_but_not_older_roots`, `threads_survive_a_restart`, `race_replies_in_one_thread`（做过变异检查）; `schema.rs`（v20 迁移）; `verify.rs`（`thread.root`、`thread.summary`）；客户端核心 `sdk/client-rust/core/tests/social.rs`；Agent SDK `sdk/agent-node/e2e.mjs`、`sdk/agent-python/e2e.py`；服务端 SDK `sdk/*/smoke*`；组件 `sdk/react/test/social.test.tsx`、`sdk/react-native-ui/test/components.test.tsx` |
| MSG-20 | `reactions.rs`: `reactions_are_added_once_and_summarised`, `reaction_keys_are_checked`, `who_may_react`, `reaction_limits_and_single_mode`, `agents_hear_of_reactions_to_their_own_messages`, `reactions_can_count_as_feedback`, `a_recall_takes_the_reactions`, `reactions_survive_a_restart`, `race_reactions_at_the_key_limit`（做过变异检查）, `race_reactions_and_recall`（做过变异检查）, `race_the_same_reaction_from_many_devices`; `logic::social::tests::emoji`（单元测试）；客户端核心 `sdk/client-rust/core/tests/social.rs`；组件 `sdk/react/test/social.test.tsx` |
| MSG-21 | `pins.rs`: `messages_are_pinned_once_and_listed`, `who_may_pin_and_unpin`, `what_can_be_pinned`, `agents_pin_only_when_the_app_allows`, `the_pin_limit`, `a_recall_unpins_and_pins_survive_a_restart`, `race_pins_at_the_limit`（做过变异检查）；客户端核心 `sdk/client-rust/core/tests/social.rs`；组件 `sdk/react/test/social.test.tsx` |

覆盖整个板块的其他测试：
- 各种格式不对的请求: `scenarios.rs`: `malformed_requests_are_rejected_without_side_effects`;
- 重启后的状态: `scenarios.rs`: `state_survives_a_restart`.


## 7. 限制和下一步

- 写入吞吐每个库约每秒 1,300 条。计划：减少每条命令的语句、扇出移出写线程、PostgreSQL 和 MySQL 每个库多个写线程。
- 大群模式（2000 到 10 万人）没做：每个会话的日志只读一份，每个用户一个"会话有更新"的收件箱，推送按节点合并。
- 没做：本机磁盘上文件的应用层加密（先用磁盘加密）；会过期的置顶；嵌套话题。
