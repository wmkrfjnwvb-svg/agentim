//! Optional span export over OTLP (docs/en/design/tracing.md, `[tracing] otlp_endpoint`).
//!
//! Spans carry the request's own trace id (`trace::current`), so what a
//! tracing backend shows matches the ids in the logs and in the message
//! rows. A trace is exported when its caller asked for it (a trusted
//! caller's sampled flag), when a dye rule matched it, or when its trace id
//! falls under `sample_ratio`: decided from the trace, so every node and
//! every span of a trace decide the same.

use opentelemetry::trace::{
    Link, SpanContext, SpanId, SpanKind, TraceContextExt, TraceFlags, TraceId, TraceState,
    TracerProvider as _,
};
use opentelemetry::{Context, KeyValue};
use opentelemetry_sdk::trace::{
    IdGenerator, RandomIdGenerator, SamplingDecision, SamplingResult, SdkTracerProvider,
    ShouldSample,
};

use crate::config::TracingConfig;
use crate::trace::Trace;

/// Whether the trace is exported (sampled by its caller, dyed, or under the
/// ratio by its id).
pub fn keep(t: &Trace, ratio: f64) -> bool {
    t.sampled || t.dye.is_some() || under_ratio(&t.trace_id, ratio)
}

/// The trace id's low 64 bits against the ratio, as OpenTelemetry's
/// TraceIdRatioBased does: the same answer on every node.
fn under_ratio(trace_id: &str, ratio: f64) -> bool {
    if ratio >= 1.0 {
        return true;
    }
    if ratio <= 0.0 || trace_id.len() != 32 {
        return false;
    }
    let low = u64::from_str_radix(&trace_id[16..], 16).unwrap_or(u64::MAX) >> 1;
    low < (ratio * (1u64 << 63) as f64) as u64
}

#[derive(Debug, Clone)]
struct Sampler {
    ratio: f64,
}

impl ShouldSample for Sampler {
    fn should_sample(
        &self,
        parent: Option<&Context>,
        _trace_id: TraceId,
        _name: &str,
        _kind: &SpanKind,
        _attributes: &[KeyValue],
        _links: &[Link],
    ) -> SamplingResult {
        let parent_sampled = parent.is_some_and(|c| c.span().span_context().is_sampled());
        let keep = parent_sampled || crate::trace::current().is_some_and(|t| keep(&t, self.ratio));
        SamplingResult {
            decision: if keep {
                SamplingDecision::RecordAndSample
            } else {
                SamplingDecision::Drop
            },
            attributes: vec![],
            trace_state: TraceState::default(),
        }
    }
}

/// New spans of a request take its trace id.
#[derive(Debug)]
struct Ids(RandomIdGenerator);

impl IdGenerator for Ids {
    fn new_trace_id(&self) -> TraceId {
        crate::trace::current()
            .and_then(|t| TraceId::from_hex(&t.trace_id).ok())
            .unwrap_or_else(|| self.0.new_trace_id())
    }
    fn new_span_id(&self) -> SpanId {
        self.0.new_span_id()
    }
}

static ENABLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Whether spans are exported (request spans are made only then).
pub fn enabled() -> bool {
    ENABLED.load(std::sync::atomic::Ordering::Relaxed)
}

/// The layer that exports spans.
pub type Layer<S> =
    tracing_opentelemetry::OpenTelemetryLayer<S, opentelemetry_sdk::trace::SdkTracer>;

/// The export layer and its provider (kept to flush at shutdown), when
/// `otlp_endpoint` is set.
pub fn layer<S>(cfg: &TracingConfig) -> Result<Option<(Layer<S>, SdkTracerProvider)>, String>
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
{
    let Some(endpoint) = &cfg.otlp_endpoint else {
        return Ok(None);
    };
    use opentelemetry_otlp::WithExportConfig;
    // reqwest is built without a default TLS provider (as for webhooks).
    let _ = rustls::crypto::ring::default_provider().install_default();
    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_http()
        .with_endpoint(endpoint)
        .build()
        .map_err(|e| format!("tracing.otlp_endpoint: {e}"))?;
    let provider = SdkTracerProvider::builder()
        .with_batch_exporter(exporter)
        .with_sampler(Sampler {
            ratio: cfg.sample_ratio,
        })
        .with_id_generator(Ids(RandomIdGenerator::default()))
        .with_resource(
            opentelemetry_sdk::Resource::builder()
                .with_service_name(cfg.service_name.clone())
                .build(),
        )
        .build();
    let tracer = provider.tracer("agentim");
    ENABLED.store(true, std::sync::atomic::Ordering::Relaxed);
    Ok(Some((
        tracing_opentelemetry::layer().with_tracer(tracer),
        provider,
    )))
}

/// Joins a new span to its caller's span (a trusted caller that sent a
/// `traceparent`), so the backend draws them as one tree. No-op when export
/// is off.
pub fn set_parent(span: &tracing::Span, t: &Trace) {
    use tracing_opentelemetry::OpenTelemetrySpanExt;
    let Some(parent) = &t.parent else { return };
    let (Ok(trace_id), Ok(span_id)) = (TraceId::from_hex(&t.trace_id), SpanId::from_hex(parent))
    else {
        return;
    };
    let flags = if t.sampled {
        TraceFlags::SAMPLED
    } else {
        TraceFlags::default()
    };
    let cx = Context::new().with_remote_span_context(SpanContext::new(
        trace_id,
        span_id,
        flags,
        true,
        TraceState::default(),
    ));
    let _ = span.set_parent(cx);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ratio_decides_by_trace_id_alone() {
        let mut t = Trace::new();
        t.trace_id = "0".repeat(16) + "0000000000000001";
        assert!(!keep(&t, 0.0));
        assert!(keep(&t, 0.5), "a low id is under any ratio");
        assert!(keep(&t, 1.0));
        t.trace_id = "0".repeat(16) + "ffffffffffffffff";
        assert!(!keep(&t, 0.5));
        assert!(!keep(&t, 0.999));
        assert!(keep(&t, 1.0));
        // Sampled by the caller or dyed: always.
        t.sampled = true;
        assert!(keep(&t, 0.0));
        t.sampled = false;
        t.dye = Some("dye_x".into());
        assert!(keep(&t, 0.0));
        // About the ratio over many ids.
        let n = (0..10_000).filter(|_| keep(&Trace::new(), 0.25)).count();
        assert!((2_000..3_000).contains(&n), "{n}");
    }
}
