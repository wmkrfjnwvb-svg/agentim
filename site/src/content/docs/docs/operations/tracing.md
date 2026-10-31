---
title: "Tracing"
description: "Follow one request or one message across nodes: trace ids, dye rules that log someone's requests in full, and a message's trace."
sidebar:
  order: 8
---

Three tools answer "where did this message go?", from the cheapest to the most detailed.

## Trace ids

Every request runs in a [W3C Trace Context](https://www.w3.org/TR/trace-context/) trace. The trace id is in every JSON log line of the request on every node (`span.trace_id`), in the message row, and in the message's trace below, so `grep` or your log system can put one request back together across a cluster.

| Where | How the trace arrives |
|---|---|
| Client SDKs | Each queued operation gets a trace when it is queued and keeps it across retries and restarts; the frame carries it as `"trace": "00-…"` |
| Open API, MCP, files, admin API | The `traceparent` header |
| Agents | `"trace"` on the agent's frames. Events pushed to an agent carry the trace of the request that caused them, so the agent can pass it to its model calls and send it back with its reply |
| Webhooks | Each delivery carries a `traceparent` header, a child of the write that produced the event |

Your backend (API keys) and agents (agent keys) are trusted: their trace is continued. A person's app is not: the server starts its own trace and keeps the app's trace id only as a link, and ignores the app's "sampled" flag, so nobody can make the server log more for them. A missing or invalid `traceparent` starts a new trace. Pushes to people never carry trace ids.

## Dye rules

A dye rule logs everything one person, agent, device, conversation or API key does, from DEBUG up, on every node, until it expires, while everyone else stays at your normal log level. Use it to debug one customer's problem in production.

- **Console:** Deployment → Dye rules. **API:** `POST /admin/v1/deploy/dye-rules` with `kind` (`user`, `agent`, `device`, `conv`, `api_key`), `target` (an id, or a person's or agent's name), `reason` and `minutes` (1–1440, default 60); `DELETE …/{id}` ends it early.
- `owner` and `operator` only. Adding and removing are audited with the reason; at most 100 rules at once.
- A dyed request leaves an INFO line `dyed request` with the rule, the operation and the result, and its DEBUG lines. What it causes is dyed too: the events it produces on every node, the agent pushes and the agent's reply with the same trace, and webhook deliveries (`dyed webhook delivery`).
- Content, tokens and whispers are never logged, dyed or not. The rule id stays inside the server: it is not sent to apps, agents or webhook receivers.

## A message's trace

`GET /admin/v1/messages/{msg_id}/trace`, or Console → Message trace (also a **trace** link next to each message when reading a conversation). It is put together from what the server stores, so it works without a tracing backend and without a dye rule. No content is shown. `owner`, `operator` and `moderator` may look; each look is audited as `admin.messages.trace`.

| Part | What it says |
|---|---|
| `message` | Conversation, database, seq, sender, state and version, the sender's `client_msg_id` (while the dedup window keeps it), what caused it, when retention deletes it |
| `trace` | Its trace id, and a link to your tracing UI when `[tracing] trace_ui_url` is set (for example `https://jaeger.example.com/trace/{trace_id}`) |
| `audience` | How many people and agents can see it, including members removed since; side-channel events anchored at it (counted, never shown) |
| `delivery` | How many devices of those members have acknowledged it, and each device that has not: its position and when it was last seen |
| `agents`, `replies`, `drafts` | Each agent that can see it (trigger, stopped, its devices), and the replies and drafts that name it as their cause |
| `webhooks` | For each of its events and each webhook that takes it: `delivered`, `pending` (with the number of events queued ahead), `dropped` after the last attempt, or `delivered_or_withdrawn` when the message was recalled after the outbox was cleaned (a recall withdraws events not yet sent) |
| `events` | Its history: created, updated, recalled and so on, by whom and when |
| `reports`, `files` | Reports about it and the files it refers to, with their states |

Not stored, so not shown: which entry point and node took the request, and whether `before_send` rewrote it. Search the logs for the trace id for those.
