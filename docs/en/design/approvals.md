# Approvals and human control

How people stay in charge of agents. Agents' messages can wait as drafts. Agents can ask people to decide. Operations that change data wait for approval, chosen by operation and amount. Everything waiting for a person is in one inbox. People can take over, hand back, interrupt, stop and destroy agents.

Code: `server/crates/agentim-core/src/logic.rs` (drafts, approval rules, take-over), `policy.rs` (approval rules), `logic/requests.rs` and `hub/requests.rs` (requests and the inbox), `hub/lifecycle.rs` and `logic/lifecycle.rs` (stop, destroy), `hub/assistants.rs` (operation approvals of built-in assistants). UI: `sdk/react/src/inbox.tsx`, `components.tsx` (`ApprovalCards`).

## 1. Purpose

The EU AI Act (Art. 14) asks that people can oversee AI used for consequential work and can decide not to use or override its output. AgentIM makes that part of the protocol:
- a person approves what an agent says when the rules ask for it;
- a person approves what an assistant does when it changes data;
- a person can take the conversation over at any moment.

## 2. Feature points

| ID | Feature | Behaviour |
|---|---|---|
| APR-01 | Drafts | When approval applies, an agent's `send` returns a `draft_id`. The draft lives on the side channel without a seq; only an approved draft enters the log. One decision wins |
| APR-02 | Approval rules in two layers | `approval` is set by the agent's owner. `required_approval` is set by the conversation's managers, and the owner can neither change nor bypass it. Whichever asks for approval wins |
| APR-03 | What triggers approval | `mode`: `off`, `all` or `rules`. Rules: `files`, `links`, `mentions`, `cards`, `first_message`, and up to 20 `topics` |
| APR-04 | Who approves | `approvers`: `owner` (default), `managers`, or named people who must still be members. Any one of them decides |
| APR-05 | Review agents | A member agent reviews first. It can only `approve` or `escalate`: it cannot reject or edit. `human_required` sends it on to a person anyway. A review that times out (`review_secs`) goes to a person |
| APR-06 | Draft expiry | `ttl_secs` (60 s – 7 days, default 24 h). An expired draft is void and the agent is told |
| APR-07 | Approved messages show who approved | An approved message records `approval: {by, by_type, edited}` |
| APR-08 | Ask a person | `request_human` with `kind` `approve`, `answer` or `choose`. It goes to the owner or named members, and the first decision wins. It expires (60 s – 30 days) and can be cancelled. The agent gets the outcome as an event or by waiting |
| APR-09 | Operation approvals | A connection's write tools are `read_only`, `confirm` or `allow`, plus up to 20 rules by operation and amount, for example: refunds over $30 need Sam; address changes run at once. The approved call runs with the stored arguments, at most once ([assistants](assistants.md)) |
| APR-10 | Operation card | The request carries `context.operation`: what, the values, the amount, the rule. The approver sees a card in the chat and in the inbox |
| APR-11 | The inbox | `inbox {limit?}` lists everything waiting for the signed-in person across conversations: requests to decide, drafts of their agents, conversations handed to them. A request that names its deciders is shown only to them |
| APR-12 | Take-over and hand-back | `takeover` locks the agent out of a conversation: its sends get `taken_over` and its streams are cancelled. `release` ends it, and so does silence for `agents.takeover_idle_minutes` (30). Everyone sees `agent.state` |
| APR-13 | Hand-off by the agent | `handoff {to?, note}`: the agent passes the conversation to a person (default its owner). `handback` returns it. While handed off, the agent is not woken |
| APR-14 | Interrupt, whisper, observe | Any person can stop a stream (the agent is told). The owner can whisper to the agent and observe what it sees (audited) |
| APR-15 | Stop and resume | `stop_agent`: every operation of the agent gets `conflict` and its connections close. It holds across restarts and in every database writer. `resume_agent` undoes it |
| APR-16 | Destroy | `destroy_agent {confirm: name}`: irreversible. It revokes the keys, deletes the state with its data key, cancels streams, requests, drafts, claims and floors, and removes the agent from every conversation. Its messages stay; its name stays taken. A crash midway is finished at start |

## 3. Design

### 3.1 Drafts and rules

- The two layers have the same shape: `{mode, rules, topics, approvers, reviewer, human_required, ttl_secs, review_secs}`. The old boolean `approval: true/false` still works and means `all` / `off`.
- When a message matches both layers, `required_approval`'s approvers and reviewer apply. `human_required` applies if either layer asks for it.
- **Streams**: the content is unknown when a stream starts. The whole stream becomes a draft when:
  - the mode is `all`;
  - the rules include `links` or `mentions`;
  - or it is the agent's first message.

  The `files`, `cards` and topic rules do not apply to streams.
- **Who sees a draft**: the agent, the approvers still in the conversation, and the reviewer. With nobody to approve, the send returns `conflict` (`draft.no_approver`).
- **Review agents**:
  - They must be members.
  - They cannot review themselves, and two agents cannot review each other.
  - `escalate {draft_id, note?}` is only for them.
- **Timers**: draft expiry, review timeouts and request expiry share one timer. They survive restarts because the deadlines are stored.
- An approved draft keeps its kind, payload and files. Its files become visible only on approval.

### 3.2 Requests

| Aspect | Rule |
|---|---|
| Create | `request_human {conv_id, kind, title, options?, context?, assignees?, ttl_secs?, client_req_id}`. `title` up to 4 KB; `context` any JSON up to 32 KB (encrypted). The same `client_req_id` returns the same request |
| Who decides | The owner by default. Named assignees (1–20 members, people only) replace the owner. Any one decides |
| Who sees it | The agent, the owner and the assignees (side channel) |
| Decide | `decide {request_id, decision, text?, choice?}`. The first decision wins and later ones get `conflict`. A decision that does not fit the kind is `bad_request` |
| End | It expires after its ttl. The agent can `cancel_request`. Removing the agent cancels its pending requests |
| Outcome | Side events `request.decided`, `request.expired`, `request.cancelled`; or `get_request {wait_secs}` waits up to 25 s |
| Limits | At most 100 pending requests per agent |

### 3.3 Operation approvals

For built-in assistants (and any agent using `request_human` with `context.operation`):
- **Who approves**: the person being answered approves a `confirm` write. When that person is not a member, the agent's owner approves. A matching rule names its own approvers; when none of them is a member, the owner approves.
- **No blocking**: the model is told "not done yet, waiting for X" and says so in its reply. The call is stored in the request, so a restart loses nothing.
- **After approval**: the stored call runs with the stored arguments; the model does not regenerate them. Before running, the server checks again that:
  - the connection still exists and still allows writes;
  - the tool is still on the role.

  A claim on `ai-q-<request>` makes it run at most once. API calls carry `Idempotency-Key: <request>`.
- **Declined** requests go back to the model with the reason. **Expired** or cancelled ones get a fixed reply: "not approved, so not done".
- **Rules**: up to 20 per connection, first match wins. Each rule has `tool`, an optional `amount_field` + `over` (a missing or non-number value counts as over), `approvers`, and `confirm: false` for a write that runs at once. Saving a connection without `approvals` keeps the rules; `[]` clears them. The console has a rule editor.

### 3.4 The inbox

- `inbox {limit?}` (1–500, default 100, with `more`) lists, newest first:
  - requests the person may decide, with `conv_title`, `agent_name` and `asker_name`;
  - drafts of the person's agents;
  - conversations handed off to them.
- New requests are pushed to online assignees.
- `<Inbox>` in React and React Native has tabs with counts, search, and a list and detail that become two pages on a phone. Its actions are decide, approve, edit and send a draft, discard, open, and hand back ([chat-components](chat-components.md)).

### 3.5 Take-over and hand-off

- Take-over and hand-off are the same record, a lock stored in the database:
  - the agent's sends and streams get `taken_over`;
  - its running stream is cancelled;
  - it is not woken.
- `agent.state` shows who has the conversation and why. A hand-off records `handoff: true` and the note.
- An idle take-over (no message from the person for `takeover_idle_minutes`) ends by itself with `reason: idle_timeout`.
- A built-in assistant's `handoff_to_person` tool hands off after its reply ends, so the reply that explains it is not cut.

### 3.6 Stop and destroy

- **Stop**:
  1. The stop is written to the control database first, so it holds across restarts.
  2. Every path (WebSocket, MCP, open API on its behalf) then refuses, except `hello` and MCP `whoami`.
  3. Every database writer checks again, so nothing of the agent appears after the stop event.
  - Only the owner's client can stop an agent. A CLI command would be another process the server does not hear at once.
- **Destroy**:
  1. One write marks the agent destroyed.
  2. Its checkpoints and data key are deleted (crypto-shredding), and its keys are revoked.
  3. Then conversation by conversation: streams and requests are cancelled; drafts, claims and summaries are deleted; floors are taken back; the agent is removed.
  - The user row stays as a tombstone. Start-up repairs a destroy that was interrupted.

## 4. Interfaces

- Client protocol:
  - `set_agent_policy {approval, required_approval, trigger, topics}`, `list_drafts`, `approve {draft_id, text?}`, `reject`, `escalate`;
  - `request_human`, `get_request`, `cancel_request`, `decide`, `inbox`;
  - `takeover`, `release`, `handoff`, `handback`, `stream_cancel`, `whisper`, `observe`;
  - `stop_agent`, `resume_agent`, `destroy_agent`.
- Open API: stop, resume and destroy on `/v1/agents/{id}`. Connections and their `approvals` are in the admin API.
- Events: `draft.*` and `request.*` on the side channel; `agent.state` in the log.

## 5. Rules and errors

Rule prefixes: `draft.*`, `approval.*`, `request.*`, `agent.*` ([catalog](../business-rules.md)).
- `conflict`: decided, expired, taken over, stopped, no approver.
- `forbidden`: not an approver, not the owner, or a reviewer acting beyond approve/escalate.
- `bad_request`: a decision that does not fit, or a bad rule.

## 6. Tests

Paths under `server/crates/agentim-core/tests/`; three databases.

| Feature | Tests |
|---|---|
| APR-01 | `scenarios.rs`: `approval_drafts_are_private_until_decided`; `coverage.rs`: `streamed_drafts_stay_private_until_approved`; `concurrency.rs`: `approve_and_reject_race_has_one_outcome`; `edges.rs`: `a_policy_hit_inside_a_streamed_draft_discards_it`; `files.rs`: `files_of_drafts_wait_for_approval` |
| APR-02–APR-06 | `approvals.rs`: `rules_pick_the_messages`, `managers_require_approval`, `a_reviewer_goes_first`, `deadlines`, `one_decision_wins`; `team_agents.rs`: `an_agent_needing_approval_cannot_speak_without_its_owner` |
| APR-07 | `requests.rs`: `approved_drafts_carry_the_approval`; `encryption.rs` (audit keeps a hash of the approved draft) |
| APR-08 | `requests.rs`: `every_kind_is_asked_and_decided`, `only_the_right_people_decide_once`, `race_decisions_have_one_winner`, `requests_expire`, `removal_cancels_and_limits_hold`, `requests_are_seen_by_their_parties_and_can_be_awaited`, `requests_survive_a_restart`, `requests_live_with_their_conversation_across_databases`, `refused_requests_leave_no_trace`; `mcp.rs`: `agents_ask_people_to_decide` |
| APR-09, APR-10 | `assistant_tools.rs`: `writes_wait_for_approval`, `approvals_by_operation_and_amount`; `server/ci/console_smoke.mjs` (the rule editor); `examples/test/e2e.mjs` (after-sales refunds) |
| APR-11 | `requests.rs`: `the_inbox_lists_what_waits_for_me`; `sdk/react/test/chat-components.test.tsx`, `sdk/react-native-ui/test/chat-components.test.tsx` |
| APR-12 | `scenarios.rs`: `takeover_rules`; `hub.rs`: `takeover_blocks_the_agent_until_released`, `a_silent_takeover_goes_back_to_the_agent`; `concurrency.rs`: `takeover_racing_agent_messages_is_consistent`; `coverage.rs`: `approving_while_taken_over_is_refused`, `race_approval_against_takeover` |
| APR-13 | `ai.rs`: `an_agent_hands_off_and_gets_it_back`, `a_handoff_survives_a_restart`, `handoff_refusals_leave_no_trace`, `race_one_handback`, `race_nothing_from_the_agent_while_handed_off` |
| APR-14 | `hub.rs`: `humans_can_interrupt_and_the_agent_is_told`; `scenarios.rs`: `whisper_is_only_between_an_owner_and_its_agent`; `coverage.rs`: `observing_through_an_agent_is_owner_only_and_audited` |
| APR-15, APR-16 | `lifecycle.rs`: all 7 tests; `developer_api.rs`: `stopping_resuming_and_destroying_agents`; `mcp.rs`: `stopped_and_destroyed_agents_are_refused` |

## 7. Limits and next steps

- Multi-step approval chains are not built.
- External agents (run by an SDK) see `writes` in their connection config and must ask with `request_human` themselves; AgentIM cannot stop their outside calls.
