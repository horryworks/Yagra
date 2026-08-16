// SPDX-License-Identifier: AGPL-3.0-only
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
//!
//! # Logs on disk ([`LOG_DIR_ENV`], ADR-045)
//!
//! Setting [`LOG_DIR_ENV`] adds a second log destination: hourly-rotated JSON lines on disk,
//! alongside — never instead of — stdout. It exists for one case, and the case is the whole
//! justification: on a deployment where nobody can open a shell, `docker logs` is unreachable, so a
//! panic or an OOM leaves **no** retrievable trace. The `/api/v1/system/support-bundle` endpoint
//! reads these files back over HTTP, which means a bundle taken *after* a recovery still carries the
//! run that died.
//!
//! Three properties are load-bearing rather than incidental:
//!
//! * **Writes never block the caller.** The appender is wrapped in `tracing_appender::non_blocking`,
//!   whose default is to *drop* lines when its queue is full. A poll loop stalling behind a slow
//!   disk would be a far worse failure than a missing log line.
//! * **A failure here is never fatal.** An unwritable directory degrades to stdout-only with a
//!   warning on stderr. This is observability plumbing; it must not be able to stop the binary that
//!   it observes from starting.
//! * **Disk use is bounded** by [`LOG_RETAIN_ENV`] hours of hourly files, so an unattended
//!   deployment cannot fill its volume with its own logs.

use std::collections::HashMap;
use std::path::PathBuf;

use opentelemetry::global;
use opentelemetry::trace::TracerProvider as _;
use opentelemetry_otlp::WithExportConfig as _;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;

/// Turns on OTLP span export and points at the collector's OTLP/HTTP endpoint
/// (e.g. `http://otel-collector:4318`). Unset ⇒ tracing export disabled (logs only).
pub(crate) const ENDPOINT_ENV: &str = "YAGRA_OTEL_ENDPOINT";
/// OpenTelemetry-standard fallback for [`ENDPOINT_ENV`].
pub(crate) const STD_ENDPOINT_ENV: &str = "OTEL_EXPORTER_OTLP_ENDPOINT";

/// Directory the rolling JSON log file is written to (e.g. `/var/log/yagra`). Unset or empty ⇒ no
/// file layer, stdout only — which is the pre-ADR-045 behaviour and stays the default for anyone
/// who has `docker logs`.
pub const LOG_DIR_ENV: &str = "YAGRA_LOG_DIR";
/// How many hourly log files to keep in [`LOG_DIR_ENV`]. Default [`DEFAULT_LOG_RETAIN_HOURS`].
pub(crate) const LOG_RETAIN_ENV: &str = "YAGRA_LOG_RETAIN_HOURS";
/// Two days of hourly files. Long enough that a fault noticed the next morning is still on disk,
/// short enough to bound the volume without anyone tuning it.
pub const DEFAULT_LOG_RETAIN_HOURS: usize = 48;

/// The configured on-disk log directory, or `None` when file logging is off.
///
/// **The one place that answers this question.** The support-bundle collector needs the same path
/// the subscriber writes to, and a second `std::env::var` call in `yagra-core` would be a second
/// place for the default to drift.
#[must_use]
pub fn log_dir() -> Option<PathBuf> {
    std::env::var(LOG_DIR_ENV)
        .ok()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
}

/// How many hourly files to retain.
fn log_retain_hours() -> usize {
    retain_hours_from(std::env::var(LOG_RETAIN_ENV).ok().as_deref())
}

/// Parse [`LOG_RETAIN_ENV`], falling back to the default for anything unusable.
///
/// Split out from the env read so it is testable without mutating process state — `std::env::set_var`
/// in one test races every other test in the binary.
///
/// **Zero falls back rather than being honoured.** `max_log_files(0)` would prune every rotated file
/// the moment it rolled, i.e. a typo would silently turn logging off, which is the failure this
/// whole feature exists to prevent.
fn retain_hours_from(raw: Option<&str>) -> usize {
    raw.and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(DEFAULT_LOG_RETAIN_HOURS)
}

/// The filename prefix for one process's rolling log files.
///
/// **Two processes must never share one.** `tracing-appender` names a file from the prefix and the
/// hour, so two writers with the same prefix in the same directory append to the same file and
/// interleave each other's lines — the hazard ADR-045 決定 2 named for HA cores, and the reason a
/// poller pool needs its id here (ADR-045 Inc.3). The instance is sanitized because it becomes a
/// path component and arrives from `YAGRA_POLLER_ID`, which an operator sets by hand.
///
/// Public because a **reader** has to name the same files: [`collect_logs`] selects by prefix, and a
/// caller that rebuilt the name itself would carry a second copy of this sanitization rule —
/// the exact drift that would make a poller unable to find its own log after somebody put a dot in
/// `YAGRA_POLLER_ID` (ADR-045 Inc.4).
#[must_use]
pub fn file_prefix(service_name: &str, instance: Option<&str>) -> String {
    let Some(raw) = instance.map(str::trim).filter(|s| !s.is_empty()) else {
        return service_name.to_owned();
    };
    let safe: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    format!("{service_name}-{safe}")
}

/// Build the non-blocking rolling-file writer for `service_name`, or `None` when file logging is
/// off or the directory cannot be written.
///
/// Returns the guard alongside the writer: dropping it stops the background flush thread, so it has
/// to live as long as the subscriber does.
fn file_writer(
    service_name: &str,
    instance: Option<&str>,
) -> Option<(
    tracing_appender::non_blocking::NonBlocking,
    tracing_appender::non_blocking::WorkerGuard,
)> {
    let dir = log_dir()?;
    let appender = tracing_appender::rolling::RollingFileAppender::builder()
        .rotation(tracing_appender::rolling::Rotation::HOURLY)
        .filename_prefix(file_prefix(service_name, instance))
        .filename_suffix("log")
        .max_log_files(log_retain_hours())
        .build(&dir);
    match appender {
        Ok(appender) => Some(tracing_appender::non_blocking(appender)),
        // Printed to stderr because the subscriber is not installed yet — and deliberately not
        // fatal: losing the file destination must never stop the binary from starting.
        Err(e) => {
            eprintln!(
                "yagra-telemetry: cannot open the log directory {} ({e}); logging to stdout only",
                dir.display()
            );
            None
        }
    }
}

// ── Reading the files back ────────────────────────────────────────────────────────────────────
// The collector lives here, beside the writer, because its correctness is entirely a property of
// how the files are named: `file_prefix` above plus `tracing-appender`'s `YYYY-MM-DD-HH` suffix.
// It has two consumers — core's support bundle (ADR-045 決定 2 / Inc.3) and a remote poller
// answering a request for its own log (Inc.4) — and they must select by the *same* rules, or a
// remote site's contribution to a bundle would be chosen differently from a co-located one's.

/// The log files one [`collect_logs`] call selected, and what it left behind.
pub struct CollectedLogs {
    /// `(filename, contents)`, newest first.
    pub files: Vec<(String, Vec<u8>)>,
    /// Files that matched the window but did not fit under the byte cap.
    pub dropped_for_size: usize,
    /// Files present in the directory but older than the window.
    pub outside_window: usize,
}

/// Collect the rolling log files for `prefix` written at or after `since`, newest first, up to
/// `max_bytes`.
///
/// Newest-first is the ordering that matters: when the cap bites, the hour you are debugging is the
/// one you keep. `tracing-appender` embeds `YYYY-MM-DD-HH` in the name, so a lexicographic sort is
/// a chronological one and no per-file metadata read is needed to order them.
///
/// The newest file is being appended to as this runs, so its last line may be truncated. That is
/// accepted rather than worked around — the alternative is skipping the current hour, which is the
/// hour that matters most.
///
/// An unreadable directory is an empty result, never an error: file logging is opt-in, so "there is
/// nothing here" is the common case and must not cost the caller the rest of its work.
#[must_use]
pub fn collect_logs(
    dir: &std::path::Path,
    prefix: &str,
    since: std::time::SystemTime,
    max_bytes: usize,
) -> CollectedLogs {
    let mut names: Vec<String> = match std::fs::read_dir(dir) {
        Ok(entries) => entries
            .filter_map(Result::ok)
            .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| n.starts_with(prefix))
            .collect(),
        Err(e) => {
            tracing::warn!(dir = %dir.display(), error = %e, "log collection: directory unreadable");
            return CollectedLogs {
                files: Vec::new(),
                dropped_for_size: 0,
                outside_window: 0,
            };
        }
    };
    names.sort_unstable();
    names.reverse();

    let mut files = Vec::new();
    let mut total = 0usize;
    let mut dropped_for_size = 0usize;
    let mut outside_window = 0usize;
    for name in names {
        let path = dir.join(&name);
        let Ok(meta) = std::fs::metadata(&path) else {
            continue;
        };
        // An mtime the platform will not give us is treated as in-window: a file we cannot date is
        // better carried than silently skipped.
        if meta.modified().is_ok_and(|m| m < since) {
            outside_window += 1;
            continue;
        }
        if total.saturating_add(usize::try_from(meta.len()).unwrap_or(usize::MAX)) > max_bytes {
            dropped_for_size += 1;
            continue;
        }
        match std::fs::read(&path) {
            Ok(bytes) => {
                total += bytes.len();
                files.push((name, bytes));
            }
            Err(e) => {
                tracing::warn!(file = %name, error = %e, "log collection: file unreadable");
            }
        }
    }
    CollectedLogs {
        files,
        dropped_for_size,
        outside_window,
    }
}

/// Flushes and stops the OTLP span pipeline on drop so the last spans aren't lost at shutdown.
/// Holds the tracer provider; `None` in logs-only mode (export disabled). Keep it alive for the
/// process lifetime — dropping it early tears down span export.
#[must_use = "hold the guard until shutdown; dropping it flushes and stops span export"]
pub struct TelemetryGuard {
    provider: Option<opentelemetry_sdk::trace::TracerProvider>,
    /// Keeps the rolling-file writer's background flush thread alive. `None` when file logging is
    /// off. Dropping it flushes what is queued — which is exactly what must happen on the way out
    /// of a process whose last log lines are the interesting ones.
    _log: Option<tracing_appender::non_blocking::WorkerGuard>,
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
    init_instance(service_name, None)
}

/// [`init`], but with an **instance id that only affects the log filename** (ADR-045 Inc.3).
///
/// A poller pool writes into one shared directory, so each poller's files must be distinguishable
/// — see [`file_prefix`] for what goes wrong otherwise. The OpenTelemetry `service.name` stays
/// `service_name` on purpose: traces are aggregated *across* a pool, and folding the instance into
/// the service name would split every existing dashboard into one series per poller.
pub fn init_instance(service_name: &str, instance: Option<&str>) -> TelemetryGuard {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let fmt_layer = tracing_subscriber::fmt::layer();

    // The on-disk destination (ADR-045), when configured. JSON rather than the stdout layer's
    // human format: this file is read back by machine — windowed by time in the support bundle,
    // then by whoever is debugging — and JSON is what keeps `tracing`'s structured fields as
    // fields instead of flattening them into a message string.
    let (file_layer, log_guard) = match file_writer(service_name, instance) {
        Some((writer, guard)) => (
            Some(
                tracing_subscriber::fmt::layer()
                    .json()
                    .with_ansi(false)
                    .with_writer(writer),
            ),
            Some(guard),
        ),
        None => (None, None),
    };

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
        .with(file_layer)
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
    // Logged after the subscriber is installed so it lands in the file it is describing — which
    // also makes "is file logging actually on?" answerable from the bundle itself.
    if let Some(dir) = log_dir() {
        tracing::info!(
            service = service_name,
            dir = %dir.display(),
            retain_hours = log_retain_hours(),
            enabled = log_guard.is_some(),
            "on-disk log file"
        );
    }
    TelemetryGuard {
        provider,
        _log: log_guard,
    }
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

    /// The property the whole parameter exists for: **two instances must not name one file.**
    /// Same prefix in one directory means two processes appending to the same hourly file, which
    /// is silent corruption rather than an error — nothing fails, the lines just interleave.
    #[test]
    fn two_instances_never_share_a_log_filename() {
        assert_ne!(
            file_prefix("yagra-poller", Some("edge-1")),
            file_prefix("yagra-poller", Some("edge-2"))
        );
        assert_eq!(
            file_prefix("yagra-poller", Some("edge-1")),
            "yagra-poller-edge-1"
        );
    }

    /// No instance ⇒ the historical name, unchanged. `yagra-core` has written
    /// `yagra-core.log.<hour>` since ADR-045 shipped and the support bundle selects by that
    /// prefix, so a suffix appearing here would make every existing deployment's core log
    /// invisible to the collector that reads it back.
    #[test]
    fn an_absent_or_blank_instance_leaves_the_service_name_alone() {
        assert_eq!(file_prefix("yagra-core", None), "yagra-core");
        assert_eq!(file_prefix("yagra-poller", Some("")), "yagra-poller");
        assert_eq!(file_prefix("yagra-poller", Some("   ")), "yagra-poller");
    }

    /// The id comes from `YAGRA_POLLER_ID`, which an operator types. It becomes a filename, so a
    /// separator or a traversal segment in it must not become one on disk.
    #[test]
    fn an_instance_id_cannot_escape_the_filename() {
        let p = file_prefix("yagra-poller", Some("../../etc/passwd"));
        assert!(!p.contains('/'), "{p}");
        assert!(!p.contains('.'), "{p}");
        assert_eq!(
            file_prefix("yagra-poller", Some("tokyo dc1")),
            "yagra-poller-tokyo-dc1"
        );
    }

    /// Newest-first with the cap applied, and the drop **counted** — a truncated log that says
    /// nothing about being truncated reads as "nothing was logged", which is a wrong answer rather
    /// than a missing one. (Moved here from `yagra-core` in ADR-045 Inc.4, when a second consumer
    /// appeared: the reader has to live beside the writer that names the files.)
    #[test]
    fn logs_are_newest_first_and_a_truncation_is_reported() {
        let dir = std::env::temp_dir().join(format!("yagra-logcollect-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for hour in ["10", "11", "12"] {
            std::fs::write(
                dir.join(format!("yagra-core.2026-08-06-{hour}.log")),
                [b'x'; 100],
            )
            .unwrap();
        }
        // An unrelated file in the same directory is not swept up.
        std::fs::write(dir.join("other.log"), b"nope").unwrap();

        let all = collect_logs(&dir, "yagra-core", std::time::UNIX_EPOCH, 1_000);
        assert_eq!(
            all.files
                .iter()
                .map(|(n, _)| n.as_str())
                .collect::<Vec<_>>(),
            vec![
                "yagra-core.2026-08-06-12.log",
                "yagra-core.2026-08-06-11.log",
                "yagra-core.2026-08-06-10.log"
            ],
            "newest first — when the cap bites, the hour being debugged is the one kept"
        );

        // A cap that fits only two files keeps the two newest and says one was dropped.
        let capped = collect_logs(&dir, "yagra-core", std::time::UNIX_EPOCH, 250);
        assert_eq!(capped.files.len(), 2);
        assert_eq!(capped.dropped_for_size, 1);
        assert_eq!(capped.files[0].0, "yagra-core.2026-08-06-12.log");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Selection is by the prefix [`file_prefix`] builds, and one poller's id can be a prefix of
    /// another's. This is what makes one shared log volume safe for a pool (ADR-045 Inc.3/Inc.4) —
    /// and it is why the reader is here rather than in either consumer.
    #[test]
    fn one_instances_files_are_not_another_instances() {
        let dir = std::env::temp_dir().join(format!("yagra-logprefix-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for name in [
            "yagra-poller-edge-1.2026-08-16-10.log",
            "yagra-poller-edge-10.2026-08-16-10.log",
            "yagra-core.2026-08-16-10.log",
        ] {
            std::fs::write(dir.join(name), b"{}\n").unwrap();
        }

        let mine = collect_logs(
            &dir,
            &format!("{}.", file_prefix("yagra-poller", Some("edge-1"))),
            std::time::UNIX_EPOCH,
            10_000,
        );
        assert_eq!(
            mine.files
                .iter()
                .map(|(n, _)| n.as_str())
                .collect::<Vec<_>>(),
            vec!["yagra-poller-edge-1.2026-08-16-10.log"],
            "edge-1 is a prefix of edge-10; the dot separator is what keeps them apart"
        );

        // Core's bundle collector selects the whole family with the bare service name, which must
        // still see both pollers and never core's own file.
        let pool = collect_logs(&dir, "yagra-poller", std::time::UNIX_EPOCH, 10_000);
        assert_eq!(pool.files.len(), 2);

        std::fs::remove_dir_all(&dir).ok();
    }

    /// A missing log directory is an empty result, never an error. File logging is opt-in, so the
    /// common case is that there is nothing here — and it must not cost the caller the rest of its
    /// work.
    #[test]
    fn an_absent_log_directory_costs_nothing() {
        let logs = collect_logs(
            std::path::Path::new("/nonexistent/yagra/logs"),
            "yagra-core",
            std::time::UNIX_EPOCH,
            1,
        );
        assert!(logs.files.is_empty());
        assert_eq!(logs.dropped_for_size, 0);
    }

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

    /// The retention parse, including the case that matters: a value that cannot be honoured falls
    /// back to the default instead of being taken literally. `max_log_files(0)` prunes every file
    /// as soon as it rotates, so honouring a `0` would turn a typo into "logging silently off" —
    /// the exact failure the on-disk log exists to prevent.
    #[test]
    fn an_unusable_retention_falls_back_to_the_default() {
        assert_eq!(retain_hours_from(Some("6")), 6);
        assert_eq!(retain_hours_from(Some(" 12 ")), 12);
        for bad in [None, Some(""), Some("0"), Some("-3"), Some("lots")] {
            assert_eq!(
                retain_hours_from(bad),
                DEFAULT_LOG_RETAIN_HOURS,
                "{bad:?} must not be honoured"
            );
        }
    }

    /// File logging is **off unless asked for**, and the collector reads the same answer the
    /// subscriber writes to. Guards the split-brain shape rather than the value: a second
    /// `std::env::var(LOG_DIR_ENV)` elsewhere is how the two would come to disagree.
    #[test]
    fn the_log_directory_is_opt_in_and_has_one_source() {
        // The test binary sets no YAGRA_LOG_DIR, so this is the default deployment's answer.
        assert!(
            log_dir().is_none(),
            "unset {LOG_DIR_ENV} must mean stdout only"
        );
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
