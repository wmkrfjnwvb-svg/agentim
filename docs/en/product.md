# Product

## 1. What AgentIM is

AgentIM lets AI get things done inside your product, safely. It is chat infrastructure with AI agents built in:

- People and agents talk in the same conversation.
- Agents call the business's APIs and MCP servers.
- A person approves anything that changes data.
- Anyone can take over at any time, and every step is on record.

It is one open-source program, `agentim` (Apache 2.0). It runs on one machine or as a cluster, on SQLite, PostgreSQL or MySQL. There is also a hosted cloud.

What sets it apart:

- **Durable, ordered, shared conversations.** A conversation's record is gap-free and ordered, shared by people and many agents. The "session" of an agent SDK is one run between one person and one agent; an AgentIM conversation outlives runs, frameworks and machines.
- **Governance is part of the protocol.** Approvals, questions to people, take-over, kill switch, budgets, guardrails, audit and AI labels are built into the server. They are not a layer the application has to add.
- **Agents move and end cleanly.** An agent's history and state live on the server. A new key lets it continue on another machine or framework. Destroying it revokes its keys, closes its connections, deletes its state and destroys its data keys.
- **One program, every feature free.** The same binary runs from a laptop to a cluster. The data stays with whoever deploys it.

## 2. Users and scenarios

AgentIM is built for developers and the teams and integrators who build products:

| User | Typical use |
|---|---|
| Startups and independent developers | Add chat and an AI assistant to their product; build AI-native support, social or collaboration apps |
| SaaS teams | Add chat and an assistant to an existing product |
| Integrators | Build assistants for their customers |
| Agent developers | Let agents work with people and with other agents in a conversation |

Two scenarios come first:

- **A. An assistant that does the work.** The assistant calls the business's APIs inside the product: it looks up an order, refunds it, changes an address or books an appointment. Calls that change data need approval. The assistant hands over to a person when it cannot help or when asked.
  The [after-sales example](../../examples/) shows this end to end.
- **D. AI between two parties.** On a marketplace, an advisory service or a booking platform, the AI looks things up for one side, drafts replies that a person sends, and passes private notes. The [advisor example](../../examples/) shows it.

## 3. Guarantees

Every change must keep these. Each design page explains how its area keeps them.

| ID | Guarantee | How we check it |
|---|---|---|
| G1 | **Stable**: a node failure does not affect correctness. Overload refuses requests instead of exhausting memory | Fault injection, kill tests, scale tests with slow clients |
| G2 | **No loss**: an acknowledged message survives crashes, restarts, node failure and database failover. The server acknowledges only after the write is durable | `kill -9` mid-traffic on Linux and Windows; failed commits must not acknowledge; failover tests |
| G3 | **Ordered**: every member of a conversation sees the same gap-free sequence. Messages from one device are committed in the order sent | Every test checks contiguous sequence numbers and in-order pushes |
| G4 | **Correct business rules**: permissions, visibility, approvals and deduplication are enforced by the server and cannot be bypassed by a client | Business scenario and race tests on three databases; `agentim verify` replays the log |
| G5 | **High concurrency**: tens of thousands of connections per node, no global sequencer or lock | Load and scale tests |
| G6 | **Cluster-ready**: one binary and one protocol from one node to many; going to a cluster changes no protocol | Multi-node tests with faults |

## 4. Feature list

The numbered feature points are in the design pages; this is the overview.

| Area | Features | Design |
|---|---|---|
| Messaging | One-to-one and group conversations; edit, recall, replies, mentions; streaming; custom message kinds and signals; deduplication; read state and unread counts across devices; whispers and messages only one member sees; files and images; roles, transfer, dissolve, block; retention | [messaging](design/messaging.md) |
| Sync and clients | Per-device positions, sync without re-reading history, history paging, gap detection, mute. One Rust client core for the browser, React Native, Swift, Kotlin, C#/Unity, Dart/Flutter and C | [sync-and-clients](design/sync-and-clients.md) |
| Agents | Agents as members with server-set sender types; connect over MCP, WebSocket or the agent SDKs; keys and rotation; agent to agent (wake-ups, budgets, task channels, claims, turns); history, search, summaries, checkpoints; signed export and import; guardrails | [agents](design/agents.md) |
| Human control | Drafts and approval rules; asking people to decide; approval by operation and amount; the approval inbox; take-over and hand-back; interrupt, kill switch, destroy | [approvals](design/approvals.md) |
| Built-in assistants | Model services (Claude, OpenAI, DeepSeek, Gemini, compatible), roles in Markdown, knowledge, Skills, connections to MCP servers and OpenAPI APIs, usage and limits | [assistants](design/assistants.md) |
| Chat UI | Data cards, form cards, steps, approval cards, the inbox; React and React Native components; theming and extension points | [chat-components](design/chat-components.md) |
| Backend integration | Open API `/v1`, server SDKs, webhooks, the before-send hook, custom fields | [open-api](design/open-api.md) |
| Access | Tokens signed by the developer's backend, devices, API keys, agent keys, client permissions, admin accounts and roles | [access](design/access.md) |
| Security and compliance | Encryption at rest, crypto-shredding, TLS, GDPR export and deletion, reports and moderation, AI labels | [security](design/security.md) |
| Tracing | W3C trace ids end to end, dye rules, message trace, numbered business rules, the verifier, OTLP | [tracing](design/tracing.md) |
| Cluster | Shards with database leases and fencing, forwarding, hand-over, rolling upgrades, database replicas and failover | [cluster](design/cluster.md) |
| Multi-tenancy | A database and instance per project, routing by host, per-tenant limits, usage and webhooks | [multi-tenancy](design/multi-tenancy.md) |
| Developer experience | One-command install, signed releases, images and Helm, SDK packages, console and `agentim admin`, docs site, examples | [developer-experience](design/developer-experience.md) |

## 5. Targets and measured numbers

| Measure | Target | Measured |
|---|---|---|
| Write throughput, one node | 10,000 msg/s (NVMe) | about 1,100–1,300 msg/s per database. The limit is the fixed cost of each SQL statement ([architecture §5](architecture.md#5-the-write-path)) |
| ACK latency, below saturation | p99 < 20 ms | 10 clients: p99 13 ms |
| End-to-end delivery, same region | p99 < 200 ms | 20,000 connections at 200 msg/s: p99 7 ms. A 2,000-member group at 100,000 pushes/s: p99 43 ms |
| Durability of acknowledged messages | no loss on `kill -9`, power loss, database failover | verified for `kill -9` on Linux and Windows, failed commits and failover |
| Recovery after a node failure | clients back within seconds | a crashed node's shards move after about 15–18 s (lease of 15 s); writes to those conversations pause meanwhile |
| Connections | 1,000,000 per cluster | 20,000 on one node tested, including a reconnect storm (about 13 KB per connection) |
| Slow clients | cannot hurt others | verified: a slow client loses only its own pushes and is told to resync |
| AI cost for conversations without agents | zero | zero |

Data limits: a message is at most 64 KB; a streaming message at most 256 KB and 10 minutes; each agent has at most 2 concurrent streams per conversation.

The full benchmark tables are in the [README](../../README.md#performance).

## 6. Out of scope

- Model inference and model gateways. Built-in assistants call model providers; AgentIM does not host models.
- A general agent runtime: sandboxes, workflow engines, memory retrieval. Agents run in their own framework and connect as members.
- A global message order. Order is per conversation.
- For now: end-to-end encryption, voice and video calls, native desktop and phone apps. AgentIM ships the server, protocol, SDKs and UI components.
