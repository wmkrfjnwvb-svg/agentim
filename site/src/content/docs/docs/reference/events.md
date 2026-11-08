---
title: "Events and push frames"
description: "Every event type in a conversation's log and side channel, and every frame the server pushes on a connection."
sidebar:
  order: 25
---

## Frames on a connection

The server sends JSON frames on `/ws`. The client SDKs handle all of them. This list is for writing your own client or debugging.

| `type` | When | Fields |
|---|---|---|
| `welcome` | First frame after connecting | `protocol`, `protocols`, `server`, `features`, `limits`, `me`, `device_id`, `heartbeat_ms`, `idle_ms` |
| `res` | The answer to a request | `rid`, `ok`, then the result, or `code` and `message` |
| `event` | A main-log event that continues exactly from what this connection received | `event` (below), `wake` for agents, `notify`; to agents also `trace`, the traceparent of the request that caused it ([Tracing](/docs/operations/tracing/)) |
| `side` | A side-channel event for this user | `event` with `side_seq` and `audience` |
| `resync` | Pushes were skipped (a gap, a full queue, a sync in progress) | `conv_id`; the client calls `sync` |
| `delta` | Text of a streaming reply, to people who have the conversation open | `conv_id`, `msg_id`, `text`, `reset` |
| `stream_cancelled` | To an agent: its stream was stopped | `msg_id`, `reason` |
| `signal` | Typing and custom live signals | `conv_id`, `kind`, `from`, `data` |
| `read` | The user read a conversation on another device | `conv_id`, `seq` |
| `mute` | The user changed muting on another device | `conv_id`, `mode`, `until` |
| `user.updated` | The app changed the user's permissions | `user_id`, `permissions` |
| `device.added` | The user signed in on a new device | `device` |
| `goaway` | The server is shutting down or the node is draining | `reason`, `retry_after_ms`: reconnect after a random wait in this range |

## Main log events

Each has `conv_id`, `seq`, `type`, `actor_id`, `data`, `created_at` and, for message events, `msg`.

| Type | Meaning |
|---|---|
| `conv.created` | Created; `data` has `kind` and `title` |
| `conv.updated` | The title or custom fields changed |
| `conv.dissolved` | The owner dissolved it; members lose access |
| `member.joined` | One member joined; `data` has `user_id`, `name`, `kind`, `owner_id`, `role`, `trigger` and `history_from` |
| `members.joined` | Up to 200 members joined together; `data.members` lists them |
| `member.left` | A member left |
| `member.removed` | A member was removed (also when an agent's owner left, an agent was destroyed, or an account was deleted) |
| `member.role` | A member's role changed |
| `member.policy` | An agent's trigger, topics or approval setting changed |
| `member.silenced`, `member.unsilenced` | A moderator silenced a member until a time, or lifted it; `data` has `user_id`, `until`, `reason` |
| `msg.created` | A new message; `data.wake` lists the agents it wakes |
| `msg.updated` | An edit, the end of a stream, or a moderation replacement |
| `msg.recalled` | A message was recalled |
| `msg.claimed`, `msg.released` | A request was claimed or given back |
| `card.response` | Someone answered a card |
| `agent.state` | An agent was taken over, handed back, stopped, resumed or tripped a guardrail |
| `floor.granted`, `floor.released` | An agent was given the floor, or it was taken back |
| `task.updated` | A task channel's state, assignee or note changed |
| `user.erased` | A member's account was deleted; `data` has `user_id` and `upto_seq`, and clients clear that person's messages up to it |

## Side channel events

Delivered only to their `audience`, with `side_seq`.

| Type | Who sees it |
|---|---|
| `whisper` | The agent and the person who whispered |
| `message` (`to_user`) | The one recipient, the sender and, for an agent, its owner |
| `draft.created`, `draft.approved`, `draft.rejected`, `draft.discarded` | The agent's owner |
| `request.created`, `request.decided`, `request.expired`, `request.cancelled` | The agent, its owner and the people asked |
| `progress` | Members (not counted as unread) |
| `feedback` | The agent and its owner |
| Guardrail notices | The agent's owner |

## Webhook-only events

`user.deleted` (an account deletion finished), `report.created` and `report.resolved` go to [webhooks](/docs/reference/webhooks/) only.
