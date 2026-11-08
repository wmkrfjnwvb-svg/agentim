---
title: "Console (dashboard)"
description: "The web console at /console, built into the server, for developers and operators."
sidebar:
  order: 2
---

Every AgentIM server includes its own console at **`https://<your server>/console`**. There is nothing to install or host separately; it is the same program and port, single node or cluster.

## First sign-in

On the first start the server prints a one-time link:

```text
Set up the console (once, from this machine): http://127.0.0.1:8080/console/setup?token=setup_…
```

Open it on the same machine to create the first **owner**. The link works once, only from that machine, and is replaced on every restart while there is no admin. In a container, where your browser isn't local:

```sh
docker exec <container> agentim admin-key create --db /var/lib/agentim/agentim.db --name me --role owner
agentim admin --url http://127.0.0.1:8080 --key adk… admin-create maya --role owner   # reads the password from stdin
```

There is no default password.

## Pages

**App**, for developers (`owner`, `developer`; `moderator` sees Overview, People, Conversations and Reports):

| Page | |
|---|---|
| Overview | People, agents, conversations, online now, open reports, keys |
| Keys | API keys (with scopes) and signing keys: create, revoke |
| People | Search; profile, devices and sign-out per device; deactivate; [client permissions](/docs/concepts/permissions/) per person |
| Conversations | Metadata. Reading messages asks for a reason and is audited |
| Agents | Create personal or team agents, stop and resume, new keys, transfer, destroy |
| Reports | The [moderation](/docs/security/moderation/) queue, by severity and deadline |
| Webhooks | What is configured and how delivery is going |
| Audit log | Who did what, with filters |
| Client permissions | The app's defaults |

**Deployment**, for operators (`owner`, `operator`):

| Page | |
|---|---|
| Status | Each database: kind, replication mode, encryption, writer state; connections; version |
| Cluster | Nodes, heartbeats, shards; drain a node |
| Admins | Admin accounts and admin keys (`owner` only) |

The console shows only what your role can use.

## Security

- Admin passwords are hashed with Argon2id; TOTP is available under **My account**; repeated failures lock the account.
- Sessions use `HttpOnly`, `SameSite=Strict` cookies and CSRF tokens, and only the server's own origin is accepted (not your apps' origins).
- Pages are served with a strict Content Security Policy, `X-Frame-Options: DENY` and `Referrer-Policy: no-referrer`.
- Everything the console does goes through the [admin API](/docs/reference/admin-api/), with the same rules and audit as the rest of AgentIM.
