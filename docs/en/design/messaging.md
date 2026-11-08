# Messaging

Conversations, the event log, messages, members, files, extensions and storage.

Code: `server/crates/agentim-core/src/logic.rs` and `logic/` (business logic in the writer), `hub.rs` (session layer), `db.rs` (SQL), `storage.rs` (databases and schema), `filestore.rs` and `hub/files.rs` (files).

## 1. Purpose

Messaging is the core every other area builds on. It keeps one record per conversation that is durable, gap-free and the same for every member, and it enforces who may read and write it. People, agents and the system all write through the same path.

## 2. Feature points

| ID | Feature | Behaviour |
|---|---|---|
| MSG-01 | Direct conversations | One per pair of users: creating it again returns the existing one. Members are fixed |
| MSG-02 | Groups | Created with a title and members. Members who join together become `members.joined` events of up to 200. A size limit applies (`clients.max_group_members`) |
| MSG-03 | Send | Text up to 64 KB, `reply_to` (same conversation only), mentions matched on whole names. Deduplicated by `(conv_id, sender, client_msg_id)` |
| MSG-04 | Edit | Versioned (`version+1`). Can require the expected version and refuses a stale one. Recomputes mentions. An edit window can be configured |
| MSG-05 | Recall | Clears the content everywhere, including earlier event snapshots, search and drafts. One recall wins. A recall window can be configured |
| MSG-06 | Streaming | A reply starts with its seq; deltas take no seq and are idempotent; the end is one `msg.updated`. Limits: 256 KB, 10 minutes (a watchdog ends overlong streams), 2 concurrent streams per agent per conversation |
| MSG-07 | Side channel | Whispers (owner ↔ own agent), messages for one member (`to_user`), drafts, private notices. The main log stays gap-free for everyone |
| MSG-08 | Read state and unread counts | Read positions per member, shared across devices. Unread counts only others' messages a member can see. Writing counts as reading |
| MSG-09 | Members and roles | Owner, admin, member. Admins add and remove ordinary members. Members may add their own agents. Removal and leaving cut access at once |
| MSG-10 | Transfer and dissolve | The owner transfers ownership or dissolves the conversation; dissolving deletes the content and stops live work |
| MSG-11 | Block | A blocked user cannot start a direct conversation, send messages only to you, or notify you with mentions. Shared groups are unaffected |
| MSG-12 | Directory | A user sees only people they share a conversation with, their agents, and people their unit allows. `find_user` looks up an exact name or email |
| MSG-13 | Custom message kinds | `kind` with a namespace and a JSON `payload` (≤ 32 KiB), stored, pushed, synced, edited and recalled like text. `text` is the fallback for old clients |
| MSG-14 | Signals | `typing` or a custom kind, only to members online now, never stored. 20 per second per user |
| MSG-15 | Files and images | Upload in parts straight to S3 or the server's disk; thumbnails made on the device; location data removed; a file belongs to one message; links signed per request |
| MSG-16 | Retention | Per-app retention period: old events are deleted, and devices behind them start over |
| MSG-17 | Several databases | A unit of the organisation can have its own database. Conversations move between databases online |
| MSG-18 | Durability | ACK only after a durable commit; a failed commit acknowledges nothing and stops writes; the server checks the databases' durability settings at start |

## 3. Design

### 3.1 The log, the side channel and messages

The model is described in [architecture §3](../architecture.md#3-data-model): one event log per conversation (`conv_seq`), a side channel (`side_seq`, `audience`, `anchor_seq`), and messages with a fixed `created_seq`. The server sets `sender_type` from the authenticated identity. `on_behalf_of` is only allowed for an agent speaking for its owner.

### 3.2 Membership

| Operation | Who | Result |
|---|---|---|
| `add_member` | Owner and admins; a member may add their own agents | `member.joined` |
| `remove_member` | Owner and admins; a member may remove an agent they added | `member.removed`. The removed user gets no more pushes and cannot read the conversation |
| `leave` | Any member (the owner transfers first) | `member.left` |
| `set_role` | Owner | Owner, admin, member |
| `transfer_owner` | Owner | A change of owner |
| `dissolve` | Owner | `conv.dissolved`. Every member loses access; the content is deleted; an audit record says it existed |
| `block`, `unblock` | Anyone | See MSG-11. The app stores require blocking |

When an agent is removed, its streams are cancelled, its pending drafts are discarded and its tasks end ("removed").

**Concurrency invariant**: if `member.removed` has seq N, the removed user has no message with a seq above N. A removal racing a send of that member ends one of two ways: the message commits before the removal, or the send is refused. The single writer gives this: both run in the same writer, one after the other.

### 3.3 Streaming

- A stream starts with `msg.created` (`state=streaming`) and holds its seq.
- Deltas are pushed to members who are watching (focus) and are appended by `(msg_id, idx)`, so a duplicate delta does nothing.
- A checkpoint is written each second. A member who focuses late gets the text so far.
- The stream ends with one `msg.updated` (`complete`, `cancelled` or `failed`). A stream left over from a crash becomes `failed(server_restart)` with the text of its last checkpoint.
- Who can interrupt a stream: the sender, the conversation's owner and admins, and the agent's owner.

### 3.4 Extensions

| Extension | Rules |
|---|---|
| Custom kinds | `kind` has at least two dot-separated parts, each starting with a lowercase letter, `a-z0-9_-`, at most 64 bytes; `agentim.` is reserved. `payload` is a JSON object of at most 32 KiB. Content policies (DLP, injection flags) read the payload too. Drafts keep the kind and payload |
| Signals | `signal {conv_id, kind, data?}`, `data` ≤ 4 KiB. Not stored, no seq. Non-members `forbidden`; a blocked direct conversation `blocked`; over 20 per second `rate_limited` |
| Custom fields | A JSON object of at most 4 KiB on users and conversations |
| Webhooks and the before-send hook | See [open-api](open-api.md) |

### 3.5 Files

The upload flow is the same for local disk and S3; only the addresses differ.

1. `upload_start {conv_id, name, size, mime?, thumb?}`. The server checks:
   - the sender may speak in the conversation (removed, blocked, dissolved and draft-mode agents are refused as for messages);
   - the size (up to `files.max_bytes`, 4 GiB by default; empty is refused);
   - the name (255 bytes; paths and control characters removed).

   The reply has the `file_id`, the part size and signed addresses. For S3 these are presigned PUTs (16 MiB parts above 64 MiB); for disk they are the server's own HMAC-signed `PUT /files/upload/{file_id}/{n}`. Addresses last one hour.
2. The client PUTs the parts, up to 4 at a time, and resumes after a network change.
3. `upload_complete {file_id, parts}`. The server checks:
   - the size;
   - the real type, from the first 64 bytes;
   - the thumbnail (JPEG or PNG, at most 320 px).

   Only then does it commit the `files` row. A mismatch is `bad_request`, and the uploaded content is deleted.
4. A message of kind `image` or `file` carries the `file_id`. A file belongs to one message: two messages racing for it give one `conflict`.

Agents, MCP and backends can also upload a whole file of up to 64 MiB in one request; the server then removes EXIF/GPS and makes the thumbnail.

| Aspect | Rule |
|---|---|
| Storage | `[files] storage = "local"` (default `./data/files`) or `"s3"` (any S3-compatible store). A cluster needs S3 or a shared disk |
| Images | JPEG, PNG, WebP and GIF are images. Everything else is a file, downloaded as an attachment with `nosniff` and a sandbox CSP. SVG is a file |
| Thumbnails | Made on the device: long side 320 px, rotated by EXIF, JPEG or PNG. The shared Rust runtime makes them on native platforms; browsers use `createImageBitmap` |
| Location data | EXIF and XMP are removed before upload by default, without re-encoding. A deployment can forbid keeping them |
| Download | `file_url {file_id, variant}` checks permission every time: the uploader, or someone who can see the message. It returns a 5-minute address (a presigned GET, or the server's signed URL with `Range`) |
| Deletion | Recall, rejected draft, dissolve, account deletion, retention: the row and a `file_trash` row commit together; a background task deletes the content with backoff |
| Expiry | A file no message references after 24 hours is deleted. `files.retention_days` sets a separate retention |
| Limits | `files.max_bytes`, `files.daily_bytes_per_user`, `files.max_open_per_user` (10 open uploads) |

### 3.6 Storage

- **Durability checks and schema migration**: see [architecture §6](../architecture.md#6-storage).
- **Several databases**: a control database holds the directory (`conv_id` → unit → database) and the membership index; it holds no content.
  - A conversation lives in exactly one database. Its metadata and content are never split.
  - Lists across databases are merged on the server.
  - **Moving a conversation**: it turns read-only, the data is copied and checked with a content hash, the directory switches, and the old copy is deleted. Writes during a move get `conv_migrating` and are retried with the same `client_msg_id`.
  - A crash during a move is repaired at start: the directory points at the complete copy, and the stale one is deleted.
- **SQLite `secure_delete`**: deleted content does not stay in free pages.

## 4. Interfaces

| Interface | Operations |
|---|---|
| Client protocol | `create_conv`, `update_conv`, `list_convs`, `send`, `whisper`, `edit`, `recall`, `stream_start`, `stream_append`, `stream_end`, `stream_cancel`, `focus`, `read`, `unread`, `pull`, `pull_side`, `add_member`, `remove_member`, `leave`, `set_role`, `transfer_owner`, `dissolve`, `block`, `unblock`, `list_blocks`, `find_user`, `signal`, `upload_start`, `upload_parts`, `upload_complete`, `file_url` |
| Open API | `/v1/conversations…`, `/v1/conversations/{id}/messages…`, `/v1/uploads`, `/v1/conversations/{id}/files` ([open-api](open-api.md)) |
| Events | `conv.*`, `member(s).*`, `msg.*` in the log; `side` events on the side channel |

The full protocol reference is generated into the docs site (`site/`, "Client protocol").

## 5. Rules and errors

Rule prefixes in the [business rule catalog](../business-rules.md):
- `conv.*` conversations;
- `member.*` members;
- `block.*` blocks;
- `msg.*` messages;
- `stream.*` streaming;
- `file.*` files;
- `sync.*` sync.

Typical codes:
- `forbidden`: not a member, or a role without the right;
- `not_found`: an unknown conversation, message or file;
- `conflict`: a stale version, an already recalled message, or a file already used;
- `bad_request`: a limit or a malformed input;
- `blocked`;
- `rate_limited`.

A refused request writes nothing to the log.

## 6. Tests

Server tests run on SQLite, PostgreSQL and MySQL unless noted. Paths are under `server/crates/agentim-core/tests/`.

| Feature | Tests |
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
| MSG-15 | `files.rs`: all 13 tests (upload, parts, completion checks, one message per file, races, sweeps, drafts, restart, open API, deletion); `files_s3.rs`: `files_go_straight_to_the_store` (MinIO); `mcp.rs`: `agents_send_and_read_files`; cluster `operations.rs`: `files_need_a_shared_store_in_a_cluster` |
| MSG-16 | `audit.rs`: `retention_takes_a_prefix_and_what_derives_from_it`; `sync.rs`: `retention_deletes_old_events_and_resets_devices`; `pages.rs`: `retention_and_the_switch` |
| MSG-17 | `multidb.rs`: all 9 tests (moves, failed moves, writes during a move, start-up repair, listing across databases); `membership.rs`: `membership_changes_in_a_unit_database_update_the_directory`; `concurrency.rs`: `many_conversations_across_databases_stay_independent` |
| MSG-18 | `hub.rs`: `acked_messages_survive_restart_and_retries_are_deduplicated`, `failed_commit_acks_nothing_and_stops_writes`; `coverage.rs`: `one_failing_command_does_not_spoil_its_batch`, `a_full_write_queue_refuses_cleanly_and_retries_succeed`, `unsafe_postgres_durability_is_refused_unless_allowed`, `writes_wait_for_a_concurrent_sqlite_writer`, `startup_waits_for_a_concurrent_sqlite_writer`; `audit.rs`: `a_lock_held_elsewhere_fails_the_batch_not_the_database`; `schema.rs`: all 7 tests; `server/crates/agentim/tests/crash.rs` (`kill -9` mid-traffic) |

Other tests that cover the whole area:
- malformed requests of every kind: `scenarios.rs`: `malformed_requests_are_rejected_without_side_effects`;
- state across restarts: `scenarios.rs`: `state_survives_a_restart`.

## 7. Limits and next steps

- Write throughput is about 1,300 msg/s per database. The plan: fewer statements per command, fan-out off the writer, several writers per PostgreSQL/MySQL database.
- Large-group mode (2,000 to 100,000 members) is not built. It would read the log once per conversation, keep a per-user inbox of "conversation changed" signals, and coalesce pushes per node.
- Not built: threads and reactions, and application-level encryption of files on local disk (use disk encryption meanwhile).
