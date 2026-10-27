// SPDX-License-Identifier: AGPL-3.0-only
//! The `metrics` recorder and its Prometheus text rendering, for both binaries (ADR-214 decision 3;
//! it was the `metrics-exporter-prometheus` crate).
//!
//! Core installs a [`Recorder`] at startup and serves [`Handle::render`] on its axum `/metrics`;
//! the poller serves the same text on its own port with [`serve`]. Tests build a [`Recorder`] and
//! run code under `metrics::with_local_recorder`, then read the text.
//!
//! What it keeps from the crate it replaced: metric and label names as written, labels in the order
//! the macro gave them, a counter as an integer, a gauge as Rust's shortest float (`2`, `0.125`),
//! and only the keys something has touched.
//!
//! 🚨 **What it changes: every histogram is a real Prometheus histogram** (`_bucket`, `_sum`,
//! `_count`). The old crate rendered a histogram it had no buckets for as a rolling summary with
//! `quantile` labels — which cannot be added up across pollers, and which nothing in Yagra read.
//! A metric with no buckets of its own gets [`DEFAULT_BUCKETS`]; give it its own with
//! [`Recorder::with_buckets`].

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::time::Duration;

use metrics::{
    Counter, CounterFn, Gauge, GaugeFn, Histogram, HistogramFn, Key, KeyName, Metadata,
    SharedString, Unit,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// Buckets, in seconds, for a histogram that names none: Prometheus's standard set plus one minute
/// and five minutes, for the background jobs that take that long.
pub const DEFAULT_BUCKETS: &[f64] = &[
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 60.0, 300.0,
];

/// The longest request head [`serve`] reads before answering anyway.
const MAX_REQUEST_HEAD: usize = 8 * 1024;
/// How long [`serve`] waits for one scraper.
const CONNECTION_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Default)]
struct Registry {
    counters: Cells<CounterCell>,
    gauges: Cells<GaugeCell>,
    histograms: Cells<HistogramCell>,
    buckets: HashMap<String, Vec<f64>>,
}

#[derive(Default)]
struct CounterCell(AtomicU64);

impl CounterFn for CounterCell {
    fn increment(&self, value: u64) {
        self.0.fetch_add(value, Ordering::Relaxed);
    }
    fn absolute(&self, value: u64) {
        self.0.fetch_max(value, Ordering::Relaxed);
    }
}

/// An `f64` kept as its bits.
#[derive(Default)]
struct GaugeCell(AtomicU64);

impl GaugeCell {
    fn get(&self) -> f64 {
        f64::from_bits(self.0.load(Ordering::Relaxed))
    }
    fn update(&self, f: impl Fn(f64) -> f64) {
        let _ = self
            .0
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |bits| {
                Some(f(f64::from_bits(bits)).to_bits())
            });
    }
}

impl GaugeFn for GaugeCell {
    fn increment(&self, value: f64) {
        self.update(|v| v + value);
    }
    fn decrement(&self, value: f64) {
        self.update(|v| v - value);
    }
    fn set(&self, value: f64) {
        self.0.store(value.to_bits(), Ordering::Relaxed);
    }
}

struct HistogramCell {
    bounds: Vec<f64>,
    state: Mutex<HistogramState>,
}

#[derive(Clone)]
struct HistogramState {
    /// Per-bucket counts, not cumulative; one more than `bounds` for `+Inf`.
    counts: Vec<u64>,
    sum: f64,
    count: u64,
}

impl HistogramFn for HistogramCell {
    fn record(&self, value: f64) {
        let slot = self
            .bounds
            .iter()
            .position(|b| value <= *b)
            .unwrap_or(self.bounds.len());
        let mut s = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        s.counts[slot] += 1;
        s.sum += value;
        s.count += 1;
    }
}

/// The recorder. Install one with [`Recorder::install`], or run code under it in a test.
#[derive(Default)]
pub struct Recorder {
    registry: Arc<Registry>,
}

/// A handle that renders what the recorder holds. Cheap to clone.
#[derive(Clone)]
pub struct Handle {
    registry: Arc<Registry>,
}

impl Recorder {
    /// A recorder with no metrics yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Give the histogram called `name` its own bucket bounds. Non-finite values are dropped and
    /// the rest sorted; an empty list leaves the metric on [`DEFAULT_BUCKETS`].
    #[must_use]
    pub fn with_buckets(mut self, name: &str, bounds: &[f64]) -> Self {
        let mut clean: Vec<f64> = bounds.iter().copied().filter(|b| b.is_finite()).collect();
        clean.sort_by(f64::total_cmp);
        clean.dedup();
        if !clean.is_empty() {
            Arc::get_mut(&mut self.registry)
                .expect("buckets are set before the recorder is shared")
                .buckets
                .insert(name.to_owned(), clean);
        }
        self
    }

    /// A handle onto this recorder's values.
    #[must_use]
    pub fn handle(&self) -> Handle {
        Handle {
            registry: Arc::clone(&self.registry),
        }
    }

    /// Make this the process-wide recorder.
    ///
    /// # Errors
    /// When a recorder has already been installed.
    pub fn install(self) -> Result<Handle, metrics::SetRecorderError<Self>> {
        let handle = self.handle();
        metrics::set_global_recorder(self)?;
        Ok(handle)
    }
}

impl metrics::Recorder for Recorder {
    fn describe_counter(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}
    fn describe_gauge(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}
    fn describe_histogram(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}

    fn register_counter(&self, key: &Key, _: &Metadata<'_>) -> Counter {
        Counter::from_arc(self.registry.counters.get_or_insert(key, Arc::default))
    }

    fn register_gauge(&self, key: &Key, _: &Metadata<'_>) -> Gauge {
        Gauge::from_arc(self.registry.gauges.get_or_insert(key, Arc::default))
    }

    fn register_histogram(&self, key: &Key, _: &Metadata<'_>) -> Histogram {
        Histogram::from_arc(self.registry.histograms.get_or_insert(key, || {
            let bounds = self
                .registry
                .buckets
                .get(key.name())
                .cloned()
                .unwrap_or_else(|| DEFAULT_BUCKETS.to_vec());
            Arc::new(HistogramCell {
                state: Mutex::new(HistogramState {
                    counts: vec![0; bounds.len() + 1],
                    sum: 0.0,
                    count: 0,
                }),
                bounds,
            })
        }))
    }
}

/// One kind's series, keyed by name and labels.
///
/// ⚠️ **Every `counter!`/`gauge!`/`histogram!` call registers** — the `metrics` macros look the
/// series up on each invocation, on the poll and ingest paths too — so the lookup takes a read lock
/// and clones nothing; only a series seen for the first time takes the write lock and its key.
struct Cells<T>(RwLock<HashMap<Key, Arc<T>>>);

impl<T> Default for Cells<T> {
    fn default() -> Self {
        Self(RwLock::default())
    }
}

impl<T> Cells<T> {
    fn get_or_insert(&self, key: &Key, make: impl FnOnce() -> Arc<T>) -> Arc<T> {
        if let Some(cell) = self
            .0
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(key)
        {
            return Arc::clone(cell);
        }
        let mut map = self.0.write().unwrap_or_else(PoisonError::into_inner);
        Arc::clone(map.entry(key.clone()).or_insert_with(make))
    }

    /// Every series as it stands, so a render formats them without holding the lock.
    fn snapshot(&self) -> Vec<(Key, Arc<T>)> {
        self.0
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .map(|(k, c)| (k.clone(), Arc::clone(c)))
            .collect()
    }
}

impl Handle {
    /// The Prometheus text exposition of every metric touched so far.
    #[must_use]
    pub fn render(&self) -> String {
        // name -> (type, lines). Sorted by name, then by the rendered label set, so the output is
        // stable from one scrape to the next.
        let mut families: BTreeMap<String, (&'static str, Vec<String>)> = BTreeMap::new();
        let mut push = |name: String, kind: &'static str, line: String| {
            families
                .entry(name)
                .or_insert((kind, Vec::new()))
                .1
                .push(line);
        };

        for (key, cell) in &self.registry.counters.snapshot() {
            let name = metric_name(key.name());
            let line = format!(
                "{name}{} {}",
                labels(key, None),
                cell.0.load(Ordering::Relaxed)
            );
            push(name, "counter", line);
        }
        for (key, cell) in &self.registry.gauges.snapshot() {
            let name = metric_name(key.name());
            let line = format!("{name}{} {}", labels(key, None), number(cell.get()));
            push(name, "gauge", line);
        }
        for (key, cell) in &self.registry.histograms.snapshot() {
            let name = metric_name(key.name());
            let s = cell
                .state
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone();
            let mut text = String::new();
            let mut running = 0u64;
            for (bound, n) in cell.bounds.iter().zip(&s.counts) {
                running += n;
                let _ = writeln!(
                    text,
                    "{name}_bucket{} {running}",
                    labels(key, Some(&number(*bound)))
                );
            }
            let _ = writeln!(
                text,
                "{name}_bucket{} {}",
                labels(key, Some("+Inf")),
                s.count
            );
            let _ = writeln!(text, "{name}_sum{} {}", labels(key, None), number(s.sum));
            let _ = write!(text, "{name}_count{} {}", labels(key, None), s.count);
            push(name, "histogram", text);
        }

        let mut out = String::new();
        for (name, (kind, mut lines)) in families {
            lines.sort();
            let _ = writeln!(out, "# TYPE {name} {kind}");
            for line in lines {
                out.push_str(&line);
                out.push('\n');
            }
            out.push('\n');
        }
        out
    }
}

/// A float as Prometheus reads it: Rust's shortest form, with the infinities spelled its way.
fn number(v: f64) -> String {
    if v.is_nan() {
        "NaN".to_owned()
    } else if v == f64::INFINITY {
        "+Inf".to_owned()
    } else if v == f64::NEG_INFINITY {
        "-Inf".to_owned()
    } else {
        v.to_string()
    }
}

/// `{a="1",b="2"}` in the key's own order, `le` last; empty when there are none.
fn labels(key: &Key, le: Option<&str>) -> String {
    let mut parts: Vec<String> = key
        .labels()
        .map(|l| format!("{}=\"{}\"", label_name(l.key()), escape(l.value())))
        .collect();
    if let Some(le) = le {
        parts.push(format!("le=\"{le}\""));
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!("{{{}}}", parts.join(","))
    }
}

fn escape(v: &str) -> String {
    v.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

/// A metric name with anything outside `[a-zA-Z0-9_:]` turned into `_`, and no leading digit.
fn metric_name(name: &str) -> String {
    sanitize(name, true)
}

/// A label name: as a metric name, but `:` is not allowed either.
fn label_name(name: &str) -> String {
    sanitize(name, false)
}

fn sanitize(name: &str, colon: bool) -> String {
    let mut out: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || (colon && c == ':') {
                c
            } else {
                '_'
            }
        })
        .collect();
    if out.starts_with(|c: char| c.is_ascii_digit()) {
        out.insert(0, '_');
    }
    out
}

/// Answer HTTP scrapes on `addr` with [`Handle::render`] until the process ends — the poller's
/// `:9100/metrics`. Any path answers, as it did before. Returns once the port is bound; the accept
/// loop runs on its own task for the life of the process, which is the lifetime of what it reports.
///
/// # Errors
/// When the port cannot be bound.
pub async fn serve(addr: SocketAddr, handle: Handle) -> std::io::Result<()> {
    let listener = TcpListener::bind(addr).await?;
    tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    let handle = handle.clone();
                    tokio::spawn(async move {
                        let _ =
                            tokio::time::timeout(CONNECTION_TIMEOUT, answer(stream, &handle)).await;
                    });
                }
                Err(e) => {
                    tracing::debug!(error = %e, "metrics listener accept failed");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
        }
    });
    Ok(())
}

/// Read one request head and answer it, then close.
async fn answer(mut stream: TcpStream, handle: &Handle) -> std::io::Result<()> {
    let mut head = Vec::new();
    let mut buf = [0u8; 1024];
    while !head.windows(4).any(|w| w == b"\r\n\r\n") && head.len() < MAX_REQUEST_HEAD {
        let n = stream.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        head.extend_from_slice(&buf[..n]);
    }
    let response = respond(&head, handle);
    stream.write_all(&response).await?;
    stream.shutdown().await
}

/// The full response to a request head: the metrics for `GET`, headers only for `HEAD`, 405 else.
fn respond(head: &[u8], handle: &Handle) -> Vec<u8> {
    let method = head.split(|&b| b == b' ').next().unwrap_or_default();
    match method {
        b"GET" | b"HEAD" => {
            let body = handle.render();
            let mut out = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/plain; version=0.0.4\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .into_bytes();
            if method == b"GET" {
                out.extend_from_slice(body.as_bytes());
            }
            out
        }
        _ => b"HTTP/1.1 405 Method Not Allowed\r\nAllow: GET, HEAD\r\nContent-Length: 0\r\n\
               Connection: close\r\n\r\n"
            .to_vec(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rendered(recorder: &Recorder, f: impl FnOnce()) -> String {
        metrics::with_local_recorder(recorder, f);
        recorder.handle().render()
    }

    /// Every macro call registers again, so a second call must find the cell the first made — from
    /// several threads at once, the shape the poll and ingest paths have.
    #[test]
    fn a_series_registered_again_and_concurrently_is_one_cell() {
        let r = Arc::new(Recorder::new());
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let r = Arc::clone(&r);
                std::thread::spawn(move || {
                    metrics::with_local_recorder(&*r, || {
                        for _ in 0..1_000 {
                            metrics::counter!("yagra_results_total", "shard" => "0").increment(1);
                        }
                    });
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        let text = r.handle().render();
        let lines: Vec<&str> = text
            .lines()
            .filter(|l| l.starts_with("yagra_results_total"))
            .collect();
        assert_eq!(lines, ["yagra_results_total{shard=\"0\"} 8000"], "{text}");
    }

    #[test]
    fn counters_and_gauges_render_as_the_old_exporter_wrote_them() {
        let r = Recorder::new();
        let text = rendered(&r, || {
            metrics::counter!("yagra_panics_total").increment(0);
            metrics::counter!("yagra_task_restarts_total", "task" => "test_zero").increment(2);
            metrics::gauge!("yagra_working_set_specs").set(2.0);
            metrics::gauge!("yagra_poll_demand_per_second").set(0.1 + 0.025);
            metrics::gauge!("yagra_inflight").increment(3.0);
            metrics::gauge!("yagra_inflight").decrement(1.0);
        });
        for line in [
            "# TYPE yagra_panics_total counter",
            "yagra_panics_total 0",
            "yagra_task_restarts_total{task=\"test_zero\"} 2",
            "# TYPE yagra_working_set_specs gauge",
            "yagra_working_set_specs 2",
            "yagra_poll_demand_per_second 0.125",
            "yagra_inflight 2",
        ] {
            assert!(
                text.lines().any(|l| l == line),
                "`{line}` missing from:\n{text}"
            );
        }
    }

    #[test]
    fn labels_keep_their_order_and_are_escaped() {
        let r = Recorder::new();
        let text = rendered(&r, || {
            metrics::counter!("yagra_x_total", "kind" => "snmp_table", "phase" => "a\"b\\c\nd")
                .increment(1);
        });
        assert!(
            text.contains("yagra_x_total{kind=\"snmp_table\",phase=\"a\\\"b\\\\c\\nd\"} 1"),
            "{text}"
        );
    }

    /// 🚨 The behaviour that changed (ADR-214 decision 3): a histogram nobody gave buckets to is a
    /// histogram with the default buckets, never a summary with quantiles.
    #[test]
    fn a_histogram_renders_cumulative_buckets_sum_and_count() {
        let r = Recorder::new().with_buckets("yagra_phase_seconds", &[0.1, 1.0, 30.0]);
        let text = rendered(&r, || {
            for v in [0.05, 0.5, 0.5, 40.0] {
                metrics::histogram!("yagra_phase_seconds", "phase" => "execute").record(v);
            }
            metrics::histogram!("yagra_other_seconds").record(2.0);
        });
        for line in [
            "# TYPE yagra_phase_seconds histogram",
            "yagra_phase_seconds_bucket{phase=\"execute\",le=\"0.1\"} 1",
            "yagra_phase_seconds_bucket{phase=\"execute\",le=\"1\"} 3",
            "yagra_phase_seconds_bucket{phase=\"execute\",le=\"30\"} 3",
            "yagra_phase_seconds_bucket{phase=\"execute\",le=\"+Inf\"} 4",
            "yagra_phase_seconds_sum{phase=\"execute\"} 41.05",
            "yagra_phase_seconds_count{phase=\"execute\"} 4",
            "yagra_other_seconds_bucket{le=\"2.5\"} 1",
            "yagra_other_seconds_bucket{le=\"300\"} 1",
        ] {
            assert!(
                text.lines().any(|l| l == line),
                "`{line}` missing from:\n{text}"
            );
        }
        assert!(!text.contains("quantile"), "{text}");
    }

    #[test]
    fn a_bad_bucket_list_falls_back_to_the_defaults() {
        let r = Recorder::new().with_buckets("yagra_h", &[f64::NAN]);
        let text = rendered(&r, || metrics::histogram!("yagra_h").record(1.0));
        assert!(text.contains("yagra_h_bucket{le=\"300\"} 1"), "{text}");
    }

    #[test]
    fn names_are_made_valid_and_special_floats_are_spelled_prometheus_style() {
        assert_eq!(metric_name("9bad-name.x"), "_9bad_name_x");
        assert_eq!(label_name("a:b"), "a_b");
        assert_eq!(number(f64::INFINITY), "+Inf");
        assert_eq!(number(f64::NAN), "NaN");
    }

    #[test]
    fn only_get_and_head_are_answered() {
        let h = Recorder::new().handle();
        let get = String::from_utf8(respond(b"GET /metrics HTTP/1.1\r\n\r\n", &h)).unwrap();
        assert!(get.starts_with("HTTP/1.1 200 OK\r\n"), "{get}");
        assert!(get.contains("version=0.0.4"));
        let post = String::from_utf8(respond(b"POST / HTTP/1.1\r\n\r\n", &h)).unwrap();
        assert!(post.starts_with("HTTP/1.1 405"), "{post}");
    }

    /// The whole listener over a real socket, as a scraper sees it.
    #[tokio::test]
    async fn the_listener_serves_the_rendered_text() {
        let r = Recorder::new();
        metrics::with_local_recorder(&r, || metrics::counter!("yagra_seen_total").increment(5));
        let port = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        let addr: SocketAddr = ([127, 0, 0, 1], port).into();
        serve(addr, r.handle()).await.unwrap();
        let mut s = TcpStream::connect(addr).await.unwrap();
        s.write_all(b"GET /metrics HTTP/1.1\r\nHost: x\r\n\r\n")
            .await
            .unwrap();
        let mut body = String::new();
        s.read_to_string(&mut body).await.unwrap();
        assert!(
            body.contains("\r\n\r\n# TYPE yagra_seen_total counter\nyagra_seen_total 5\n"),
            "{body}"
        );
    }
}
