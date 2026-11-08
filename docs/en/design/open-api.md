# Open API, webhooks and the before-send hook

The HTTPS API for the developer's backend, the events AgentIM sends to the backend, and the hook that lets the backend check a message before it is stored.

Code: `server/crates/agentim-core/src/open_api.rs` (the `/v1` routes, OpenAPI 3.1 document), `webhook.rs` (delivery, signatures, the before-send hook), `hub/developer.rs` (developer operations), `hub/export.rs`, `hub/compliance.rs`. Server SDKs: `sdk/node`, `sdk/python`, `sdk/go`.

## 1. Purpose

There are four interfaces, each with its own credentials:

| Interface | Used by | Credentials |
|---|---|---|
| Client protocol (`/ws`) | End users' devices and agents | A user token or an agent key, one device each |
| **Open API** (`/v1`) | The developer's backend | An app API key with scopes |
| Admin API (`/admin/v1`) | The console and `agentim admin` | Admin accounts or admin keys ([access](access.md)) |
| **Webhooks**, MCP | The backend receives events; agents use MCP | Signed callbacks; agent keys |

The open API works on the data and behaviour inside the IM. The admin API works on the deployment. Neither key can do the other's work.

## 2. Feature points

| ID | Feature | Behaviour |
|---|---|---|
| API-01 | API keys and scopes | Several keys per app (for rotation), each with scopes, an optional IP allowlist and expiry. Stored as hashes. `Authorization: Bearer`, HTTPS required (except on loopback) |
| API-02 | Acting as a user or as the system | `as: <user>` follows that user's business rules exactly. `as: system` can manage conversations and send system notices, but cannot speak as a user. There is no "bypass the rules" mode |
| API-03 | Developer-chosen user ids | `PUT /v1/users/{id}` creates or updates by the developer's own id (1–64 characters of letters, digits, `_-.@:`); idempotent |
| API-04 | Users | Create, read, update (profile, custom fields, permissions), batch (100), deactivate, reactivate, devices, logout everywhere |
| API-05 | Agents | Create (`kind: agent`), keys, stop and resume, transfer, destroy, list, export and import ([agents](agents.md)) |
| API-06 | Conversations | Create (with the developer's own id), read, update, dissolve; members in bulk with history, remove, roles, transfer; silence a member for 1 s – 1 year |
| API-07 | Messages | Send as a user or the system (with envelopes and sources); "message a user" (creates the direct conversation); batch send to up to 500 people; edit, recall; history by seq (`messages:read`, audited); mark read |
| API-08 | Files | Upload in parts or whole (64 MiB) as a member or the system; signed read links |
| API-09 | Compliance and moderation | Export a user's data (NDJSON, resumable), delete an account (background, with progress), reports and decisions |
| API-10 | Conventions | RFC 9457 problem details with the same codes as the protocol plus `rule`; `Idempotency-Key` on every `POST`; cursor paging (≤ 200); per-key rate limits with `RateLimit-Policy`, `RateLimit` and `Retry-After`; per-item results in batches |
| API-11 | Failed authentication | An address that keeps sending unknown keys is refused for a while |
| API-12 | OpenAPI document | `GET /v1/openapi.json` (OpenAPI 3.1); the docs site reference is generated from it |
| API-13 | Webhooks | Signed (Standard Webhooks), ordered, at-least-once. Events are written to an outbox in the same transaction as the change, so a refused or uncommitted write sends nothing |
| API-14 | Before-send hook | A synchronous call before each `send` can allow, reject (`policy_blocked`) or replace a message |
| API-15 | Server SDKs | Node, Python and Go: every endpoint, `createUserToken` (signs a user token locally), webhook signature checks |

## 3. Design

### 3.1 Scopes

| Scope | Allows |
|---|---|
| `users:write` / `users:read` | Create, import, change, deactivate users; read profiles and devices |
| `tokens:revoke` | Log a person out on every device (user tokens are signed by the developer's own signing key and never pass through the API) |
| `agents:write` / `agents:read` | Create and manage agents; list and export them (exporting conversations also needs `messages:read`) |
| `conversations:write` / `conversations:read` | Conversations, members, roles, dissolve |
| `messages:write` | Send, edit, recall, mark read, upload files |
| `messages:read` | Read history and files. A separate scope, because it reads private conversations; every read is audited |
| `moderation` | Reports, silencing members |
| `compliance` | Export and delete a user's data (GDPR) |

### 3.2 Same path, same rules

- Writes through the API go through the same writer and group commit as clients, and succeed only after the commit is durable.
- Members receive API messages in real time like any other. A message sent as a user syncs to all that user's devices.
- Deduplication works with `Idempotency-Key`, or with `client_msg_id` for messages.
- Every call is audited: the key, the identity, the action, the target and the result. Logs never contain keys or message content.

### 3.3 Webhooks

| Aspect | Rule |
|---|---|
| Configuration | `[[webhooks]]` with `name`, `url`, `secret` (`whsec_…` base64, as Standard Webhooks; at least 16 bytes), `events` (exact types, `msg.*` prefixes or `*`), `max_attempts` (8), `timeout_ms`, `backoff_ms`. Tenants manage their own ([multi-tenancy](multi-tenancy.md)) |
| No loss | Matching events are written to `webhook_outbox` in the same transaction as the event |
| Order | One delivery task per webhook per database sends in commit order; the next waits for the previous |
| At least once | The cursor moves through the writer after success; a restart resumes from it. Receivers dedupe by `webhook-id` |
| Signature | `webhook-id`, `webhook-timestamp`, `webhook-signature: v1,<base64 HMAC-SHA256(id.timestamp.body)>` |
| Body | `{type, timestamp, database, data: {conv_id, seq, event}}` |
| Retry | Exponential backoff up to 5 minutes. After `max_attempts` the event is dropped, audited and counted. Receivers can catch up with the history API |
| Content later removed | Content recalled or dissolved while a webhook was down is not delivered later |
| New webhooks | Start at the current position; history is not replayed |
| Event types | `conv.*`, `member.*`, `members.joined`, `msg.*`, `card.response`, `friend.*`, and the rest of the log |

### 3.4 The before-send hook

- `[before_send]` sets `url`, `secret`, `timeout_ms` (default 2000) and `on_error` (`allow` or `reject`).
- The hook is called before the writer, so it does not slow group commit. The request is signed like a webhook: `{type: "message.before_send", conv_id, client_msg_id, kind, text, payload, reply_to, to_user, sender}`.
- Answers:
  - `{action: "allow"}`;
  - `{action: "reject", reason}`, which returns `policy_blocked` with the reason;
  - `{action: "replace", text, payload}`, after which the message is checked again.
- A timeout, a non-2xx, invalid JSON or an unknown action follows `on_error`.
- Malformed requests and non-members are refused before the hook is called, and retries of a sent message skip it. Edits and streams do not go through it.

## 4. Interfaces

The full list of endpoints is in `GET /v1/openapi.json` and on the docs site. The resources are:
- `/v1/users…`, `/v1/agents…`, `/v1/roles…`;
- `/v1/conversations…` with members, messages, read, uploads and files;
- `/v1/messages…`, `/v1/reports…`.

## 5. Rules and errors

The codes and rules are the same as for clients, mapped to HTTP status codes:

| Code | HTTP status |
|---|---|
| `bad_request` | 400 |
| `forbidden` | 403 |
| `not_found` | 404 |
| `conflict` | 409 |
| `rate_limited` | 429 |

Prefixes: `apikey.*`, plus the prefix of each resource ([catalog](../business-rules.md)). A missing scope is 403. The problem details carry `code`, `rule` and `detail`.

## 6. Tests

Paths under `server/crates/agentim-core/tests/`; three databases.

| Feature | Tests |
|---|---|
| API-01, API-10, API-11 | `open_api.rs`: `authentication_and_scopes`, `errors_are_problem_details`, `idempotency_keys`, `input_limits`, `rate_limits_per_key`, `failed_authentications_block_one_address`, `race_key_reloads_against_new_and_revoked_keys`, `batches_report_each_item`, `address_rules`; cluster `operations.rs`: `api_keys_created_or_revoked_on_one_node_change_at_once_on_the_others` |
| API-02, API-06, API-07 | `open_api.rs`: `conversations_as_users_and_the_system`, `messages_as_users_and_the_system`; `developer_api.rs`: `silenced_members_cannot_speak`, `silences_end_and_survive_a_restart`, `race_sends_while_a_member_is_silenced`, `a_silenced_agents_drafts_wait` |
| API-03, API-04 | `open_api.rs`: `users_lifecycle`, `tokens_for_users`; `developer_api.rs`: `devices_are_listed_and_logged_out` |
| API-05 | `developer_api.rs`: `agents_are_handed_out_and_listed`, `stopping_resuming_and_destroying_agents`, `agents_move_by_export_and_import`, `team_agents_are_transferred` |
| API-08 | `files.rs`: `the_open_api_uploads_and_reads_files` |
| API-09 | `compliance.rs`: all 8 tests |
| API-12 | `site/scripts/complete.mjs` (CI: every endpoint, error and protocol message has a docs page) |
| API-13 | `webhooks.rs`: `events_are_delivered_in_order_and_signed`, `failed_deliveries_are_retried_in_order`, `an_event_is_dropped_after_the_last_attempt`, `a_restart_resumes_without_loss`, `refused_and_uncommitted_writes_send_nothing`, `concurrent_senders_are_all_delivered_once`, `delivered_events_leave_the_outbox`, `events_of_every_database_are_delivered`, `filters_choose_the_event_types`, `a_new_webhook_does_not_replay_history`, `recalled_and_dissolved_content_is_not_delivered_later`, `race_recall_and_dissolve_against_writes_in_one_batch`, `friend_events_are_delivered` |
| API-14 | `webhooks.rs`: `before_send_allows_rejects_and_replaces`, `before_send_failures_follow_the_configuration`, `before_send_skips_retries_and_non_members` |
| API-15 | `sdk/node/smoke.mjs`, `sdk/python/smoke.py`, `sdk/go/smoke` (every endpoint against a real server, in CI) |

## 7. Limits and next steps

- Not built: HMAC request signing for the API, broadcasts to all users, importing message history from another provider, and asynchronous job resources.
