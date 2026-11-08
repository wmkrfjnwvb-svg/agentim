---
title: "Encryption at rest"
description: "Envelope encryption with a key per conversation, the master key, rotation, and turning encryption on or off."
sidebar:
  order: 5
---

Encryption at rest is on by default. It costs about 2% of write throughput.

```text
master key (KEK)          from a file or the environment; never in the database
 └─ data keys (DEK)       one per conversation per 30 days, wrapped by the KEK
     └─ content           AES-256-GCM, random 96-bit nonce, bound to its table, column and row
```

**Encrypted**: message content and payloads, event snapshots, whispers and other side-channel content, drafts, audit details, the webhook outbox, report evidence. **Not encrypted**: ids, sequence numbers, times, kinds, membership and roles, conversation titles and profiles (needed for routing, sorting and permission checks).

Moving ciphertext to another row or column fails to decrypt. Content that can't be decrypted is an error, never returned as text.

## The master key

32 random bytes, base64. The server looks in order for `encryption.key`, `encryption.key_file`, and `AGENTIM_MASTER_KEY`. A single-SQLite deployment with none of them generates `<database>.key` next to the database (mode 0600) and tells you to back it up. Other deployments refuse to start without a key. The database records the key's fingerprint, so a wrong key is refused at start.

**Back the key up separately from the database.** Without it the content cannot be read. With it, a stolen backup can be.

## Operations

| | Command (server stopped) |
|---|---|
| Rotate the master key | `agentim encryption rotate-key --new-key-file new.key`: re-wraps the data keys only |
| Encrypt data written before encryption was on | `agentim encryption encrypt-existing` (repeatable, in batches) |
| Turn encryption off | `mode = "off"` (keep the key configured to read old content), then `agentim encryption decrypt-existing` |

Data keys rotate on their own every 30 days.

## Deleting is destroying the key

Dissolving a conversation deletes its data keys with its content. Copies of the ciphertext in backups can no longer be decrypted either.

## Not included

End-to-end encryption is not available: server-side features (moderation, approvals, search, the open API sending as users) need to read content. If it is added, it will use MLS (RFC 9420).
