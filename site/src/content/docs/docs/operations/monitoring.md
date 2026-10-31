---
title: "Monitoring"
description: "Health checks, Prometheus metrics, alert rules and logs."
sidebar:
  order: 7
---

## Health

| | |
|---|---|
| `GET /healthz` | The process is alive |
| `GET /readyz` | Every database is writable and no writer queue is above 90%. Returns each database's state; `503` when not ready, also while a node drains |

## Metrics

`GET /metrics` in Prometheus format. Protect it with `--metrics-token` (or `AGENTIM_METRICS_TOKEN`); scrapers then send `Authorization: Bearer …`. Labels never contain user or conversation ids.

| Area | Metrics |
|---|---|
| Connections | `agentim_ws_connections{kind}`, `agentim_ws_connects_total`, `agentim_ws_disconnects_total` |
| Requests | `agentim_requests_total{op,result}`, `agentim_refusals_total{op,code,rule}` (refusals by [business rule](/docs/reference/business-rules/)), `agentim_request_duration_seconds{op}`, `agentim_ack_latency_seconds`, `agentim_clients_total{protocol,client}` |
| Writes | `agentim_commit_batch_size`, `agentim_commit_duration_seconds{database}`, `agentim_write_queue_depth{database}`, `agentim_write_rejected_total{reason}`, `agentim_messages_committed_total{sender_type}`, `agentim_dedup_hits_total` |
| Pushes | `agentim_push_total`, `agentim_push_dropped_total`, `agentim_resync_total`, `agentim_stream_deltas_total` |
| Streams | `agentim_streams_active`, `agentim_streams_finished_total{state,reason}` |
| Agents | `agentim_guard_trips_total{reason}`, `agentim_guard_rejects_total{code}`, `agentim_inbound_flags_total{flag}`, `agentim_drafts_total{state}`, `agentim_approval_latency_seconds`, `agentim_takeovers_total{action}` |
| Integrations | `agentim_webhook_deliveries_total{webhook,result}`, `agentim_webhook_dropped_total{webhook}`, `agentim_before_send_total{result}`, `agentim_policy_blocks_total{hook}`, `agentim_login_failures_total` |
| Storage | `agentim_storage_healthy{database}`, `agentim_storage_errors_total{kind}`, `agentim_db_failovers_total`, `agentim_conversation_migrations_total{result}` |

## Alerts

`deploy/alerts.yml` has Prometheus rules for: database writes stopped, replicas behind, a database failover, a node not ready, shards without an owner, failing lease renewals, missing nodes and a full writer queue.

## Logs

Logs go to stderr; command output goes to stdout. `--log-format json` writes JSON lines for journald, Docker or your collector, and `RUST_LOG` sets the level (default `info`). Request lines carry `conn_id`, `user_id`, `op`, `rid`, `result` and `latency_ms`. **Logs never contain message content, tokens or whispers**, only ids and error codes. The audit log (who did what) is in the database and shown in the console.
