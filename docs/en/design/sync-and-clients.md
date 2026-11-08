# Sync and client SDKs

How devices catch up and stay current, how history is paged, and how the client SDKs keep a reliable local copy on every platform.

Code: server `server/crates/agentim-core/src/hub/sync.rs`, `registry.rs` (in-order delivery), `db.rs` (cursors, retention). Client: `sdk/client-rust/core` (the sans-IO core), `sdk/client-rust/native` (the runtime and C ABI for native platforms), `sdk/client-web` (browser), and the wrappers `sdk/client-swift`, `client-kotlin`, `client-dotnet`, `client-flutter`, `client-react-native`.

## 1. Purpose

A person has several devices, goes offline, reinstalls, and switches networks. Each device must end up with exactly what the person may see, once and in order, without the client being able to ask for more. The client SDKs hide all of this behind a simple API: a local list of conversations and messages that is always consistent and works offline.

## 2. Feature points

| ID | Feature | Behaviour |
|---|---|---|
| SYN-01 | Positions per device | The server keeps `(device, conv) → acked_seq, acked_side_seq`. The client only says "sync" and acknowledges; it cannot choose where to start |
| SYN-02 | One delivery channel | Catch-up and live pushes are one ordered stream. A push that does not follow the last delivered seq is replaced by `resync`, so an event is never both pushed and synced |
| SYN-03 | Batches and flow control | Up to `sync.batch_size` (200) events per conversation per batch; at most `sync.max_unacked` (1000) unacknowledged |
| SYN-04 | Permissions | Only conversations the user is a member of now. Removal and dissolution are reported once per device; a member removed and added back while offline gets the part before removal, then continues from the new start |
| SYN-05 | How much history a device gets | New devices get the recent window (`window_days`, `max_messages` per conversation). Devices far behind or silent too long start over (`reset`). New members get the history the adder chose, within limits |
| SYN-06 | History paging | `history {conv_id, anchor, before, after}` pages through what the member may see: back, forward, jump to a message or to the first unread. It does not move sync positions |
| SYN-07 | Position check | `heads {conv_ids}` returns the latest seq, so a client finds a lost last push |
| SYN-08 | Read state, mute and notifications | Read positions reach every device. With `read_silences_others`, a message already read elsewhere does not notify. Mute per conversation, with an end time |
| SYN-09 | Own messages | `msg.created` carries the sender's own `client_msg_id` back, so the client merges its local copy |
| SYN-10 | Keep-alive and reconnect hints | `ping` returns server time; `welcome` gives `heartbeat_ms` and `idle_ms`; a node that is leaving sends `goaway` with a random wait; refused handshakes carry `Retry-After` |
| SYN-11 | Rate limits | Sync is limited per user across connections (10/s, burst 20); agents are stricter (2/s, burst 5) |
| SYN-12 | Retention | Events older than the retention period are deleted as a prefix per conversation; devices behind them get `reset` |
| CLI-01 | One Rust core | A single-threaded core with no IO of its own (sans-IO): commands in, effects and events out. Every platform runs the same core |
| CLI-02 | Connection states | `connecting`, `connected`, `disconnected`, `suspended` (after 2 minutes), `failed` (needs the app), `closed` |
| CLI-03 | Reconnect policy | Full-jitter backoff (500 ms base, 30 s cap; 1 s for overload); immediate retry on network change or return to foreground; a retry budget stops retry storms |
| CLI-04 | Failure classes | Offline, dead connection, network change, server unreachable, server restart, overload, expired token, refused token, SDK too old, protocol error: each has its own reaction |
| CLI-05 | Offline queue | Writes are stored before they are sent, one in flight per conversation and 8 overall. Edits, recalls, reads and feedback merge while queued. Only business refusals fail an operation; network and server trouble retry forever |
| CLI-06 | Local cache | Messages, conversations, members and the queue in SQLite (native) or encrypted IndexedDB (browser). A damaged cache is rebuilt; a full disk keeps the app working in memory; logout wipes it |
| CLI-07 | Gaps and islands | Pushes apply only in order; a gap is filled by sync. Paging back or jumping to a message creates an "island" that joins up later |
| CLI-08 | Files | `send_file` strips location data, makes the thumbnail, uploads in parts, resumes after a restart, and never holds back later messages |
| CLI-09 | Browser tabs | One connection is shared by all tabs; the core runs in a worker |
| CLI-10 | Platform wrappers | Swift (`@Observable`), Kotlin (`StateFlow`), C# and Unity (with a WebGL bridge), Dart and Flutter (FFI), React Native (JSI), C |
| CLI-11 | Tracing | Every attempt of an operation carries its trace id, also across a restart |

## 3. Design

### 3.1 Server: positions per device

- Each device has a row per conversation: `device_cursors (device_id, conv_id, user_id, acked_seq, acked_side_seq, told_state)`. A person's device is chosen by the app when it connects (`device=`); each agent key is one device.
- **The client cannot forge a position.** It may only acknowledge.
  - An ack beyond what was delivered on this connection is `bad_request`.
  - An ack below the stored position is ignored.

  Forging can only make a client miss messages, never get more.
- **One channel.** A connection only gets live pushes that follow its delivered position. Pushes before the first `sync`, out of sequence, or during a sync of that conversation are replaced by `resync`. The next `sync` fills the gap.
- **History through the same channel.** A new device or member is given an earlier start, and the same channel delivers from there. There is one set of rules for order, acknowledgement and permission.
- **Removed and added back.** The device gets the events up to the removal, then continues from the new visible start. The part in between is never sent.
- **Device logout** deletes the device's positions.

### 3.2 How much history

Configured in `[sync]` and `[retention]`, with hard caps:

| Setting | Default | Meaning |
|---|---|---|
| `batch_size` | 200 | Events per conversation per batch |
| `max_unacked` | 1000 | Unacknowledged events per conversation |
| `max_gap` | 10000 | Further behind than this: reset |
| `device_ttl_days` | 30 | A device not acknowledging for this long starts over |
| `window_days` | 90 (cap 90) | A new device syncs only this many days |
| `max_messages` | 30 | A new device syncs at most this many recent messages per conversation; older ones are paged |
| `member_messages`, `member_days` | 30, 30 | History a new human member gets by default |
| `share_max` | 200 | The most history an adder may share |
| `rate_per_second`, `agent_rate_per_second` | 10, 2 | Sync rate per user and per agent |
| `retention.days` | 180 | 0 keeps everything |

The "first visible seq" is fixed on the member row when they join, so later setting changes do not change what existing members see. Adding a person to a direct conversation creates a new group instead, so the direct conversation's history is never exposed.

### 3.3 History paging

`history {conv_id, anchor?, before?, after?}` takes:
- `anchor`: a seq, `"newest"` (the default) or `"first_unread"`;
- `before`: up to this many messages with seq ≤ anchor;
- `after`: up to this many with seq > anchor.

It returns messages in their current state (edited, recalled, or streaming so far), with `has_before`, `has_after` and `before_end`. `before_end` says why there is nothing earlier: `start`, `visible_from`, `retention` or `disabled`.

The rules:
- Only conversations the user is a member of now.
- Only from the member's visible start and within retention.
- Side-channel content never comes this way.
- Rate-limited per user.
- `[history] enabled = false` returns empty pages.

Agents page with their own form: `after_seq`, `before_seq`, `limit`. Those reads are audited ([agents](agents.md)).

### 3.4 Retention

Each conversation's log is deleted as a prefix by seq (never leaving a hole), at most 20,000 events per round. Derived data goes with it: messages, side events, summaries, feedback, card answers, claims, decided drafts and requests, and old webhook outbox rows.

A device is forgotten only when all its positions in a database are older than `device_ttl_days`.

### 3.5 Client core

```
app / UI components
  └─ platform wrapper (thin): WebSocket, storage driver, network and lifecycle signals,
     timers, key storage, turns core events into platform observables
       └─ interface: C ABI (native) / wasm (browser): commands in (JSON), events out (JSON)
            └─ agentim-client-core (Rust, single-threaded, no IO)
                 connection state machine · sync and gaps · history · offline queue
                 · local storage rules · conversation and message state · diagnostics
```

- **No IO in the core.** Inputs are app calls, frames, connection changes, timers, storage results, network and foreground changes. Outputs are effects: send a frame, open a connection, write rows, set a timer, tell the UI what changed. The same core runs natively and in WebAssembly, and deterministic tests drive it with a fake clock, network and storage.
- **Single-threaded**: only the core writes its state, so there are no locks.
- **Native IO** comes from the shared Rust runtime (`native/`: WebSocket, SQLite, timers, files). Wrappers add only platform signals: network, foreground, proxies, key storage. Browsers provide WebSocket, IndexedDB and WebCrypto in TypeScript.

### 3.6 Reconnect and requests

- Reconnect uses full-jitter backoff (`random(0, min(cap, base·2^n))`, base 500 ms, cap 30 s).
  - After 2 minutes down: `suspended`, with one try a minute.
  - A network change, a return to the foreground or the network coming back: try at once.
  - Each attempt has a 10-second timeout.
- **Heartbeat**: a `ping` every 25 s when quiet (60 s on cellular or in the background). A connection silent for `2 × heartbeat + 5 s` is dead.
- **A server restart** (close 1012 or `goaway`): reconnect after the wait the server gives, or a random 0–5 s.
- **Token**:
  - `unauthorized` asks the token provider once for a new token;
  - `logged_out` and `client_too_old` stop reconnecting.
- **Retry budget** (gRPC style): 10 tokens; each retry after server trouble costs 1, each success returns 0.1. Below half, the queue pauses until the next good connection or 60 s.
- **Two kinds of requests**:
  - **Queued writes**: send, edit, recall, read, feedback, card answers. They survive disconnects and are resent with the same idempotency key.
  - **Online-only requests**: approvals, decisions, take-over, queries. They fail at once when offline and report "outcome unknown" on timeout.

### 3.7 Offline queue

- One queue per conversation, persisted before sending. What is shown is the server's copy with the queued operations laid on top, so a refusal makes the view fall back on its own.

| Operation | Shown at once | Merged while queued | Safe to resend because | Refused |
|---|---|---|---|---|
| Send | `sending` | — | `client_msg_id` dedup | `failed`, can `retry` or `discard` |
| Edit | new text, `pending` | Folds into an unsent send; consecutive edits keep the last | `base_version`; `conflict` with the same text counts as done | rolled back |
| Recall | recalled | Drops an unsent send and its edits | "already recalled" counts as done | rolled back |
| Read | unread count | One per conversation, the highest seq | only moves forward | dropped |
| Feedback | shown | Last one per message | overwrite | rolled back |
| Card answer | submitted | — | "already answered" counts as done | rolled back |

- An operation that depends on an unsent message waits for it and fails with it.
- A business refusal does not block later operations. Temporary trouble holds them, to keep the order.

### 3.8 Platforms

| Platform | Package | State |
|---|---|---|
| Browser, Node | `@agentimhq/client-web` | The core in WebAssembly, encrypted IndexedDB, one connection for all tabs |
| React | `@agentimhq/react` | Components and hooks ([chat-components](chat-components.md)) |
| React Native | `client-react-native`, `react-native-ui` | JSI binding; TypeScript tested against a fake binding |
| iOS, macOS | `agentim-swift` | Swift Package with an XCFramework |
| Android, JVM | `com.agentim:client` | Kotlin, JNI, an AAR for 4 ABIs with 16 KB pages |
| .NET, Unity | NuGet, a UPM package | C#, with a WebGL bridge over `client-web` |
| Flutter, Dart | `agentim_client` | Dart FFI with a build hook for the core |
| C | `agentim.h` | The ABI the wrappers use |

The web and React Native components that sit on these SDKs are shown in [chat-components › Screenshots](chat-components.md#screenshots). The native SDKs (Swift, Kotlin, C#, Dart, C) have no UI of their own; apps draw their own screens on their state.

## 4. Interfaces

- Server operations: `sync {limit?}`, `ack {acks}`, `heads {conv_ids}`, `history`, `unread`, `read`, `set_mute`, `set_prefs`, `ping`, `list_devices`, `logout_device`.
- Clients cannot use `pull` or `pull_side`; those are for agents.
- The client SDK API is generated into the docs site for each language.

## 5. Rules and errors

Rule prefixes: `sync.*`, `session.*`, `account.*` ([catalog](../business-rules.md)). An ack beyond what was delivered is `bad_request`; a sync for a non-member is never possible because the client names no conversation; over the rate is `rate_limited` with `retry_after_ms`.

## 6. Tests

Server tests under `server/crates/agentim-core/tests/` run on three databases. Client tests are under `sdk/`.

| Feature | Tests |
|---|---|
| SYN-01 | `sync.rs`: `acknowledgements_are_checked`, `positions_persist_and_unacknowledged_events_come_again`, `positions_survive_a_restart`, `race_two_sessions_of_one_device_ack_monotonically` |
| SYN-02 | `sync.rs`: `live_pushes_continue_the_sync_in_order`, `race_senders_while_a_device_syncs_get_every_event_once`, `a_device_receives_everything_once_in_batches` |
| SYN-03 | `sync.rs`: `unacknowledged_events_are_bounded`, `a_side_channel_backlog_comes_in_full` |
| SYN-04 | `sync.rs`: `agents_sync_only_what_they_may_see`, `clients_cannot_read_history_by_seq`, `removal_and_dissolution_reach_offline_devices`, `removed_and_added_back_while_offline`, `new_members_see_what_they_are_allowed_to` |
| SYN-05 | `sync.rs`: `new_devices_get_a_recent_window`, `devices_far_behind_or_silent_too_long_start_over`, `a_quiet_conversation_keeps_its_position`, `quiet_conversations_follow_the_older_switch`, `a_logged_out_device_starts_over`, `positions_move_with_a_conversation_between_databases` |
| SYN-06 | `pages.rs`: all 7 tests (anchors, limits, current state, visibility, retention, races while sending and editing, refusals over a socket) |
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
| CLI-08 | `sdk/client-rust/core/tests/files.rs`: all 19 tests |
| CLI-09 | `sdk/client-web/test/browser.mjs` (Playwright: several tabs, one connection) |
| CLI-10 | `sdk/client-rust/native/tests/conformance.rs`: 7 scenarios against a real server (talk, files, offline queue across a restart, server restart, paging, deactivated person, the C ABI from C). The same scenarios run in each wrapper: `client-swift/test.sh`, `client-kotlin/test.sh`, `client-dotnet`, `client-flutter/test.sh`, `client-react-native/test`, `client-web/test/conformance.test.mjs` |
| CLI-11 | `rules.rs`: `every_attempt_of_an_operation_carries_its_trace`, `a_queued_operation_keeps_its_trace_across_a_restart` |
| Invariants | `sdk/client-rust/core/tests/simulate.rs`: thousands of random seeds of faults and user actions; checks no loss, no duplicates, server order, the queue drains or fails clearly |

The client core is held at 100% line and branch coverage (`cargo llvm-cov`) in CI.

## 7. Limits and next steps

- Conversations are synced in membership order, not most-recently-active first.
- No HTTPS long-polling fallback when WebSockets are blocked; the SDK reports `websocket_blocked` with per-step diagnostics.
- iOS and Android builds are tested in simulators in CI only; no device farm.
