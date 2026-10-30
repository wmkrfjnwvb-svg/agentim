---
title: "Cluster"
description: "Several nodes on shared databases. Any node answers any client, a dead node is replaced in about 15 seconds, and planned changes cost no request."
sidebar:
  order: 6
---

A cluster is several `agentim` processes on the same PostgreSQL or MySQL databases. Conversations are split into shards. Each shard is written by the node holding its lease, any node answers any client and forwards to the right node, and every batch checks the lease in the same transaction (fencing), so a node that lost its lease can never commit.

| | |
|---|---|
| **High availability** | A node that crashes is replaced when its leases expire, in about 15–18 s. Clients reconnect to another node and sync. No acknowledged message is lost |
| **Planned changes** | Adding a node, draining one and rolling upgrades hand shards over without failing a request |
| **Scale** | Connections and fan-out spread over the nodes |
| **Not covered** | The databases' own high availability (use your provider's), multi-region active-active |

## Configuration

```toml
[cluster]
enabled = true
listen = "0.0.0.0:7946"           # node-to-node traffic
# node_id = "n1"                  # or AGENTIM_NODE_ID
# advertise = "10.0.0.11:7946"    # or AGENTIM_CLUSTER_ADVERTISE
```

`deploy/compose` (three nodes) and the Helm chart are ready-made clusters. Nodes must share the file storage too: S3, or a local folder with `files.shared = true`.

## Start checks

A node refuses to start, with the reason and the fix, if: the database is SQLite or a replica; durability settings are off; PostgreSQL is older than 14 or MySQL older than 8.0 or without GTID; the replication mode can't be determined; the cluster's shard count or timing doesn't match; or another live process uses its node id.

## Lifecycle

| | |
|---|---|
| Start | Checks, joins as `joining`, connects to the other nodes, takes its preferred shards, then `active` |
| Add a node | It becomes preferred for some shards, and the current holders hand them over, at most 4 per round per database |
| Graceful stop (SIGTERM) | `leaving`: each shard is drained (no new commands, queued ones finish), then handed to the next node. `/readyz` fails and clients are told to reconnect (close code 1012) |
| Drain without stopping | `agentim cluster drain --node n2` (or `agentim admin drain n2`): the node hands everything over and waits for you to stop it |
| Crash | Others take over after the lease expires |
| Status | `agentim cluster status`: nodes, heartbeats, shards per node, unowned shards |

**Rolling upgrades:** two adjacent versions can run side by side. Schema changes add first and remove a version later. A request whose result is unknown is never sent twice.
