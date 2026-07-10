//! Yagra-telemetry — process-wide observability init (structured logs + OpenTelemetry traces).
//!
//! Every Yagra binary calls [`init`] once at startup to install a `tracing` subscriber that
//! writes structured logs to stdout AND, when an OTLP endpoint is configured, exports spans for
//! distributed tracing (self-observability; requirements §5 / monitoring-conventions).
//!
//! **Trace export is opt-in.** Without [`ENDPOINT_ENV`] (or the OpenTelemetry-standard
//! `OTEL_EXPORTER_OTLP_ENDPOINT`) the OTel layer is absent and the binary only logs — so the
//! single-node MVP needs no collector and the poll hot path carries zero tracing overhead. When
//! enabled, spans cross the core⇄poller bus seam via W3C trace-context propagation
//! ([`current_trace_context`] / [`set_span_parent`]) so one poll is a single distributed trace.
//!
//! The carrier is an opaque `HashMap<String, String>` (the W3C `traceparent`/`tracestate` bag)
//! so the message-contract crate (`yagra-bus`) needs no OpenTelemetry dependency, and the field
//! serializes to nothing when empty — N/N-1 peers simply ignore it (ADR-017).

use std::collections::HashMap;

use opentelemetry::global;
use opentelemetry::trace::TracerProvider as _;
use opentelemetry_otlp::WithExportConfig as _;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;

/// Turns on OTLP span export and points at the collector's OTLP/HTTP endpoint
/// (e.g. `http://otel-collector:4318`). Unset ⇒ tracing export disabled (logs only).
pub const ENDPOINT_ENV: &str = "YAGRA_OTEL_ENDPOINT";
/// OpenTelemetry-standard fallback for [`ENDPOINT_ENV`].
pub const STD_ENDPOINT_ENV: &str = "OTEL_EXPORTER_OTLP_ENDPOINT";

/// Flushes and stops the OTLP span pipeline on drop so the last spans aren't lost at shutdown.
/// Holds the tracer provider; `None` in logs-only mode (export disabled). Keep it alive for the
/// process lifetime — dropping it early tears down span export.
#[must_use = "hold the guard until shutdown; dropping it flushes and stops span export"]
pub struct TelemetryGuard {
    provider: Option<opentelemetry_sdk::trace::TracerProvider>,
}

impl Drop for TelemetryGuard {
    fn drop(&mut self) {
        if let Some(provider) = self.provider.take() {
            // Best-effort: flush pending spans, then shut the pipeline down. We're exiting, so
            // errors here are logged at most — never fatal.
            let _ = provider.shutdown();
        }
    }
}

/// Install the process-wide `tracing` subscriber for `service_name` (e.g. `"yagra-core"`).
///
/// Always installs a structured stdout log layer filtered by `RUST_LOG` (default `info`). If an
/// OTLP endpoint is configured it additionally installs an OpenTelemetry layer that batch-exports
/// spans over OTLP/HTTP and registers the W3C trace-context propagator. Call once per process; the
/// returned [`TelemetryGuard`] must outlive all traced work.
pub fn init(service_name: &str) -> TelemetryGuard {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let fmt_layer = tracing_subscriber::fmt::layer();

    let endpoint = std::env::var(ENDPOINT_ENV)
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| {
            std::env::var(STD_ENDPOINT_ENV)
                .ok()
                .filter(|s| !s.is_empty())
        });

    let provider = endpoint.and_then(|endpoint| build_provider(service_name, &endpoint));

    let otel_layer = provider.as_ref().map(|provider| {
        // Register the W3C propagator only when export is on, so injection is a no-op otherwise.
        global::set_text_map_propagator(
            opentelemetry_sdk::propagation::TraceContextPropagator::new(),
        );
        let tracer = provider.tracer(service_name.to_owned());
        tracing_opentelemetry::layer().with_tracer(tracer)
    });

    tracing_subscriber::registry()
        .with(filter)
        .with(fmt_layer)
        .with(otel_layer)
        .init();

    match &provider {
        Some(_) => tracing::info!(
            service = service_name,
            "OpenTelemetry span export enabled (OTLP/HTTP)"
        ),
        None => tracing::debug!(
            service = service_name,
            "tracing export disabled (no OTLP endpoint); logs only"
        ),
    }
    TelemetryGuard { provider }
}

/// Build the batch-exporting tracer provider for `endpoint`. Returns `None` (and warns to stderr,
/// since the subscriber isn't installed yet) if the exporter can't be constructed — a bad endpoint
/// must degrade to logs-only, never abort the binary.
fn build_provider(
    service_name: &str,
    endpoint: &str,
) -> Option<opentelemetry_sdk::trace::TracerProvider> {
    let exporter = match opentelemetry_otlp::SpanExporter::builder()
        .with_http()
        .with_endpoint(endpoint)
        .build()
    {
        Ok(exporter) => exporter,
        Err(e) => {
            eprintln!("yagra-telemetry: OTLP exporter init failed ({e}); tracing export disabled");
            return None;
        }
    };
    let resource = opentelemetry_sdk::Resource::new(vec![
        opentelemetry::KeyValue::new("service.name", service_name.to_owned()),
        opentelemetry::KeyValue::new("service.version", env!("CARGO_PKG_VERSION")),
    ]);
    Some(
        opentelemetry_sdk::trace::TracerProvider::builder()
            .with_batch_exporter(exporter, opentelemetry_sdk::runtime::Tokio)
            .with_sampler(sampler_from_env())
            .with_resource(resource)
            .build(),
    )
}

/// Resolve the trace sampler from the OpenTelemetry-standard `OTEL_TRACES_SAMPLER` /
/// `OTEL_TRACES_SAMPLER_ARG` env vars, defaulting to `parentbased_always_on` (record every trace,
/// honouring an upstream sampling decision). **At scale (tens of thousands of nodes) set
/// `OTEL_TRACES_SAMPLER=parentbased_traceidratio` with e.g. `OTEL_TRACES_SAMPLER_ARG=0.01`** so the
/// poll hot path doesn't emit a span per poll per node. `parentbased_*` keeps a whole trace's
/// sampling decision consistent across the core⇄poller hops.
fn sampler_from_env() -> opentelemetry_sdk::trace::Sampler {
    use opentelemetry_sdk::trace::Sampler;
    // Ratio for the `*traceidratio` variants; clamped to [0, 1], default 1.0 (sample all).
    let ratio = std::env::var("OTEL_TRACES_SAMPLER_ARG")
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .map_or(1.0, |v| v.clamp(0.0, 1.0));
    match std::env::var("OTEL_TRACES_SAMPLER").ok().as_deref() {
        Some("always_on") => Sampler::AlwaysOn,
        Some("always_off") => Sampler::AlwaysOff,
        Some("traceidratio") => Sampler::TraceIdRatioBased(ratio),
        Some("parentbased_always_off") => Sampler::ParentBased(Box::new(Sampler::AlwaysOff)),
        Some("parentbased_traceidratio") => {
            Sampler::ParentBased(Box::new(Sampler::TraceIdRatioBased(ratio)))
        }
        // Default (also "parentbased_always_on"): inherit the parent's decision, else sample.
        _ => Sampler::ParentBased(Box::new(Sampler::AlwaysOn)),
    }
}

/// Capture the current span's W3C trace context into a fresh carrier map, ready to travel on a bus
/// message. Empty when tracing export is off or there's no active span — so an empty result means
/// "no trace to propagate" and the field is simply omitted from the wire.
#[must_use]
pub fn current_trace_context() -> HashMap<String, String> {
    use tracing_opentelemetry::OpenTelemetrySpanExt;
    let mut carrier = HashMap::new();
    let cx = tracing::Span::current().context();
    global::get_text_map_propagator(|prop| prop.inject_context(&cx, &mut carrier));
    carrier
}

/// Make `span` a child of the trace carried in `carrier` (the producer's context extracted from a
/// bus message). A no-op when the carrier is empty, so a message from an untraced / N-1 producer
/// just starts a fresh local trace.
pub fn set_span_parent(span: &tracing::Span, carrier: &HashMap<String, String>) {
    use tracing_opentelemetry::OpenTelemetrySpanExt;
    if carrier.is_empty() {
        return;
    }
    let parent = global::get_text_map_propagator(|prop| prop.extract(carrier));
    span.set_parent(parent);
}

// ── Graceful shutdown ─────────────────────────────────────────────────────────────────────────
// Process-lifecycle coordination shared by every binary. One `CancellationToken` fans a
// SIGTERM/Ctrl-C out to the background loops AND the HTTP server so a rolling upgrade drains
// in-flight work instead of being hard-killed mid-write — the "no data loss on upgrade" contract
// (ADR-017). Lives here because this is already the process-wide lifecycle crate.

/// Re-exported so binaries share one cancellation type without each depending on `tokio-util`.
pub use tokio_util::sync::CancellationToken;

/// Await the process shutdown signal: SIGTERM (unix — what `docker stop` / Kubernetes send) or
/// Ctrl-C (all platforms). Resolves once, when the first of the two arrives.
pub async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        use tokio::signal::unix::{signal, SignalKind};
        match signal(SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            // No SIGTERM handler available ⇒ this arm never resolves; fall back to Ctrl-C only.
            Err(e) => {
                tracing::warn!(error = %e, "SIGTERM handler unavailable; Ctrl-C only");
                std::future::pending::<()>().await;
            }
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }
}

/// Spawn a process-lifetime background task that stops promptly when `shutdown` fires. On cancel the
/// task's future is dropped at its next `.await` — the right semantics for the best-effort
/// scheduler / refresh / bus-consumer loops (durable writes are idempotent + expand-contract, so a
/// dropped in-flight write is simply redone next round, ADR-017). Returns the `JoinHandle`.
pub fn spawn_cancellable<F>(shutdown: &CancellationToken, fut: F) -> tokio::task::JoinHandle<()>
where
    F: std::future::Future + Send + 'static,
    F::Output: Send + 'static,
{
    let token = shutdown.clone();
    tokio::spawn(async move {
        tokio::select! {
            _ = token.cancelled() => {}
            _ = fut => {}
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shutdown contract: a task wrapped by [`spawn_cancellable`] stops promptly once the token
    /// is cancelled, even when its inner future would otherwise run forever — this is what lets a
    /// SIGTERM drain the background loops instead of hard-killing them.
    #[tokio::test]
    async fn spawn_cancellable_stops_on_cancel() {
        let token = CancellationToken::new();
        // Inner future never completes on its own; only cancellation can end the wrapper.
        let handle = spawn_cancellable(&token, std::future::pending::<()>());
        token.cancel();
        tokio::time::timeout(std::time::Duration::from_secs(5), handle)
            .await
            .expect("cancelled task must finish promptly")
            .expect("wrapper task must not panic");
    }

    /// With no propagator registered (export disabled — the default in tests) capturing the
    /// current context yields an empty carrier, and an empty carrier serializes to nothing / is a
    /// no-op parent. This is the zero-overhead off path.
    #[test]
    fn context_is_empty_when_export_disabled() {
        let carrier = current_trace_context();
        assert!(
            carrier.is_empty(),
            "no propagator ⇒ nothing to inject (logs-only mode)"
        );
        // Applying an empty carrier as a parent must not panic and must leave the span parentless.
        let span = tracing::info_span!("noop");
        set_span_parent(&span, &carrier);
    }

    /// The mechanism the bus relies on: with the W3C propagator registered, a producer's span
    /// context injected into a carrier is recoverable by a consumer — i.e. the trace id survives a
    /// serialize-shaped `HashMap` hop, which is exactly what `trace_context` carries on a bus
    /// message.
    #[test]
    fn w3c_context_round_trips_through_carrier() {
        use opentelemetry::trace::{
            SpanContext, SpanId, TraceContextExt, TraceFlags, TraceId, TraceState,
        };
        use opentelemetry::Context;

        global::set_text_map_propagator(
            opentelemetry_sdk::propagation::TraceContextPropagator::new(),
        );

        // A valid remote span context, as if extracted from a parent process.
        let trace_id = TraceId::from_hex("0af7651916cd43dd8448eb211c80319c").unwrap();
        let span_id = SpanId::from_hex("b7ad6b7169203331").unwrap();
        let sc = SpanContext::new(
            trace_id,
            span_id,
            TraceFlags::SAMPLED,
            true,
            TraceState::default(),
        );
        let cx = Context::new().with_remote_span_context(sc);

        // Inject (producer side) → the carrier gains a W3C `traceparent`…
        let mut carrier = HashMap::new();
        global::get_text_map_propagator(|prop| prop.inject_context(&cx, &mut carrier));
        assert!(
            carrier.contains_key("traceparent"),
            "propagator writes a W3C traceparent"
        );

        // …extract (consumer side) → the trace id is preserved end to end.
        let extracted = global::get_text_map_propagator(|prop| prop.extract(&carrier));
        assert_eq!(
            extracted.span().span_context().trace_id(),
            trace_id,
            "trace id survives the carrier hop"
        );
    }
}
