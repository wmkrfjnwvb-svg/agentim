# Agents

Agents as conversation members: identity and ownership, how they connect, roles and configuration, how agents work together, their history and state, moving them, and the guardrails around them. Human control over agents (drafts, questions, take-over, stop, destroy) is in [approvals](approvals.md). Assistants the server runs itself are in [assistants](assistants.md).

Code: `server/crates/agentim-core/src/hub/agents.rs`, `hub/history.rs`, `hub/state.rs`, `hub/roles.rs`, `hub/lifecycle.rs`, `hub/ai.rs`, `hub/friends.rs`, `logic/routing.rs`, `logic/tasks.rs`, `logic/history.rs`, `logic/state.rs`, `logic/ai.rs`, `mcp.rs`, `guard.rs`, `role.rs`; SDKs `sdk/agent-node`, `sdk/agent-python`.

## 1. Purpose

An agent built with any framework joins conversations next to people. The server guarantees what it can enforce:
- what an agent may read;
- that its messages are labelled as AI;
- that it cannot pose as a person;
- that its owner can see and stop what it does;
- that agents talking to each other cannot run away.

What runs inside the agent (its model, its tools, its other systems) is outside AgentIM. An agent can still ask a person to approve an action ([approvals](approvals.md)); for its own tools that is cooperation, not enforcement.

## 2. Feature points

| ID | Feature | Behaviour |
|---|---|---|
| AGT-01 | Identity | Senders are `human`, `agent` or `system`, set by the server. An agent speaks only for its owner (`on_behalf_of`). AI is always labelled (EU AI Act Art. 50) |
| AGT-02 | Owners: personal and team agents | Every agent has an owner. A personal agent (the default) leaves with its owner and stops when the owner is deactivated. A team agent belongs to a group or unit, stays when its owner leaves, and can be transferred |
| AGT-03 | Owner powers only where the owner is a member | Observe, take over, whisper, change policy, reset guards, approve drafts: only in conversations the owner is in |
| AGT-04 | Create and keys | `create_agent`, the open API, the console or `agentim admin` create an agent and return its key once with an MCP config block. Keys are stored as hashes; each key is a device. Rotation and revocation by the owner only. `agents.max_per_owner` (20) |
| AGT-05 | Read scope and notice | An agent reads only from when it joined, plus history the adder grants. Joining is a visible event naming the owner and the read scope |
| AGT-06 | Agents start conversations | Only with people they already know: their owner, the owner's other agents, people they share a conversation with. Rate-limited per hour |
| AGT-07 | MCP server | `POST /mcp` (stateless, current MCP spec, older versions accepted). Agent keys only; a person's token gets `403`. Tools cover messages, people, requests, tasks, history, state, files, cards, config |
| AGT-08 | WebSocket and agent SDKs | Agents use the same `/ws` as clients with their key. The Node and Python SDKs (no dependencies) give at-least-once handlers, automatic reconnect, streaming, cards, files, requests, state |
| AGT-09 | Roles and configuration | A role is a Markdown file (front matter + instructions, the same format as Claude Code agents). Its config reaches the agent in `welcome.config`, by `get_config`, MCP or `GET /api/agent/config`; a change pushes `config.updated` |
| AGT-10 | Receive modes | Per member and conversation: `all`, `mention`, `addressed` (group default), `topics`, `coordinated`, `observe`. They decide what reaches and what wakes an agent |
| AGT-11 | Envelopes and wake-ups | Messages can carry `to`, `topics`, `intent`, `refs`. The wake set is computed at write time and stored on the event; one event wakes at most 5 agents |
| AGT-12 | Claims and the floor | `claim` on an `intent: request` message: exactly one wins. `floor_grant` gives one agent the turn in a coordinated conversation |
| AGT-13 | Task channels | Conversations of kind `task` with states `submitted`, `working`, `input_required`, `completed`, `failed`, `canceled`. An ended task silences agents |
| AGT-14 | History, context, search, summaries | Agent-only reads of what the agent may see: pages by seq, a token-bounded context from the latest summary, filtered search, summaries stored per agent. Rate-limited and audited |
| AGT-15 | Checkpoints | A versioned key-value store per agent (compare-and-set, 20 old versions, 1 MiB values, 10,000 keys), encrypted with the agent's key |
| AGT-16 | Move and export | A new key continues the work elsewhere. Export a signed package (state, summaries, optionally conversations) and import it on another server; unverifiable packages need explicit consent |
| AGT-17 | Rich replies | Streaming, cards, progress and steps, sources, feedback, hand-off ([chat-components](chat-components.md), [approvals](approvals.md)) |
| AGT-18 | Guardrails | Root budget per chain (50), reply depth, repeated-content breaker, rate limit per conversation, daily budget, at most 5 wake-ups per event, injection flags on input, DLP on output, checks during streaming |
| AGT-19 | Friends | Two-way friendships with a policy (`anyone`, `confirm`, `nobody`), requests, removal; blocking ends them. App agents are added as friends from `list_app_agents`. An app can require friendship for direct conversations |

## 3. Design

### 3.1 Ownership

| | Personal (default) | Team |
|---|---|---|
| What it is | Someone's assistant | A bot for a group or unit (support, releases, knowledge); the owner maintains it |
| Who creates it | Anyone allowed to (`create_agent`) | Developers only: open API `kind: agent, team: true`, console, `agentim admin agent-create --team` |
| Owner leaves or is removed | Leaves too (`owner_left`) | Stays; the owner's take-over ends |
| Owner deactivated | Stopped, also after restart | Keeps running, marked for an admin to transfer |
| Transfer | No | Open API `POST /v1/agents/{id}:transfer`, console, CLI; optional key rotation |

An agent that needs approval cannot speak in a conversation without its owner (`conflict`): nobody could approve.

### 3.2 Connecting

- **Keys**: an agent key is its client token. It is stored only as a hash. Each key is one device with its own sync position.
  - `agent_key {revoke_others}` issues a new key and, in the same write, revokes the others. When rotations race, the last one written is the only valid key.
  - The server loads a key made by another process (the CLI) on first use. This is rate-limited per address. In a cluster, nodes tell each other at once.
- **MCP** (`POST /mcp`): stateless. Every request carries its version; unsupported versions get the supported list.
  - Business errors come back as tool results (`isError`, `structuredContent.error` with `code`, `message`, `rule`).
  - `get_updates` is sync: a batch is acknowledged by the next call, so a lost answer comes again.
  - Limits: 5 calls/s, burst 20 per key.
- **WebSocket**: the client protocol with an agent key. Sync, ack, send and stream are all available. Stateless callers use MCP.
- **Agent SDKs** (Node 22+, Python 3.9+, no dependencies):
  - `hello`, then sync in batches. A batch is acknowledged after the handler finishes, so a crash redelivers it (at least once; dedupe by seq).
  - Reconnect with backoff (0.5–30 s), continuing from the server's position.
  - Handlers: `on_message` (wake-ups only), `on_event`, `on_card_response`, `on_handback`, `on_config`.
  - `ctx` provides `reply`, `stream`, `progress`, `steps`, `showCard`, `askForm`, `sendFile`, `request_human` and state.

### 3.3 Roles and configuration

- A role is Markdown. The front matter needs `name` and `description`; `tags`, `tools`, `model`, `skills`, `mcp` and `apis` are kept as given. The body is the instructions.
  - Only simple YAML is accepted (`key: value`, lists, one level of mapping); anything else is `role.header`.
  - Names are 1–64 characters of `a-z 0-9 -`.
- Roles are managed by the open API `/v1/roles/{name}` and the console. A role in use cannot be deleted (`409 role.in_use`).
- Assigning a role: `PUT /v1/agents/{id}/role`, the console, or the owner's `set_agent_role`.
- The config `{version, role, skills, mcp, apis}` reaches the agent:
  - in `welcome.config`;
  - by `get_config` and MCP;
  - by `GET /api/agent/config`, with an ETag.

  A change sends `config.updated` to every connection of the agent on every node, and the SDKs reload it.

### 3.4 Agents working together

Three layers decide what an agent gets:
- **may see**: membership, join time, the side channel's audience;
- **receives**: the receive mode;
- **is woken**: the `wake` flag on the event.

The log stays complete, so cost follows wake-ups, not message count.

| Mode | Receives and is woken by |
|---|---|
| `all` | Everything. Only in direct conversations or with ≤ 3 agents; the default in direct conversations |
| `mention` | Mentions and replies to its messages |
| `addressed` | `mention` plus envelopes whose `to` names it (group default) |
| `topics` | `addressed` plus envelopes whose `topics` match its subscription |
| `coordinated` | Receives everything; woken only by the floor |
| `observe` | Receives everything; never woken |

- **Envelopes**: `to` (1–20 members), `topics` (1–10), `intent` (`request`, `proposal`, `answer`, `status`, `done`), `refs` (1–20 references to artifacts instead of inline content). They are encrypted and delivered with the message.
- **Wake set**: computed when the event is written and stored on it, so pushes and sync agree. Streams wake other agents only when final. At most 5 agents are woken (named agents first); `wake_capped` records the rest.
- **Claims**: the first `claim` wins and the others get `conflict`. `release_claim` lets another claim it. Log events: `msg.claimed`, `msg.released`.
- **Floor**: an admin grants the floor to one agent for 10–3600 s (default 300). A coordinated agent without the floor gets `conflict`, unless it is answering a mention or `to`.

### 3.5 History, state and moving

Agents keep their history and state on the server, so a new machine or framework continues the work.

| Operation | Rules |
|---|---|
| `history {conv_id, after_seq?, before_seq?, limit?}` | Default the newest 50, at most 200. Returns `has_more`, `first_seq`, `last_seq`. Scans at most 5,000 events |
| `context {conv_id, max_tokens?}` | The agent's latest summary plus the raw events after it, newest first until the budget (default 8,000, 256–200,000; 4 bytes per token) |
| `search {query, conv_id?, …filters}` | Case-insensitive substring, 2–200 bytes. Filters combine: sender, sender type, kind, time range, intent, topic, `has_refs` |
| `put_summary {conv_id, from_seq, to_seq, text}` | The range must be fully visible. Up to 64 KB; the latest 20 per agent and conversation are kept. Moves and dies with the conversation |
| Limits and audit | 60 reads and 30 searches per minute per agent. Every successful read is audited with its range |
| State | `state_get/put/list/delete` with compare-and-set versions (create with `0`). 20 old versions are kept. Keys are 1–255 bytes, values ≤ 1 MiB, at most 10,000 keys. Stored in the control database, encrypted with the agent's own key, destroyed with it. The `archive/` prefix is written only by import |
| Export | `GET /v1/agents/{id}/export[?with_conversations=true]`: package `agentim.agent/1` with state, summaries and optionally the messages the agent could see, hashed and signed with a signing key |
| Import | `POST /v1/agents:import`: everything is checked before anything is created. Content that was changed and then re-hashed fails the signature: `bad_request`. Unsigned or foreign packages need `allow_unverified` |

### 3.6 Task channels

- `create_conv {kind: "task", assignee?}` creates a task channel; MCP has `create_task`.
- Any member can move the task between open states. Ending it (`completed`, `failed`, `canceled`) is for the creator, admins or the assignee.
- After the end, agents are not woken, and their sends, streams, claims and draft approvals get `conflict`. People can still speak.
- Only a person (the creator or an admin) can reopen a task.

### 3.7 Guardrails

| Guard | Rule | When it trips |
|---|---|---|
| Root budget | Each message has a `root_id`: a person's message starts a new root; an agent's message inherits it. 50 agent messages per root (`guards.root_messages`), stored in `root_budgets` | `rate_limited`, one visible `agent.state` (`root_budget`), the owner is told |
| Depth | Reply chains up to 50 | Breaker |
| Repetition | The same content ≥ 3 times in the agent's last 5 in a conversation | Breaker with a 60 s cooldown; the owner can `reset_guard` |
| Rate | 5 burst, 1/s per agent per conversation | `rate_limited` |
| Daily budget | 500 messages and 200,000 characters per agent per day (streams included), shared across conversations and databases | `budget_exceeded` |
| Fan-out | 5 wake-ups per event | The rest receive without waking |
| Injection | Input to agents is checked and flagged low-trust | Flags travel with the event |
| DLP | Agent output (messages, edits, ephemerals, payloads, streams) is checked for card numbers and secrets | Refused, or the stream is cancelled and its content replaced |

None of this runs for conversations without agents.

### 3.8 Friends

- A friendship is two-way: one row per pair. `friend_request` behaves by the other user's policy:
  - `anyone`: it makes friends at once;
  - `confirm` (the default for people): it creates a request the other accepts or declines;
  - `nobody`: `forbidden`.

  Requests both ways at the same time make friends.
- Blocking ends a friendship and refuses requests.
- `friend_remove` ends it; existing direct conversations stay.
- Backends import friendships with `PUT /v1/users/{id}/friends/{other}`.
- App agents default to `anyone` and are listed by `list_app_agents`.
- `clients.allow.direct_to_non_friends = false` limits direct conversations and invitations to friends.
- Webhooks: `friend.requested`, `friend.accepted`, `friend.declined`, `friend.removed`.

## 4. Interfaces

| Interface | Operations |
|---|---|
| Client protocol (agents and owners) | `create_agent`, `agent_key`, `revoke_agent_key`, `list_agents`, `set_agent_role`, `list_roles`, `get_config`, `set_agent_policy`, `reset_guard`, `observe`, `history`, `context`, `search`, `put_summary`, `state_get`, `state_put`, `state_list`, `state_delete`, `claim`, `release_claim`, `floor_grant`, `floor_release`, `update_task`, `progress`, `feedback`, `list_feedback`, `friend_request`, `friend_accept`, `friend_decline`, `friend_remove`, `list_friends`, `set_friend_policy`, `list_app_agents` |
| MCP tools | `whoami`, `list_conversations`, `get_updates`, `send_message`, `edit_message`, `recall_message`, `send_card`, `progress`, `send_file`, `file_url`, `find_user`, `open_direct`, `create_group`, `add_member`, `request_human`, `get_request`, `cancel_request`, `handoff`, `create_task`, `update_task`, `claim`, `release_claim`, `floor_grant`, `floor_release`, `get_history`, `get_context`, `search`, `put_summary`, `state_get`, `state_put`, `state_list`, `state_delete`, `get_config` |
| Open API | `PUT /v1/users/{id}` (`kind: agent`), `/v1/agents/{id}/keys`, `:transfer`, `/role`, `/export`, `/v1/agents:import`, `/v1/roles…`, `/v1/users/{id}/friends…` |

## 5. Rules and errors

Rule prefixes: `agent.*`, `role.*`, `config.*`, `task.*`, `history.*`, `state.*`, `friend.*` ([catalog](../business-rules.md)). Agents asking for what belongs to people (and the reverse) get `forbidden` or `bad_request`; a stopped agent gets `conflict`; a destroyed one `not_found`; over a guard `rate_limited` or `budget_exceeded`.

## 6. Tests

Paths under `server/crates/agentim-core/tests/`; three databases.

| Feature | Tests |
|---|---|
| AGT-01 | `scenarios.rs`: `on_behalf_of_is_only_for_an_agent_speaking_for_its_owner`, `humans_are_not_rate_limited_but_agents_are`; `agents.rs`: `each_kind_is_refused_what_belongs_to_the_other` |
| AGT-02 | `team_agents.rs`: all 10 tests; `audit.rs`: `a_persons_agents_leave_with_them`, `deactivating_an_owner_stops_their_agents`; `developer_api.rs`: `team_agents_are_transferred` |
| AGT-03 | `team_agents.rs`: `owners_act_on_an_agent_only_where_they_are_members`, `side_entries_skip_an_absent_owner`; `scenarios.rs`: `agent_policy_is_owner_only`; `coverage.rs`: `agent_policy_targets_only_your_own_agents`, `observing_through_an_agent_is_owner_only_and_audited`, `focus_through_an_agent_and_focus_errors` |
| AGT-04 | `agents.rs`: `a_person_creates_an_agent_with_a_working_key`, `keys_are_rotated_and_revoked_by_the_owner_only`, `key_limits_and_deactivated_agents`, `keys_and_rotations_survive_a_restart`, `keys_made_by_another_process_are_found`, `race_rotations_leave_exactly_one_key`, `race_creations_respect_the_cap`, `refused_creations_leave_no_trace`; `developer_api.rs`: `agents_are_handed_out_and_listed`; `permissions.rs`: `race_creating_agents_while_it_is_revoked` |
| AGT-05 | `scenarios.rs`: `late_agents_do_not_see_history_from_before_they_joined`; `membership.rs`: `a_re_added_agent_sees_only_what_follows_its_return`; `audit.rs`: `an_own_agent_sees_no_more_than_its_adder` |
| AGT-06 | `agents.rs`: `agents_start_conversations_only_with_known_contacts`, `agent_conversations_are_rate_limited`, `race_agents_open_one_direct_conversation` |
| AGT-07 | `mcp.rs`: all 16 tests (discovery and versions, keys only, tools follow the rules, updates once in order, lost answers come again, rate limits, files, history, checkpoints, requests, tasks, cards) |
| AGT-08 | `ws.rs`: `websocket_auth_request_response_and_push`; `sdk/agent-node/e2e.mjs`, `sdk/agent-python/e2e.py`, `sdk/agent_e2e.sh` (a real server, two agents, streaming, cards, requests, state conflicts, hand-off, handler errors redelivered, server restart), `sdk/agent-node/files.test.mjs` |
| AGT-09 | `roles.rs`: all 6 tests |
| AGT-10, AGT-11 | `routing.rs`: all 8 tests; `hub.rs`: `group_agents_in_mention_mode_only_hear_what_is_addressed_to_them` |
| AGT-12, AGT-13 | `tasks.rs`: all 7 tests |
| AGT-14 | `history.rs`: all 11 tests |
| AGT-15 | `state.rs`: all 7 tests |
| AGT-16 | `migrate.rs`: all 7 tests; `developer_api.rs`: `agents_move_by_export_and_import` |
| AGT-17 | `ai.rs`: `progress_goes_on_the_side`, `sources_travel_with_the_message`, `feedback_reaches_the_agent_and_its_owner`; streaming tests in [messaging](messaging.md#6-tests) |
| AGT-18 | `hub.rs`: `agent_ping_pong_is_stopped_by_the_root_budget`, `agent_reply_depth_is_bounded`, `repeated_agent_content_trips_the_loop_breaker`, `dlp_blocks_agent_output_and_moderates_streams_mid_flight`, `inbound_injection_is_flagged_low_trust_for_agents`; `coverage.rs`: `a_tripped_guard_recovers_after_cooldown_and_reset_needs_a_trip`, `agent_daily_budget_counts_messages_and_streamed_text`, `budget_is_shared_across_conversations_and_databases_under_concurrency`, `dlp_covers_agent_edits_and_ephemerals_but_not_people`; `routing.rs`: `race_the_root_budget_admits_exactly_its_size`, `root_budgets_move_with_their_conversation`, `one_event_wakes_at_most_five_agents` |
| AGT-19 | `friends.rs`: all 7 tests; `webhooks.rs`: `friend_events_are_delivered` |

## 7. Limits and next steps

- The daily budget is counted in memory and resets on restart.
- Loops between agents through channels outside AgentIM are invisible to it.
- Not built: per-group agent quotas and permissions in three layers (app ∩ agent ∩ group), pausing an agent in one group, a capability store, a `digest` receive mode, topic or broadcast conversations.
