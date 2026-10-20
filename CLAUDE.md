# AgentIM development rules

These rules come from the project owner and apply to every change.

## Before writing code
- Design first: think the design through, research how the industry does it, show the design (in `docs/`), and write code only after it is approved.
- When something is unclear or uncertain (a design choice, a protocol detail, how a product behaves), search how the industry does it (official docs of comparable products, mature open-source code and its issues) before deciding or answering; cite the sources and say what was not found instead of guessing.
- Every change must keep the IM core guarantees: stable, no message loss, ordered, correct business rules, high concurrency, cluster-ready (docs/01-product.md §3).
- Most users are overseas: compliance (GDPR, DSA, EU AI Act, App Store rules) is a requirement, not an add-on.

## Tests (mandatory for every business feature)
- **Cover every logic node the feature touches.** List every operation and code path that reads or depends on the feature (permissions, pushes, pulls, side channel, streams, drafts, agents, directory, audit, restart, multiple databases) and test each one, not only the feature's own operations.
- **Boundaries**: limits, empty and maximum values, unknown ids, repeated and idempotent calls, a user acting on themselves.
- **Error handling must follow the business meaning** and use consistent codes: `forbidden` (not allowed / not a member), `not_found` (unknown or gone), `conflict` (state does not allow it now), `bad_request` (invalid input or not applicable), `blocked`, `rate_limited`. Refused requests must leave no trace in the log.
- **Concurrency**: race the feature against everything it conflicts with, repeat many rounds, then check invariants (exactly one winner, log replay equals state, nothing after a terminal event).
- Run on SQLite, PostgreSQL and MySQL (`AGENTIM_TEST_PG`, `AGENTIM_TEST_MYSQL`), on Linux and Windows in CI. Prove that a test can fail (mutation check) when it guards a race.
- Document the coverage in docs/05-operations.md §5.

## Docs
- Update docs in place; delete what is wrong or useless instead of appending new versions.
- Pending decisions go to docs/06-plan.md §1.1 and are raised with the owner at the next opportunity.

## Commands
```bash
cd server
cargo fmt --check && cargo clippy --all-targets -- -D warnings
cargo test                                   # SQLite
AGENTIM_TEST_PG=postgres://user:pass@127.0.0.1 AGENTIM_TEST_MYSQL=mysql://user:pass@127.0.0.1 cargo test
bench/single-node.sh                         # performance (README numbers)
```
