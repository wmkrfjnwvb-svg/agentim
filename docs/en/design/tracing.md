# Tracing and business rules

How a message can be followed through every step of its path, on every node: trace ids, dye rules, the message trace, numbered business rules, and the verifier that checks the log against the state.

Code: `server/crates/agentim-core/src/trace.rs` (trace context, dye matching), `otel.rs` (OTLP export), `hub/dye.rs`, `hub/msgtrace.rs` and `db/msgtrace.rs` (the message trace), `hub/verify.rs` and `db/verify.rs` (the verifier), `metrics.rs`, the `rule!` macro in `lib.rs`; client trace in `sdk/client-rust/core`.

## 1. Purpose

"My message never arrived" can have a dozen causes, spread over many nodes:
- an expired token;
- a permission or a rate limit;
- a before-send hook;
- a conversation being moved;
- overload;
- being silenced or blocked;
- a full push queue;
- a client that never acknowledged;
- a webhook failing;
- an agent's breaker.

Tracing makes each of these visible without logging content. Numbered rules make every refusal name its reason.

## 2. Feature points

| ID | Feature | Behaviour |
|---|---|---|
| TRC-01 | Trace ids everywhere | Every request has a W3C trace context. It flows from the client SDK (the same trace across retries and restarts) through the session layer, the writer batch, cluster forwarding, fan-out, webhooks (`traceparent`) and agents (`ctx.trace`) |
| TRC-02 | Trust boundary | A trace from an end user's client is linked, not continued: the server starts its own trace and records `client.trace_id`, so nobody can force full logging. Agents and backends are continued |
| TRC-03 | Logs carry the trace | Every log line has `trace_id` and `span_id`, so logs from several nodes join up in any log system |
| TRC-04 | OTLP export | `[tracing] otlp_endpoint`, `sample_ratio` (0 by default: ids in logs only). Dyed traces are always exported |
| TRC-05 | Dye rules | An operator marks a `user`, `device`, `conv`, `agent` or `api_key`, with a reason, for up to 24 h (default 1 h), at most 100 rules. Matching requests are sampled and logged from DEBUG on every node; work they cause stays dyed. The rule id travels in `tracestate` inside the cluster only |
| TRC-06 | Message trace | `GET /admin/v1/messages/{msg_id}/trace` builds the path from stored data alone: received, committed, who could see it, which devices have it and which do not, the state per webhook, agents and their replies, and later history. It shows metadata only and every view is audited |
| TRC-07 | Numbered business rules | Every business check is `rule!("conv.not_member", "forbidden", …)`. A refusal carries `rule` in the protocol, the open API and MCP. `agentim_refusals_total{op, code, rule}` counts them. The catalog is generated from the code |
| TRC-08 | Every refusal is a rule | A test fails when business code refuses without a named rule. CI fails when a rule is never triggered by a test (except a short, shrinking list) |
| TRC-09 | The verifier | `agentim verify [--conv] [--since]` replays each conversation's log against the current state and checks the invariants: contiguous seq, membership, message states, drafts, files, cursors. It is read-only and exits non-zero when it finds damage |
| TRC-10 | Low overhead | About 2% of write throughput with trace ids; exporting every trace costs about 8% more |

## 3. Design

### 3.1 The path of a message

```
 client SDK (the core makes the trace, stores it with the queued operation, retries with the same trace)
 ① entry: WS client / WS agent / open API / MCP / agent files / admin API / CLI
 ② authentication: token, signing key, API key, agent key; failure counting
 ③ session layer: agent stopped → people-only ops → app permissions → per-person rate
 ④ before-send hook (optional)
 ⑤ routing: conversation → database → shard; forwarding to the shard's node
 ⑥ the writer's queue (full: overloaded) → a group-commit batch, one savepoint per command
 ⑦ business checks in the writer → log, dedup, file references, drafts and approval
 ⑧ commit (one fsync) → ACK to the sender
 ⑨ fan-out: local connections (push or resync), side channel, other nodes, live deltas
 ⑩ receivers: push → cache → ack; offline devices on their next sync
 ⑪ outbox → webhook delivery (retries, drops)
 ⑫ agents woken → their replies (causation_id points back) → again from ①
 ⑬ background: retention, file deletion, draft expiry, conversation moves
```

The business rules carry their stage: ③ for the session layer, ⑦ for the writer, and the file store.

### 3.2 Where the trace travels

| Place | How |
|---|---|
| Client SDK | The core gives each queued operation a `traceparent`, stored with it. Every attempt gets a new parent id under the same trace id. Frames carry an optional `trace` |
| HTTP entries | Read `traceparent`; start a new trace when it is missing or invalid |
| Inside the server | `Op` carries the context. A writer batch span links to every command in it. Cluster forwarding and publishing carry the context |
| Storage | `messages.trace_id` (16 bytes, nullable), so the message trace can find it. It is not copied into events or pushed to people |
| Pushes to agents | Carry the trigger's trace; agents send it back with their reply |
| Webhooks | `traceparent` header (`webhooks.trace = true`), without `tracestate` or baggage |

### 3.3 Dye rules

- Rules are stored in the control database. Every node applies them within seconds through the same notification path as key revocations.
- The log level is raised per trace, so other requests keep their normal level.
- Dyeing logs metadata only (ids, stages, timings, results, rule ids), never content, tokens or whispers. A dye rule singles out one person, so it needs a reason, is audited and expires.

### 3.4 The message trace

| Section | Source |
|---|---|
| Received | `trace_id`, `client_msg_id` (within the dedup window) |
| Committed | Database, seq, time, state and version, `causation_id` |
| Who could see it | Members with `visible_from ≤ seq`, plus those removed since who could see it then; side events on that seq |
| Delivery | For every device of those people: whether its acknowledged position passed the seq. Up to 100 undelivered devices are listed with their position and last activity |
| Webhooks | Per event and webhook: `pending` (with the queue ahead), `dropped` (from the audit), `delivered` |
| Agents | Agents that can see it, how they are triggered, whether they are stopped, their replies and drafts |
| Later | The event history (edits, recall), reports, files, the retention date |

What is not stored, and so not shown: the entry and node that took the request, and whether before-send changed it. Use the trace id in the logs for those. `[tracing] trace_ui_url` adds a link to Jaeger, Tempo or another trace UI.

### 3.5 Rules

- Rule ids are `<domain>.<name>`, and each id has exactly one error code. The domains are listed in the [catalog](../business-rules.md).
- `AGENTIM_WRITE_RULES=1 cargo test --test rules` regenerates the catalog. The test fails when the catalog is stale, when an id has two codes, or when a business refusal is not a rule.
- `server/ci/rules-untested.txt` lists rules no test triggers yet. CI fails when a listed rule becomes tested, so that it is removed from the list, and when an unlisted rule is not tested.

## 4. Interfaces

- Admin API:
  - `/admin/v1/deploy/dye-rules` (list, add, remove; `operator` or `owner`);
  - `/admin/v1/messages/{id}/trace`.
- Console pages: dye rules, message trace.
- CLI: `agentim verify`.
- Metrics: `agentim_refusals_total`.
- Configuration: `[tracing] otlp_endpoint`, `sample_ratio`, `trace_ui_url`; `[webhooks] trace`.

## 5. Rules and errors

Rule prefix: `dye.*` (reason required, at most 24 hours, at most 100 rules; [catalog](../business-rules.md)). Reading a message trace needs `owner`, `operator` or `moderator` and is audited.

## 6. Tests

Paths under `server/crates/agentim-core/tests/`; three databases.

| Feature | Tests |
|---|---|
| TRC-01–TRC-03 | `tracing.rs`: all 5 tests (a retry keeps the first trace; agents and backends are continued; agents get the trace by sync; invalid or missing headers start a new trace; people get a new trace that links theirs); cluster `cluster.rs`: `the_trace_crosses_nodes`; client `sdk/client-rust/core/tests/rules.rs`: `every_attempt_of_an_operation_carries_its_trace`, `a_queued_operation_keeps_its_trace_across_a_restart` |
| TRC-04 | `otlp.rs`: `sampled_and_dyed_traces_reach_the_collector` |
| TRC-05 | `dye.rs`: all 5 tests; cluster `cluster.rs`: `dye_rules_reach_every_node_and_follow_the_request` |
| TRC-06 | `msgtrace.rs`: all 6 tests |
| TRC-07, TRC-08 | `rules.rs`: `the_catalog_matches_the_code`, `business_refusals_are_named_rules`, `every_rule_is_triggered_by_a_test` (CI, after every other suite); `http.rs`: `refusals_name_their_rule_and_are_counted` |
| TRC-09 | `verify.rs`: `a_clean_database_has_nothing_to_report`, `each_kind_of_damage_is_found` (mutation: each kind of damage is planted and must be found); `server/crates/agentim/tests/cli.rs`: `verify_reports_and_exits_by_what_it_finds`; CI runs `agentim verify` after the scenario and concurrency suites on all three databases |
| TRC-10 | `server/bench/single-node.sh` (paired runs; numbers in the README) |

## 7. Limits and next steps

- Chain audit is not built: a nightly check that every chain of messages, wake-ups and replies has an end and a reason.
- A few rules are not yet triggered by any test (`server/ci/rules-untested.txt`).
