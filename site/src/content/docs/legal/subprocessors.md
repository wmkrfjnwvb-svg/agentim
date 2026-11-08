---
title: "Subprocessors"
description: "The companies that help AgentIM run AgentIM Cloud, what they do, and where."
sidebar:
  order: 4
---

_Last updated: 6 November 2026._

These companies process personal data for AgentIM Cloud. We tell organization owners by email at least 30 days before adding or replacing one that touches your users' data ([DPA §3](/legal/dpa/#3-subprocessors)).

| Company | What it does for us | Data | Location |
|---|---|---|---|
| **Supabase, Inc.** | Database for the console (accounts, organizations, projects, usage, audit) and sign-in (email, Google, GitHub), sign-in emails | Developer accounts and team data; no end-user data | United States |
| **Stripe, Inc.** | Payments, subscriptions, invoices, tax | Billing contacts, payment details (Stripe holds the card), invoices; no end-user data | United States |
| **Cloudflare, Inc.** | DNS, TLS, protection against attacks, hosting of agentim.co | Request data (IP addresses, headers) for all traffic that passes through it | Global network; United States |
| **Anthropic, PBC** · **OpenAI, L.L.C.** · **Google LLC** (Gemini API) · **Hangzhou DeepSeek Artificial Intelligence Co., Ltd.** | Models for built-in agents that use AgentIM's keys; only the one provider your organization's AI credit is for | The messages and knowledge an agent answers from, for each reply | United States (Anthropic, OpenAI, Google); **China** (DeepSeek) |
| **[HOSTING PROVIDER]** | Servers that run the IM clusters, the cloud API and project databases | All project data (end users' messages, files and profiles), encrypted at rest | United States (US East) |

**Not subprocessors:** model providers reached with **your own** keys, and other services your agents call. They are your processors, chosen and contracted by you. Google and GitHub, when you choose to sign in with them, act on their own terms.

Questions or objections: privacy@agentim.co.

<!-- Owner: name the server hosting provider before launch (docs/06 §2.1 O4, O5). -->
