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
| `GET`, `POST /admin/v1/app/agents`; `POST`, `DELETE …/{id}`; `POST …/{id}/keys` | Create, stop, resume, transfer and destroy agents; issue keys. The list shows each agent's `role` |
| `GET /admin/v1/app/roles`; `GET`, `PUT`, `DELETE …/roles/{name}`; `PUT /admin/v1/app/agents/{id}/role` | Agent roles: Markdown files (a `---` header with `name` and `description`, then the instructions; Claude Code's `agents/*.md` load as they are). `PUT` takes the file (`text/markdown`) or `{"markdown"}`; a role an agent has cannot be deleted (`409 role.in_use`). Giving a role (`{"role": "name"}` or `null`) tells the agent's connections `config.updated`. Owner and developer write; moderator reads |
| `GET /admin/v1/app/model-services`; `PUT`, `DELETE …/model-services/{name}`; `GET …/model-services/{name}/models` | Model services for built-in agents: the deployment's own (`[[ai.builtin]]`, keys from environment variables, listed with `builtin: true` and whether the key is set) and the app's (`{"provider": "anthropic" \| "openai" \| "gemini" \| "deepseek" \| "compatible", "base_url"?, "api_key", "daily_tokens"?}`). Saving checks the key with the provider (`400 model_service.key_refused`, `model_service.unreachable`); the key is stored encrypted and only its last four characters are shown; leaving `api_key` out keeps the saved one. At most 10 per app (`429 model_service.limit`). `models` lists the provider's chat models with their price from the price table; with a built-in key only priced models are usable. Owner and developer write; moderator reads |
| `GET /admin/v1/app/assistants`; `GET …/assistants/catalog`; `POST …/assistants/catalog/{slug}`; `PUT`, `DELETE /admin/v1/app/agents/{id}/assistant`; `GET …/assistants/usage?since=&limit=` | Built-in agents: the server answers for an agent with a model service and model. The catalog has five roles (`assistant`, `translator`, `notes`, `support`, `moderator`); enabling one (`{"owner", "service", "model", "id"?}`) adds the role, creates a team agent maintained by `owner` and runs it. `PUT …/assistant` takes `{"service", "model", "settings"?}` (`max_tokens` 256–32000, `context_tokens` 256–200000, `replies_per_minute` 1–600, `replies_per_day`, `fallback`). With a built-in key only priced models are accepted (`400 assistant.model_unpriced`). Replies are streamed, one per message (also across restarts and nodes); `usage` lists each model call with its tokens and cost in nano-dollars, never text. Owner and developer write; moderator reads |
| `GET /admin/v1/app/agents/{id}/knowledge`; `PUT`, `DELETE …/knowledge/{file name}` | An agent's knowledge files: the file is the body of the `PUT` (Markdown, text, CSV, JSON or PDF; up to 5 MB, 50 files, 2 million characters of text in all). The text is taken out, stored encrypted and searched; a built-in agent gets the best-matching parts in its prompt and cites them by name. With the assistant setting `only_knowledge` it answers only from them, and with `handoff` it hands questions they don't cover to a person. Refusals: `knowledge.type`, `knowledge.encoding`, `knowledge.empty`, `knowledge.unreadable`, `knowledge.size`, `knowledge.name`, `429 knowledge.limit`, `knowledge.total` |
| `GET /admin/v1/app/skills`; `GET`, `PUT`, `DELETE …/skills/{name}`; `PUT`, `DELETE …/skills/{name}/files/{path}` | Skills in the Agent Skills format: `PUT` takes the `SKILL.md` (`text/markdown` or `{"markdown"}`, header with `name` and `description`); reference files are text up to 512 KB, 20 per skill. A role names the skills it uses (`skills: [name]` in its header): its agents get them in their configuration, and a built-in agent loads them when needed (`load_skill`, `read_skill_file`). Scripts are not run. A skill a role names can't be deleted (`409 skill.in_use`) |
| `GET /admin/v1/app/connections`; `PUT`, `DELETE …/connections/{name}`; `GET …/connections/{name}/tools` | Connect: `{"kind": "mcp" \| "api", "url", "auth_header"?, "secret"?, "spec"?, "writes": "read_only" \| "confirm" \| "allow"}`. An MCP server (Streamable HTTP) must list its tools before it is saved; an API needs its OpenAPI 3 document (JSON), whose operations become tools. The secret is stored encrypted, never listed or audited, and sent only to the connection's address (no private addresses). A role names its connections (`mcp: [name]`, `apis: [name]`); built-in agents use the read-only tools (MCP `readOnlyHint`, `GET`) unless writes are allowed; with `confirm` each write call first asks the person being answered to approve it (an `approve` request with the tool and its arguments), then runs once with `Idempotency-Key: <request id>`; external agents get the connection, with its header, in their configuration. At most 20 (`429 connection.limit`); in use: `409 connection.in_use` |
| `GET /admin/v1/app/reports`, `GET`, `POST …/{id}` | The report queue and resolving reports |
| `GET /admin/v1/messages/{msg_id}/trace` | What happened to one message, from stored data: who could see it, which devices have it, agents' replies and drafts, webhook states, edits, recalls, reports, files. No content. `owner`, `operator` and `moderator`; every look is audited ([tracing](/docs/operations/tracing/)) |
| `GET /admin/v1/app/webhooks` | Configured webhooks and their delivery state (read-only) |
| `GET /admin/v1/app/audit` | The audit log |
| `GET`, `PUT /admin/v1/app/settings/clients` | The app's default [client permissions](/docs/concepts/permissions/) |
| `GET /admin/v1/deploy/status` | Databases (type, replication, encryption, writers), connections, version |
| `GET /admin/v1/deploy/cluster`, `POST …/nodes/{node}` | Cluster nodes; drain one |
| `GET`, `POST /admin/v1/deploy/dye-rules`; `DELETE …/{id}` | Dye rules: log one person's, agent's, device's, conversation's or API key's requests in full, for up to 24 hours. `kind`, `target`, `reason` and `minutes`; audited |
| `GET`, `POST /admin/v1/deploy/tenants`; `PATCH`, `DELETE …/{id}` | Tenants of a multi-tenant server (`[tenancy]`; each tenant has its own database on one of `[[tenancy.clusters]]`): create (`id`, `name`, `limits`, and with `[[tenancy.clusters]]` the `cluster` its own database is created on, default the first), rename, change limits, suspend or resume (`suspended`). A suspended tenant's connections close and its requests get 404. `DELETE` ends its connections and drops its database and files. `409 tenant.disabled` on a single-tenant server |
| `GET /admin/v1/deploy/tenants/{id}/usage?day=YYYY-MM-DD` | A tenant's usage for a UTC day (default today): `mau` (people with a device seen this month so far; agents do not count), `messages` and `agent_messages` that day, `storage_bytes`, and `connections` |
| `GET /admin/v1/deploy/tenants/{id}/webhooks`; `PUT`, `DELETE …/webhooks/{name}` | A tenant's own webhooks: `PUT` creates or replaces one (`url` https, `secret` at least 16 bytes, `events` patterns like `msg.*`, `max_attempts` 1–20, `timeout_ms` 100–30000, `backoff_ms`, `enabled`); the secret is never returned. Each receives only its tenant's events, without the internal prefix, with `tenant` in the body. Private, loopback and link-local addresses are refused when set and when sending (`webhook.url`, `webhook.address`); one failing for `tenancy.webhook_disable_after_ms` (5 days) or answered with `webhook-delivery: disable` is turned off (audited `webhook.disabled`). At most `tenancy.webhooks_per_tenant` (10) per tenant (`409 webhook.limit`) |
| `POST /admin/v1/deploy/tenants/{id}/api-keys`, `…/signing-keys` | An open API key (`name`, optional `scopes`) or a signing key (`name`) inside a tenant; the secret is returned once. They work only on `<id>.<host_suffix>` |
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
