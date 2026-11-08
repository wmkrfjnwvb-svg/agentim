# Cluster and database replicas

Several `agentim` nodes share the databases. Every conversation still has exactly one writer, a node failure loses no acknowledged message, and a database failover keeps every acknowledged message. There is no component besides the database: no ZooKeeper, etcd, Redis or message queue.

Code: `server/crates/agentim-cluster/` (`lib.rs` the node, `assign.rs` rendezvous hashing and leases, `peer.rs` connections between nodes, `codec.rs` frames, `ca.rs` the cluster's own certificates, `sql.rs`, `admin.rs`), `server/crates/agentim-core/src/shard.rs`, `cluster.rs` (the hooks in the core), `replication.rs`; deployment `deploy/compose`, `deploy/helm`, `deploy/alerts.yml`.

## 1. Purpose

| Solves | Does not solve |
|---|---|
| High availability: a node may crash, be upgraded or be cut off without losing, reordering or duplicating messages | The database's own high availability: that is the customer's database or managed service. AgentIM makes sure a database failover keeps acknowledged messages |
| Spreading connections over nodes | Active-active across regions |
| Spreading fan-out of large groups | The write limit of one database (see [architecture §5](../architecture.md#5-the-write-path) and several databases) |

## 2. Feature points

| ID | Feature | Behaviour |
|---|---|---|
| CLU-01 | Equal nodes | One program, the same role on every node. Nodes find each other in the database's node table. A node id taken by a live process is refused at start |
| CLU-02 | Shards and leases | Each database's conversations map to V virtual shards (`hash(conv) mod V`, V = 256, fixed at init). There is also a control shard (users, tokens, agents, keys, the directory) and a maintenance shard. A shard is written only by the node holding its lease in the database |
| CLU-03 | Fencing in every batch | The last statement of every write batch checks the lease's epoch and holder for each shard in the batch, in the same transaction. A stale holder commits nothing |
| CLU-04 | Forwarding | Any node accepts any client. Reads run where they arrive; writes go to the shard's holder. While a shard moves, requests follow for up to 4 s. An uncertain outcome is `unavailable`, never "failed", and is never resent: the client retries with the same id |
| CLU-05 | Failover | A crashed holder's shards are taken after the lease expires (15 s). The holder stops writing on its own after 10 s without a renewal |
| CLU-06 | Planned moves | Joining, leaving, draining and rolling upgrades hand shards over at once, after draining the writer, with no failed request. Shards rebalance to their preferred node (rendezvous hashing) a few at a time |
| CLU-07 | Notifications | After a commit, the holder publishes events to every other node, in seq order per conversation. A lost notification only delays: clients find gaps by seq and by the `heads` position check |
| CLU-08 | Invalidation | Blocks, revoked tokens and keys, deactivations, stopped and destroyed agents, signing keys, dye rules and settings reach every node at once. Nodes cut off from the rest poll the database and apply them within 3 s |
| CLU-09 | Node links | One TLS connection from each node to each other node, with the cluster's own CA (kept encrypted in the database). A handshake checks the cluster, the node and versions. Heartbeats every 2 s; bounded queues |
| CLU-10 | Database clock | All lease times use the database's clock in SQL; local clocks only time the node's own self-stop |
| CLU-11 | Start-up checks | Bad setups are refused: timing rules, files without a shared store, a node id in use, SQLite in a cluster |
| CLU-12 | Database replicas | Three modes detected at start: none, synchronous in the database, or the application waits for asynchronous replicas. `managed_sync` can be declared for managed services. A write is acknowledged only once the replicas have it |
| CLU-13 | Read watermark | When the application waits for replicas, reads return only up to `confirmed_seq`, so nobody sees a message that a failover could lose |
| CLU-14 | Failover of the database | The primary's identity is recorded; read-only errors and dropped connections end the batch as uncertain; the writer reconnects and checks it is on a writable primary; changes are counted |
| CLU-15 | Operations | `agentim cluster status`, `agentim cluster drain --node`, health and readiness per database, Prometheus metrics and alerts, a three-node Compose file and a Helm chart |
| CLU-16 | Mixed versions | Capabilities are negotiated per link; frames and commands a node does not know are refused as `unsupported_op`, so rolling upgrades work |

## 3. Design

### 3.1 Correctness rules

| # | Rule | Prevents |
|---|---|---|
| 1 | Acknowledge only after the write is committed (and replicated, §3.6), then push | Loss of acknowledged messages on any crash |
| 2 | One writer per conversation at any time; seq allocated in the transaction | Disorder and duplicate seqs |
| 3 | Every batch checks the lease epoch in the same transaction | A paused old holder waking up and writing |
| 4 | Idempotency lives in the database (`client_msg_id` unique) | Duplicates when an outcome is uncertain |
| 5 | An uncertain write is treated as "maybe committed" | Duplicates or loss from wrongly assuming failure |
| 6 | Nodes exchange only requests and notifications, never data | Network trouble between nodes cannot break correctness |
| 7 | Reads return only what is confirmed replicated | A message seen, then gone after a database failover |
| 8 | `(conv, seq)` is unique as a second line of defence | Duplicate seqs even if the shard logic had a bug |

### 3.2 Leases

```sql
cluster_meta(k PK, v)
cluster_nodes(node_id PK, boot_id, addr, slot UNIQUE, version, state, heartbeat_at)
shard_lease(db, shard_id, PK(db, shard_id), owner /* node_id:boot_id */, epoch, expires_at, fence_count)
```

| Operation | How |
|---|---|
| Take | `UPDATE … SET owner, epoch=epoch+1, expires_at=db_now+D WHERE … AND (owner IS NULL OR expires_at < db_now)`. Exactly one row means success |
| Renew | One statement renews every held shard (`expires_at > db_now`). A shard missing from the result stops at once |
| Hand over | After draining: `UPDATE … SET owner=target, epoch=epoch+1 … WHERE owner=me AND epoch=current`, then a `CHECK` frame tells the target, which can write immediately |
| Release | On leaving: `owner=NULL`, so others take it at once |
| Fence (every batch) | `UPDATE shard_lease SET fence_count=fence_count+1 WHERE db=? AND (shard_id, owner, epoch) IN (…)` as the last statement before commit. The row count must equal the shards in the batch, or the batch rolls back and those shards stop |

The fence holds the lease rows' locks until commit, so a taker waits for it. A commit by the old holder and a take by the new one are always ordered, and two epochs can never both commit. The epoch is compared for equality, so a lease table rolled back by a failover makes the node stop and take the shard again.

| Timing | Default | Constraint |
|---|---|---|
| Lease D | 15 s | Worst takeover ≈ D + 3 s |
| Renew interval R | 3 s | Several retries before expiry |
| Database call timeout T | 2 s | Every call |
| Self-stop F | 10 s from before the renewal request (monotonic clock) | `R + 2T ≤ F < D`, `D / F ≥ 1.5`; checked at start |
| Lock wait | 2 s (`lock_timeout`, `innodb_lock_wait_timeout`) | |

A **progress gate** stops a writer that is stuck (not committing) from keeping its shards.

### 3.3 Routing

| Request | Runs on |
|---|---|
| Reads (sync, history, search, context, lists, unread) | The node that received it (within the watermark) |
| Writes to a conversation | Its shard's holder |
| Writes to the control database | The control shard's holder |
| Operations across shards (create, dissolve, destroy) | Each step on the right holder; repair logic is the same as on one node |

Refusals keep their code and rule through forwarding and leave no trace.

### 3.4 Links between nodes

```
frame header (16 bytes): "AIMB" · body length (u32 BE) · codec version · message type · flags · reserved
body: JSON (new fields optional, unknown fields ignored)
HELLO · FORWARD_REQ · FORWARD_RESP (ok | error code | not_owner | unavailable | unsupported_op)
PUBLISH (events and invalidations) · CHECK · PING/PONG
```

| Aspect | Rule |
|---|---|
| Liveness | PING/PONG every 2 s; 10 s silence closes the link |
| Reconnect | Backoff from 100 ms to 5 s with jitter |
| Send queue | 100,000 frames per link. When full, the link closes and the peer's clients resync. The writer only enqueues and never waits for a peer |
| Frame limit | 8 MB |
| Restarted peer | A new boot id: its pending forwards are uncertain and its late frames are dropped |

### 3.5 Lifecycle

- **Joining**: a new node gets shards only after it has been seen for two renew intervals. Then up to 10 takes and 4 hand-overs per round per database.
- **Draining** (`agentim cluster drain`, or a shutdown signal):
  1. Stop taking new clients.
  2. Hand over every shard.
  3. Tell clients `goaway` with a random wait.
  4. Exit.
- **Crash**: the other nodes take the shards when the leases expire. The preferred node takes first; the others wait one interval longer.

### 3.6 Replicas

| Mode | Detection | When a commit counts as replicated |
|---|---|---|
| None | No replicas seen, or `mode = "none"` | At commit |
| Synchronous in the database | PostgreSQL `synchronous_standby_names` with `synchronous_commit ≥ on`; MySQL semi-sync `AFTER_SYNC` | At commit. MySQL is checked after each batch, and falling back to async holds results |
| **Application waits** (the default with async replicas) | Replicas seen without a synchronous setup | When every replicating replica (or `replicas_required`) has flushed the commit's LSN or GTID |
| Managed (declared) | `mode = "managed_sync"` (RDS Multi-AZ, Cloud SQL HA, Azure) | At commit; the declaration is audited |

- **Zero replicas when they are expected is "not satisfied"**, not "nothing to wait for". So an isolated old primary can never acknowledge.
- **Timeout** (`wait_ms`, 5 s): the batch stays committed but unconfirmed. Its events, pushes and wake-ups are held, and the client gets `unavailable`. A retry with the same id finds the message and waits for confirmation. The server never degrades silently.
- **Lease renewals** do not wait for replicas, so slow replicas cannot cost a node its leases.

## 4. Interfaces

- Configuration:
  - `[cluster]`: `node_id`, `addr`, the cluster key, shards at init, timing;
  - `[storage.databases.replication]`: `mode`, `wait_ms`, `replicas_required`, `replica_urls`.
- CLI: `agentim cluster status`, `agentim cluster drain --node <id>`.
- Admin API: `/admin/v1/deploy/cluster` (nodes, drain), `/admin/v1/deploy/status` (databases and replica modes).
- Health: `/healthz`, `/readyz` (per database).
- Metrics:
  - `agentim_cluster_*` (holders, forwards, exhausted follows);
  - `agentim_db_failovers_total`, replication waits.

## 5. Rules and errors

- `unavailable` means the outcome is uncertain; retry with the same id.
- `not_owner` stays inside the cluster.
- `unsupported_op` comes from an older peer.
- Every business error is the same as on one node.

## 6. Tests

`server/crates/agentim-cluster/tests/`; several nodes in one process on PostgreSQL and MySQL; faults injected with fail points.

| Feature | Tests |
|---|---|
| CLU-01, CLU-04, CLU-07 | `cluster.rs`: `requests_are_forwarded_and_pushes_reach_every_node`, `refusals_are_the_same_through_forwarding`, `one_message_per_client_msg_id_across_nodes`; `operations.rs`: `held_and_delayed_notifications_arrive_in_order`, `heads_from_any_node_show_where_conversations_stand` |
| CLU-02, CLU-03, CLU-05 | `cluster.rs`: `a_crashed_holder_is_replaced_without_loss_or_duplicates`, `a_stale_holder_commits_nothing`; `faults.rs`: `a_crash_under_concurrent_traffic_loses_nothing`, `a_batch_paused_before_its_check_leaves_no_trace`, `a_clock_jump_never_extends_holding`, `a_holder_in_a_database_blackhole_stops_writing`, `a_holder_whose_database_connections_reset_stops_writing`, `cut_off_from_the_database`, `nodes_cut_off_from_each_other_keep_one_holder`, `every_node_restarts`, `an_answer_lost_after_commit_is_found_by_the_retry`, `frames_lost_one_way_give_a_retryable_answer`; mutation checks: `mutation_without_the_check_a_stale_holder_writes`, `mutation_without_the_progress_gate_a_stuck_writer_keeps_its_shards` |
| CLU-06 | `cluster.rs`: `a_joining_node_gets_its_share_while_traffic_flows`, `leaving_hands_shards_over_at_once`; `operations.rs`: `nodes_joining_under_traffic_cost_no_request`, `a_node_leaving_under_traffic_costs_no_request`, `an_operator_drains_a_node_under_traffic`, `failing_renewals_stop_a_node_taking_clients` |
| CLU-08 | `cluster.rs`: `revocations_and_blocks_reach_every_node`, `signed_tokens_and_logouts_reach_every_node`, `dye_rules_reach_every_node_and_follow_the_request`; `faults.rs`: `revocations_reach_cut_off_nodes_within_3s`; `operations.rs`: `api_keys_created_or_revoked_on_one_node_change_at_once_on_the_others`, `mutation_without_the_cache_poll_a_cut_off_node_keeps_a_revoked_token` |
| CLU-11 | `cluster.rs`: `start_up_refuses_bad_setups`; `operations.rs`: `files_need_a_shared_store_in_a_cluster` |
| CLU-12–CLU-14 | `server/crates/agentim-core/tests/replicas.rs`: `postgres_replicas_keep_acknowledged_writes`, `mysql_replicas_keep_acknowledged_writes` (real primary/replica pairs, failover in CI); `.github/workflows/server.yml` "acknowledged writes survive a failover" |
| CLU-16 | `operations.rs`: `commands_this_version_does_not_know_are_unsupported`, `outputs_of_a_newer_node_are_skipped_one_by_one` |
| Tracing | `cluster.rs`: `the_trace_crosses_nodes` |
| Load | `server/bench/cluster.sh` (one and three nodes; numbers in the README) |

## 7. Limits and next steps

- Not built:
  - detection and repair when the defences are bypassed: a database rolled back after a failover the replicas missed;
  - manual moves of single shards (automatic rebalancing covers adding and draining nodes).
- Cluster numbers so far come from one machine. Tests on separate machines are next.
