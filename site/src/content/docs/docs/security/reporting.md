---
title: "Reporting a vulnerability"
description: "How to report a security problem in AgentIM, and what happens next."
sidebar:
  order: 5
---

Please report security problems privately, not in public issues.

- **GitHub:** use **Report a vulnerability** on the repository's [Security tab](https://github.com/wmkrfjnwvb-svg/agentim/security/advisories/new). Only maintainers see it.
- **Email:** security@agentim.co, for AgentIM Cloud or if you can't use GitHub.

Include what is affected (version, configuration), how to reproduce it, and what an attacker could do. A proof of concept helps.

| Step | When |
|---|---|
| We confirm we got your report | within 3 business days |
| We tell you whether we can reproduce it and how serious it is | within 10 business days |
| Fix released (critical and high) | as soon as possible, aiming for 30 days; AgentIM Cloud is patched before the advisory |
| Public advisory | with the fixing release, at the latest 90 days after your report unless we agree otherwise |

We keep you informed while we work on a fix and publish a GitHub security advisory (with a CVE where it applies) with the release that fixes it, with credit to you if you want it.

**Supported versions:** the latest release gets security fixes; self-hosted users should upgrade to it.

**In scope:** the AgentIM server, SDKs, console, and AgentIM Cloud (app.agentim.co, api.agentim.co, project addresses). **Out of scope:** denial of service by volume, social engineering, reports from automated scanners without a working attack, missing headers without impact, and other people's projects or data.

Please give us 90 days, or until the fix is released if sooner, before you publish it, and don't access other people's data or disrupt services while testing. We won't take legal action against good-faith research that follows these rules.
