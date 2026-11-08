---
title: "Reports and moderation"
description: "Reporting and blocking for users, the moderation queue for your team, and the statements of reasons the DSA asks for."
sidebar:
  order: 4
---

App Store guideline 1.2 and Google Play's user-generated content policy require apps with chat to let people report content and block users, and to act on reports within 24 hours. The EU Digital Services Act (Articles 16 and 17) asks for a reporting channel and a statement of reasons when content is restricted. AgentIM provides the parts.

## In the app

- **Block**: `block {user_id}`. The blocked person can't open a direct conversation with you, send you private messages or notify you with mentions.
- **Report**: `report {target_kind, target_id, category, text?}` on a `message`, `user` or `conversation`.

| Category | |
|---|---|
| `child_safety`, `self_harm`, `violence`, `illegal` | High severity, first in the queue |
| `spam`, `harassment`, `hate`, `sexual`, `impersonation`, `other` | |

You can report only what you can see. Reporting the same thing again while it is open returns the same report. 20 reports per person per hour. **Evidence is saved** when the report is made (content, sender, conversation, time) and encrypted, so a recalled message can still be reviewed.

## For your team

In the console (Reports) or with the open API (scope `moderation`):

| | |
|---|---|
| `GET /v1/reports?state=open` | By severity, then deadline (24 hours after the report by default) |
| `GET /v1/reports/{id}` | The report, the evidence, other reports on the same target |
| `POST /v1/reports/{id}:resolve` | `action`: `none`, `remove_content` (recall the message as the system), `suspend_user` (for `days`), `ban_user`; with `reason`, `ground` and `automated` |
| `POST /v1/conversations/{id}/members/{user}:silence` | Silence a member of a conversation for 1 s to 1 year; they can read but not send |

Resolving handles every open report on the same target. Two moderators resolving at once: only one succeeds. Each decision is recorded with the fields of a DSA statement of reasons: the action, the facts, the legal or contractual ground, whether automation was used, how to appeal (`moderation.appeal_url`), when and by whom. Webhooks `report.created` and `report.resolved` let you notify the people involved.

```toml
[moderation]
due_hours = 24
keep_days = 180        # resolved reports and evidence are then deleted
appeal_url = "https://example.com/appeal"
report_per_hour = 20
```

## Automated help

The [before-send hook](/docs/guides/before-send/) lets your own filter or moderation model see every message first. Agent output is also checked by the [guardrails](/docs/concepts/guardrails/).
