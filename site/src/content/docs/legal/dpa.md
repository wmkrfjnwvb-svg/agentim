---
title: "Data Processing Addendum"
description: "How AgentIM processes your users' personal data on your behalf: GDPR Art. 28, UK GDPR, CCPA service provider terms, security measures."
sidebar:
  order: 3
---

_Last updated: 6 November 2026._

This Data Processing Addendum ("DPA") is part of the [Terms of Service](/legal/terms/) between you ("Customer") and **[COMPANY LEGAL NAME]** ("AgentIM"). It applies when AgentIM processes personal data in Customer's projects on AgentIM Cloud. It takes effect when you accept the Terms; no signature is needed. If you need a signed copy, email legal@agentim.co.

## 1. Roles

For **Customer Personal Data** (personal data in Customer's projects, described in [Annex 1](#annex-1-details-of-processing)), Customer is the controller (or a processor for its own customer) and AgentIM is the processor (or subprocessor). For account and billing data about Customer's team, AgentIM is a controller under its [Privacy Policy](/legal/privacy/).

## 2. AgentIM's obligations (GDPR Art. 28(3))

AgentIM will:

1. **Instructions**: process Customer Personal Data only on Customer's documented instructions. The Terms, Customer's configuration of its projects and its use of the APIs are those instructions. AgentIM tells Customer if it thinks an instruction breaks data protection law, and processes otherwise only where the law requires, telling Customer first unless the law forbids it.
2. **Confidentiality**: make sure people allowed to process the data are bound by confidentiality.
3. **Security**: keep the measures in [Annex 2](#annex-2-security-measures) in place, and may improve them but not lower the overall level.
4. **Subprocessors**: follow §3.
5. **Data subjects' requests**: help Customer answer them. The open API does most of it: `GET /v1/users/{id}/export` (access, portability), `DELETE /v1/users/{id}` (erasure), and changes to profiles (rectification). AgentIM forwards to Customer any request it receives directly and doesn't answer it itself.
6. **Help with compliance**: give reasonable help with Customer's security, breach notification, impact assessments (DPIAs) and prior consultation duties (GDPR Art. 32–36), taking into account the information AgentIM has.
7. **Breaches**: notify Customer without undue delay, and within 48 hours of becoming aware, of a personal data breach affecting Customer Personal Data, with what is known (nature, categories and approximate numbers, likely consequences, measures taken), and update it as more is learned.
8. **Deletion**: when Customer deletes a project, delete that project's database at once; it leaves backups within 30 days. When the Terms end, delete the remaining Customer Personal Data the same way, unless the law requires keeping it. Customer can export everything before deleting (open API).
9. **Audits**: make available the information needed to show compliance with this DPA (this page, the security documentation, answers to reasonable questionnaires once a year), and allow audits by Customer or an auditor it appoints, with 30 days' notice, during business hours, under confidentiality, at Customer's cost, no more than once a year unless a breach or an authority requires it.

## 3. Subprocessors

Customer authorizes AgentIM to use the subprocessors on [Subprocessors](/legal/subprocessors/). AgentIM:

- has a written contract with each that protects the data at least as much as this DPA;
- announces new or replaced subprocessors on that page and by email to organization owners **at least 30 days** before they start processing Customer Personal Data;
- if Customer objects on reasonable data protection grounds within that time, discusses it in good faith; if there is no solution, Customer can terminate the affected projects and get a refund of prepaid fees for them;
- remains responsible for its subprocessors.

## 4. International transfers

AgentIM Cloud runs in the United States. For transfers of personal data from the EEA, Switzerland or the UK to AgentIM, the **Standard Contractual Clauses** (Commission Implementing Decision (EU) 2021/914) are incorporated by reference: Module 2 (controller to processor) where Customer is a controller, Module 3 (processor to processor) where Customer is a processor; clause 7 (docking) applies; clause 9 option 2 (general authorization, with the notice in §3); clause 11 optional language does not apply; clause 17 and 18: the law and courts of Ireland. Annexes I and II of the Clauses are Annexes 1 and 2 of this DPA. For the UK, the UK International Data Transfer Addendum applies; for Switzerland, the Clauses with the FDPIC as the authority and references to the GDPR read as references to the Swiss FADP. AgentIM ensures its subprocessors transfer data under the Clauses or another valid mechanism (such as the EU–US Data Privacy Framework).

## 5. US state privacy laws (CCPA/CPRA and similar)

AgentIM is a **service provider** (processor) for Customer Personal Data. It will not sell or share it, will not retain, use or disclose it for any purpose other than providing the service (including not for targeted advertising or training AI models), will not combine it with personal data from other sources except as the law permits, will comply with these laws and give the same level of protection they require, and will tell Customer if it can no longer meet these obligations. Customer may take reasonable steps to stop unauthorized use.

## 6. Customer's obligations

Customer is responsible for having a lawful basis and giving notices for the data it puts in its projects, for its instructions being lawful, for configuring retention (`retention.days`) and access in its projects, and for the agents and model providers it connects (those providers are Customer's own processors, not AgentIM's subprocessors).

## 7. Liability and precedence

The liability limits in the Terms apply to this DPA, except where the law does not allow it. If this DPA conflicts with the Terms, this DPA wins for personal data; the Standard Contractual Clauses win over both.

## Annex 1: Details of processing

| Item | Details |
|---|---|
| Subject matter and duration | Hosting Customer's AgentIM projects, for as long as the Terms last and until deletion under §2.8 |
| Nature and purpose | Storing, transmitting, syncing, searching, exporting and deleting messages and related data so Customer's apps work: delivering messages between people and agents, push to connected devices, webhooks to Customer's systems, moderation tools Customer uses |
| Data subjects | Customer's end users; the owners of agents; people named in content |
| Categories of data | User ids and profile fields Customer sets (display name, avatar, custom fields); device ids and names, sign-in tokens (hashes); conversation membership and roles; message content and attachments (files, images); reactions, read positions, drafts, feedback; reports and moderation decisions; block lists and friends; IP addresses, used in memory for rate limits and allow lists and kept in request logs for up to 30 days |
| Special categories | None intended. If Customer's app lets users send them (for example health information in a message), Customer is responsible for the lawful basis; Annex 2 applies to all content |
| Frequency | Continuous |
| Retention | As Customer configures (`retention.days`, 180 days by default; plan limits apply), then until deletion under §2.8 |
| Subprocessors | See [Subprocessors](/legal/subprocessors/) |
| Competent supervisory authority (SCCs) | The authority of the EU member state where Customer is established, or where its EU representative is |

## Annex 2: Security measures

| Area | Measures |
|---|---|
| Isolation | Each project has its own database and its own keys; requests are routed by the project's address; one project's keys don't work on another |
| Encryption | TLS for all traffic; message content and files encrypted at rest with per-conversation keys under a managed master key; dissolved conversations' keys are destroyed (crypto-shredding) |
| Keys and secrets | API keys, agent keys and client tokens stored only as hashes; signing keys and webhook secrets encrypted; shown once; can be replaced at any time |
| Access control | Console roles (Owner, Admin, Developer, Billing); production access only for named staff, with multi-factor authentication; least privilege |
| Logging | Logs contain ids and errors, not message content; audit log of administrative actions without content; tracing without content |
| Availability | Clustered servers with leases and failover; database backups daily, kept up to 30 days; restore tested |
| Application security | Business rules enforced on the server and tested on every change; rate limits per user, key and IP; refused requests leave no trace in the conversation log; dependency scanning; signed releases |
| Data minimisation | Clients strip GPS data from photos by default; retention deletes old messages; the operator can't read content through the console |
| AI | Agent messages are marked as AI (EU AI Act Art. 50); agents only see conversations they are members of; humans can take over |
| Incidents | Monitoring and alerts; breach notification under §2.7 |
| People | Confidentiality obligations; access removed when someone leaves |
| Vulnerabilities | Private reporting ([Reporting a vulnerability](/docs/security/reporting/)); fixes released with advisories |

<!-- Owner: fill in the company name (docs/06 §2.1 O5) and have a lawyer review §4 (SCC options) before signing customers. -->
