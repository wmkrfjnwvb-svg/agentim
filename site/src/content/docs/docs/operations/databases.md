---
title: "Databases"
description: "SQLite, PostgreSQL and MySQL, durability checks, replicas that never lose an acknowledged message, and splitting data by unit."
sidebar:
  order: 3
---

The same code and the same test suite run on all three databases.

| | Use for | Notes |
|---|---|---|
| **SQLite** | One machine, development, small deployments | Bundled; one file. Not for clusters |
| **PostgreSQL** 14+ | Production and clusters (recommended) | |
| **MySQL** 8.0+ or MariaDB | Production and clusters | GTID on for replicas and clusters |

```toml
[storage]
control = "main"              # holds users, units and the conversation directory

[[storage.databases]]
name = "main"
url = "postgres://agentim:…@db.internal/agentim"
read_pool = 8
```

The schema is created and migrated at start. Several processes starting together migrate only once. A database newer than the program (but compatible) is used as it is; an incompatible one is refused without changes.

## Durability is checked at start

The server refuses to start on a database that could lose acknowledged writes, and says how to fix it:

| | Required |
|---|---|
| PostgreSQL | `fsync = on`, `full_page_writes = on`; sessions use `synchronous_commit` of at least `local` |
| MySQL | `innodb_flush_log_at_trx_commit = 1`, `sync_binlog = 1`, `binlog_format = ROW` (clusters) |
| SQLite | WAL with `synchronous = FULL` |

`unsafe_durability = true` skips the check, for throwaway test databases only.

## How writes work

One writer per database. Commands that arrive together share one transaction and one `fsync` (group commit), each in its own savepoint, so one failed command never takes the others down or leaves a gap in `seq`. Clients are answered only after the commit. When the writer's queue is full the server answers `overloaded` instead of queueing without bound.

## Replicas

With replicas, a message is acknowledged only once the replicas have it, so a failover never loses an acknowledged message. The mode is detected at start:

| Mode | When | Acknowledged after |
|---|---|---|
| No replicas | None visible, or `mode = "none"` | The commit |
| Database-synchronous | PostgreSQL with `synchronous_standby_names`; MySQL semi-sync `AFTER_SYNC` | The commit (the database waits) |
| **Application wait** (default with async replicas) | Replicas visible, nothing synchronous configured | AgentIM waits until every replicating replica has the commit (`replicas_required` to wait for fewer) |
| Managed | Declared `mode = "managed_sync"` (RDS Multi-AZ, Cloud SQL HA) | The commit |

```toml
[storage.databases.replication]
mode = "auto"
wait_ms = 5000
# replicas_required = 1
# replica_urls = ["mysql://…@replica-1/agentim"]   # MySQL application wait only
```

If the replicas don't confirm within `wait_ms`, the client gets `unavailable` and retries with the same `client_msg_id`. The events are held back until a later batch is confirmed, and nothing is silently downgraded. Reads never go beyond what the replicas confirmed. PostgreSQL needs the `pg_monitor` role to see replica positions.

After a failover the server notices the new primary (`agentim_db_failovers_total`), refuses read-only servers, and answers the batch in flight as `unavailable`. The database address must follow the primary (a VIP, DNS or a proxy).

## Units: several databases

A deployment can keep some conversations in another database, for example a subsidiary whose data must stay in its own region:

```toml
[[storage.databases]]
name = "unit-b"
url = "postgres://…@db-eu.internal/agentim_b"

[[units]]
name = "subsidiary-b"
database = "unit-b"
```

Users belong to a unit (`unit` in `PUT /v1/users/{id}`). Changing a unit's database affects new conversations; move the existing ones with `agentim --config agentim.toml migrate-unit --unit subsidiary-b` while the server is stopped.
