---
title: "Protocol versions"
description: "How the protocol evolves without breaking your apps, feature flags, minimum client versions and fallback text."
sidebar:
  order: 11
---

## The promise

The protocol has whole-number major versions. Within a major version, changes are only additions:

| Allowed | Not allowed |
|---|---|
| New optional fields, event types, operations, error codes and feature names | Removing or renaming fields, changing a field's meaning or type, making an optional field required, changing what an error code means |

Clients must ignore fields they don't know, show fallback text for kinds they don't know, and treat unknown error codes as generic errors. The SDKs do. Every message has plain `text`, so an old app shows "[Poll] Lunch: pizza or sushi? Update the app to vote" for a kind it doesn't know.

## Handshake

1. The client connects to `GET /ws?v=2,1`, listing the versions it speaks. With no common version the server answers HTTP 400 `unsupported_protocol` before upgrading, with the versions it supports.
2. The server sends `welcome`: the chosen `protocol`, the server's name and version, `features`, `limits` (`message_bytes`, `frame_bytes`) and `me`.
3. The client sends `hello` with its `client {name, version, platform}` and the `features` it uses.

New capabilities are announced as named features (`files`, `tasks`; experimental ones as `x.<domain>.<name>`). A client uses only what the server announced.

## Minimum client versions

```toml
[clients.min_versions]
agentim-js = "1.2.0"
```

An older client gets `client_too_old` with an upgrade notice and is disconnected (close code 1008). Use it when a released app has a bug you must stop.

## Unknown operations

An operation the server doesn't know is `unknown_op`, not `bad_request`. Your app can tell "the server is too old" from "the request is wrong".

The server's own version follows semantic versioning, separately from the protocol version. The database schema migrates itself at start, and nodes of different versions work side by side during a rolling upgrade ([upgrades](/docs/operations/backup-and-upgrade/)).
