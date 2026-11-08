# Multi-tenancy

One deployment can serve many independent projects (tenants). Each tenant has its own database and its own AgentIM instance in the same process: its own address, users, keys, settings, webhooks and limits. The hosted cloud uses this; a self-hosted deployment without `[tenancy]` is one tenant, exactly as before.

Code: `server/crates/agentim-core/src/tenant.rs`, `hub/silos.rs` (instances per tenant), `hub/tenants.rs` (the deployment's tenant API), `hub/tenant_webhooks.rs`, `server.rs` (routing by host).

## 1. Purpose

Hosted projects must be isolated from each other as strongly as separate deployments, without a separate process per project and without features only one database offers (such as PostgreSQL row-level security). The design gives each tenant its own database: the simplest model and the strongest isolation. It works on all three databases, and moving a tenant later is moving a database.

The common patterns are silo (storage per tenant: strongest isolation, highest cost), pool (shared tables: cheapest, weakest) and bridge (a mix by plan); see the AWS SaaS Lens and the Azure SQL multi-tenant patterns. AgentIM uses silo.

## 2. Feature points

| ID | Feature | Behaviour |
|---|---|---|
| TEN-01 | A tenant is a full instance | Its own `Hub`, databases, writers, caches, signing keys, API keys, webhooks and limits. Inside, everything is the normal single-tenant code path |
| TEN-02 | A database per tenant | `[[tenancy.clusters]] name, url`, where `{tenant}` in the URL becomes the tenant id: a file per tenant on SQLite, a database per tenant on PostgreSQL and MySQL. Free and paid tenants can live on different database clusters |
| TEN-03 | Routing by host | `<tenant>.<host_suffix>` goes to the tenant's instance. Other hosts are the deployment (its admin API and console). Unknown and suspended tenants are 404 |
| TEN-04 | Keys only on their tenant | A key or token works only on its tenant's host |
| TEN-05 | Opening instances | On first use (concurrent first requests open one instance) and at start for every active tenant |
| TEN-06 | Provisioning | `POST /admin/v1/deploy/tenants {id, name, limits, cluster?}` creates the database first (directory, or `CREATE DATABASE`), then the tenant row. An unknown cluster leaves nothing. Keys and signing keys are created per tenant |
| TEN-07 | Limits in the instance | `connections`, `mau` (new people per month; first logins beyond it are refused), `requests_per_second` (WebSocket and open API together, burst ×2), and hosted AI credit. Changes apply at once |
| TEN-08 | Usage | `GET …/tenants/{id}/usage?day=`: monthly distinct people (agents excluded), the day's messages and agent messages, file storage, current connections |
| TEN-09 | Suspend and resume | Suspending closes the instance (connections, writers, pools) and makes the host 404. The data stays |
| TEN-10 | Delete | Closes the instance and drops its database and local files; S3 files under `tenants/<id>/` expire by a bucket lifecycle rule |
| TEN-11 | Tenant webhooks | Stored in the tenant's database with encrypted secrets, at most 10 per tenant. Each webhook has its own delivery task and cursor, so one failing receiver slows only its own tenant |
| TEN-12 | Webhook safety | `https` only. Private, loopback, link-local and similar addresses are refused when set and checked again on every delivery after DNS. A webhook failing for 5 days, or answered with `webhook-delivery: disable`, is turned off |

## 3. Design

- The deployment's own control database holds only `tenants` (id, name, limits, suspended, cluster) and the deployment's admins, keys and audit. It holds no tenant data.
- `[tenancy]` requires at least one `[[tenancy.clusters]]`, or the server does not start.
- Each open tenant costs one write connection and a small read pool (2 by default). With thousands of active tenants on PostgreSQL, put PgBouncer in front. SQLite costs a file per tenant.
- A multi-tenant deployment runs on one node today. Spreading tenants over nodes (each tenant open on one node, others forwarding) is the next step. Self-hosted clusters are unchanged.

## 4. Interfaces

- Deployment admin API (`owner`, `operator`):
  - `/admin/v1/deploy/tenants` and `/admin/v1/deploy/tenants/{id}`: create, change, suspend, delete;
  - `…/{id}/usage`, `…/{id}/api-keys`, `…/{id}/signing-keys`, `…/{id}/webhooks/{name}`.
- Configuration: `[tenancy] host_suffix, webhooks_per_tenant, webhook_disable_after_ms`, `[[tenancy.clusters]]`.

## 5. Rules and errors

Rule prefixes: `tenant.*` (ids, names, clusters, limits, `tenant.mau`, `tenant.rate`, `tenant.day`) and `webhook.*` (URL, count) ([catalog](../business-rules.md)). A refused change leaves no trace.

## 6. Tests

Paths under `server/crates/agentim-core/tests/`; three databases.

| Feature | Tests |
|---|---|
| TEN-01–TEN-05, TEN-09, TEN-10 | `silos.rs`: `each_tenant_is_its_own_instance`, `keys_work_only_on_their_tenants_host`, `race_first_uses_open_one_instance`, `suspension_closes_the_instance`, `tenants_survive_a_restart`, `deleting_a_tenant_drops_its_database` |
| TEN-06, TEN-08 | `tenancy.rs`: `the_deployment_api_provisions_tenants`, `tenant_ids_names_and_changes_are_checked`, `a_single_tenant_server_has_no_tenants_api` |
| TEN-07 | `silos.rs`: `plan_limits_hold_in_the_tenants_instance`; `assistants.rs`: `hosted_projects_spend_their_ai_credit` |
| TEN-11, TEN-12 | `silos.rs`: `webhooks_live_in_the_tenants_database`; `tenancy.rs`: `the_deployment_api_manages_tenant_webhooks`; `tenant_webhooks.rs`: all 10 tests (only own events, a failing receiver delays only its tenant, starts at the end, suspended tenants get nothing, turned off after failing too long or by the receiver, private addresses refused, the last places under races, refusals leave no trace, the outbox cleaned per tenant) |

## 7. Limits and next steps

- Not built:
  - tenant-scoped admin API and console (a per-project back office);
  - moving a tenant's database to another cluster when its plan changes;
  - several nodes for a multi-tenant deployment;
  - closing instances that have been idle for a long time.
