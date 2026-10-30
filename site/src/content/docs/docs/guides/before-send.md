---
title: "Before-send hook"
description: "Let your backend allow, reject or rewrite every message before it is written."
sidebar:
  order: 3
---

The before-send hook calls your backend synchronously for every `send`, before the message reaches the writer. Use it for your own content rules, a moderation model, or rewriting links.

```toml
[before_send]
url = "https://api.example.com/agentim/before-send"
secret = "whsec_…"
timeout_ms = 2000
on_error = "allow"     # or "reject": what to do on timeout, non-2xx, bad JSON or an unknown action
```

## The request

Signed like a [webhook](/docs/reference/webhooks/) (`webhook-id`, `webhook-timestamp`, `webhook-signature`):

```json
{
  "type": "message.before_send",
  "conv_id": "c_…",
  "client_msg_id": "…",
  "kind": "text",
  "text": "Call me at 555-0100",
  "payload": null,
  "reply_to": null,
  "to_user": null,
  "sender": { "id": "u_…", "name": "maya", "kind": "human" }
}
```

## The answer

| Answer | Effect |
|---|---|
| `{"action": "allow"}` | The message goes on as it is |
| `{"action": "reject", "reason": "Phone numbers are not allowed"}` | The sender gets `policy_blocked` with your reason; nothing is written |
| `{"action": "replace", "text": "Call me at [hidden]", "payload": null}` | The message is written with your text and payload, after the usual checks (`bad_request` if they are invalid) |

## What it sees, and what it doesn't

- Malformed requests are refused before the hook is called. Membership, blocks and the other rules are checked after it, as usual.
- Edits and streamed replies don't go through the hook. Agent output also passes the built-in [guardrails](/docs/concepts/guardrails/).
- The hook runs outside the writer, so it doesn't slow other conversations down. It does add its latency to each send: keep it fast and close to the server.

`agentim_before_send_total{result}` counts the outcomes ([monitoring](/docs/operations/monitoring/)).
