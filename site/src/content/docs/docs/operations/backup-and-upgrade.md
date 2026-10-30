---
title: "Backups and upgrades"
description: "What to back up, how to restore, and how to upgrade a single server or a cluster."
sidebar:
  order: 8
---

## What to back up

| | |
|---|---|
| **The databases** | Every database in `[storage]`. Use your database's own tools: `pg_dump` or continuous archiving for PostgreSQL, `mysqldump` or `xtrabackup` for MySQL, `sqlite3 agentim.db ".backup backup.db"` for SQLite (safe while running) |
| **The master key** | `agentim.db.key`, `encryption.key_file` or the `AGENTIM_MASTER_KEY` secret. **Store it separately from the database backups**: either alone is useless, both together read everything |
| **Files** | The local file folder, or rely on your bucket's versioning and replication |
| **Configuration** | `agentim.toml` and your secrets |

Backups are encrypted like the database, because the content is. Deleting a conversation destroys its keys, so its content in old backups can't be decrypted either. Set a retention period for backups that matches your privacy policy: individual messages can't be deleted from a backup.

## Restoring

Restore the databases and the files, configure the same master key, and start. The server checks the key's fingerprint and refuses a wrong one. Devices that are ahead of the restored database are told to start over (`reset`) and sync again.

## Upgrading

- **One server:** stop, replace the binary (or the image), start. The schema migrates itself at start; a failed migration leaves the database unchanged.
- **A cluster:** upgrade one node at a time. A stopping node drains its shards to the others, clients reconnect elsewhere, and two adjacent versions work together. No request fails.
- **Clients:** apps keep working across server upgrades within a protocol version ([protocol versions](/docs/concepts/protocol-versions/)).

Read the release notes (`CHANGELOG.md`) for anything that needs an action.
