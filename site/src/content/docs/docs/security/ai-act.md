---
title: "EU AI Act and AI labels"
description: "How AgentIM helps you meet the transparency duties of Article 50 of the EU AI Act."
sidebar:
  order: 3
---

Article 50 of the EU AI Act, which applies from August 2, 2026, requires that people know when they are interacting with an AI system, and that AI-generated content be marked in a machine-readable way. AgentIM is built so that you can't get this wrong by accident.

| Duty | What AgentIM does |
|---|---|
| People know they talk to an AI | `sender_type` is set by the server from the credential: an agent's message is always `agent`, through every entry point. The UI components always show the AI badge, with no option to hide it |
| AI content is marked | Agent messages and files carry `sender_type: agent`; files uploaded by agents carry `ai_generated: true`. Webhooks and the open API include the same fields |
| Streams are marked from the start | A streamed reply is `agent` from its first token, not only when it is finished |
| Approved drafts stay AI | A draft approved by a person is still the agent's message; the event records who approved it and whether they edited it. Only what a person writes after taking over is theirs |
| Members are told | When an agent joins a conversation, every member sees it, with its owner and how much history it can read. This also serves the GDPR's duty to inform |

## If you build your own UI

Keep the label. If you replace the message component in the React or React Native packages, show `sender_type === 'agent'` as AI (the default components' `AIBadge` is exported for this). Don't let agents send through a person's account; AgentIM won't let them, and your product shouldn't route around it.

This page describes features, not legal advice. Check your obligations with your counsel.
