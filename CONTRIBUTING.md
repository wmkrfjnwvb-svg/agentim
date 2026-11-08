# Contributing to AgentIM

Thank you for helping. This page tells you how the project is organised and what a change needs before it is merged. 中文说明见 [docs/zh/README.md](docs/zh/README.md)。

## Find your way

- **What the product does**: [docs/en/product.md](docs/en/product.md).
- **How it works inside**: [docs/en/architecture.md](docs/en/architecture.md).
- **One area in depth**: the design pages in [docs/en/design/](docs/README.md#detailed-design-by-area). Each lists numbered feature points (for example `MSG-04`), how they work, their rules and errors, and the tests that cover each one.
- **How we test**: [docs/en/testing.md](docs/en/testing.md).
- **Every business rule and its error code**: [docs/en/business-rules.md](docs/en/business-rules.md) (generated from the code).
- **For people building with AgentIM** (quick start, guides, API and SDK references): the docs site in [`site/`](site).

## Before you write code

1. **Open an issue first** for anything beyond a small fix. Describe the problem, what comparable products do (with links), and the design you propose. Larger changes update the area's design page in the same pull request, and the design is agreed before the code.
2. **Keep the guarantees.** No change may weaken them: no loss, ordered, correct business rules, stable under overload, cluster-ready ([product §3](docs/en/product.md#3-guarantees)).
3. **Compliance is a requirement.** Most users are in the EU and the US: GDPR, the DSA, the EU AI Act and the app store rules apply.

## What a pull request needs

- **Tests for every logic node the change touches.** That includes permissions, pushes, pulls, the side channel, streams, drafts, agents, restart, and the three databases. Add boundaries and refusals (a refused request must leave no trace), and race tests with a mutation check when the change guards a race. See [testing](docs/en/testing.md#1-rules-for-every-business-feature).
- **Error codes with their business meaning**: `forbidden`, `not_found`, `conflict`, `bad_request`, `blocked`, `rate_limited`. Every business refusal is a named rule (`rule!(…)`). Regenerate the catalog with `AGENTIM_WRITE_RULES=1 cargo test --test rules`.
- **Business enums are matched variant by variant**, never with `_` (clippy `wildcard_enum_match_arm`).
- **Docs in both languages.** Update the design page in `docs/en/design/` and its Chinese twin in `docs/zh/design/`. Use a feature-point ID for anything new, and add its tests to the table. Update the user docs in `site/` when behaviour visible to developers changes.
- **Changelog**: add a line under "Unreleased" in [CHANGELOG.md](CHANGELOG.md).

## Commands

```bash
cd server
cargo fmt --check && cargo clippy --all-targets -- -D warnings
cargo test                                                   # SQLite
AGENTIM_TEST_PG=postgres://user:pass@127.0.0.1 \
AGENTIM_TEST_MYSQL=mysql://user:pass@127.0.0.1 cargo test    # all three databases
```

Client SDKs: `cd sdk/client-rust && cargo test`. Components: `cd sdk/react && npm test`. Examples: `cd examples && npm test`. Docs site: `cd site && npm run build`.

## Writing style

Docs and messages are written in plain, short sentences, with one idea per sentence and concrete words. Prefer a table to a long paragraph. Say what is not built yet instead of leaving it out.

## Security

Do not open public issues for vulnerabilities. Follow [SECURITY.md](SECURITY.md).

## License

By contributing you agree that your contribution is licensed under the [Apache License 2.0](LICENSE).
