//! Prometheus metrics (see docs/05-operations.md §3). Labels never carry user
//! or conversation ids.

use std::sync::OnceLock;

use metrics_exporter_prometheus::{Matcher, PrometheusBuilder, PrometheusHandle};

static HANDLE: OnceLock<PrometheusHandle> = OnceLock::new();

const LATENCY_BUCKETS: &[f64] = &[
    0.0005, 0.001, 0.0025, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
];
const SIZE_BUCKETS: &[f64] = &[1.0, 2.0, 4.0, 8.0, 16.0, 32.0, 64.0, 128.0, 256.0, 512.0];

/// Installs the global recorder once per process and returns its handle.
pub fn init() -> &'static PrometheusHandle {
    HANDLE.get_or_init(|| {
        let recorder = PrometheusBuilder::new()
            .set_buckets_for_metric(Matcher::Suffix("_seconds".into()), LATENCY_BUCKETS)
            .and_then(|b| {
                b.set_buckets_for_metric(Matcher::Full(COMMIT_BATCH_SIZE.into()), SIZE_BUCKETS)
            })
            .expect("valid buckets")
            .build_recorder();
        let handle = recorder.handle();
        // A recorder may already be installed (e.g. several hubs in one test
        // process); the first one wins and keeps collecting.
        let _ = metrics::set_global_recorder(recorder);
        handle
    })
}

pub fn render() -> String {
    init().render()
}

pub const WS_CONNECTIONS: &str = "agentim_ws_connections";
pub const WS_CONNECTS: &str = "agentim_ws_connects_total";
pub const WS_DISCONNECTS: &str = "agentim_ws_disconnects_total";
pub const REQUESTS: &str = "agentim_requests_total";
/// Handshakes by negotiated protocol and client name (bounded label set).
pub const CLIENTS: &str = "agentim_clients_total";
pub const REQUEST_DURATION: &str = "agentim_request_duration_seconds";
pub const ACK_LATENCY: &str = "agentim_ack_latency_seconds";
pub const COMMIT_BATCH_SIZE: &str = "agentim_commit_batch_size";
pub const COMMIT_DURATION: &str = "agentim_commit_duration_seconds";
pub const WRITE_QUEUE_DEPTH: &str = "agentim_write_queue_depth";
pub const WRITE_REJECTED: &str = "agentim_write_rejected_total";
pub const MESSAGES_COMMITTED: &str = "agentim_messages_committed_total";
pub const EVENTS_COMMITTED: &str = "agentim_events_committed_total";
pub const DEDUP_HITS: &str = "agentim_dedup_hits_total";
pub const PUSH: &str = "agentim_push_total";
pub const PUSH_DROPPED: &str = "agentim_push_dropped_total";
pub const RESYNC: &str = "agentim_resync_total";
pub const STREAMS_ACTIVE: &str = "agentim_streams_active";
pub const STREAMS_FINISHED: &str = "agentim_streams_finished_total";
pub const STREAM_DELTAS: &str = "agentim_stream_deltas_total";
pub const GUARD_TRIPS: &str = "agentim_guard_trips_total";
pub const GUARD_REJECTS: &str = "agentim_guard_rejects_total";
pub const POLICY_BLOCKS: &str = "agentim_policy_blocks_total";
pub const INBOUND_FLAGS: &str = "agentim_inbound_flags_total";
pub const DRAFTS: &str = "agentim_drafts_total";
pub const APPROVAL_LATENCY: &str = "agentim_approval_latency_seconds";
pub const TAKEOVERS: &str = "agentim_takeovers_total";
pub const STORAGE_ERRORS: &str = "agentim_storage_errors_total";
pub const STORAGE_HEALTHY: &str = "agentim_storage_healthy";
pub const MIGRATIONS: &str = "agentim_conversation_migrations_total";
