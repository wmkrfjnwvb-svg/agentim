# 同步和客户端 SDK

设备怎么追上并保持最新、怎么往回翻历史，以及客户端 SDK 怎么在每个平台上保持一份可靠的本地副本。

代码：服务端 `server/crates/agentim-core/src/hub/sync.rs`、`registry.rs`（按顺序下发）、`db.rs`（同步位置、保留期）。客户端 `sdk/client-rust/core`（不做 IO 的核心）、`sdk/client-rust/native`（原生平台的运行层和 C ABI）、`sdk/client-web`（浏览器），以及各平台封装 `sdk/client-swift`、`client-kotlin`、`client-dotnet`、`client-flutter`、`client-react-native`。

## 1. 用途

一个人有多台设备，会离线、重装、换网络。每台设备最终都要拿到这个人能看的全部内容，每条一次、按顺序，而客户端没法多要。客户端 SDK 把这些藏在一个简单的接口后面：本地的会话和消息列表，始终一致，离线也能用。

## 2. 功能点

| 编号 | 功能 | 行为 |
|---|---|---|
| SYN-01 | 按设备记位置 | 服务端记 `(设备, 会话) → acked_seq, acked_side_seq`。客户端只说"同步"和确认，不能指定从哪开始 |
| SYN-02 | 一条下发通道 | 追赶和实时推送是同一条有序的流。接不上已下发位置的推送换成 `resync`，所以一个事件不会既被推送又被同步 |
| SYN-03 | 分批和流量控制 | 每批每个会话最多 `sync.batch_size`（200）条；未确认的最多 `sync.max_unacked`（1000）条 |
| SYN-04 | 权限 | 只下发现在是成员的会话。移出和解散对每台设备只报告一次；离线期间被移出又拉回的，先补移出前那段，再从新的可见起点继续 |
| SYN-05 | 设备能拿到多少历史 | 新设备拿最近一段（`window_days`，每个会话 `max_messages` 条）。落后太多或太久没确认的重新开始（`reset`）。新成员拿到拉人时选的历史，有上限 |
| SYN-06 | 往回翻历史 | `history {conv_id, anchor, before, after}` 按页读成员能看的内容：往回翻、往后翻、跳到某条、跳到第一条未读。不改变同步位置 |
| SYN-07 | 位置校验 | `heads {conv_ids}` 返回最新序号，客户端据此发现丢了的最后一条推送 |
| SYN-08 | 已读、免打扰和通知 | 已读位置同步到每台设备。开了 `read_silences_others` 时，别处已读的消息不提醒。按会话免打扰，可设结束时间 |
| SYN-09 | 自己发的消息 | `msg.created` 给发送者带回它自己的 `client_msg_id`，客户端据此合并本地副本 |
| SYN-10 | 保活和重连提示 | `ping` 返回服务端时间；`welcome` 给出 `heartbeat_ms` 和 `idle_ms`；要下线的节点发 `goaway` 带随机等待时间；被拒的握手带 `Retry-After` |
| SYN-11 | 限流 | 同步按用户限流（所有连接合计，每秒 10 次、突发 20）；Agent 更严（每秒 2 次、突发 5） |
| SYN-12 | 保留期 | 超过保留期的事件按会话从头删除；落在后面的设备收到 `reset` |
| CLI-01 | 一个 Rust 核心 | 单线程、不自己做 IO（sans-IO）：命令进去，效果和事件出来。所有平台跑同一个核心 |
| CLI-02 | 连接状态 | `connecting`、`connected`、`disconnected`、`suspended`（2 分钟后）、`failed`（要应用处理）、`closed` |
| CLI-03 | 重连策略 | 全抖动退避（基数 500 毫秒、上限 30 秒；过载时 1 秒）；网络变化、回到前台立即重试；重试预算防止重试风暴 |
| CLI-04 | 故障分类 | 没网、连接僵死、换网络、服务端不可达、服务端重启、过载、令牌过期、令牌被拒、SDK 太旧、协议错误：各有各的反应 |
| CLI-05 | 离线队列 | 写操作先存下再发，每个会话同时一个、全局最多 8 个。编辑、撤回、已读、反馈在排队时合并。只有业务拒绝才算失败；网络和服务端问题一直重试 |
| CLI-06 | 本地缓存 | 消息、会话、成员和队列存在 SQLite（原生）或加密的 IndexedDB（浏览器）。缓存坏了就重建；磁盘满了在内存里照常工作；退出登录全部清掉 |
| CLI-07 | 缺口和孤岛 | 推送只按顺序应用，缺口用同步补。往回翻或跳到某条会形成一个"孤岛"，之后自动连上 |
| CLI-08 | 文件 | `send_file` 去掉位置信息、生成缩略图、分片上传、重启后续传，不挡住后面的消息 |
| CLI-09 | 浏览器多标签页 | 所有标签页共用一个连接，核心跑在 Worker 里 |
| CLI-10 | 平台封装 | Swift（`@Observable`）、Kotlin（`StateFlow`）、C# 和 Unity（含 WebGL 桥）、Dart 和 Flutter（FFI）、React Native（JSI）、C |
| CLI-11 | 链路 | 一个操作的每次尝试都带它的链路编号，重启后也一样 |

## 3. 设计

### 3.1 服务端：按设备记位置

- 每台设备在每个会话有一行：`device_cursors (device_id, conv_id, user_id, acked_seq, acked_side_seq, told_state)`。人的设备由 App 连接时选定（`device=`）；Agent 的每把密钥是一台设备。
- **客户端伪造不了位置**，它只能确认：
  - 确认超过这条连接已下发的位置 → `bad_request`；
  - 比已存的位置小 → 忽略。

  伪造只会让自己少收，不会多拿。
- **一条通道。** 连接只收能接上已下发位置的实时推送。第一次 `sync` 之前的、跳号的、这个会话正在同步时到的推送，都换成 `resync`，由下一次 `sync` 补齐。
- **历史也走这条通道。** 新设备或新成员只是起点往前设，由同一条通道往后下发。顺序、确认、权限只有一套规则。
- **被移出又拉回**：设备先拿到移出之前的事件，再从新的可见起点继续，中间那段永远不发。
- **设备下线**会删除这台设备的同步位置。

### 3.2 能拿到多少历史

在 `[sync]` 和 `[retention]` 里设，有硬上限：

| 设置 | 默认 | 含义 |
|---|---|---|
| `batch_size` | 200 | 每批每个会话多少条 |
| `max_unacked` | 1000 | 每个会话未确认的上限 |
| `max_gap` | 10000 | 落后超过这么多就重置 |
| `device_ttl_days` | 30 | 这么久没确认的设备重新开始 |
| `window_days` | 90（上限 90） | 新设备只同步这么多天 |
| `max_messages` | 30 | 新设备每个会话最多同步最近多少条；更早的按页取 |
| `member_messages`、`member_days` | 30、30 | 新加入的人默认带的历史 |
| `share_max` | 200 | 拉人时最多能带多少条 |
| `rate_per_second`、`agent_rate_per_second` | 10、2 | 每个用户、每个 Agent 的同步频率 |
| `retention.days` | 180 | 0 表示永久保留 |

"可见的第一个序号"在加入时记在成员记录上，所以之后改设置不影响已经在群里的人。单聊里加人会新建一个群，单聊的历史不会暴露给新加的人。

### 3.3 往回翻历史

`history {conv_id, anchor?, before?, after?}` 的参数：
- `anchor`：一个序号、`"newest"`（默认）或 `"first_unread"`；
- `before`：序号 ≤ anchor 的最多几条；
- `after`：序号 > anchor 的最多几条。

返回消息的当前状态（编辑过的、撤回的、流式中已生成的），以及 `has_before`、`has_after` 和 `before_end`。`before_end` 说明为什么前面没有了：`start`、`visible_from`、`retention` 或 `disabled`。

规则：
- 只能读现在是成员的会话；
- 只能从成员的可见起点开始，并在保留期内；
- 侧信道的内容不从这里返回；
- 按用户限流；
- `[history] enabled = false` 时返回空页。

Agent 用自己的形式翻页：`after_seq`、`before_seq`、`limit`，这些读取写审计（[agents](agents.md)）。

### 3.4 保留期

每个会话的日志按序号从头删除（不会删出空洞），每轮最多 2 万条。衍生数据一起删：消息、侧信道事件、摘要、反馈、卡片回应、抢单、已决定的草稿和请求、旧的 Webhook 发件箱。

一台设备只有在它在某个库里的**全部**位置都超过 `device_ttl_days` 时才被遗忘。

### 3.5 客户端核心

```
应用 / 界面组件
  └─ 平台封装（薄）：WebSocket、存储驱动、网络和前后台信号、
     计时、密钥存储，把核心的事件变成本平台可订阅的对象
       └─ 接口：C ABI（原生）/ wasm（浏览器）：命令进（JSON），事件出（JSON）
            └─ agentim-client-core（Rust，单线程，不做 IO）
                 连接状态机 · 同步和缺口 · 历史 · 离线队列
                 · 本地存储规则 · 会话和消息状态 · 诊断
```

- **核心不做 IO。** 输入是应用调用、收到的帧、连接变化、计时器、存储结果、网络和前后台变化；输出是效果：发这帧、开连接、写这些行、定个时、告诉界面什么变了。同一个核心在原生和 WebAssembly 里跑；确定性测试用假时钟、假网络、假存储驱动它。
- **单线程**：状态只有核心写，没有锁。
- **原生平台的 IO** 来自共用的 Rust 运行层（`native/`：WebSocket、SQLite、计时、文件）。平台封装只补本平台的信号：网络、前后台、代理、密钥存储。浏览器用 TypeScript 提供 WebSocket、IndexedDB 和 WebCrypto。

### 3.6 重连和请求

- 重连用全抖动退避（`random(0, min(上限, 基数·2^n))`，基数 500 毫秒，上限 30 秒）：
  - 断开 2 分钟后进入 `suspended`，每分钟试一次；
  - 网络变化、回到前台、网络恢复时立即试；
  - 每次尝试 10 秒超时。
- **心跳**：没有流量时每 25 秒一个 `ping`（蜂窝网络和后台 60 秒）。`2 × 心跳 + 5 秒`内没有任何帧就判定连接已死。
- **服务端重启**（关闭码 1012 或 `goaway`）：按服务端给的时间等，或随机等 0–5 秒，再重连。
- **令牌**：
  - `unauthorized` 时向令牌提供者要一次新令牌；
  - `logged_out` 和 `client_too_old` 时停止重连。
- **重试预算**（gRPC 的做法）：10 个令牌，服务端故障后的每次重试扣 1，每次成功加 0.1；低于一半时暂停队列，等下一次连接成功或 60 秒。
- **两类请求**：
  - **排队的写操作**：发送、编辑、撤回、已读、反馈、卡片回应。断线不丢，用同一个幂等键重发。
  - **只能在线的请求**：审批、决定、接管、查询。离线时立即失败；超时时报"结果不明"。

### 3.7 离线队列

- 每个会话一个队列，先存下再发。界面显示的是服务端的副本叠上排队中的操作，所以操作被拒绝时，显示会自动回到服务端的样子。

| 操作 | 立即显示 | 排队时合并 | 重发为什么安全 | 被拒绝时 |
|---|---|---|---|---|
| 发送 | `sending` | — | `client_msg_id` 去重 | `failed`，可以 `retry` 或 `discard` |
| 编辑 | 新内容，`pending` | 还没发出的消息直接改；连续编辑只留最后一次 | `base_version`；`conflict` 但内容已一样就算成功 | 退回 |
| 撤回 | 已撤回 | 还没发出的消息直接丢掉它和它的编辑 | "已撤回"就算成功 | 退回 |
| 已读 | 未读数 | 每个会话一个，取最大序号 | 只往前走 | 丢弃 |
| 反馈 | 显示 | 每条消息只留最后一次 | 覆盖 | 退回 |
| 卡片回应 | 已提交 | — | "已回应过"就算成功 | 退回 |

- 依赖一条还没发出的消息的操作，等它发出；它失败了，这些操作一起失败。
- 业务拒绝不挡后面的操作；暂时性的问题会让后面的等着，以保证顺序。

### 3.8 平台

| 平台 | 包 | 状态 |
|---|---|---|
| 浏览器、Node | `@agentimhq/client-web` | 核心是 WebAssembly，IndexedDB 加密，所有标签页共用一个连接 |
| React | `@agentimhq/react` | 组件和 hooks（[chat-components](chat-components.md)） |
| React Native | `client-react-native`、`react-native-ui` | JSI 绑定；TypeScript 对着假绑定测试 |
| iOS、macOS | `agentim-swift` | Swift Package，带 XCFramework |
| Android、JVM | `com.agentim:client` | Kotlin、JNI，四种 ABI、16 KB 页的 AAR |
| .NET、Unity | NuGet、UPM 包 | C#，带基于 `client-web` 的 WebGL 桥 |
| Flutter、Dart | `agentim_client` | Dart FFI，构建钩子提供核心 |
| C | `agentim.h` | 各平台封装用的 ABI |

基于这些 SDK 的 Web 和 React Native 组件截图见 [chat-components › 截图](chat-components.md#截图)。原生 SDK（Swift、Kotlin、C#、Dart、C）没有自带界面，应用按 SDK 的状态自己画界面。

## 4. 接口

- 服务端操作：`sync {limit?}`、`ack {acks}`、`heads {conv_ids}`、`history`、`unread`、`read`、`set_mute`、`set_prefs`、`ping`、`list_devices`、`logout_device`。
- 客户端不能用 `pull`、`pull_side`，那是给 Agent 的。
- 每种语言的客户端 SDK 接口由代码生成到文档站。

## 5. 规则和错误

规则前缀：`sync.*`、`session.*`、`account.*`（[目录](../../en/business-rules.md)）。确认超过已下发的位置是 `bad_request`；客户端不指定会话，所以不可能同步别人的会话；超过频率是 `rate_limited`，带 `retry_after_ms`。

## 6. 测试

服务端测试在 `server/crates/agentim-core/tests/` 下，三种数据库都跑；客户端测试在 `sdk/` 下。

| 功能点 | 测试 |
|---|---|
| SYN-01 | `sync.rs`: `acknowledgements_are_checked`, `positions_persist_and_unacknowledged_events_come_again`, `positions_survive_a_restart`, `race_two_sessions_of_one_device_ack_monotonically` |
| SYN-02 | `sync.rs`: `live_pushes_continue_the_sync_in_order`, `race_senders_while_a_device_syncs_get_every_event_once`, `a_device_receives_everything_once_in_batches` |
| SYN-03 | `sync.rs`: `unacknowledged_events_are_bounded`, `a_side_channel_backlog_comes_in_full` |
| SYN-04 | `sync.rs`: `agents_sync_only_what_they_may_see`, `clients_cannot_read_history_by_seq`, `removal_and_dissolution_reach_offline_devices`, `removed_and_added_back_while_offline`, `new_members_see_what_they_are_allowed_to` |
| SYN-05 | `sync.rs`: `new_devices_get_a_recent_window`, `devices_far_behind_or_silent_too_long_start_over`, `a_quiet_conversation_keeps_its_position`, `quiet_conversations_follow_the_older_switch`, `a_logged_out_device_starts_over`, `positions_move_with_a_conversation_between_databases` |
| SYN-06 | `pages.rs`: 全部 7 个测试 (anchors, limits, current state, visibility, retention, races while sending and editing, refusals over a socket) |
| SYN-07 | `sync.rs`: `heads_show_where_conversations_stand`; cluster `operations.rs`: `heads_from_any_node_show_where_conversations_stand` |
| SYN-08 | `sync.rs`: `read_and_mute_state_reach_every_device`, `reading_on_one_device_silences_the_others`, `mute_and_notifications`, `unread_counts` |
| SYN-09 | `sync.rs`: `the_sender_gets_its_client_msg_id_back` |
| SYN-10 | `pages.rs`: `heartbeat_goaway_and_refusals_over_a_socket` |
| SYN-11 | `sync.rs`: `sync_is_rate_limited` |
| SYN-12 | `sync.rs`: `retention_deletes_old_events_and_resets_devices`; `audit.rs`: `retention_takes_a_prefix_and_what_derives_from_it` |
| CLI-01–CLI-04 | `sdk/client-rust/core/tests/rules.rs`: `connects_with_a_token_hello_list_and_sync`, `backoff_is_full_jitter_growing_to_the_cap`, `after_two_minutes_down_it_is_suspended_and_tries_once_a_minute`, `an_unauthorized_token_is_replaced_once_at_once`, `refusals_that_retrying_cannot_fix_stop`, `a_busy_server_is_waited_for_at_least_as_long_as_it_says`, `a_leaving_node_is_left_after_the_wait_it_gives`, `logged_out_and_too_old_stop_reconnecting`, `heartbeat_pings_when_quiet_and_drops_a_dead_connection`, `an_attempt_that_hangs_times_out_after_ten_seconds`, `offline_waits_without_retrying_and_online_connects_at_once`, `a_new_route_is_probed_and_a_silent_connection_replaced`, `back_in_the_foreground_it_checks_the_connection_and_syncs`, `close_stops_everything`; `details.rs`: `more_connection_edges`, `the_retry_budget_pauses_the_queue_then_resumes` |
| CLI-05 | `rules.rs`: `sends_offline_show_at_once_and_go_out_once`, `a_lost_answer_is_resent_with_the_same_ids_and_applied_once`, `server_trouble_is_retried_and_business_refusals_fail`, `a_change_to_a_message_whose_send_is_refused_fails_too`, `queued_changes_merge_before_they_go`, `one_operation_per_conversation_and_eight_overall_in_flight`, `the_queue_survives_a_restart_and_is_sent_once`, `an_edit_queued_behind_its_send_survives_a_restart`, `online_only_requests_fail_at_once_offline`; `details.rs`: `queue_edges`, `retrying_after_a_week_gets_a_new_id` |
| CLI-06 | `rules.rs`: `nothing_happens_before_the_cache_is_read`, `a_full_store_withholds_acknowledgements_until_it_recovers`, `a_damaged_cache_is_rebuilt_and_the_queue_kept`, `another_account_never_sees_the_cache`; `details.rs`: `a_store_that_cannot_write_keeps_the_app_working_in_memory`, `logout_wipes_everything` |
| CLI-07 | `rules.rs`: `pushes_apply_in_order_and_a_gap_is_filled_by_sync`, `a_lost_last_push_is_found_by_the_position_check`, `many_events_come_in_batches`, `older_messages_come_from_the_cache_then_the_server`, `jumping_to_a_message_makes_an_island`; `details.rs`: `history_edges`, `jumping_near_the_end`, `pushes_for_unknown_or_old_positions_and_two_conversations`, `resets_and_departures_from_sync`, `ended_conversations_stay_read_only_until_added_back` |
| CLI-08 | `sdk/client-rust/core/tests/files.rs`: 全部 19 个测试 |
| CLI-09 | `sdk/client-web/test/browser.mjs` (Playwright: several tabs, one connection) |
| CLI-10 | `sdk/client-rust/native/tests/conformance.rs`: 7 scenarios against a real server (talk, files, offline queue across a restart, server restart, paging, deactivated person, the C ABI from C). The same scenarios run in each wrapper: `client-swift/test.sh`, `client-kotlin/test.sh`, `client-dotnet`, `client-flutter/test.sh`, `client-react-native/test`, `client-web/test/conformance.test.mjs` |
| CLI-11 | `rules.rs`: `every_attempt_of_an_operation_carries_its_trace`, `a_queued_operation_keeps_its_trace_across_a_restart` |
| Invariants | `sdk/client-rust/core/tests/simulate.rs`: thousands of random seeds of faults and user actions; checks no loss, no duplicates, server order, the queue drains or fails clearly |

客户端核心在 CI 里保持 100% 行和分支覆盖率（`cargo llvm-cov`）。


## 7. 限制和下一步

- 会话按成员关系的顺序下发，还不是"最近活跃的先发"。
- WebSocket 被挡时没有 HTTPS 长轮询的备用通道；SDK 报 `websocket_blocked`，并给出每一步的诊断。
- iOS 和 Android 只在 CI 的模拟器里测，没有真机。
