---
title: "Error codes"
description: "Every error code, what it means for your app, and its HTTP status in the open API."
sidebar:
  order: 40
---

Errors have the same `code` everywhere: on the WebSocket protocol, in the client and agent SDKs, over MCP and in the open API. The open API returns them as RFC 9457 problem details:

```json
{
  "type": "https://agentim.dev/errors/forbidden",
  "title": "Forbidden",
  "status": 403,
  "code": "forbidden",
  "detail": "not a member of this conversation"
}
```

**A refused request leaves no trace**: nothing is written, no event is sent, nothing is counted against limits.

## Business rules

HTTP is the open API's status, or the status of the refused WebSocket upgrade (— means it only occurs on a connection). These mean the request will not work as it is. Don't retry it unchanged.

| Code | HTTP | Meaning | What to do |
|---|---|---|---|
| <span id="forbidden">`forbidden`</span> | 403 | Not allowed: not a member, not the owner, wrong kind of caller, the app does not allow it, missing scope | Show why; don't retry |
| <span id="not_found">`not_found`</span> | 404 | Unknown or gone: no such conversation, message, user or file; dissolved; destroyed | Refresh your data |
| <span id="conflict">`conflict`</span> | 409 | The current state does not allow it now: already decided, already claimed, the agent is stopped, the version moved on, the name is taken | Reload the state, then decide |
| <span id="bad_request">`bad_request`</span> | 400 | Invalid input, or not applicable here: a field missing or out of range, a kind that needs a payload, rating your own message | Fix the request |
| <span id="blocked">`blocked`</span> | 403 | One of the two people blocked the other (direct conversations) | Show that the message can't be delivered |
| <span id="policy_blocked">`policy_blocked`</span> | 403 | The [before-send hook](/docs/guides/before-send/) or a content policy refused the message; `detail` has the reason | Show the reason |
| <span id="taken_over">`taken_over`</span> | — | For agents: a person has taken over this conversation | Wait for `handback` |
| <span id="budget_exceeded">`budget_exceeded`</span> | — | For agents: the daily message or character budget is used up | Stop until tomorrow, or ask the operator |
| <span id="client_too_old">`client_too_old`</span> | — | The app's version is below the server's minimum | Ask the user to update |
| <span id="unknown_op">`unknown_op`</span> | 400 | The server doesn't know this operation (it is older than the client) | Fall back or hide the feature |
| <span id="unsupported_protocol">`unsupported_protocol`</span> | 400 | No protocol version in common, before the WebSocket upgrade | Update the app or the server |
| <span id="deactivated">`deactivated`</span> | 403 | The user was deactivated; the connection is refused | Sign the user out |

## Temporary

These are about load or time. Retry with backoff; the SDKs do it for you and keep the message `sending`.

| Code | HTTP | Meaning |
|---|---|---|
| <span id="rate_limited">`rate_limited`</span> | 429 | Too many requests. The answer says how long to wait (`retry_after_ms`, or `Retry-After` in HTTP) |
| <span id="overloaded">`overloaded`</span> | 503 | The writer's queue is full; the server refuses instead of queueing without bound |
| <span id="storage_unavailable">`storage_unavailable`</span> | 503 | A database can't be written (or a replica is not keeping up); `/readyz` fails meanwhile |
| <span id="conv_migrating">`conv_migrating`</span> | 409 | The conversation is moving between databases, for a moment |

## Open API only

| Code | HTTP | Meaning |
|---|---|---|
| `unauthorized` | 401 | No API key, or an unknown, revoked or expired one |
| `missing_scope` | 403 | The key lacks the scope this call needs |
| `ip_not_allowed` | 403 | The key has an IP allow list and this address is not on it |
| `https_required` | 403 | Plain HTTP from a non-local address; use HTTPS, or set `open_api.behind_proxy` behind a TLS proxy |
| `idempotency_mismatch` | 422 | The same `Idempotency-Key` was used with a different body |
| `idempotency_in_progress` | 409 | The first request with this key is still running |
| `payload_too_large` | 413 | The body is over the limit (64 MiB for whole-file uploads) |

## Client SDK only

| Code | Meaning |
|---|---|
| `disconnected` | An online-only request (search, history from the server) while not connected |
| `timeout` | The command got no answer within `commandTimeout` (60 s by default) |
| `closed` | The client was closed or disposed |
| `unauthenticated` | The token provider refused, or the server refused the token |

Unknown codes may be added within a protocol version. Treat a code you don't know as a generic error.
