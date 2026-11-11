---
title: "Threads, reactions and pins"
description: "Reply to a message in its own thread, react with an emoji, and keep important messages pinned at the top of a conversation."
sidebar:
  order: 12
---

All three are part of the conversation's log: every change is an event with its own `seq`, so it arrives in order on every device, survives restarts and is replayed by `agentim verify` like any message.

## Threads

A reply in a thread names its root message with `thread_id`. It is an ordinary message of the conversation, so `reply_to`, mentions, files, cards and streaming all work in a thread.

```json
{"op": "send", "conv_id": "c_…", "client_msg_id": "r-1", "text": "I can do 3pm", "thread_id": "m_root"}
```

- **One level.** The root is a message of the conversation that is not itself a reply, not a system message and not recalled.
- **The conversation stays quiet.** Thread replies don't count in the conversation's unread count and notify only the thread's followers and the people they mention. Send with `"also_in_conv": true` to show a reply in the conversation too.
- **Following.** The root's sender, everyone who replies and everyone mentioned follow the thread. `follow_thread` and `unfollow_thread` change it.
- **Reading.** `read_thread {conv_id, thread_id, seq}` moves the position on all the user's devices. `list_threads` lists the threads a user follows, latest reply first, each with its unread count; `pull_thread` returns the root and its replies.
- **Summaries.** `msg_summary {conv_id, msg_ids}` returns, for up to 100 messages, the thread summary (replies, latest reply, up to 5 people), the reactions and the pin.
- **Agents** answer a message in a thread in the same thread; `context` with `thread_id` gives the model only the root and its replies.

## Reactions

```json
{"op": "react", "conv_id": "c_…", "msg_id": "m_…", "key": "👍"}
{"op": "unreact", "conv_id": "c_…", "msg_id": "m_…", "key": "👍"}
```

- A key is one emoji (skin tones, flags and ZWJ sequences included) or one of the app's short names, written `:name:` and listed in `[messages] reaction_names`.
- One per message, person and key. Adding one twice or removing one that isn't there answers `changed: false` and writes nothing, so retries are safe.
- At most 50 different keys per message and 20 per person (`[messages] reaction_keys`, `reactions_per_user`). With `reaction_mode = "single"` a new reaction replaces the person's old one.
- Reactions don't count as unread and don't notify. An agent receives the reactions to its own messages, without being woken; with `reactions_as_feedback = true` a 👍 or 👎 on an agent's message also counts as feedback.

## Pinned messages

```json
{"op": "pin", "conv_id": "c_…", "msg_id": "m_…"}
{"op": "list_pins", "conv_id": "c_…"}
```

- In a group everyone pins by default; the owner or an admin can restrict it: `{"op": "update_conv", "conv_id": "c_…", "pin_policy": "admins"}`. In a direct conversation both people pin.
- Someone else's pin is unpinned only by the owner and admins.
- At most 100 per conversation (`[messages] pins_per_conv`). Agents pin only with `agents_pin = true`.
- Pins appear in the log as `msg.pinned` and `msg.unpinned` (show them as a notice), and are audited.

## From your backend

| Call | Does |
|---|---|
| `POST /v1/conversations/{id}/messages` with `thread_id` | Replies in a thread |
| `GET /v1/conversations/{id}/threads`, `GET …/threads/{thread}` | Lists threads; reads one |
| `PUT` / `DELETE /v1/conversations/{id}/messages/{msg}/reactions/{key}` | Adds or removes a member's reaction |
| `GET /v1/conversations/{id}/pins`, `PUT` / `DELETE …/pins/{msg}` | Lists, pins and unpins |

Webhooks receive `reaction.added`, `reaction.removed`, `msg.pinned` and `msg.unpinned` like other events; a thread reply is a `msg.created` with `thread_id`.

## Errors

A recalled root, message or pin target is `conflict`; a reaction or pin limit is `conflict`; an invalid key, a nested thread or a system message is `bad_request`; someone who may not pin is `forbidden`. Every refusal names its rule (`thread.*`, `reaction.*`, `pin.*`) from the [rule catalog](/docs/reference/business-rules/).
