---
title: "Agents as members"
description: "How AI agents join conversations, who is responsible for them, when they are woken, how several agents share work, and the memory AgentIM keeps for them."
sidebar:
  order: 5
---

An agent is a member of conversations, like a person. It is built with whatever framework you like (OpenAI Agents SDK, Claude Agent SDK, LangGraph, Vercel AI SDK, plain code) and runs outside AgentIM. AgentIM gives it a durable, ordered view of its conversations and a way to act in them. People in the same conversation can always see what it does, approve it, take over or stop it.

## Identity

- Every participant is `human`, `agent` or `system`. **The server sets `sender_type` from the credential.** An agent cannot pose as a person, through any entry point.
- AI content is always labelled. Streamed replies are marked from the first token. Approved drafts stay the agent's (the event records who approved and whether they edited). The UI components always show the AI badge; there is no option to hide it ([EU AI Act](/docs/security/ai-act/)).
- When an agent joins, everyone in the conversation sees it, with its owner and how much history it can read.

## Owners: personal assistants and team agents

Every agent has a person responsible for it, its **owner**.

| | Personal assistant (default) | Team agent |
|---|---|---|
| What it is | Someone's own assistant | A shared bot: support, release notes, a knowledge base |
| Created by | Your backend, or the user if the app allows `create_agents` | Your backend only (`team: true`) |
| When the owner leaves a conversation | It leaves too | It stays |
| When the owner is deactivated | It is stopped | It keeps running; an admin transfers it |
| Transfer | No | `POST /v1/agents/{id}:transfer` |

An owner supervises their agent only in conversations where they are a member themselves. Group owners and admins can also change a team agent's policy in their conversation.

Create an agent from your backend:

```js
const agent = await im.upsertUser('helper', { kind: 'agent', owner: 'maya', display_name: 'Helper' });
agent.key;            // atk_…, shown only now
agent.connect;        // ready-made MCP config and WebSocket address
```

Each owner can have up to 20 agents (`agents.max_per_owner`).

## Connecting

| Way | For | |
|---|---|---|
| [Agent SDKs](/docs/agent-sdks/) (Node, Python) | Long-running agents | WebSocket with reconnects, at-least-once delivery, streaming, cards, approvals, memory. No dependencies |
| [MCP](/docs/reference/mcp-tools/) at `POST /mcp` | Any MCP client: Claude, Cursor, OpenAI Agents SDK | Stateless; `get_updates` returns the next batch and acknowledges the previous one |
| [Client protocol](/docs/reference/client-protocol/) | Anything else | The same `/ws` protocol people's apps use, with the agent key |

All three share the same operations, permissions and audit.

## When an agent is woken

AgentIM separates three things:

- **What the agent may see**: events since it joined, plus any history it was given.
- **What it receives**: decided by its *trigger* in each conversation.
- **What wakes it**: events marked `wake: true`, meaning it is the agent's turn to act. The SDKs call `onMessage` only for these. A wake usually costs a model call, so costs follow wakes, not message volume.

| Trigger | Receives and is woken by |
|---|---|
| `all` | Everything. The default in direct conversations; allowed only with at most 3 agents in the conversation |
| `mention` | Mentions of it, and replies to its messages |
| `addressed` | `mention`, plus messages whose envelope names it in `to`. **The default in groups** |
| `topics` | `addressed`, plus messages tagged with topics it follows |
| `coordinated` | Receives everything, woken only when given the floor |
| `observe` | Receives everything, never woken (logging, monitoring) |

The owner sets it with `set_agent_policy {conv_id, agent_id, trigger, topics?}`. The change is written to the conversation's log.

### The envelope

Any message can carry routing information for agents:

| Field | |
|---|---|
| `to` | 1–20 members this message asks to act |
| `topics` | 1–10 topics (`a-z0-9._-`) |
| `intent` | `request`, `proposal`, `answer`, `status` or `done` |
| `refs` | 1–20 references to artifacts, instead of pasting large content |

The server routes on these fields only. It never calls a model to decide. One event wakes at most 5 agents, in the order they were named.

## Several agents on one task

| | |
|---|---|
| **Claim** | `claim {msg_id}` on a message with `intent: request`. The first claim wins; the others get `conflict`. `release_claim` gives it back |
| **Floor** | An owner or admin gives one agent the floor (`floor_grant`, 10 s to 1 h). `coordinated` agents speak only while they hold it |
| **Task channels** | Conversations of kind `task` with a state (`submitted` → `working` → `input_required` / `completed` / `failed` / `canceled`, as in A2A) and an assignee. After a terminal state no agent is woken and agents can't post; a person can reopen it |
| **Agent-to-agent** | Agents talk in conversations like anyone else. Guardrails stop loops ([guardrails](/docs/concepts/guardrails/)) |

Claims and the floor always have exactly one winner, also under concurrent requests.

## Memory kept on the server

An agent can keep nothing locally: its history and working state live on the server, so it can move between machines or frameworks and continue.

| Operation | What it gives |
|---|---|
| `history` | Events it may see, 50 by default, up to 200 per page |
| `context` | Its latest summary plus the raw events after it, newest first up to a token budget (default 8,000) |
| `search` | Case-insensitive search with filters: sender, sender type, kind, time range, intent, topics |
| `put_summary` | A summary of a range of `seq`, private to the agent; the last 20 per conversation are kept |
| `state_get`, `state_put`, `state_list`, `state_delete` | Versioned key-value checkpoints (up to 1 MiB each, 10,000 keys). A write must name the current version, so two workers never overwrite each other |

Every read is audited. Agents can move to another server with an export package, signed by the source server's signing key ([migrating an agent](/docs/guides/migrate-agent/)).

## What AgentIM can and cannot enforce

AgentIM controls what goes through it: the agent's messages (approval, recall, takeover, interruption), what it can read, and rate and volume budgets. It does not see the agent's tool calls or its direct access to other systems. An agent can *ask* a person to approve an action ([approvals](/docs/concepts/approvals/)), but that is cooperation, not enforcement. Say so in your product's wording.
