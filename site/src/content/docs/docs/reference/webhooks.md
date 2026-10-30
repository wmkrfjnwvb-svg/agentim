---
title: "Webhooks"
description: "Signed, ordered, at-least-once delivery of conversation events to your backend, and how to verify them."
sidebar:
  order: 50
---

Webhooks tell your backend what happens in conversations: new messages for your search index, analytics, notifications, or an agent that lives on your servers.

## Configuration

```toml
[[webhooks]]
name = "backend"
url = "https://api.example.com/agentim/webhook"
secret = "whsec_…"            # Standard Webhooks format, at least 16 bytes
events = ["msg.created", "member.*", "report.*"]
max_attempts = 8
timeout_ms = 5000
```

`events` takes exact types (`msg.created`), prefixes (`msg.*`) or `*`. Subscribing to `member.joined` or `member.*` also delivers `members.joined`. A new webhook starts from now; it doesn't replay history.

## Guarantees

| | |
|---|---|
| **No loss** | The delivery is written to an outbox in the same transaction as the event. What was not committed is never sent; what was committed is sent |
| **Order** | One delivery task per webhook per database sends events one at a time, in commit order. The next waits until the previous one succeeded |
| **At least once** | The position moves only after a 2xx answer, and survives restarts. Deduplicate with the `webhook-id` header |
| **Retries** | Exponential backoff, up to 5 minutes apart. After `max_attempts` the event is skipped, audited as `webhook.dropped` and counted in `agentim_webhook_dropped_total`. Fill the gap from the history endpoint |

## Request

```http
POST /agentim/webhook
webhook-id: msg_2Ksl…
webhook-timestamp: 1792503600
webhook-signature: v1,K5oZfzN95Z9UVu1EsfQmfVNQhnkZ2pj9o9NDN/H/pI4=
content-type: application/json

{"type": "msg.created", "timestamp": 1792503600123, "database": "main",
 "data": {"conv_id": "c_…", "seq": 42, "event": { … }}}
```

The signature is the [Standard Webhooks](https://www.standardwebhooks.com/) scheme: `base64(HMAC-SHA256(secret, id + "." + timestamp + "." + body))`. For a `whsec_` secret the HMAC key is the base64-decoded part after the prefix. The official libraries verify it:

```js
import { Webhook } from 'standardwebhooks';
const wh = new Webhook(process.env.AGENTIM_WEBHOOK_SECRET);
const event = wh.verify(rawBody, req.headers);   // throws if the signature or timestamp is wrong
```

Verify against the raw body bytes, before parsing the JSON. Reject timestamps more than five minutes old.

## Events

Every main-log event type ([events](/docs/reference/events/)), plus `user.deleted` (an account deletion finished: delete the user in your systems), `report.created` and `report.resolved` (notify the reporter and the person concerned).
