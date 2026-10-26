# Changelog

All notable changes to AgentIM are written here, in the format of
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow
[Semantic Versioning](https://semver.org/spec/v2.0.0.html). Before 1.0 a
minor version may break compatibility; the client protocol stays
compatible with the neighbouring versions (docs/11 §7).

Each change adds its line under "Unreleased" in the same commit. A release
renames that section to the version and date (docs/12 §1); the release
notes are taken from it.

## [Unreleased]

### Added

- One program (`agentim`) from a single node on SQLite to a cluster on
  PostgreSQL or MySQL: conversations, ordered and gap-free event logs,
  sync by device position, drafts and streams, read state, mentions,
  search, retention, webhooks and a hook before each send.
- Agents as first-class members: keys, an MCP server, requests for a
  decision and approvals, agent-to-agent messages, handover to a person
  and back, history service, emergency stop and destruction; Python and
  TypeScript agent SDKs.
- Developer entry points: client tokens your backend signs (JWT), the
  open API with server SDKs for Node, Python and Go, the admin API, the
  `agentim admin` command and the built-in console.
- Compliance basics: data export, account deletion, reports and a
  moderation queue, audit log, encryption at rest.
- Cluster: shard leases, planned hand-overs, graceful leave and drain,
  database replicas, Docker image, compose example and Helm chart.
- Client SDK: a Rust core shared by every platform, with the browser
  (wasm), React components, React Native, Swift, Kotlin/Android, C# and
  Unity (including WebGL) and Flutter wrappers.
- Releases: static builds for Linux (x86_64, aarch64), macOS (Intel and
  Apple silicon) and Windows; one-line install scripts that check the
  download's SHA-256; Sigstore signatures and build provenance; images
  for amd64 and arm64 with SBOM; the Helm chart as an OCI package.
- First run with no arguments: the database and its master key are
  created in the current folder, and start-up says where the key is and
  to back it up.
- UI components with one look on the web and in React Native
  (`@agentimhq/react`, `@agentimhq/react-native-ui`), from shared design
  tokens checked for WCAG AA contrast: runs per sender, day separators,
  a New messages line, jump to latest, search, editing and deleting
  your own messages, timeline notices. The demo at `/` is rebuilt on
  them and on the browser SDK, in English.
- A group created with many members writes one `members.joined` event
  per 200 members instead of one `member.joined` each (D128).

### Security

- Updated rustls (0.23.46), rustls-webpki (0.103.16), tokio (1.53.3),
  ctutils (0.4.4) and webpki-root-certs (1.0.10) for RUSTSEC-2026-0346,
  -0347, -0348, -0349, -0382, -0383, -0385 and -0386.
- Dependencies are checked for known vulnerabilities and licenses on
  every change, weekly and before each release.
- The generated master key file must be readable by its owner only;
  otherwise the server does not start.
