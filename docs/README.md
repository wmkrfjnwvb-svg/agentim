# AgentIM documentation

English is the main language; a Chinese translation of every page is in [`zh/`](zh/README.md). 中文文档见 [`zh/`](zh/README.md)。

These pages are for people who build AgentIM or want to understand how it works inside. Developers who build *with* AgentIM start at the docs site ([`site/`](../site), published at agentim.co): quick start, guides, and the API and SDK references.

## How the docs are organised

| Layer | What it answers | Pages |
|---|---|---|
| **Product** | What AgentIM is, who it is for, what it guarantees, the full feature list | [Product](en/product.md) |
| **Technical overview** | How the pieces fit: the program, the data model, the write path, the protocol, the cluster | [Architecture](en/architecture.md) |
| **Detailed design** | One page per area: its feature points, how each one works, its limits and error rules, and the tests that cover it | the table below |
| **Quality** | How we test, what each test suite covers, and the business rule catalog | [Testing](en/testing.md), [Business rules](en/business-rules.md) |

## Detailed design, by area

Every design page has the same layout:

1. **Purpose**: what the area does and for whom.
2. **Feature points**: a numbered list (for example `MSG-04`). Each ID is used in code reviews, issues and the test table.
3. **Design**: data, flows, limits and the reasons behind them.
4. **Interfaces**: the operations, endpoints and events.
5. **Rules and errors**: the business rules and the error codes they return.
6. **Tests**: for every feature point, the test files and test names that cover it.
7. **Limits and next steps**: what is not done yet.

| Area | Page | Code |
|---|---|---|
| Messaging: conversations, the log, members, files, extensions | [messaging](en/design/messaging.md) | `server/crates/agentim-core/src/`: `logic.rs`, `logic/`, `hub.rs`, `filestore.rs` |
| Sync and client SDKs | [sync-and-clients](en/design/sync-and-clients.md) | `hub/sync.rs`, `sdk/client-rust/`, `sdk/client-*` |
| Agents: connect, agent to agent, history, guardrails | [agents](en/design/agents.md) | `hub/agents.rs`, `hub/history.rs`, `logic/tasks.rs`, `mcp.rs`, `guard.rs`, `sdk/agent-*` |
| Approvals and human control | [approvals](en/design/approvals.md) | `logic.rs` (drafts), `logic/requests.rs`, `hub/requests.rs`, `policy.rs` |
| Built-in assistants: models, knowledge, Skills, connections | [assistants](en/design/assistants.md) | `ai/`, `hub/assistants.rs`, `hub/knowledge.rs`, `hub/skills.rs`, `hub/connections.rs` |
| Chat components: cards, forms, steps, inbox, theming | [chat-components](en/design/chat-components.md) | `logic/cards.rs`, `sdk/react/`, `sdk/react-native-ui/` |
| Open API, webhooks and the before-send hook | [open-api](en/design/open-api.md) | `open_api.rs`, `webhook.rs` |
| Access: who connects how, tokens, keys, permissions, admin roles | [access](en/design/access.md) | `auth.rs`, `hub/login.rs`, `registry.rs`, `admin_api.rs` |
| Security and compliance | [security](en/design/security.md) | `crypto.rs`, `hub/compliance.rs`, `logic/compliance.rs` |
| Tracing and business rules | [tracing](en/design/tracing.md) | `trace.rs`, `otel.rs`, `hub/dye.rs`, `hub/msgtrace.rs`, `hub/verify.rs` |
| Cluster and database replicas | [cluster](en/design/cluster.md) | `server/crates/agentim-cluster/`, `shard.rs`, `replication.rs` |
| Multi-tenancy | [multi-tenancy](en/design/multi-tenancy.md) | `tenant.rs`, `hub/silos.rs`, `hub/tenants.rs`, `hub/tenant_webhooks.rs` |
| Developer experience: install, release, SDK packages, console, docs site | [developer-experience](en/design/developer-experience.md) | `deploy/`, `.github/workflows/`, `server/console/`, `site/` |

## Keeping the docs true

- A change that adds or changes behaviour updates the design page of its area in the same pull request, in both languages. The English page leads; the Chinese page follows the same structure and IDs.
- A new feature gets a new feature-point ID, and the test table names the tests that cover it. A feature point without a test is a bug in the docs or in the tests.
- The business rule catalog is generated from the code (`AGENTIM_WRITE_RULES=1 cargo test --test rules`). CI fails when it is out of date.
- Delete what is wrong instead of adding a correction below it.
