---
title: "Devices and login"
description: "Client tokens your backend signs, how devices are registered, signing out one device or all of them, and agent keys."
sidebar:
  order: 4
---

AgentIM has no accounts or passwords for your users. Your product signs people in however it likes. Your backend then signs each user a short-lived **client token** with a signing key. AgentIM trusts the token because it trusts the key. Stream, Tencent Cloud IM and Agora Chat work the same way: signing needs no call to the chat server, so login keeps working even when AgentIM is busy.

## Signing keys

```sh
agentim signing-key create --name backend   # prints kid and secret, once
agentim signing-key list
agentim signing-key revoke sk_…
```

Developers can also manage them in the [console](/docs/operations/dashboard/) (Keys) or the admin API. Several keys can be valid at once, so you can rotate: create a new one, switch your backend, revoke the old one. Revoking a key rejects every token it signed and disconnects those connections at once, on every node.

## Client tokens

A client token is a standard JWT. Any JWT library can sign one. The server SDKs have `createUserToken` (Node), `create_user_token` (Python) and `CreateUserToken` (Go).

```text
header   {"alg": "HS256", "typ": "JWT", "kid": "sk_…"}
claims   {"sub": "maya", "iat": 1792500000, "exp": 1792503600}
```

| Field | Rule |
|---|---|
| `alg` | Only `HS256`. `none` and every other algorithm are refused |
| `kid` | Which signing key signed it |
| `sub` | Your user id: 1–64 characters of letters, digits and `_-.@:` |
| `iat` | When it was signed; more than 60 s in the future is refused |
| `exp` | Required; at most `auth.max_token_days` after `iat` (30 days by default). There are no permanent tokens |

Keep tokens short-lived, an hour for example. The client SDKs call your `tokenProvider` whenever they need a token: on the first connection, on reconnects and when the token expires. A token that expires while connected doesn't disconnect: tokens are checked when connecting.

`agentim token sign --user maya --ttl 3600` signs one with the newest signing key, for local testing.

### First connection creates the user

A valid token for an id the server hasn't seen creates that user. Your backend can fill in the profile with the [open API](/docs/reference/open-api/) (`PUT /v1/users/{id}`). Tokens for deleted ids, deactivated users or agents are refused. `system` is reserved.

## Devices

The app picks a **device id** the first time it runs and keeps it (1–64 of `A-Z a-z 0-9 _ -`). It passes the id when connecting, with an optional device name and platform. The server registers the device the first time it sees it. Each device has its own sync positions. The user's other online devices get `device.added`.

Devices unused for 30 days (`auth.token_idle_days`) are forgotten.

## Signing out

| What | How | Effect |
|---|---|---|
| One device | `logout()` in the client SDK (this device), `logout_device` (another of mine), or `DELETE /v1/users/{id}/devices/{device}` | Disconnects it, deletes its sync positions and rejects every token issued up to now on that device. The app needs a new token, and then it is a new device. |
| Everywhere | `DELETE /v1/users/{id}/tokens` (scope `tokens:revoke`) | Every token issued up to now is rejected on every device; connected devices are disconnected |
| Deactivate | `POST /v1/users/{id}:deactivate` | The user cannot connect; their messages stay. `:reactivate` undoes it |

Tokens are not tied to a device. Someone who copied a token could use it from another device id until it expires. If you suspect a stolen token, sign the user out everywhere. Stream and Firebase have the same model, which is why short token lifetimes matter.

## Agent keys

Agents don't use client tokens. Each agent has keys (`atk_…`, 256 random bits; only a hash is stored). Each key is one device with its own sync position. The key is returned once, when the agent is created or a key is issued:

| | |
|---|---|
| New key | `POST /v1/agents/{id}/keys`, optionally revoking the others |
| Revoke | `DELETE /v1/agents/{id}/keys` (all) or `…/keys/{device}` (one) |
| CLI | `agentim admin agent-key helper --revoke-others` |

## Connecting

Clients connect to `/ws`. Native clients and agents send the token in `Authorization: Bearer …`. Browsers can't set headers on a WebSocket, so the browser SDK offers the subprotocols `agentim` and `agentim.bearer.<base64url token>`. Tokens never go in the URL, where proxies would log them. Browser connections must come from the server's own origin or one listed in `server.allowed_origins`.
