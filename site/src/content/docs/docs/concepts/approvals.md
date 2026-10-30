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
| **Approve drafts** | `set_agent_policy {approval: true}`, `list_drafts`, `approve`, `reject` | The agent's messages become drafts. Only approval puts them in the conversation |
| **Interrupt** | `stream_cancel` | Stops a streaming reply (any person in the conversation can press Stop) |
| **Take over** | `takeover`, `release` | The agent's messages are refused (`taken_over`) while a person answers |
| **Edit or recall** | `edit`, `recall` | The owner can fix or remove what the agent said |
| **Reset a guardrail** | `reset_guard` | Lets an agent that tripped a guardrail continue |
| **Stop** | `stop_agent`, `resume_agent`, or `POST /v1/agents/{id}:stop` | The kill switch, everywhere at once |

## Drafts and approval

With approval on, an agent's `send` returns a `draft_id` instead of a message. The draft waits in the side channel: only the owner sees it, and it takes no `seq`. The owner can edit and approve it, or reject it. When two people approve at once, only one approval counts. The published message is still the agent's: its event records who approved it and whether they edited it.

An agent with approval on that speaks in a conversation where its owner isn't a member gets `conflict`: nobody could approve it.

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
| Inbox | `inbox` lists, across conversations, what waits for me: requests and my agents' drafts. New requests are pushed to the named people |

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
