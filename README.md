# AgentIM

English · [中文](README.zh-CN.md)

**Let AI get things done inside your product, safely.** AgentIM is open-source chat infrastructure with AI agents built in, written in Rust. Your users talk to people and to agents in the same conversation. Agents call your APIs and MCP servers. A person approves anything that changes data. Anyone can take over at any time. Every step is on record.

It ships as one program, `agentim`. It runs on one machine or as a cluster, on SQLite, PostgreSQL or MySQL. Self-host it under Apache 2.0 with every feature free, or use the hosted cloud (early access).

![The after-sales example: the manager's inbox with a $120 refund waiting for approval, next to the conversation where the assistant showed its steps and the customer's form](docs/images/examples/aftersales-desk.png)

*The [after-sales example](examples/). The assistant looked up the order and showed it as a card. It asked the customer why with a form, then stopped at a $120 refund. That refund waits in the manager's inbox because the rule says refunds over $30 need a manager.*

## What's in the box

```mermaid
flowchart LR
  subgraph app["Your product"]
    UI["Your app<br/>Client SDKs · UI components"]
    BE["Your backend<br/>Server SDKs"]
    AG["Your agents<br/>Agent SDKs · MCP"]
  end
  subgraph im["agentim (one program, 1 to N nodes)"]
    S["IM server<br/>conversations · sync · files"]
    A["Built-in assistants<br/>models · connections · approvals"]
    C["Console and admin API"]
  end
  DB[("SQLite · PostgreSQL · MySQL")]
  EXT["Your APIs and MCP servers<br/>Model providers"]
  UI -- "WebSocket" --> S
  BE -- "Open API /v1 · webhooks" --> S
  AG -- "WebSocket · MCP" --> S
  A -- "tools, after approval" --> EXT
  S --> DB
```

| Component | What it is | Where |
|---|---|---|
| **Server** `agentim` | One Rust program with the IM server, the built-in assistants, the cluster, the open API, the MCP endpoint, webhooks and file storage. It runs one node or a cluster, and its subcommands do operations (`cluster`, `verify`, `api-key`, `signing-key`, …) | [`server/`](server) |
| **Console** | The admin web app at `/console`, built into the server. Keys, people, agents, conversations, connections and approval rules, model services, reports, message traces, the cluster and databases. Admin accounts use a password plus TOTP and have four roles | [`server/console/`](server/console) |
| **Admin CLI** | `agentim admin`: everything the console does, from the command line or scripts | [`server/`](server) |
| **Client SDKs** | For your app: one Rust core wrapped for the browser, React Native, Swift, Kotlin, C# and Unity, Dart and Flutter, and C | [`sdk/client-*`](sdk/README.md#client-sdks) |
| **UI components** | `<Chat>`, `<Inbox>`, the conversation list and the support window for React and React Native. Includes cards, forms, steps, approval cards, and themes | [`sdk/react/`](sdk/react), [`sdk/react-native-ui/`](sdk/react-native-ui) |
| **Server SDKs** | For your backend: the open API, user tokens and webhook signatures, for Node, Python and Go | [`sdk/node/`](sdk/node), [`sdk/python/`](sdk/python), [`sdk/go/`](sdk/go) |
| **Agent SDKs** | For your own agents: Node and Python, with no dependencies. MCP needs no SDK. There are templates for the OpenAI Agents SDK, Claude Agent SDK and LangGraph | [`sdk/agent-node/`](sdk/agent-node), [`sdk/agent-python/`](sdk/agent-python), [`examples/agents/`](examples/agents) |
| **Hosted cloud** | A managed AgentIM (sign-up, projects, usage, billing) built on the multi-tenant mode below. The cloud's own code is not in this repository | — |
| **Deployment** | The install scripts, Docker image, Docker Compose (three nodes), Helm chart and Prometheus alerts | [`deploy/`](deploy) |
| **Docs site** | The homepage and the documentation: guides, plus references generated from the source for every API, protocol message, error and SDK method | [`site/`](site) |
| **Examples** | An after-sales assistant, an advisor with an AI aide, and a multi-agent task room. Each runs with one command | [`examples/`](examples) |

## Features

Everything below is built and tested on SQLite, PostgreSQL and MySQL. Each row links to its design doc.

### Messaging

| Feature | What it does |
|---|---|
| Conversations | One-to-one (one per pair) and groups, with members added in bulk at creation |
| Messages | Text, edit, recall, replies, @mentions, custom message kinds with a plain-text fallback, and signals such as typing |
| Streaming | Agents' replies appear word by word |
| Ordered, no loss | A message is acknowledged only after it is durably committed. Every member sees the same gap-free order. Retries are deduplicated |
| Sync across devices | Each device keeps its own position and syncs only what it lacks. History can be paged back. Read state and unread counts are shared across devices, and conversations can be muted |
| Private side channel | Whispers, and messages only one member sees |
| Files and images | Local disk or S3, direct uploads in parts, thumbnails made on the device, location data removed. Links check permission every time |
| Members | Roles, remove, leave, transfer, dissolve, block. The user directory is private |
| Retention | Per-app retention period. Old content is deleted in the background |

Design: [messaging](docs/en/design/messaging.md) · [sync and clients](docs/en/design/sync-and-clients.md)

### AI agents

| Feature | What it does |
|---|---|
| Agents as members | Agents join conversations next to people. The server sets the sender type, so an agent cannot pose as a person, and AI is always labelled (EU AI Act Art. 50) |
| Bring your own agent | Any framework: OpenAI Agents SDK, Claude Agent SDK, LangGraph, Vercel AI SDK. Connect over MCP (`POST /mcp`), WebSocket, or the Node and Python agent SDKs (at-least-once, reconnects). One command creates an agent and its key |
| Built-in assistants | Run inside the server, with no agent process. Models from Claude, OpenAI, DeepSeek, Gemini or any OpenAI-compatible service, with your key or the deployment's. Usage is counted at the providers' prices |
| Roles and Skills | A role is written in Markdown and pushed to the agent when it changes. Knowledge, Skills, MCP servers and API docs come in a library |
| Connections | Your remote MCP servers, and your REST APIs imported from OpenAPI, where every operation becomes a tool. Secrets are stored encrypted, hosts must be on an allowlist, and internal addresses are refused |
| Agent to agent | Wake-ups, a budget for each chain, task channels where workers claim steps (exactly one wins), and turns to speak |
| History and state | Each agent's history, context, full-text search, summaries and checkpoints. Signed export and import |
| Rich replies | Data cards, forms, steps, sources, feedback, and hand-off to a person |
| Guardrails | Content filter, DLP, prompt-injection flags, hop limit, loop breaker, watchdog, rate limits and budgets |

Design: [agents](docs/en/design/agents.md) · [built-in assistants](docs/en/design/assistants.md)

### People stay in charge

| Feature | What it does |
|---|---|
| Approve before it changes data | Read-only calls run directly. A call that changes data shows an operation card first, with what, the values and the amount |
| Approval rules | By operation and by amount, for example: refunds up to $30 the customer confirms, above $30 a manager approves, address changes run at once. The approver can be the asker, named people, or a review agent, and requests expire |
| Drafts | An agent's reply waits until its owner sends it, edited or not, or discards it |
| Ask a person | Approve, answer or choose. Exactly one decision counts |
| Approval inbox | Everything waiting for one person across conversations: operations, drafts, hand-offs and questions |
| Take over | A person takes the conversation and the agent goes quiet. It is handed back by the person, or automatically after silence |
| Stop | Interrupt a reply, pause an agent, kill switch, destroy |

Design: [approvals and human control](docs/en/design/approvals.md)

### Chat UI

| Feature | What it does |
|---|---|
| Components | `<Chat>`, `<Inbox>`, a conversation list and a support window for React and React Native: grouping, dates, unread line, edit and delete, images and files, approval cards |
| Data cards | Facts, tables and lists. Currency, dates and percentages are formatted for the viewer's language and region, and statuses get coloured badges |
| Form cards | Text, multi-line, number, select, checkbox, date, time and email fields, checked before sending. A form can be limited to one person, and stays answered after a reload |
| Steps | "Look up order ✓ → Refund $120 · waiting for Sam" next to the reply, folded into one line when done |
| Theming | Light, dark and your own colours through CSS variables, with contrast checked. A [live preview](site/src/content/docs/docs/guides/theming.mdx) generates the CSS |
| Extension points | Your own card blocks, form fields, steps and inbox items. Headless hooks let you draw everything yourself |

Design: [chat components](docs/en/design/chat-components.md)

### Client SDKs

One Rust core does reconnects with backoff, the offline queue (sent once and in order), the encrypted local cache, history and gap detection. Every platform wraps it:

| Platform | Package |
|---|---|
| Browser, Node | `@agentimhq/client-web` (the core in WebAssembly, one connection for all tabs) |
| React | `@agentimhq/react` |
| React Native | `client-react-native` + `react-native-ui` |
| iOS, macOS | [`agentim-swift`](https://github.com/wmkrfjnwvb-svg/agentim-swift) (Swift Package) |
| Android, JVM | `com.agentim:client` (Kotlin) |
| .NET, Unity | NuGet and a Unity package (WebGL included) |
| Flutter, Dart | `agentim_client` |
| Native | The C ABI (`agentim.h`) |

[sdk/README.md](sdk/README.md) · Design: [sync and clients](docs/en/design/sync-and-clients.md)

### For your backend

| Feature | What it does |
|---|---|
| Your own accounts | Your backend signs each user a token (a JWT). Users have no AgentIM password and there is no login call |
| Open API | `/v1` with OpenAPI 3.1, scoped API keys, idempotency keys, rate limits and RFC 9457 errors. Server SDKs for Node, Python and Go |
| Webhooks | Signed, ordered, at-least-once, for message, member, card and friend events |
| Before-send hook | Your backend can check, change or refuse a message before it is stored |
| Custom fields | A JSON object of up to 4 KiB on users and on conversations |
| Permissions | Which client operations each app allows, and per-user limits |

Design: [open API and webhooks](docs/en/design/open-api.md) · [access](docs/en/design/access.md)

### Security and compliance

| Feature | What it does |
|---|---|
| Encryption at rest | On by default: AES-256-GCM with a key per conversation. Dissolving a conversation destroys its key. Existing databases can be encrypted, and the master key can be rotated |
| Keys and devices | Agent and API keys are stored only as hashes. Single devices can be logged out. Built-in TLS and Origin checks |
| GDPR | Export a user's data, delete an account, retention periods. Logs never contain message content |
| Trust and safety | Reports, a moderation queue, blocks, and a notice when an agent joins |
| Admin accounts | Passwords plus TOTP. Four roles. Reading content needs a reason and is audited |

Design: [security and compliance](docs/en/design/security.md)

### Tracing and audit

| Feature | What it does |
|---|---|
| Trace ids | A W3C trace id follows every request through the client, the writer, other nodes, agents and webhooks. OTLP export |
| Dyeing | Trace one user, device, conversation, agent or API key in full detail on every node for a while, without changing the log level for everyone |
| Message trace | One page in the console shows a message's path: who sent it, which rules applied, who received it |
| Business rule ids | Every refusal names its rule. A catalog of every rule is in [docs/en/business-rules.md](docs/en/business-rules.md) |
| Verifier | `agentim verify` replays each conversation's log and checks it against the current state |

Design: [tracing](docs/en/design/tracing.md)

### Operations and cluster

| Feature | What it does |
|---|---|
| One program | `agentim` runs a single node or a cluster, with no other component except the database |
| Databases | SQLite, PostgreSQL or MySQL with the same behaviour. Automatic schema migration. Several databases with online moves between them |
| Cluster | Nodes are equal: no leader and no election. Each conversation shard is written by the node holding its database lease, and every batch is checked against the lease (fencing). Any node answers any client and forwards to the owner. Nodes talk directly to each other |
| Failover | A node that dies is replaced within about 15 s, with no acknowledged message lost, reordered or duplicated |
| Planned changes | Adding or draining a node and rolling upgrades hand shards over without failing a request. Shards rebalance automatically |
| Database replicas | Replicas are detected at start. A message is acknowledged only once the replicas have it, so a database failover keeps every acknowledged message. Primary changes are detected |
| Monitoring | Prometheus metrics and alerts, health and readiness endpoints. `agentim cluster status` and `agentim cluster drain` |
| Deploy | Static builds for Linux, macOS and Windows. A one-command install that checks the download. Docker images (amd64, arm64, SBOM), Docker Compose (three nodes), a Helm chart. Sigstore signatures and build provenance |
| Console and CLI | `/console` and `agentim admin`: keys, people, agents, connections, approval rules, model services, conversations, reports, the cluster and databases |

Design: [cluster](docs/en/design/cluster.md) · [developer experience](docs/en/design/developer-experience.md)

### Multi-tenant and hosted cloud

| Feature | What it does |
|---|---|
| Multi-tenant | Each project gets its own database and instance, routed by host name, with its own limits, usage and webhooks |
| Hosted cloud (early access) | Sign-up, organizations, projects, keys, webhooks, usage, and Stripe billing by peak connections and messages |

Design: [multi-tenancy](docs/en/design/multi-tenancy.md)

### Planned

These are next:
- the client SDKs on their package registries;
- offline push (APNs, FCM);
- per-user OAuth connections to SaaS apps;
- group agent quotas and a capability store;
- write throughput up to 10,000 msg/s per node;
- enterprise SSO and SCIM.

## Screenshots

These are taken by the test suites from the real components against a real server. Light and dark mode, phone sizes and right-to-left are covered.

**Chat components (web)**

| Data cards, form, steps | The approval inbox | Theme preview |
|---|---|---|
| <img src="docs/images/components/cards.png" width="300" alt="An order table, a refund form and steps waiting for a manager"> | <img src="docs/images/components/inbox.png" width="300" alt="The inbox with a $120 refund to approve, drafts, hand-offs and questions"> | <img src="docs/images/components/theming.png" width="180" alt="The theme preview with accent, radius and the generated CSS"> |
| <img src="docs/images/components/cards-dark.png" width="300" alt="The same cards in dark mode"> | <img src="docs/images/components/inbox-dark.png" width="300" alt="The inbox in dark mode"> | <img src="docs/images/components/inbox-phone.png" width="150" alt="The inbox on a phone"> |

**Chat (web and React Native)**

| Web, light | Web, dark | Web, right-to-left |
|---|---|---|
| <img src="docs/images/components/web-chat-light.png" width="300" alt="Conversation list, chat with an AI card and a streaming reply, members"> | <img src="docs/images/components/web-chat-dark.png" width="300" alt="The same in dark mode"> | <img src="docs/images/components/web-chat-rtl.png" width="300" alt="The same in a right-to-left language"> |

| Support window | Files and images | Image viewer | Phone | React Native |
|---|---|---|---|---|
| <img src="docs/images/components/web-support.png" width="200" alt="A support chat window"> | <img src="docs/images/components/web-files.png" width="200" alt="Image and file messages"> | <img src="docs/images/components/web-viewer.png" width="200" alt="The image viewer with View original"> | <img src="docs/images/components/web-phone.png" width="110" alt="The chat on a phone"> | <img src="docs/images/components/rn-light.png" width="220" alt="React Native: list and chat"> |

**Examples**

| After-sales: the customer | After-sales: the manager's desk | Advisor with an AI aide | Multi-agent task room |
|---|---|---|---|
| <img src="docs/images/examples/aftersales-customer.png" width="230" alt="The customer confirms a $25 refund in the chat"> | <img src="docs/images/examples/aftersales-desk.png" width="230" alt="The manager approves a $120 refund in the inbox"> | <img src="docs/images/examples/advisor.png" width="230" alt="The advisor's drafts and private notes"> | <img src="docs/images/examples/tasks.png" width="230" alt="Agents claim and report steps"> |

**Console**

| Overview | Connections and approval rules | Message trace |
|---|---|---|
| <img src="docs/images/console/overview.png" width="300" alt="Console overview"> | <img src="docs/images/console/connections.png" width="300" alt="An OpenAPI connection with approval rules"> | <img src="docs/images/console/trace.png" width="300" alt="The path of one message: who could see it, which devices have it"> |

## Quick start

```bash
# Linux and macOS
curl -fsSL https://github.com/wmkrfjnwvb-svg/agentim/releases/latest/download/install.sh | sh
# Windows (PowerShell)
irm https://github.com/wmkrfjnwvb-svg/agentim/releases/latest/download/install.ps1 | iex
# or Docker
docker run -p 8080:8080 -v agentim:/var/lib/agentim ghcr.io/wmkrfjnwvb-svg/agentim --db /var/lib/agentim/agentim.db

agentim          # in an empty folder: creates agentim.db and its key, prints the address and a console setup link
agentim --demo   # or try it: open http://127.0.0.1:8080 and sign in as Alice, Bob or Carol
```

Release files are checked with SHA-256 at install, signed with Sigstore, and carry build provenance ([developer experience](docs/en/design/developer-experience.md)). Until v0.1.0 is out, build from source with `cd server && cargo run --release --bin agentim -- --demo`. Back up `agentim.db.key` with the database, because the encrypted content cannot be read without it.

**See the examples.** Each runs with one command and needs no model key; set `ANTHROPIC_API_KEY` to use Claude instead of the scripted model:

```bash
cd server && cargo build && cd ../examples && npm install
npm run aftersales   # an assistant that looks up orders, refunds with approval, and hands over to a person
npm run advisor      # an advisor whose AI aide drafts every reply for them to send
npm run tasks        # a lead agent and two workers that claim and report steps
```

**Wire it into your app:**

| | How | Docs |
|---|---|---|
| Your users | `agentim signing-key create`. Your backend signs each user a token (`createUserToken` in the server SDKs) | [access](docs/en/design/access.md) |
| Your backend | `agentim api-key create --scopes users:write,messages:write`, then the open API or the [server SDKs](sdk/README.md) | [open API](docs/en/design/open-api.md) |
| An agent | `PUT /v1/users/helper` with `{"kind": "agent", "owner": "alice"}` returns its key and an MCP config block | [agents](docs/en/design/agents.md) |
| A built-in assistant | In the console: add a model service, a connection (MCP or OpenAPI) with its approval rules, and the assistant's role | [assistants](docs/en/design/assistants.md) |
| Your app's UI | `@agentimhq/react` (`<AgentIM>`, `<Chat>`, `<Inbox>`) or the client SDK for your platform | [sdk/README.md](sdk/README.md#client-sdks) |
| Configuration | Databases, cluster, webhooks, TLS, login, models: [`server/agentim.example.toml`](server/agentim.example.toml) | [server/README.md](server/README.md) |

## Guarantees and how they are tested

| Guarantee | What it means | Tested by |
|---|---|---|
| No loss | An acknowledged message survives `kill -9`, restarts, node failure and database failover | A real process is killed mid-traffic on Linux and Windows. Faults are injected into commits, which must then not acknowledge. Multi-node failover tests |
| Order | Every member sees the same gap-free sequence | Every test checks contiguous sequence numbers and in-order pushes |
| Correct business rules | Permissions, visibility, approvals and deduplication are enforced by the server, for clients and the open API alike. Refusals leave no trace | About 1,700 server tests across SQLite, PostgreSQL and MySQL, including concurrency races and checks that the tests can fail. A business verifier replays the log ([testing](docs/en/testing.md)) |
| Slow clients cannot hurt others | A client that stops reading loses only its own pushes and is told to resync | Scale tests in CI |
| Scale | Tens of thousands of connections per node, groups in the thousands | 20,000 connections with a reconnect storm, and 2,000-member groups, in CI |

## Performance

Measured with [`server/bench/single-node.sh`](server/bench/single-node.sh) and [`server/bench/cluster.sh`](server/bench/cluster.sh); run them on your own hardware to get your numbers.

The machine was a 4-core Intel Xeon @ 2.1 GHz cloud VM with 15 GB RAM, with the load generator on the same machine. The databases were SQLite (bundled), PostgreSQL 16, and MariaDB 10.11 (the MySQL protocol). Every acknowledgement waits for fsync. Every run also checks correctness: gap-free logs, exactly-once messages, in-order pushes, nothing missing.

### Connections and delivery

End-to-end means from the sender's send to each receiver's push.

| Scenario | SQLite | PostgreSQL | MySQL |
|---|---|---|---|
| 10,000 connections, groups of 100, 1% sending: end-to-end p50 · p99 | 3 · 21 ms | 4 · 11 ms | 4 · 14 ms |
| All 10,000 dropped at once and reconnected (no message lost) | 1.0 s | 1.4 s | 1.4 s |
| Server memory per connection | 13.6 KB | 13.5 KB | 13.5 KB |
| 1,000-member group at 50 msg/s (50,000 pushes/s): end-to-end p50 · p99 | 7 · 13 ms | 7 · 17 ms | 7 · 15 ms |
| 200-member group, 20 members stop reading, 2 KB messages: everyone else's p50 · p99 | 4 · 12 ms | 4 · 14 ms | 5 · 29 ms |
| Restart after `kill -9`, from process start to ready | 236 ms | 128 ms | 131 ms |

### Scale tests

These run on a standard GitHub runner, SQLite, with each scenario sending for 120 s ([testing](docs/en/testing.md)):

| Scenario | Result |
|---|---|
| 20,000 connections, then all dropped at once | Connected in 2.5 s and all back in 2.3 s, with no failed send during the storm. End-to-end p50 3 ms, p99 7 ms; 2.4M deliveries, none lost or out of order |
| 2,000-member group, 50 msg/s (100,000 pushes/s) | 12M deliveries, all in order. End-to-end p50 14 ms, p99 43 ms; server memory 54 MB |
| 500-member group, 50 members stop reading, 300 msg/s of 2 KB | The 50 are told to resync and catch up. The other 450 miss nothing: p99 30 ms; server memory 34 MB |

### Writes and fan-out

Throughput, then ACK latency p50 · p99.

| Scenario | SQLite | PostgreSQL | MySQL |
|---|---|---|---|
| 1 client | 582 msg/s · 1.5 · 3.5 ms | 604 msg/s · 1.5 · 3.0 ms | 385 msg/s · 2.2 · 5.7 ms |
| 10 clients | 1,080 msg/s · 8.2 · 13 ms | 995 msg/s · 8.5 · 14 ms | 660 msg/s · 13 · 26 ms |
| 50 clients | 1,218 msg/s · 35 · 60 ms | 1,219 msg/s · 36 · 58 ms | 919 msg/s · 47 · 80 ms |
| 200 clients | 1,320 msg/s · 137 · 182 ms | 1,327 msg/s · 134 · 192 ms | 1,034 msg/s · 170 · 253 ms |
| Group of 50, everyone sending, 10 agents streaming (251k pushes) | 1,151 msg/s · 38 · 56 ms | 794 msg/s · 53 · 101 ms | 817 msg/s · 55 · 73 ms |
| Group of 200, everyone sending, 10 agents streaming (1.2M pushes) | 685 msg/s · 166 · 765 ms | 641 msg/s · 153 · 1,000 ms | 514 msg/s · 235 · 962 ms |

### Cluster

All nodes, the database, and one load generator per node ran on the same machine. So this table shows the cost of forwarding between nodes, not how separate machines scale. Throughput, then ACK latency p50 · p99:

| | 1 node | 3 nodes |
|---|---|---|
| PostgreSQL, 50 clients per node | 874 msg/s · 53 · 78 ms | 1,988 msg/s · 42 · 164 ms |
| PostgreSQL, a group of 50 per node, all sending | 882 msg/s · 51 · 73 ms | 1,038 msg/s · 105 · 517 ms |
| MySQL, 50 clients per node | 680 msg/s · 68 · 105 ms | 1,359 msg/s · 68 · 254 ms |
| MySQL, a group of 50 per node | 600 msg/s · 76 · 131 ms | 965 msg/s · 128 · 515 ms |

### Costs of encryption and tracing

Both are on by default. Each costs about 2% of write throughput:

| | Off | On |
|---|---|---|
| Encryption at rest | 818 msg/s | 800 msg/s |
| Trace ids | 794 msg/s | 780 msg/s |

Exporting every trace over OTLP costs 8% more; by default only sampled and dyed traces are exported.

### Where the limit is

Write throughput tops out at about 1,300 msg/s per database today. fsync is not the bottleneck, because group commit puts hundreds of messages in one commit. The limit is the fixed cost of each SQL statement, about nine per message. The planned optimisations are fewer statements per message, fan-out off the writer thread, and several writers per database. They aim at 10,000 msg/s per node ([architecture §5](docs/en/architecture.md#5-the-write-path)). Tests with separate load machines and multiple servers are next.

## Deployment

[`deploy/`](deploy) has the Docker image, a three-node Docker Compose setup, a Helm chart and Prometheus alerts. The configuration covers databases, the cluster, webhooks, TLS, login and models; it is in [`server/agentim.example.toml`](server/agentim.example.toml). How the cluster works, with its tests, is in [the cluster design](docs/en/design/cluster.md).

## Documentation

| | |
|---|---|
| **Using AgentIM** | The docs site source in [`site/`](site): quick start, concepts, guides, and references generated from the source for the open API, the client protocol, MCP tools, configuration and every SDK |
| **How it is built** | [`docs/`](docs/README.md), in English with a Chinese translation: [product](docs/en/product.md), [architecture](docs/en/architecture.md), a detailed design page per area with numbered feature points and the tests that cover each one, [testing](docs/en/testing.md) and the [business rule catalog](docs/en/business-rules.md). 中文：[docs/zh](docs/zh/README.md) |
| **Contributing** | [CONTRIBUTING.md](CONTRIBUTING.md) |
| **Security** | [SECURITY.md](SECURITY.md) |

## License

Apache 2.0.
