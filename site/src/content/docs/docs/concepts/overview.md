---
title: "How AgentIM fits in"
description: "The four kinds of callers, what each one may do, and how a message travels from a phone to an agent and back."
sidebar:
  order: 1
---

AgentIM is the chat part of your product. Your product keeps its own accounts, sign-in and business data. AgentIM keeps the conversations: who is in them, what was said, in which order, and who has seen it. AI agents join those conversations as members, next to people.

## Four callers, four entry points

| Caller | Who it is | Entry point | Credential |
|---|---|---|---|
| **End user** | A person using your app | The client protocol, `/ws`, through a [client SDK](/docs/client-sdks/) | A client token your backend signs ([devices and login](/docs/concepts/devices-and-login/)) |
| **Agent** | An AI model connected as a member | `/ws` with an [agent SDK](/docs/agent-sdks/), or MCP at `/mcp` | An agent key (`atk_…`) |
| **Your backend** | Your own servers | The [open API](/docs/reference/open-api/) `/v1`, [webhooks](/docs/reference/webhooks/), the [before-send hook](/docs/guides/before-send/) | An API key with scopes (`ak_…`) |
| **Operator** | Whoever runs the server; often also the developer | The `agentim` command line, the configuration file, `/metrics`, the [console](/docs/operations/dashboard/) | Shell access, admin accounts or admin keys |

The split is deliberate:

- **The client protocol does what a person does while chatting.** Send, edit, recall, read, block, report, manage the conversations they are allowed to manage, and supervise their own agents (approve drafts, take over, stop).
- **Your backend does everything that belongs to your product.** Create, deactivate, export and delete accounts. Sign users in. Decide what users may do in the app ([permissions](/docs/concepts/permissions/)). Create and retire agents. Moderate.
- **The operator runs the deployment.** Databases, the cluster, encryption keys, global statistics. An API key cannot do any of this, and an admin key cannot act as a user.

## The path of one message

```text
 app ── send ──▶ /ws ──▶ writer for the conversation's database ──▶ COMMIT (fsync)
                                                                      │
       ◀── ack (seq 42) ─────────────────────────────────────────────┤
                                                                      ▼
                                          fan-out: every member's devices, agents, webhooks
```

1. **The client SDK stores the message first, then sends it.** It shows the message at once as `sending`. If the app is closed or the network drops, the message is still queued and goes out later, exactly once ([sync and offline](/docs/concepts/sync-and-offline/)).
2. **The server checks the business rules.** Is the sender a member? Did someone block them? Is the conversation dissolved? The sender's role decides what they may do. A refused request writes nothing: no message, no event, no audit noise.
3. **One writer per database commits it.** Messages that arrive together are committed in one transaction (group commit), but each command keeps its own savepoint. The message gets the next sequence number (`seq`) of its conversation. The client is answered only after the commit is durable on disk.
4. **The event fans out.** The sender's other devices, the other members, agents and webhooks get the same event with the same `seq`. A slow client loses only its own pushes and is told to catch up. It never slows anyone else down.
5. **Agents are woken when the message is for them.** In a direct conversation that is every message. In a group it is a mention, a reply, or a message that addresses the agent ([agents](/docs/concepts/agents/)).

## What AgentIM guarantees

| Guarantee | What it means |
|---|---|
| **No loss** | An acknowledged message survives a crash (`kill -9`) and restarts. A database failover with replicas keeps it too ([cluster](/docs/operations/cluster/)). |
| **Order** | Every member sees the same gap-free sequence for a conversation. A client never applies event 43 before 42. |
| **Correct rules** | Permissions, visibility, approval and deduplication are enforced by the server, for clients, agents and the open API alike. |
| **Stability** | Overload is refused (`overloaded`, `rate_limited`) instead of queued without bound. A slow or stuck client cannot hurt others. |
| **Scale** | Tens of thousands of connections per node and groups of thousands. Several nodes share the databases as a [cluster](/docs/operations/cluster/). |

These are tested on SQLite, PostgreSQL and MySQL, on Linux and Windows, on every change. The test list is in the repository (`docs/05-operations.md` §5).

## Where things are stored

- **Databases.** SQLite for one machine. PostgreSQL or MySQL (MariaDB) for production and clusters. A deployment can split conversations across several databases by *unit*, for example to keep one subsidiary's data in its own region ([databases](/docs/operations/databases/)).
- **Files.** A local folder, or S3-compatible object storage. With S3, clients upload and download directly ([files and images](/docs/concepts/files/)).
- **Encryption at rest** is on by default. Message content is encrypted with AES-256-GCM, with a key per conversation. Deleting a conversation destroys its key ([encryption](/docs/operations/encryption/)).
