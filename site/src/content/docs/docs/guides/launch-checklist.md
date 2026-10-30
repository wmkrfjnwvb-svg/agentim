---
title: "Before you go live"
description: "The checklist for a production deployment, from TLS and databases to backups, monitoring, compliance and app store rules."
sidebar:
  order: 1
---

## Server

- [ ] **HTTPS.** Built-in TLS (`[server.tls]`) or a reverse proxy. Behind a proxy that terminates TLS, set `open_api.behind_proxy = true` and make sure the proxy passes WebSocket upgrades ([install](/docs/operations/install/)).
- [ ] **`server.public_url`** set to the address clients use, so agent connection settings are right.
- [ ] **`server.allowed_origins`** lists your web apps' origins, and nothing else.
- [ ] **PostgreSQL or MySQL** for anything beyond one machine; every database configured for full durability ([databases](/docs/operations/databases/)).
- [ ] **The master key** in a secret store (`AGENTIM_MASTER_KEY` or `encryption.key_file`), backed up separately from the database ([encryption](/docs/operations/encryption/)).
- [ ] **Backups** of the databases, the master key and the file storage, with a tested restore ([backup and upgrade](/docs/operations/backup-and-upgrade/)).
- [ ] **Files**: S3 with CORS, a lifecycle rule for incomplete multipart uploads and server-side encryption ([file storage](/docs/operations/file-storage/)).
- [ ] **Monitoring**: scrape `/metrics` with a token (`--metrics-token`), load `deploy/alerts.yml`, probe `/readyz` ([monitoring](/docs/operations/monitoring/)).
- [ ] **Restart on failure**: the server exits (code 70) on a storage failure and expects systemd, Docker or Kubernetes to start it again.
- [ ] **Retention** (`retention.days`) matches your privacy policy.

## Your backend

- [ ] API keys with only the scopes each service needs; `messages:read` and `compliance` only where required.
- [ ] Client tokens with short lifetimes (an hour) and a `tokenProvider` that checks your own session first.
- [ ] Webhook signatures verified on the raw body ([webhooks](/docs/reference/webhooks/)).
- [ ] Account deletion in your app calls `DELETE /v1/users/{id}` ([GDPR](/docs/security/gdpr/)).
- [ ] Someone works the report queue within 24 hours ([moderation](/docs/security/moderation/)).

## Your apps

- [ ] The device id is generated once and kept.
- [ ] Report and block are reachable from messages and profiles. App Store guideline 1.2 and Google Play's user-generated content policy require both.
- [ ] AI messages are labelled. The components do it by default; custom message components must keep the badge ([EU AI Act](/docs/security/ai-act/)).
- [ ] `[clients.min_versions]` is ready for the day you need to stop an old app version.

## Agents

- [ ] Every agent has a real owner who gets its approvals and alerts.
- [ ] Guardrail limits fit your traffic ([guardrails](/docs/concepts/guardrails/)).
- [ ] Someone knows how to stop an agent: the owner in the app, or `POST /v1/agents/{id}:stop` ([people in control](/docs/concepts/approvals/)).
