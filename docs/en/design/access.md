# Access

Who connects to AgentIM, how each one proves who they are, and what each may do: end users with tokens signed by the developer, agents with keys, backends with API keys, and admins with accounts and admin keys. This page also covers client permissions, devices, the admin API, the console and the CLI.

Code: `server/crates/agentim-core/src/auth.rs` (tokens, signing keys), `auth_limit.rs` (failed-authentication limits), `hub/login.rs`, `registry.rs` (keys and devices, in memory and across nodes), `admin_api.rs`, `admin_cli.rs`, `console.rs` (the embedded console), `server.rs` (transport, TLS, Origin); console UI `server/console/`.

## 1. Purpose

AgentIM has no accounts of its own for end users and never stores their passwords. The developer's backend vouches for its users by signing their tokens.

| Who | Entry | Credential |
|---|---|---|
| End users (people) | `/ws` from the app | A JWT signed by the developer's backend with a signing key |
| Agents | `/ws`, `/mcp`, `/api/…` | An agent key |
| The developer's backend | `/v1` | An API key with scopes |
| Developers and operators | `/console`, `/admin/v1`, `agentim admin` | An admin account (password + TOTP) or an admin key |
| The deployer on the machine | `agentim` local commands | Access to the machine and the database |

## 2. Feature points

| ID | Feature | Behaviour |
|---|---|---|
| ACC-01 | Signed user tokens | HS256 JWT with `kid`, `sub`, `iat`, `exp`, signed offline by the backend (`createUserToken` in the server SDKs). `alg: none` and other algorithms are refused; `iat` more than 60 s in the future is refused; `exp - iat` ≤ `max_token_days` (30) |
| ACC-02 | Signing keys | Several keys for rotation; shown once; stored encrypted (verification needs the secret); revoking one ends its tokens and connections at once on every node |
| ACC-03 | First login creates the user | A valid token for an unseen id creates the person once. Deleted ids, deactivated users, agent ids and `system` are refused |
| ACC-04 | Devices | The app sends its own device id (`device=…`, with a name and platform). The server registers devices, lists them, logs one out (tokens issued up to that second on it stop working) and forgets devices unused too long |
| ACC-05 | Logout everywhere | `DELETE /v1/users/{id}/tokens`: tokens issued up to that second stop working, connections close; for an agent, all keys are revoked |
| ACC-06 | Tokens travel in headers | `Authorization: Bearer`, or for browsers the subprotocol `agentim.bearer.<base64url>`; never in the URL |
| ACC-07 | Client permissions | App defaults (`create_groups`, `create_direct`, `add_members`, `find_users`, `create_agents` (off), `edit_minutes`, `recall_minutes`), set in the console and stored, plus per-user overrides by the open API. `hello` returns the effective permissions; changes push `user.updated` |
| ACC-08 | Human control is never taken away | Without `create_agents`, existing agents keep running and their owner can still stop, approve, take over, whisper and observe |
| ACC-09 | API keys | Scoped, optional IP allowlist and expiry, stored as hashes; created and revoked in the console, the admin API or `agentim api-key`; changes reach every node at once |
| ACC-10 | Failed authentication limits | Unknown keys and tokens are looked up at most once per second per address (20/s overall); an address that keeps failing is refused for a while |
| ACC-11 | Admin accounts | Roles `owner`, `developer`, `moderator`, `operator`; Argon2id passwords, TOTP, lockout after failures. The last owner cannot be demoted or deactivated |
| ACC-12 | First owner | With no admin yet, the server prints a one-time setup link that works only from the machine itself; or `agentim admin-key create --role owner` locally |
| ACC-13 | Console sessions | `HttpOnly`, `SameSite=Strict` cookies (12 h idle, 7 days max), a CSRF token on every change, same-origin only; guesses inside a session count as failed logins |
| ACC-14 | Admin API areas | `/admin/v1/app/…` for developers (keys, users, conversations, agents, assistants, connections, roles, reports, webhooks, audit, settings) and `/admin/v1/deploy/…` for operators (databases, cluster, admins). Every route checks the role |
| ACC-15 | Reading content needs a reason | Listing conversations shows metadata only; reading messages needs a `reason` (1–500 bytes) and is audited |
| ACC-16 | Admin CLI and keys | `agentim admin --url --key` calls the same admin API; admin keys (`adk…`) have a role and are stored as hashes |
| ACC-17 | Transport | Built-in TLS (certificates checked at start), Origin checks for browsers (`allowed_origins`), a cap on connections per user, idle connections closed |

## 3. Design

### 3.1 Tokens and devices

- **One way to log in**: the backend signs tokens offline. The alternatives:
  - exchanging tokens through an API call adds a call and a dependency;
  - calling the backend on every connection (a connect proxy) makes every reconnect storm hit the backend.
- **Verification**: tokens are checked locally on every node. Signing keys are cached, and changes are announced to the other nodes.
- **Revocation by time**: a device logout and "logout everywhere" record a cut-off time. A token with `iat` at or before it is refused. Tokens are not tied to devices, so someone holding a token can still connect as a new device until it expires.
- **The device id belongs to the app.** Generated on first run and kept, it gives each device its own sync position ([sync-and-clients](sync-and-clients.md)).

### 3.2 Permissions

```
effective permission = per-user override (if set) else the app default
```

- A refused operation is `forbidden` ("the app does not allow this: …") and writes nothing.
- `as: <user>` in the open API obeys the same permissions. `as: system` does not.
- An agent is bound by its owner's permissions.
- `create_agents` is checked again in the control database's writer, so once a revocation returns, no new agent of that person can appear.

### 3.3 Admin

| Area | For | Roles |
|---|---|---|
| App `/admin/v1/app/…` | Developers | `owner`, `developer` (write); `moderator` (reports, read-only elsewhere) |
| Deployment `/admin/v1/deploy/…` | Operators | `owner`, `operator`; admin accounts and keys only `owner` |

- Every change uses the same business logic and error codes as clients and the open API.
- Every change is audited as `admin.<action>`, by `admin:<name>` or `admin:key:<name>`.
- Refused requests leave no trace.
- On the hosted cloud, customers get only the app area.
- The console is a React single-page app embedded in the binary at build time. Without a built front end, a placeholder page is embedded, so the Rust build never needs Node.

## 4. Interfaces

- Client protocol: `hello` (with `me.permissions`), `list_devices`, `logout_device`.
- Open API: `PATCH /v1/users/{id}` (`permissions`), `/v1/users/{id}/devices…`, `DELETE /v1/users/{id}/tokens`.
- Admin API: `/admin/v1/setup`, `/admin/v1/login`, `/admin/v1/logout`, `/admin/v1/app/*`, `/admin/v1/deploy/*`.
- CLI:
  - `agentim signing-key create|list|revoke`, `agentim api-key create|list|revoke`, `agentim admin-key create`, `agentim token sign`;
  - `agentim admin <command>` (`overview`, `users`, `user`, `permissions`, `deactivate`, `reactivate`, `agents`, `agent-create`, `agent-key`, `agent-stop`, `agent-resume`, `agent-transfer`, `agent-destroy`, `admin-create`, …).

## 5. Rules and errors

Rule prefixes: `account.*`, `apikey.*`, `signing.*`, `admin.*`, `session.*` ([catalog](../business-rules.md)).
- 401 means an unknown or expired token; the client asks for a new one once.
- 403 means a deactivated or deleted person; the client stops.
- `forbidden` means a permission or role is missing.
- `conflict` covers the last owner, a code used twice, and a setup link used twice.

## 6. Tests

Paths under `server/crates/agentim-core/tests/`; three databases.

| Feature | Tests |
|---|---|
| ACC-01–ACC-06 | `login.rs`: all 16 tests (signed tokens, forged and malformed tokens, first login once, racing first logins, devices, logout one or all, revoking a signing key, keys stored encrypted, header transport, the token over the WebSocket, cut-offs across a restart, races of revocation and connection); `audit.rs`: `idle_and_old_tokens_expire`, `revocations_by_another_process_take_effect`; cluster `cluster.rs`: `signed_tokens_and_logouts_reach_every_node`, `revocations_and_blocks_reach_every_node`; `faults.rs`: `revocations_reach_cut_off_nodes_within_3s` |
| ACC-07, ACC-08 | `permissions.rs`: all 7 tests |
| ACC-09, ACC-10 | `open_api.rs`: `authentication_and_scopes`, `failed_authentications_block_one_address`, `race_key_reloads_against_new_and_revoked_keys`; `http.rs`: `websocket_rejects_bad_tokens_oversized_frames_and_bad_requests`; `transport.rs`: `a_logout_before_registration_is_caught`, `race_logout_during_the_handshake` |
| ACC-11–ACC-16 | `admin_api.rs`: all 12 tests (setup once and only locally, roles on every route, the last owner, lockout and TOTP, CSRF and same origin, guesses in a session, reading content needs a reason, the app area follows the business rules, defaults apply everywhere, restart, another process); `server/crates/agentim/tests/cli.rs`: `admin_commands_reach_a_running_server`; `server/ci/console_smoke.mjs` (the console in a browser) |
| ACC-17 | `transport.rs`: `the_server_speaks_tls_itself`, `certificates_are_checked_at_startup`, `cross_site_browser_pages_are_refused`, `origin_settings_are_checked`, `connections_are_capped_and_idle_ones_closed`; `audit.rs`: `a_persons_limits_count_all_their_connections` |
| Closed demo endpoints | `http.rs`: `outside_the_demo_login_user_creation_and_stats_are_closed`, `demo_endpoints_work_in_the_demo`; `coverage.rs`: `account_and_stats_ops_are_gone`; `edges.rs`: `refused_user_creation_and_tokens` |

## 7. Limits and next steps

- Not built: OIDC login, SSO/SCIM for admins, and HMAC-signed open API requests.
- Most app settings other than client permissions (webhooks, the before-send hook, moderation deadlines) are read-only in the console and set in the configuration file.
