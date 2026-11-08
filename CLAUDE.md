# AgentIM development rules

These rules come from the project owner and apply to every change.

## Before writing code
- Design first: think the design through, research how the industry does it, show the design (in `docs/`), and write code only after it is approved.
- When something is unclear or uncertain (a design choice, a protocol detail, how a product behaves), search how the industry does it (official docs of comparable products, mature open-source code and its issues) before deciding or answering; cite the sources and say what was not found instead of guessing.
- Every change must keep the IM core guarantees: stable, no message loss, ordered, correct business rules, high concurrency, cluster-ready ([docs/en/product.md §3](docs/en/product.md#3-guarantees)).
- Most users are overseas: compliance (GDPR, DSA, EU AI Act, App Store rules) is a requirement, not an add-on.

## Tests (mandatory for every business feature)
- **Cover every logic node the feature touches.** List every operation and code path that reads or depends on the feature (permissions, pushes, pulls, side channel, streams, drafts, agents, directory, audit, restart, multiple databases) and test each one, not only the feature's own operations.
- **Boundaries**: limits, empty and maximum values, unknown ids, repeated and idempotent calls, a user acting on themselves.
- **Error handling must follow the business meaning** and use consistent codes: `forbidden` (not allowed / not a member), `not_found` (unknown or gone), `conflict` (state does not allow it now), `bad_request` (invalid input or not applicable), `blocked`, `rate_limited`. Refused requests must leave no trace in the log.
- **Concurrency**: race the feature against everything it conflicts with, repeat many rounds, then check invariants (exactly one winner, log replay equals state, nothing after a terminal event).
- Run on SQLite, PostgreSQL and MySQL (`AGENTIM_TEST_PG`, `AGENTIM_TEST_MYSQL`), on Linux and Windows in CI. Prove that a test can fail (mutation check) when it guards a race.
- Document the coverage in the "Tests" table of the area's design page (`docs/en/design/*.md` and its Chinese twin in `docs/zh/design/`): every feature point maps to the tests that cover it. See [docs/en/testing.md](docs/en/testing.md).

## Code
- **Business enums are matched variant by variant, never with `_`** (clippy `wildcard_enum_match_arm`, on for the whole `server/` workspace): a new state, role, kind or operation must be handled everywhere it matters. Exceptions (an external crate's enum such as `serde_json::Value`, protocol values that must tolerate unknowns) take `#[allow(clippy::wildcard_enum_match_arm)]` with a one-line reason.

## Docs
- Docs are in `docs/`: English (`docs/en/`) leads, Chinese (`docs/zh/`) mirrors it with the same structure and feature-point IDs. Update both in the same change.
- Every area has a design page: purpose, numbered feature points, design, interfaces, rules and errors, tests, limits. A new feature gets a new feature-point ID and its tests in the table.
- Update docs in place; delete what is wrong or useless instead of appending new versions.
- Open design questions are raised with the owner before code is written; once decided, the design page states the decision.

## Commands
```bash
cd server
cargo fmt --check && cargo clippy --all-targets -- -D warnings
cargo test                                   # SQLite
AGENTIM_TEST_PG=postgres://user:pass@127.0.0.1 AGENTIM_TEST_MYSQL=mysql://user:pass@127.0.0.1 cargo test
bench/single-node.sh                         # performance (README numbers)
```
