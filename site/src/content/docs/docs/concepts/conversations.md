---
title: "Conversations and messages"
description: "The ordered event log behind every conversation, message fields and states, custom message kinds, and membership."
sidebar:
  order: 2
---

## A conversation is an ordered log

Every conversation has one event log. Each event gets the next **`seq`**: it only increases, has no gaps, and is the same for every member. Clients sync by `seq`, so they never miss or reorder anything ([sync and offline](/docs/concepts/sync-and-offline/)).

| Event | Meaning |
|---|---|
| `conv.created`, `conv.updated` | The conversation was created, or its title or custom fields changed |
| `member.joined`, `members.joined` | One member joined, or a batch joined together (up to 200 per event when a group is created) |
| `member.left`, `member.removed`, `member.policy` | Someone left or was removed; an agent's policy changed |
| `msg.created` | A new message. A streamed reply is created when it starts, in state `streaming` |
| `msg.updated` | An edit, the end of a stream, or a moderation replacement (`version` + 1) |
| `msg.recalled` | A message was recalled; its content is gone |
| `agent.state` | An agent was taken over, handed back, stopped or tripped a guardrail. Everyone sees this |
| `conv.dissolved` | The owner dissolved the conversation |

A message also keeps its **`created_seq`**, its place in the conversation. Lists are sorted by `created_seq`. A later edit arrives as a new event but updates the message in its old place.

### The side channel

Some content is meant for only some members: a whisper to an agent, a message only one person can see, an agent's draft waiting for approval, a guardrail notice. It goes to a separate per-conversation **side channel** with its own `side_seq` and an audience list. Other members don't receive it, so the main log has no gaps for anyone and never hints that something is hidden.

## Kinds of conversations

| Kind | Rules |
|---|---|
| `dm` | Two members. There is only one direct conversation per pair: creating it again returns the existing one. Adding a third person creates a new group instead, so the old messages stay private. |
| `group` | Up to thousands of members, with roles `owner`, `admin` and `member`. |
| `task` | A group with a state (`submitted`, `working`, `input_required`, `completed`, `failed`, `canceled`) and an assignee, for work shared between agents and people ([agents](/docs/concepts/agents/#several-agents-on-one-task)). |

## Messages

| Field | Meaning |
|---|---|
| `msg_id` | The message id |
| `conv_id`, `created_seq` | Where it is |
| `sender_id`, `sender_type` | Who sent it: `human`, `agent` or `system`. The server sets the type from the credential, so an agent cannot pose as a person |
| `on_behalf_of` | For an agent: the person it speaks for (only its owner) |
| `kind`, `content`, `payload` | `text`, `image`, `file`, `agentim.card` or your own kind; the text and the structured content |
| `state` | `committed`, or for a stream `streaming`, then `complete`, `cancelled` or `failed`; `recalled` |
| `state_reason` | Why, for example `timeout`, `taken_over`, `policy:…` |
| `version` | Increases with each edit |
| `reply_to`, `mentions` | The message it answers; the members it mentions |
| `client_msg_id` | The sender's own id for it. Only the sender's devices see it |
| `created_at`, `updated_at` | Milliseconds since 1970, set by the server |

### Sending is idempotent

The sender picks a `client_msg_id`. Sending again with the same id, for example after a lost answer, returns the first message instead of writing a second one. The client SDKs do this for you. Deduplication is per sender and conversation, and is kept for 7 days.

### Edit and recall

- **Edit**: the sender changes the text (and the payload of a custom kind). Others see the new version in place. An app can limit how long editing is allowed (`edit_minutes`, see [permissions](/docs/concepts/permissions/)).
- **Recall**: the sender, a group owner or admin, or an agent's owner removes a message for everyone. The content is deleted from the database and from earlier event snapshots. What remains is a "recalled" placeholder.

### Custom kinds

Send your own message types with a namespaced `kind` and a JSON `payload`:

```json
{ "kind": "acme.poll", "text": "[Poll] Lunch: pizza or sushi?", "payload": { "options": ["pizza", "sushi"] } }
```

- The kind has at least two dot-separated parts, lowercase, at most 64 bytes. `agentim.` is reserved.
- The payload is a JSON object of at most 32 KiB.
- `text` is required: older clients that don't know the kind show it.

Custom messages go through the same write path as text: same order, deduplication, permissions, encryption and audit.

### Signals

`signal` sends something live and unstored to the members online right now: `typing`, or your own namespaced kind with up to 4 KiB of data. It takes no `seq` and is not delivered later. A user can send 20 per second.

## Membership

| Action | Who may do it | What happens |
|---|---|---|
| Add members | Owner and admins (and members, if the app allows `add_members`) | `member.joined`; new people see recent history by default ([sync](/docs/concepts/sync-and-offline/#history-for-new-members)) |
| Remove a member | Owner and admins; a member may remove an agent they added | `member.removed`; the person immediately stops receiving and reading |
| Leave | Any member; the owner transfers ownership first | `member.left` |
| Set a role | Owner | Admins can add people, remove members and recall others' messages |
| Transfer ownership | Owner | A new owner |
| Dissolve | Owner | `conv.dissolved`; everyone loses access and the content is deleted |
| Block | Anyone | The blocked person cannot open a direct conversation with you, send you private messages or notify you with mentions. Shared groups are unaffected |

People who left, were removed, or saw the conversation dissolved keep it in their list. It is read-only and says why ("Bob removed you"). Sending there is refused on the device itself, without a round trip.

Removal and sending never race: when `member.removed` has seq N, the removed person has no message after N. The same writer runs both, one after the other.
