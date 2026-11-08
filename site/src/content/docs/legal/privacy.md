---
title: "Privacy Policy"
description: "What AgentIM Cloud collects about developers and their teams, why, how long we keep it, and your rights."
sidebar:
  order: 2
---

_Last updated: 6 November 2026. Early access._

This policy explains what the company named in [Who we are](#who-we-are) ("we") collects when you use **AgentIM Cloud** (the console at app.agentim.co, the cloud API at api.agentim.co, and your projects' addresses under agentim.co) and how we use it.

It covers **you, the developer, and the people on your team**. The people who use the apps you build on AgentIM ("your end users") are covered by **your** privacy policy: for their data you are the controller and we are your processor, under the [Data Processing Addendum](/legal/dpa/).

## Summary

- We collect what we need to run your account, your projects and your bill, and nothing for advertising.
- No analytics, no tracking pixels, no advertising cookies, no selling or sharing of personal information.
- Card details go to Stripe, never to us.
- Your users' messages and files belong to you; we don't read them, train models on them or log them.
- You can export or delete your account from the console at any time.

## What we collect and why

| What | Examples | Why | Legal basis (GDPR Art. 6) |
|---|---|---|---|
| **Account** | Email address, name, the sign-in method (email, Google or GitHub) and the account id it gives us, when you signed up and last signed in | To create your account, sign you in and contact you about it | Contract (6(1)(b)) |
| **Organizations and team** | Organization names, members and their roles, invitations (the invited email address, who invited, when) | To let you work with your team and control who can do what | Contract |
| **Projects** | Project names, region, address, status; API keys and signing keys (we keep only a hash or an encrypted copy) | To run your projects | Contract |
| **Usage** | Per project and day: monthly active people, peak connections, number of messages, agent messages, storage used | To enforce plan limits, show your usage and bill overage | Contract |
| **Billing** | Your plan, Stripe customer and subscription ids, subscription status, billing period, invoices (amounts, dates). Stripe collects your card, billing name, address and tax id | To charge you, collect tax and keep accounting records | Contract; legal obligation (6(1)(c)) for tax records |
| **Audit log** | Who did what in your organization and when (for example "invited a@b.com as Admin", "deleted project X"); never message content | Security and to let you review changes | Legitimate interest in security (6(1)(f)) |
| **Technical data** | IP address, browser or client, request time, address and result, for each request | To deliver the service, limit abuse (rate limits), investigate problems | Legitimate interest in security and reliability |
| **Support** | What you write to us and our replies | To help you | Contract; legitimate interest |

We don't collect sensitive categories of data about you, and we don't make decisions about you by automated means that have legal or similarly significant effects.

**Your users' data** (their ids, names, messages, files, devices, the agents in their conversations) is stored in your project's own database and processed only on your instructions. See the [Data Processing Addendum](/legal/dpa/).

## Cookies and browser storage

The console keeps your sign-in session in your browser's local storage so you stay signed in. It is strictly necessary and is removed when you sign out. This website keeps only your choice of light or dark theme. Neither uses analytics or advertising cookies, so there is no cookie banner.

## Who we share it with

- **Subprocessors** that run parts of the service for us, under contracts that protect the data: listed on [Subprocessors](/legal/subprocessors/) (database and sign-in, payments, network and hosting).
- **Your organization**: other members see your name, email and role, and the audit log of what you did there.
- **Google or GitHub**, only if you choose to sign in with them; they tell us your email and name. Their own privacy policies apply to them.
- **Authorities**, only when the law requires it, and we will tell you unless we are not allowed to.
- **A buyer**, if the company is sold or merged; this policy keeps applying to your data.

We don't sell or "share" personal information as the CCPA defines it, and we don't use it for targeted advertising.

## Where it is processed

AgentIM Cloud runs in the United States (first region: US East). If you are in the European Economic Area, the United Kingdom or Switzerland, your data is transferred to the US under the European Commission's Standard Contractual Clauses (and the UK Addendum), and, where a subprocessor is certified, the EU–US Data Privacy Framework.

## How long we keep it

| What | How long |
|---|---|
| Account, organizations, projects | Until you delete them; then removed from the live database at once and from backups within 30 days |
| Usage records | While the organization exists, and up to 24 months after for billing disputes |
| Billing and invoices | As long as tax and accounting law requires (up to 10 years); held mostly by Stripe |
| Audit log | While the organization exists; then deleted with it |
| Technical request logs | 30 days |
| Support messages | 2 years after the conversation ends |

When you delete a project, its database (your users' data) is dropped at once.

## Your rights

Depending on where you live (GDPR in the EEA and UK, CCPA/CPRA in California, similar laws in other US states), you can:

- **access** your data and get a **portable copy**: Console → Settings → Account → Export;
- **correct** it: change your name in the console, or ask us;
- **delete** it: Console → Settings → Account → Delete account (first transfer or delete the organizations you are the only owner of);
- **object** to or **restrict** processing based on legitimate interest;
- **withdraw consent** where we asked for it;
- **complain** to your data protection authority.

You can also email privacy@agentim.co. We answer within one month (45 days under the CCPA), and won't treat you differently for using your rights. We may need to confirm it is you.

## Security

Encryption in transit (TLS) everywhere; your users' data is encrypted at rest; every project has its own database; keys are stored as hashes; access to production is limited to the people who need it; logs don't contain message content. Details: [Security](/docs/security/) and the [Data Processing Addendum](/legal/dpa/#annex-2-security-measures). If a breach affects your personal data, we will tell you without undue delay.

## Children

AgentIM Cloud is a service for developers and businesses. It is not meant for anyone under 16, and we don't knowingly collect their data.

## Changes

We will post changes here and change the date at the top. For important changes we will email account owners at least 30 days before they apply.

## Who we are

The controller is **[COMPANY LEGAL NAME]**, [REGISTERED ADDRESS] ("AgentIM"). Contact: privacy@agentim.co.

<!-- Owner: fill in the company name and address before opening sign-ups (docs/06 §2.1 O5). If you serve the EU without an establishment there, also name an EU representative (GDPR Art. 27) and a UK representative. -->
