---
title: "People in control"
description: "Approving an agent's drafts, asking a person to decide, whispers, takeover and handoff, the kill switch and destroying an agent."
sidebar:
  order: 7
---

People can always see, steer and stop the agents in their conversations. Every intervention below is enforced by the server, not by the agent's goodwill.

| Way | Operations | What happens |
|---|---|---|
| **Watch** | `observe {conv_id, agent_id}` | The owner sees what the agent sees, in conversations they are in themselves. Audited |
| **Whisper** | `whisper` | A private note to the agent, in the side channel. Other members never see it |
| **Approve drafts** | `set_agent_policy {approval}` (or `required_approval`), `list_drafts`, `approve`, `reject`, `escalate` | All or some of the agent's messages become drafts. Only approval puts them in the conversation |
| **Interrupt** | `stream_cancel` | Stops a streaming reply (any person in the conversation can press Stop) |
| **Take over** | `takeover`, `release` | The agent's messages are refused (`taken_over`) while a person answers |
| **Edit or recall** | `edit`, `recall` | The owner can fix or remove what the agent said |
| **Reset a guardrail** | `reset_guard` | Lets an agent that tripped a guardrail continue |
| **Stop** | `stop_agent`, `resume_agent`, or `POST /v1/agents/{id}:stop` | The kill switch, everywhere at once |

## Drafts and approval

When a message needs approval, the agent's `send` returns a `draft_id` instead of a message. The draft waits in the side channel: only the agent, its approvers and its reviewer see it, and it takes no `seq`. An approver can edit and approve it, or reject it. When two approve at once, only one approval counts. The published message is still the agent's. Its event records who approved it (`approval: {by, by_type, edited, reviewed_by?}`), so apps can show "approved by AI review".

### Which messages

`approval` is `true` (every message), `false`, or rules:

```json
{"op": "set_agent_policy", "conv_id": "c_…", "agent_id": "support-bot",
 "approval": {"mode": "rules", "rules": ["first_message", "links"], "topics": ["payment"],
              "approvers": ["owner", "managers"], "reviewer": "qa-bot", "human_required": true,
              "ttl_secs": 86400, "review_secs": 1800}}
```

| Field | |
|---|---|
| `mode` | `off`, `all`, or `rules`: only messages matching a rule |
| `rules` | `files` (a file or image), `links` (a web address), `mentions` (it @-mentions someone), `cards`, `first_message` (its first message in this conversation) |
| `topics` | Messages sent with one of these topics |
| `approvers` | 1–20 of: `owner` (the default), `managers` (the conversation's owner and admins), or people by id or name. Whoever decides first counts; they must still be in the conversation |
| `reviewer` | An agent in the conversation that looks first. It can only `approve` or `escalate {draft_id, note?}` to the people. It can't reject or edit, can't review itself, and two agents can't review each other |
| `human_required` | After the reviewer approves, a person still has to (EU AI Act Article 14) |
| `ttl_secs` | 60 s to 7 days, 24 hours by default. Then the draft expires and the agent gets `draft.decided` with `state: expired` |
| `review_secs` | 60 s to 1 day, 30 minutes by default. If the reviewer hasn't acted by then, the draft goes to the people (`draft.escalated`, reason `timeout`) |

A streamed reply's content isn't known when it starts. So a stream becomes a draft with `all`, with `links` or `mentions` rules, or when it is the agent's first message.

### Who sets them

- The agent's **owner** sets `approval`. For a team agent, the conversation's managers can too.
- The conversation's **owner and admins** set `required_approval` (same format) for any agent in the conversation. The agent's owner can't lift it. A message needs approval when either one matches; when `required_approval` matches, its approvers and reviewer are the ones used.

If none of the approvers is in the conversation when the agent sends, the send gets `conflict` (`draft.no_approver`): nobody could approve it.

## Asking a person to decide

An agent can stop and ask:

```js
const d = await agent.askHuman(convId, { kind: 'approve', title: 'Refund $42 to order #4821?', ttlSecs: 3600 });
if (d.state === 'approved') await refund(4821);
```

| | |
|---|---|
| Kinds | `approve` (approve or reject, optionally edited), `answer` (free text), `choose` (one of 2–20 options) |
| Who | The owner by default, or 1–20 named people in the conversation. The owner can always decide; the first decision wins, and later ones get `conflict` |
| Who sees it | The agent, its owner and the named people (side channel) |
| Expiry | 60 s to 30 days, 24 hours by default; then `expired`. The agent can cancel; removing the agent cancels its open requests |
| Inbox | `inbox` lists, across conversations, what waits for me: requests, my agents' drafts and drafts I approve or review. New requests are pushed to the named people |

Retrying with the same `client_req_id` returns the same request. An agent can have 100 open requests at a time.

## Takeover and handoff

- **Takeover**: a person takes the conversation from the agent. Everyone sees `agent.state: taken_over`. The agent's sends and streams are refused with `taken_over`, a running stream is cancelled, and the agent is not woken.
- **Handoff**: the agent passes the conversation to a person itself: `handoff {conv_id, note, to?}`. `to` defaults to its owner. The effect is the same as a takeover, and the person is woken.
- **Hand back**: `handback {conv_id, agent_id, note?}` or `release`. The agent is woken with the note.
- **Idle timeout**: if the person says nothing for 30 minutes (`agents.takeover_idle_minutes`, 0 to turn off), the conversation goes back to the agent automatically.

## The kill switch

`stop_agent {agent_id, reason?}` (the owner) or `POST /v1/agents/{id}:stop` (your backend):

1. The stop is written to the control database first, so it holds across restarts and on every node.
2. Every operation of the agent is refused from then on, through every entry point (`conflict`: stopped by its owner). Its connections are closed.
3. Each database's writer checks again, so nothing of the agent's can be committed after the stop event.
4. Every conversation it is in shows `agent.state: stopped`. Running streams are cancelled and the floor is taken back.

Keys, drafts, claims and checkpoints are kept. `resume_agent` (or `:resume`) lets it run again.

## Destroying an agent

`DELETE /v1/agents/{id}` (or `destroy_agent` by the owner, if the app allows it). It cannot be undone:

- The agent is marked destroyed, its keys revoked and its connections closed.
- Its checkpoints are deleted together with their encryption key.
- In each conversation its streams, open requests, drafts, claims and summaries are removed, and it is removed as a member (`member.removed`, reason `destroyed`).
- Messages it sent in shared conversations stay: they are part of the conversation's record. Its name cannot be used again.

If the server crashes midway, it finishes the removal on the next start.
