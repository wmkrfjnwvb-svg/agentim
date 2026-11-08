---
title: "Security overview"
description: "How AgentIM protects accounts, keys, content and the server, and what you are responsible for."
sidebar:
  order: 1
---

## Identity and keys

| | |
|---|---|
| **No chat passwords** | Your users sign in to your product. AgentIM only accepts client tokens signed with your signing key: HS256 JWTs with `kid`, `sub`, `iat` and `exp`, at most 30 days long; `none` and every other algorithm are refused ([devices and login](/docs/concepts/devices-and-login/)) |
| **Hashed secrets** | API keys, agent keys and admin keys are stored as SHA-256 hashes. Signing keys, which must be kept to verify tokens, are encrypted with the master key |
| **Revocation** | Revoking a signing key, an API key, an agent key or a user's tokens takes effect at once and disconnects live connections, on every node |
| **Tokens never in URLs** | Tokens travel in `Authorization` or the browser subprotocol, never in the query string where proxies log them |
| **Guessing is throttled** | 30 failed authentications from one address in a minute block it for 5 minutes, across `/ws`, `/mcp` and the open API |
| **Origins** | Browsers can connect only from the server's own origin and `server.allowed_origins` (no cross-site WebSocket hijacking) |
| **Admins** | Argon2id passwords, lockout, optional TOTP, `SameSite=Strict` sessions with CSRF tokens; the console accepts only its own origin |

## Content

- **Encrypted at rest** by default with a key per conversation ([encryption](/docs/operations/encryption/)). Dissolving a conversation destroys its key.
- **TLS** built in (rustls) or from your proxy.
- **Logs** never contain message content, tokens or whispers.
- **Reading someone's messages** as a developer needs the `messages:read` scope or, in the console, a written reason. Both are audited.
- **Files** are checked by content, not by name; SVG is never shown as an image; downloads can't run as pages.

## Rules are enforced by the server

Membership, roles, blocks, approval, takeover and the kill switch are checked on the server for every caller: apps, agents, MCP and the open API acting as a user. An agent cannot pose as a person. A refused request writes nothing.

## Supply chain

Releases are built in CI, signed with Sigstore and come with build provenance and an SBOM for the images. Dependencies are checked for known vulnerabilities and licenses on every change and every week.

## Your part

- Keep signing keys, API keys and the master key in a secret store, never in apps or repositories.
- Keep client tokens short-lived, and check your own session before signing one.
- Give API keys only the scopes they need.
- Back up the master key separately from the database.
- Keep the server and its dependencies updated.

Found a vulnerability? See [reporting a vulnerability](/docs/security/reporting/).
