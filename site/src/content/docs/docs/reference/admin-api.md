---
title: "Admin API"
description: "The API behind the console, for developers (the app) and operators (the deployment), with roles, sessions and admin keys."
sidebar:
  order: 60
---

The [console](/docs/operations/dashboard/) is built on this API, and `agentim admin` calls it from the command line. It has two parts with separate roles:

| Part | For | Roles |
|---|---|---|
| **App** `/admin/v1/app/…` | Developers: keys, people, agents, conversations, moderation, settings | `owner`, `developer`; `moderator` sees the overview, people and conversations and works the report queue |
| **Deployment** `/admin/v1/deploy/…` | Operators: databases, the cluster, admin accounts | `owner`, `operator` |

## Signing in

| | |
|---|---|
| Browser | `POST /admin/v1/login {name, password, totp?}` sets an `HttpOnly`, `SameSite=Strict` session cookie and returns a CSRF token. Every change must send `X-CSRF-Token` and come from the server's own origin. Sessions end after 12 hours idle, 7 days at most |
| Scripts | An admin key (`adk…`, with a role): `Authorization: Bearer adk…`. Create one in the console (Admins) or with `agentim admin-key create --name ops --role owner` on the server |
| First owner | When there is no admin yet, the server prints a one-time setup link at start, usable only from the same machine: `…/console/setup?token=…` |

Passwords are hashed with Argon2id. Repeated failures lock the account (`auth.max_failures`, `auth.lock_minutes`). TOTP is optional; each code works once. The last `owner` cannot be demoted or deactivated.

## Endpoints

| Endpoint | Does |
|---|---|
| `GET /admin/v1/app/overview` | People, agents, conversations, online now, open reports, keys |
| `GET`, `POST /admin/v1/app/api-keys`; `GET`, `POST /admin/v1/app/signing-keys` | List and create keys; revoke with a `POST` action |
| `GET /admin/v1/app/users`, `GET`, `PATCH`, `POST /admin/v1/app/users/{id}` | Search people; profile, devices, permissions; deactivate, reactivate, sign out a device |
| `GET /admin/v1/app/conversations`, `…/{id}` | Conversation metadata only |
| `GET /admin/v1/app/conversations/{id}/messages?reason=…` | Read content. `reason` (1–500 bytes) is required and every read is audited |
| `GET`, `POST /admin/v1/app/agents`; `POST`, `DELETE …/{id}`; `POST …/{id}/keys` | Create, stop, resume, transfer and destroy agents; issue keys |
| `GET /admin/v1/app/reports`, `GET`, `POST …/{id}` | The report queue and resolving reports |
| `GET /admin/v1/messages/{msg_id}/trace` | What happened to one message, from stored data: who could see it, which devices have it, agents' replies and drafts, webhook states, edits, recalls, reports, files. No content. `owner`, `operator` and `moderator`; every look is audited ([tracing](/docs/operations/tracing/)) |
| `GET /admin/v1/app/webhooks` | Configured webhooks and their delivery state (read-only) |
| `GET /admin/v1/app/audit` | The audit log |
| `GET`, `PUT /admin/v1/app/settings/clients` | The app's default [client permissions](/docs/concepts/permissions/) |
| `GET /admin/v1/deploy/status` | Databases (type, replication, encryption, writers), connections, version |
| `GET /admin/v1/deploy/cluster`, `POST …/nodes/{node}` | Cluster nodes; drain one |
| `GET`, `POST /admin/v1/deploy/dye-rules`; `DELETE …/{id}` | Dye rules: log one person's, agent's, device's, conversation's or API key's requests in full, for up to 24 hours. `kind`, `target`, `reason` and `minutes`; audited |
| `GET`, `POST /admin/v1/deploy/admins`, `…/admin-keys` | Admin accounts and admin keys (`owner` only) |
| `GET /admin/v1/me`, `POST …/me/password`, `POST …/me/{totp action}` | The signed-in admin |

Every change goes through the same business rules and error codes as the client protocol and the open API, and is audited as `admin.<action>` with the admin's name or key. Lists page by id with `after` and `before`.

## From the command line

```sh
export AGENTIM_ADMIN_URL=https://chat.example.com AGENTIM_ADMIN_KEY=adk…
agentim admin overview
agentim admin agent-create helper --owner maya --team
agentim admin agent-stop helper --reason "Answering with stale prices"
agentim admin reports
agentim admin cluster
agentim admin drain n2
```

`agentim admin --help` lists every command.
