# Architecture

This page gives the whole picture: the program, the data model, the write path, the protocol, storage and the cluster. Each area has its own detailed design page; this page links to them.

## 1. Principles

1. **Order is per conversation.** Conversations are the unit of sharding; there is no global order.
2. **A sequence number and the data are committed in one transaction, and the server acknowledges only after the commit is durable** (fsync).
3. **Pushes are hints; pulling is the truth.** A push may be dropped. Sync always recovers what was missed.
4. **Only storage is strongly consistent.** Routing, presence, stream buffers and caches are soft state that can be rebuilt.
5. **Agents run outside the message core.** A slow or failing model never slows the core. Built-in assistants run in the same process but talk to the core through the same protocol as any agent.
6. **Overload refuses requests**; there are no unbounded queues. Stability comes before throughput.
7. **Extension points are traits, webhooks and hooks**, so behaviour can be customised without changing server code.

## 2. The program

`agentim` is one Rust program. The same binary serves clients, runs the cluster, and provides the operator and admin commands.

```
server/crates/
  agentim/          the binary: CLI entry point (main.rs)
  agentim-core/     everything else
    src/server.rs        HTTP and WebSocket server (axum), TLS, Origin checks
    src/hub.rs, hub/     the session layer: authentication, permissions, routing,
                         fan-out, side effects, agents, assistants, admin
    src/logic.rs, logic/ business logic that runs in a database's writer
    src/db.rs, db/       every SQL statement (no SQL elsewhere)
    src/storage.rs       dialects, connections, schema and migrations
    src/protocol.rs      the client protocol (ops, frames, versions)
    src/open_api.rs      the open API /v1
    src/mcp.rs           the MCP server
    src/admin_api.rs     the admin API behind the console and `agentim admin`
    src/ai/              model providers and connections for built-in assistants
    src/crypto.rs        encryption at rest
    src/trace.rs, otel.rs  tracing
  agentim-cluster/  leases, forwarding between nodes, hand-over
server/console/     the admin web app, embedded in the binary
```

Request path, from a client to the database:

```
client ──WebSocket──▶ server.rs ──▶ hub (session layer: auth, permission, rule checks ③)
                                      │  read  ──▶ read pool ──▶ database
                                      │  write ──▶ the conversation's writer (one per database or shard)
                                      │              logic (rule checks ⑦, same transaction) ──▶ commit + fsync
                                      │              after commit: ACK + committed events
                                      ▼
                                   fan-out ──▶ each connection's bounded send queue
                                   webhooks ◀── outbox rows written in the same transaction
```

## 3. Data model

### 3.1 The conversation log

Every conversation has one event log. `conv_seq` is the event sequence number: it increases by one with every event, has no gaps, and is the same for every member.

| Event | Meaning |
|---|---|
| `conv.created`, `conv.updated`, `conv.dissolved` | A conversation is created, changed or dissolved |
| `member.joined`, `members.joined`, `member.left`, `member.removed`, `member.policy` | Membership changes. Members who join a new group together come as `members.joined` events of up to 200 people |
| `msg.created` | A new message. A streaming message is created when it starts, with `state=streaming` |
| `msg.updated` | An edit, the end of a stream, or a moderation replacement (`version+1`) |
| `msg.recalled` | A recall (the content is cleared) |
| `agent.state` | Take-over, hand-back, circuit breaker, guardrail reset. Visible to everyone |

Each message also has a fixed `created_seq`: its place in the conversation. **Messages are displayed by `created_seq`; sync follows `conv_seq`.** So one incremental sync returns new messages and changes to old ones.

### 3.2 The side channel

Each conversation also has a side channel, numbered by `side_seq`, for content only some members may see:
- whispers, and messages for one member;
- drafts waiting for approval;
- private notices from guardrails and filters;
- agent progress and steps.

Each side event has an `audience` (who may see it) and an `anchor_seq` (the log's latest seq when it was made). Agents and approvers merge the two logs by `(anchor_seq, side_seq)`. Ordinary members never get side events meant for others. So the main log has no gaps for anyone and does not reveal that a hidden message exists.

### 3.3 Messages

```
Message {
  conv_id, msg_id, created_seq,
  sender_id, sender_type,   // human | agent | system — set by the server from the authenticated identity
  on_behalf_of,             // whom an agent speaks for (its owner or a delegate)
  kind,                     // text, image, file, agentim.card, custom kinds with a namespace
  text, payload, version,
  state,                    // committed | streaming | complete | cancelled | failed | recalled
  state_reason,
  hop, causation_id,        // computed by the server for loop breaking
  reply_to, mentions,
  ai_generated,             // machine-readable AI label (EU AI Act Art. 50)
  trace_id,
  created_at, updated_at    // set by the writer
}
```

Every message carries plain `text`. For a custom kind, the text is the fallback that older clients show.

## 4. Reliability and order

| Mechanism | How |
|---|---|
| ACK after durable commit | The writer acknowledges only after `COMMIT` returns |
| Deduplication | `(conv_id, sender_id, client_msg_id)` is written in the same transaction as the message. A retry gets the original result |
| Push and pull | Pushes carry the seq. A client that cannot follow gets `resync` and pulls with `sync` |
| Order from one sender | Requests on one connection are handled one after another, in order |
| Streaming | The message takes its seq when it starts. Deltas take no seq and are appended idempotently by `(msg_id, idx)`. A checkpoint is written every second; the end is a `msg.updated` |
| Crash recovery | At start, messages left `streaming` become `failed(server_restart)`, keeping the text up to the last checkpoint |

## 5. The write path

There is a single writer per database (or per shard in a cluster), with group commit and fan-out after commit:

```
connections (in arrival order)          writer (one per database or shard)
──cmd──▶ bounded command queue ──▶ ┌──────────────────────────────────────────┐
                                    │ take one command, then all that are queued │
                                    │ (up to 512 or 1 MB; no extra wait)          │
                                    │ BEGIN                                       │
                                    │   SAVEPOINT; cmd1; RELEASE  (a failure rolls back only itself)
                                    │   SAVEPOINT; cmd2; RELEASE                  │
                                    │ COMMIT  ← one fsync for the whole batch     │
                                    └──────┬─────────────────────────────────────┘
                                           │ after commit: ACKs and events, in order
                                           ▼
                           fan-out: each event serialised once, shared bytes
                           → each connection has a bounded send queue
                           → queue full: drop the push, mark resync
```

Why it is correct:

| Risk | Handling |
|---|---|
| ACK before durable | ACKs and pushes happen only after `COMMIT` returns |
| One failing command spoils the batch | Each command runs in its own SAVEPOINT; a failure rolls back only that command |
| A rolled-back command leaves a gap in seq | The seq counter is a database row updated inside the SAVEPOINT; it rolls back too |
| The whole commit fails (disk full, fsync error) | Every command in the batch gets an error and is retried with the same `client_msg_id`. An fsync error is fatal: the process stops writing, `/readyz` fails, and it exits (code 70) to recover from the database's log |
| Slow clients exhaust memory | Bounded send queues; when full, pushes are dropped and the client is told to resync |
| Write overload | A bounded command queue; when full, `overloaded`, and the client retries (idempotent) |
| A refused request that must leave a record (a circuit-breaker event, a DLP audit) | The command can be marked "commit even on error" |

**Where the limit is.** fsync is no longer the bottleneck. The limit is the fixed cost of each SQL statement: about nine per message, plus SAVEPOINTs. That gives about 1,300 messages per second per database. The next steps are:
- fewer statements per command;
- fan-out off the writer;
- several writers per PostgreSQL or MySQL database, keyed by conversation.

The target is 10,000 msg/s per node.

## 6. Storage

- **Databases**: SQLite (embedded, the default), PostgreSQL (recommended in production) and MySQL/MariaDB, with the same behaviour. The same test suite runs on all three.
- **Layers**: `storage.rs` (dialects, connections, schema, durability checks) and `db.rs` (all business SQL). Business logic never sees SQL.
- **Durability check at start**:
  - SQLite needs `synchronous=FULL`.
  - PostgreSQL needs `fsync=on` and `synchronous_commit` not `off`.
  - MySQL needs `innodb_flush_log_at_trx_commit=1`, and `sync_binlog=1` with binlog on.

  The server refuses to start otherwise, unless `unsafe_durability = true`.
- **Schema migration**: `meta.schema_version` and `meta.schema_compat`. A new database is created at the latest schema. An old one is migrated step by step at start; each step is re-entrant. One node migrates at a time, using an advisory lock or `GET_LOCK`. Within a major version, migrations only add, so old and new nodes can run side by side during a rolling upgrade.
- **Several databases** (advanced): a unit of the organisation can have its own database.
  - A conversation lives in exactly one database. A control database holds the directory.
  - Conversations move online between databases, with a content hash check.
  - One database failing affects only its conversations.

Details: [messaging](design/messaging.md#storage).

## 7. Protocol

- **Client protocol**: JSON over WebSocket, `GET /ws?v=2,1`.
  - The token travels in a header, or in a WebSocket subprotocol for browsers, never in the URL.
  - The server answers with `welcome`: protocol version, features, limits, and `me`.
  - Requests are `{op, …}` with an `id`; replies carry the same `id`. Pushes are events.
  - Details: [sync-and-clients](design/sync-and-clients.md).
- **Versions**: integer major versions. Within one, only additions are allowed: new optional fields, events, operations, error codes, features. Clients ignore unknown fields and show the fallback text of unknown kinds.
- **Open API** `/v1` for backends ([open-api](design/open-api.md)). **MCP** at `POST /mcp` for agents ([agents](design/agents.md)). The **admin API** for the console and the CLI ([access](design/access.md)).
- **Errors** use a fixed set of codes: `forbidden`, `not_found`, `conflict`, `bad_request`, `blocked`, `rate_limited`, `policy_blocked`, `budget_exceeded`, `overloaded`. Every refusal also names its business rule ([business rules](business-rules.md)).

## 8. Agents and assistants in the architecture

- An **agent** is a user of kind `agent` with an owner. It connects like a client (WebSocket, agent SDK) or through MCP. It is a conversation member, and every rule applies to it.
- The server adds what agents need on top:
  - the side channel for drafts and steps;
  - wake-ups and budgets for agent-to-agent work;
  - guardrails (hop limit, loop breaker, rate limits, filters, DLP, injection flags);
  - take-over and the kill switch.
- A **built-in assistant** is an agent the server runs itself. Each enabled assistant has an in-process session on the same protocol. When it is woken, it builds the context, calls a model provider, streams the reply and calls tools. A tool that changes data waits for approval. In a cluster, one node runs each assistant, chosen by rendezvous hashing; deduplication by `client_msg_id` makes a double run harmless.

Details: [agents](design/agents.md), [approvals](design/approvals.md), [assistants](design/assistants.md).

## 9. Cluster

The cluster needs no component except the database:
- Nodes are equal; there is no leader and no election.
- Conversations map to a fixed number of virtual shards. A shard is written by the node that holds its lease in the database, and every batch checks the lease's fencing token in the same transaction.
- Any node accepts any client and forwards writes to the shard's node. Nodes talk directly to each other over TLS.
- A crashed node's shards are taken over after the lease expires. Planned moves (adding or draining a node, rolling upgrades) hand shards over at once.
- With database replicas, the server waits until the replicas have a write before it acknowledges, so a database failover keeps every acknowledged message.

Details: [cluster](design/cluster.md).

## 10. How the guarantees are kept

| Guarantee | Main mechanism |
|---|---|
| G1 stable | Bounded queues everywhere; overload refuses; slow clients resync; fault isolation per database |
| G2 no loss | ACK after durable commit; deduplication; fsync errors are fatal; replica waits; lease fencing |
| G3 ordered | One writer per conversation; seq inside the transaction; FIFO fan-out |
| G4 correct | Rules checked in the session layer and again in the writer's transaction; refusals leave no trace; the verifier replays the log |
| G5 concurrency | Group commit; shared serialised events; no global lock or sequencer |
| G6 cluster | Same binary; shards and leases; forwarding keeps the protocol unchanged |
