# Testing

How AgentIM is tested, what every suite covers, and how to run them. Each [design page](../README.md#detailed-design-by-area) maps its feature points to the exact tests; this page is the overview.

## 1. Rules for every business feature

These come from [CLAUDE.md](../../CLAUDE.md) and apply to every change:

1. **Cover every logic node the feature touches**, not only its own operations. That includes permissions, pushes, pulls, the side channel, streams, drafts, agents, the directory, audit, restart and several databases.
2. **Boundaries**: limits, empty and maximum values, unknown ids, repeated and idempotent calls, a user acting on themselves.
3. **Errors follow the business meaning**, with consistent codes:
   - `forbidden`: not allowed, or not a member;
   - `not_found`: unknown or gone;
   - `conflict`: the state does not allow it now;
   - `bad_request`: invalid input, or not applicable;
   - `blocked`, `rate_limited`.

   **A refused request leaves no trace in the log.**
4. **Concurrency**: race the feature against everything it conflicts with, for many rounds. Then check the invariants: exactly one winner, a log replay equal to the state, nothing after a terminal event.
5. **Prove that a race test can fail**: break the guarded code on purpose (a mutation) and see the test catch it.
6. **Three databases**: SQLite, PostgreSQL and MySQL, on Linux and Windows in CI.

## 2. Suites

| Suite | Where | What | Runs |
|---|---|---|---|
| Server business tests | `server/crates/agentim-core/tests/*.rs` (about 500 test functions) | Every operation and rule, races, restarts, several databases. Tests in `backend_tests!` run once per database | Every push, three databases |
| Cluster tests | `server/crates/agentim-cluster/tests/` (36) | Several nodes on one database: forwarding, failover, fencing, hand-over, invalidation, fault injection, mutation checks | Every push, PostgreSQL and MySQL |
| Crash tests | `server/crates/agentim/tests/crash.rs` | A real process killed with `kill -9` mid-traffic; every acknowledged message must be there | Every push, Linux and Windows |
| CLI tests | `server/crates/agentim/tests/cli.rs` | First run, admin commands against a running server, the demo, `verify` | Every push |
| Replica failover | `tests/replicas.rs` + `server.yml` | Real PostgreSQL and MySQL primary/replica pairs, failover under load | Every push |
| Business verifier | `agentim verify` after the suites | Log replay against state on every database the tests used | Every push |
| Rule coverage | `tests/rules.rs` | The catalog matches the code; every refusal is a rule; every rule is triggered by some test | Every push, after the other suites |
| Scale tests | `.github/workflows/scale.yml` | 20,000 connections with a reconnect storm, a 2,000-member group, slow consumers | Nightly |
| Client core | `sdk/client-rust/core/tests/` | Deterministic tests with fake clock, network and storage; random simulation over thousands of seeds; 100% line and branch coverage | Every SDK change |
| SDK conformance | `sdk/client-rust/native/tests/conformance.rs` and each wrapper's test | The same scenarios against a real server in Rust, C, Swift, Kotlin, C#, Dart, React Native, the browser | Every SDK change |
| Agent SDKs | `sdk/agent_e2e.sh`, `sdk/agent-node/e2e.mjs`, `sdk/agent-python/e2e.py` | Two real agents with a person: streaming, cards, forms, requests, state, hand-off, redelivery, restart | Every push, Linux and Windows |
| Server SDKs | `sdk/node/smoke.mjs`, `sdk/python/smoke.py`, `sdk/go/smoke` | Every endpoint against a real server | Every push |
| UI components | `sdk/react/test/`, `sdk/react-native-ui/test/` | Components, chat components, formats, contrast, an end-to-end run against a real server, screenshots compared with baselines | Every UI change |
| Console | `server/ci/console_smoke.mjs` | The console in a browser against a real server | Every push |
| Examples | `examples/test/e2e.mjs` | The three example apps in a browser | Every push |
| Docs site | `site/scripts/complete.mjs` | Every endpoint, message, error, rule and SDK method has a page | Every site change |
| Benchmarks | `server/bench/` | Throughput and latency with correctness checks | By hand; the numbers are in the README |

## 3. Running them

```bash
cd server
cargo fmt --check && cargo clippy --all-targets -- -D warnings
cargo test                                            # SQLite
AGENTIM_TEST_PG=postgres://user:pass@127.0.0.1 \
AGENTIM_TEST_MYSQL=mysql://user:pass@127.0.0.1 cargo test   # all three databases
AGENTIM_WRITE_RULES=1 cargo test --test rules         # regenerate docs/en/business-rules.md
bench/single-node.sh                                  # performance
```

- The PostgreSQL and MySQL URLs have no database name: each test creates its own database. CI fails if any test was skipped for lack of a database.
- Client SDK tests: `cd sdk/client-rust && cargo test`; the wrappers have `test.sh` scripts.
- UI tests: `cd sdk/react && npm test`.
- Examples: `cd examples && npm test`.

## 4. What the server tests check in every scenario

- Sequence numbers are contiguous for every member, and pushes arrive in order.
- Every message exists exactly once, however often it was retried.
- A refused request changed nothing: no event, no audit of the attempt, no side effect.
- After a restart, everything acknowledged is still there and the state is the same.
- In race tests, the invariant holds after every round: one winner, replay equals state, nothing after a terminal event.

## 5. Fault injection

The server has fail points (`failpoint.rs`, compiled into test builds) for:
- failed commits;
- paused batches;
- lost answers;
- database black holes and connection resets;
- clock jumps;
- frames lost between nodes.

The cluster suite uses them to prove the lease and fencing rules. Each guard has a matching "mutation" test that removes it and expects the failure.

## 6. Coverage by area

| Area | Feature points | Page |
|---|---|---|
| Messaging | MSG-01 … MSG-18 | [messaging](design/messaging.md#6-tests) |
| Sync and clients | SYN-01 … SYN-12, CLI-01 … CLI-11 | [sync-and-clients](design/sync-and-clients.md#6-tests) |
| Agents | AGT-01 … AGT-19 | [agents](design/agents.md#6-tests) |
| Approvals | APR-01 … APR-16 | [approvals](design/approvals.md#6-tests) |
| Built-in assistants | AST-01 … AST-15 | [assistants](design/assistants.md#6-tests) |
| Chat components | UI-01 … UI-14 | [chat-components](design/chat-components.md#6-tests) |
| Open API and webhooks | API-01 … API-15 | [open-api](design/open-api.md#6-tests) |
| Access | ACC-01 … ACC-17 | [access](design/access.md#6-tests) |
| Security and compliance | SEC-01 … SEC-17 | [security](design/security.md#6-tests) |
| Tracing and rules | TRC-01 … TRC-10 | [tracing](design/tracing.md#6-tests) |
| Cluster | CLU-01 … CLU-16 | [cluster](design/cluster.md#6-tests) |
| Multi-tenancy | TEN-01 … TEN-12 | [multi-tenancy](design/multi-tenancy.md#6-tests) |
| Developer experience | DX-01 … DX-14 | [developer-experience](design/developer-experience.md#6-tests) |
