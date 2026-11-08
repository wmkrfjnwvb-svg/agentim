# Security and compliance

Encryption in transit and at rest, keys, security engineering, and the compliance features every deployment needs: AI labels, data export and deletion, retention, reports and moderation, silencing, and organisational units.

Code: `server/crates/agentim-core/src/crypto.rs` (encryption at rest), `storage.rs` (encrypted columns, `secure_delete`), `hub/compliance.rs` and `logic/compliance.rs` (export, deletion, reports, moderation), `hub/export.rs`, `tls.rs`, `guard.rs` (content filters, DLP, injection flags).

## 1. Purpose

Most users are outside China, so GDPR, the DSA, the EU AI Act, the UK Online Safety Act and the app store rules are requirements, not extras. AgentIM gives each deployment the tools to meet them, and it keeps content safe even when a backup or a disk leaks.

## 2. Feature points

| ID | Feature | Behaviour |
|---|---|---|
| SEC-01 | TLS | The server speaks TLS itself (certificates checked at start), or runs behind a proxy (`behind_proxy`) |
| SEC-02 | Encryption at rest | On by default. AES-256-GCM with a data key (DEK) per conversation per 30 days, wrapped by a master key (KEK). Content, payloads, event snapshots, side events, drafts, audit details and webhook bodies are encrypted |
| SEC-03 | Master key | From `key_file` or `AGENTIM_MASTER_KEY`. A single SQLite deployment generates `<db>.key` (0600) next to the database. A wrong key is refused at start (the database stores its fingerprint) |
| SEC-04 | Tamper detection | The AAD is (table, column, row): moved or changed ciphertext fails to decrypt and is refused (fail closed) |
| SEC-05 | Crypto-shredding | Dissolving a conversation deletes its data keys, so backups of its ciphertext become unreadable. Destroying an agent deletes its own key |
| SEC-06 | Turning encryption on, off and rotating | Off to on: new content is encrypted at once; `agentim encryption encrypt-existing` encrypts old data. On to off keeps old content readable. `agentim encryption rotate-key` re-wraps the data keys only |
| SEC-07 | Keys at rest | API, agent and admin keys are stored as hashes. Signing keys and connection credentials are stored encrypted. Keys never appear in logs or errors |
| SEC-08 | Logs without content | Logs and metrics never contain message content, tokens or whispers. Audit holds who, what and when, not content |
| SEC-09 | AI labels | `sender_type` is set by the server; messages carry `ai_generated`; an agent joining is a visible notice naming its owner and read scope (EU AI Act Art. 50, GDPR notice) |
| SEC-10 | Export a person's data | `GET /v1/users/{id}/export` (`compliance`): streamed NDJSON, resumable. It contains the person's profile, devices, conversations, own messages, feedback, card answers, reports and agents. Other people's messages are not included |
| SEC-11 | Delete an account | `DELETE /v1/users/{id}`: the person is blocked at once, then each conversation's writer erases their content (like a recall, plus `user.erased`). They leave the conversation (ownership passes on); then their profile, devices and agents are removed. Resumable after a restart; ids are never reused |
| SEC-12 | Retention | Per deployment; old events are deleted as a prefix with everything derived from them ([sync-and-clients](sync-and-clients.md)) |
| SEC-13 | Reports | `report {target_kind, target_id, category, text?}` on messages, users and conversations, with an evidence snapshot (encrypted) that survives recall; duplicates merge; 20 per hour per user |
| SEC-14 | Moderation queue | Ordered by severity and deadline (24 h by default). Decisions: `none`, `remove_content`, `suspend_user` (days), `ban_user`, with a reason and grounds (DSA Art. 17), applied to all open reports on the target. Moderators decide once |
| SEC-15 | Silencing | A member can be silenced in a conversation for 1 s – 1 year; they can read but not speak, and an agent's drafts cannot be approved meanwhile |
| SEC-16 | Content filters | DLP on agent output; injection flags on agent input; checks during streams ([agents](agents.md#37-guardrails)) |
| SEC-17 | Units | Optional organisational units with their own databases, directory visibility and policies; lower levels can only tighten ([messaging](messaging.md#36-storage)) |

## 3. Design

### 3.1 Encryption at rest

| Aspect | How |
|---|---|
| Algorithm | AES-256-GCM (`ring`), 96-bit random nonces. DEKs are wrapped with the KEK, AAD = the DEK id |
| DEKs | One per conversation per 30 days (`data_keys`), cached in memory. Shared content (audit, webhook outbox) uses a database-level DEK per month. A new DEK commits in the same transaction as the data that uses it |
| Encrypted columns | `messages.content`, `messages.payload`, `events.payload`, `side_events.data`, `drafts.content`, `drafts.payload`, `audit.detail`, `webhook_outbox.body`, report snapshots, request contexts, sources, envelopes, feedback reasons, summaries, agent state |
| Not encrypted | Ids, seqs, times, kinds, membership, roles, sender ids, conversation titles and profiles (needed for routing, ordering and permission checks) |
| Format | `enc:v1:<key_id>:<base64(nonce‖ciphertext‖tag)>`. A value without the prefix is plaintext written before encryption was on and is read as such |
| Moving between databases | The DEKs move with the conversation; the ciphertext is copied unchanged |
| Cost | About 2% of write throughput ([README](../../../README.md#performance)) |

### 3.2 Account deletion and concurrency

- The conversation's writer marks the person deleted before it erases their content. Any send of theirs queued after that is refused, so nothing of theirs appears after the erasure.
- Progress is stored in `erasures`, so a restart finishes the job, and a repeated request returns the same progress.
- Audit records stay: they hold no content and are a legal duty. Backups expire with their own retention.

### 3.3 Compliance map

| Rule | Requirement | Feature |
|---|---|---|
| EU AI Act Art. 50 | People must know they talk to AI; machine-readable marks | SEC-09 |
| EU AI Act Art. 14 | Human oversight | [approvals](approvals.md) |
| GDPR Art. 15, 20 | Access and portability | SEC-10 |
| GDPR Art. 17 | Erasure | SEC-11 |
| GDPR Art. 5 | Minimisation and storage limits | SEC-08, SEC-12 |
| DSA Art. 16, 17 | Notice and action; statements of reasons | SEC-13, SEC-14 |
| App Store 1.2, Google Play UGC | Filters, reports, blocking, action within 24 h | SEC-13, SEC-14, SEC-16, blocking ([messaging](messaging.md)) |
| App Store 5.1.1(v) | Account deletion in the app | SEC-11 (the app calls its backend, which calls the API) |
| CCPA / CPRA | Access and deletion | SEC-10, SEC-11 |
| Data residency | Data in a chosen region | Self-hosting decides; units can have their own database |

### 3.4 Security engineering

- **Input limits** on frame size, JSON depth and request rates.
- **Permission matrices** in tests: roles × operations × conversation kinds, with positive and negative cases.
- **Dependencies** are checked for advisories and licences on every change and weekly (`cargo-deny`, npm audit).
- **Releases** are signed (Sigstore), carry build provenance, and their images have an SBOM ([developer-experience](developer-experience.md)).
- **Vulnerability reports** follow [`SECURITY.md`](../../../SECURITY.md).

## 4. Interfaces

- Open API: `/v1/users/{id}/export`, `DELETE /v1/users/{id}`, `/v1/users/{id}/erasure`, `/v1/reports…`, `…/members/{user}:silence`.
- Client protocol: `report`, `block`.
- Configuration: `[encryption]` (`key_file`, mode), `[retention]`, `[moderation]` (deadlines).
- CLI: `agentim encryption encrypt-existing|decrypt-existing|rotate-key`.

## 5. Rules and errors

Rule prefixes: `report.*`, `account.*`, `member.*` (silencing) ([catalog](../business-rules.md)).
- `not_found`: reporting something you cannot see.
- `bad_request`: reporting yourself.
- `rate_limited`: too many reports.
- `conflict`: deciding a decided report.
- `forbidden`: sending while deleted or silenced.

## 6. Tests

Paths under `server/crates/agentim-core/tests/`; three databases.

| Feature | Tests |
|---|---|
| SEC-01 | `transport.rs`: `the_server_speaks_tls_itself`, `certificates_are_checked_at_startup` |
| SEC-02–SEC-06 | `encryption.rs`: all 10 tests (nothing readable at rest in any table, generated key for a single SQLite database, master key checks, tamper and move detection, one data key per conversation under races, shredding on dissolve, encrypting an old database in place, turning it off, rotating the master key, moving conversations with their keys); `audit.rs`: `text_that_looks_encrypted_is_kept_as_text` |
| SEC-07 | `login.rs`: `signing_keys_are_kept_encrypted_and_listed_without_secrets`; `agents.rs` (keys as hashes) |
| SEC-08 | `coverage.rs` (DLP and logs: `dlp_covers_agent_edits_and_ephemerals_but_not_people`); `dye.rs`: `only_dyed_requests_are_logged_in_full` |
| SEC-10, SEC-11 | `compliance.rs`: `export_has_the_persons_own_data`, `deleting_an_account_erases_what_the_person_wrote`, `an_interrupted_deletion_finishes_after_a_restart`, `deletion_refusals_and_repeats`, `race_sends_while_the_account_is_deleted` |
| SEC-12 | see [sync-and-clients](sync-and-clients.md#6-tests) (SYN-12) |
| SEC-13, SEC-14 | `compliance.rs`: `reports_follow_the_rules`, `the_queue_and_decisions`, `race_moderators_decide_once` |
| SEC-15 | `developer_api.rs`: `silenced_members_cannot_speak`, `silences_end_and_survive_a_restart`, `race_sends_while_a_member_is_silenced`, `a_silenced_agents_drafts_wait` |
| SEC-16 | see [agents](agents.md#6-tests) (AGT-18) |
| SEC-17 | `multidb.rs`, `membership.rs`: `the_directory_follows_membership_and_units` |

## 7. Limits and next steps

- Not built:
  - application-level encryption of files on local disk (use disk or bucket encryption);
  - end-to-end encryption;
  - C2PA marks on media;
  - an age-gate hook (COPPA);
  - a DPIA template;
  - deferring retention for reported content.
