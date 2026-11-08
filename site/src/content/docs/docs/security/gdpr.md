---
title: "GDPR, CCPA and data rights"
description: "Exporting a person's data, deleting accounts, retention, and where the data lives."
sidebar:
  order: 2
---

You are the controller of your users' data; AgentIM gives your backend the tools to answer their requests. Exports and deletions go through your backend (scope `compliance`), because your product verifies who is asking and delivers the result. Clients have no export or delete endpoint.

## Access and portability (GDPR Art. 15, 20; CCPA)

`GET /v1/users/{id}/export` streams NDJSON, one object per line, resumable with `after`:

- the profile, devices, preferences and block list;
- the conversations they are in (id, title, role, joined);
- **the messages they sent** (conversation, seq, time, kind, content, state);
- their feedback, card answers and reports;
- the agents they own.

Other people's messages are not included: they are those people's personal data. Each export is audited (which key, when; not the content).

## Erasure (GDPR Art. 17; App Store 5.1.1(v); CCPA)

`DELETE /v1/users/{id}` answers `202` with progress; `GET /v1/users/{id}/erasure` shows it.

1. **At once:** the account is marked deleted, every token is revoked and every connection closed.
2. **Each conversation**, run by that conversation's writer so it can't race with sending: every message the person sent is emptied like a recall (content, payloads, old event snapshots, drafts, pending webhooks, feedback, card answers), a `user.erased` event tells clients to clear their copies, and the person leaves. If they owned a group, ownership passes to the earliest admin, else the earliest member; an empty group is dissolved.
3. **Finally:** the profile becomes "Deleted user", devices, preferences and blocks go, their agents are destroyed, and the `user.deleted` webhook tells your systems.

Progress is stored, so a restart continues where it stopped, and repeating the request returns the same progress. The id is never reused. The audit log keeps that the deletion happened, without content. Backups can't be edited: they expire with your backup retention, and dissolved conversations' keys are destroyed.

Apple requires apps that let people create accounts to let them delete them in the app: your app's button calls your backend, which calls `DELETE /v1/users/{id}`.

## Retention (GDPR Art. 5)

`retention.days` (180 by default, 0 to keep everything) deletes events older than that from every conversation, with everything derived from them. `files.retention_days` does the same for files. Resolved reports are kept for `moderation.keep_days`.

## Where the data lives

Self-hosted, the data is wherever you run it. With [units](/docs/operations/databases/#units-several-databases) one deployment can keep a group of users' conversations in a database in another region.

## Agents and processors

When an agent joins, every member is told who owns it. If agents send content to a model provider, that provider is your processor: list it in your privacy policy and records of processing.
