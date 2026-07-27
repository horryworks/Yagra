// SPDX-License-Identifier: AGPL-3.0-only
//! Passive-data forwarding — the "tee" (ADR-034).
//!
//! Core relays everything it receives to zero or more external collectors. Sending is centralized
//! here rather than done at the poller edge so there is **one egress point** to allow through a
//! firewall, and so filters, credentials and status live where the admin already configures them.
//!
//! ```text
//! consume_events ─────try_send──▶ [dispatcher] ──try_send──▶ [sender per destination] ──▶ collector
//! consume_raw_flows ──try_send──▶  (bounded)     (bounded)     rate limit, circuit breaker
//!                                  filter+render
//! ```
//!
//! **Forwarding must never slow intake.** Both hops are bounded channels written with `try_send`;
//! a full queue drops the message and counts it (ADR-024/025/S27 — the same shape as the event and
//! flow persist paths). A collector that black-holes traffic degrades forwarding and nothing else:
//! the event engine, alerting and the durable stores never see backpressure from here. Forwarding
//! is a best-effort observation tier, like the log store.
//!
//! The dispatcher runs **leader-only** (spawned from `leader_work`); a passive core in an HA pair
//! must not double-send.
//!
//! **Fidelity.** A destination marked `verbatim` relays the original datagram from
//! [`EventMsg::raw`] (or [`RawFlowDatagram::bytes`] for flow). When that is absent — an inbound
//! webhook was never a datagram, or an N-1 poller predates raw capture — the message is rebuilt from
//! the parsed fields and counted in `rendered`, so the Forwarding page can show that the promise was
//! not kept rather than quietly shipping degraded output. Flow has no rendered form at all, so a
//! flow destination is byte-exact or nothing.
//!
//! **Flow filtering is per record, relaying per datagram.** A v9/IPFIX export is a template plus
//! many records; records cannot be removed without re-encoding into a different datagram. So for a
//! `flow_udp` destination the filter is an any-record test and the whole datagram is relayed,
//! non-matching records included. Decoding needs the exporter's templates, which is why the
//! dispatcher keeps its own [`FlowTemplates`] cache — the same bounded, FIFO-evicting one the poller
//! uses. A **BigQuery** flow destination has no such constraint: rows are independent, so its filter
//! is exact per record and non-matching records are simply not written.
//!
//! **BigQuery is the one destination that is not a datagram relay** (ADR-034 Increment 3). It takes
//! normalized rows — one per event, one per flow record — batched and streamed via `insertAll`
//! ([`crate::bigquery`]). It shares everything structural with the relay kinds (the same inlet, the
//! same bounded per-destination queue, the same rate limiter and circuit breaker) and differs only
//! in what it puts on the queue and how its sender drains it.
//!
//! **Log discipline** (security.md): syslog bodies routinely carry credentials, and forwarding
//! sends them off-box. Payloads are never logged; errors carry the destination name and the
//! transport error only, and the SNMP community is never logged at any level.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::Value;
use tokio::io::AsyncWriteExt;
use tokio::net::{lookup_host, TcpStream, UdpSocket};
use tokio::sync::{mpsc, Notify};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use yagra_bus::{EventKind, EventMsg, RawFlowDatagram, RawFlowProto};
use yagra_forward::{
    render_syslog_5424, render_trap_v2c, CompiledFilter, DestKind, FilterView, FlowFields,
    SourceKind, DEFAULT_TRAP_COMMUNITY,
};
use yagra_ingest::{carries_template_set, parse_flow_export, parse_sflow, FlowTemplates, RawFlow};

use crate::bigquery::{BigQueryClient, FLUSH_INTERVAL, MAX_INSERT_BYTES, MAX_ROWS_PER_INSERT};
use crate::forward_store::{DestSecret, ForwardStore, OpenDestination};

/// Queue between the event consumer and the dispatcher. Sized like the persist channels — deep
/// enough to ride out a burst, shallow enough that a stuck dispatcher is visible rather than
/// unbounded memory.
const FORWARD_CHANNEL_CAP: usize = 8192;
/// Queue between the dispatcher and one destination's sender.
const DEST_QUEUE_CAP: usize = 4096;
/// Drift backstop for reloading destinations (the API also pokes the dispatcher for instant
/// effect). Mirrors the alert-config refresh interval.
const RELOAD_TICK: Duration = Duration::from_secs(30);
/// Consecutive send failures that trip a destination's circuit breaker.
const CIRCUIT_FAILURES: u32 = 5;
/// How long the breaker stays open before one probe is allowed through.
const CIRCUIT_OPEN: Duration = Duration::from_secs(30);
/// How long a resolved target address is reused before re-resolving (picks up DNS changes without
/// a lookup per datagram).
const RESOLVE_TTL: Duration = Duration::from_secs(300);
/// Bound on connect/write so a black-holing collector cannot park a sender task forever.
const IO_TIMEOUT: Duration = Duration::from_secs(5);
/// Longest error text kept for the status API (device/DNS errors can be verbose).
const MAX_ERROR_CHARS: usize = 200;
/// How many of a datagram's decoded records the any-record filter inspects. A real export carries
/// tens; a 64 KiB datagram of minimal records could carry thousands, and evaluating a 32-condition
/// filter against every one of those on every datagram is work a hostile exporter could dictate.
/// Past this bound a match is missed and the datagram is treated as non-matching — the same outcome
/// as a filter that genuinely did not match, and consistent with the best-effort tier.
const MAX_FILTERED_RECORDS: usize = 1024;

// ── Status (read by `GET /api/v1/forwarding/status`) ─────────────────────────────────────────

/// Live counters for one destination.
#[derive(Debug, Default)]
struct DestCounters {
    sent: AtomicU64,
    filtered: AtomicU64,
    dropped: AtomicU64,
    errors: AtomicU64,
    rendered: AtomicU64,
    queue_depth: AtomicU64,
    circuit_open: AtomicBool,
    last_success_unix_ms: AtomicI64,
    last_error: Mutex<Option<String>>,
}

impl DestCounters {
    fn record_error(&self, err: &str) {
        self.errors.fetch_add(1, Ordering::Relaxed);
        let mut slot = self
            .last_error
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        *slot = Some(err.chars().take(MAX_ERROR_CHARS).collect());
    }
}

/// A destination's runtime status, as the API serializes it.
#[derive(Debug, Clone, Serialize)]
pub struct DestStatus {
    /// Destination id.
    pub id: Uuid,
    /// Destination name.
    pub name: String,
    /// Messages handed to the collector.
    pub sent: u64,
    /// Messages the filter rejected.
    pub filtered: u64,
    /// Messages dropped without being sent (full queue, rate cap, or open circuit).
    pub dropped: u64,
    /// Failed sends.
    pub errors: u64,
    /// Messages sent re-rendered because no raw payload was available, despite `verbatim`.
    pub rendered: u64,
    /// Messages currently queued for this destination.
    pub queue_depth: u64,
    /// Whether the circuit breaker is currently open (sends are being dropped fast).
    pub circuit_open: bool,
    /// Last successful send, Unix ms; `None` if there has not been one.
    pub last_success_unix_ms: Option<i64>,
    /// Last error text, truncated.
    pub last_error: Option<String>,
}

#[derive(Debug)]
struct RegistryEntry {
    id: Uuid,
    name: String,
    counters: Arc<DestCounters>,
}

/// Shared, cheaply-readable view of the running destinations for the status endpoint.
#[derive(Debug, Default)]
pub struct ForwardRegistry {
    entries: RwLock<Vec<RegistryEntry>>,
    /// How many destinations tee the **event** streams (syslog/traps), readable without taking the
    /// lock. The event consumer checks this on **every** received message, and with no destinations
    /// configured — the default — that check is the entire cost of the feature.
    active_event: AtomicUsize,
    /// The same, for **flow** destinations. Counted separately so a syslog-only installation pays
    /// nothing per flow datagram and vice versa; the two streams have wildly different rates.
    active_flow: AtomicUsize,
}

impl ForwardRegistry {
    /// Snapshot every running destination's counters.
    #[must_use]
    pub fn snapshot(&self) -> Vec<DestStatus> {
        let entries = self.entries.read().unwrap_or_else(PoisonError::into_inner);
        entries
            .iter()
            .map(|e| {
                let c = &e.counters;
                let last_success = c.last_success_unix_ms.load(Ordering::Relaxed);
                DestStatus {
                    id: e.id,
                    name: e.name.clone(),
                    sent: c.sent.load(Ordering::Relaxed),
                    filtered: c.filtered.load(Ordering::Relaxed),
                    dropped: c.dropped.load(Ordering::Relaxed),
                    errors: c.errors.load(Ordering::Relaxed),
                    rendered: c.rendered.load(Ordering::Relaxed),
                    queue_depth: c.queue_depth.load(Ordering::Relaxed),
                    circuit_open: c.circuit_open.load(Ordering::Relaxed),
                    last_success_unix_ms: (last_success > 0).then_some(last_success),
                    last_error: c
                        .last_error
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .clone(),
                }
            })
            .collect()
    }
}

/// One item on the tee inlet. The two received streams share a channel (and therefore one bounded
/// buffer and one dispatcher task) because they contend for the same destinations and the same
/// per-destination queues; splitting them would just move the ordering question somewhere less
/// visible.
enum Teed {
    /// A syslog / trap / webhook event.
    Event(Box<EventMsg>),
    /// A verbatim flow-export datagram.
    Flow(Box<RawFlowDatagram>),
}

/// What the API and the bus consumers hold: the tee inlet, the status registry, and a poke to
/// reload destinations immediately after a config change.
#[derive(Clone)]
pub struct ForwardHandle {
    tx: mpsc::Sender<Teed>,
    registry: Arc<ForwardRegistry>,
    reload: Arc<Notify>,
}

impl ForwardHandle {
    /// Offer one received event to the forwarder. **Never blocks and never fails**: a full inlet
    /// drops the copy and counts it, because intake must not wait on forwarding.
    ///
    /// With no event destinations configured this returns after one relaxed load — no clone, no
    /// channel traffic — so an installation that never uses forwarding pays nothing per event.
    pub fn offer(&self, msg: &EventMsg) {
        if self.registry.active_event.load(Ordering::Relaxed) == 0 {
            return;
        }
        self.push(Teed::Event(Box::new(msg.clone())));
    }

    /// Offer one received flow datagram to the forwarder. Same contract as [`Self::offer`], and the
    /// same zero-cost path when no **flow** destination exists — which matters more here, because
    /// the poller relays these unconditionally and a busy exporter sends hundreds a second.
    pub fn offer_flow(&self, datagram: &RawFlowDatagram) {
        if self.registry.active_flow.load(Ordering::Relaxed) == 0 {
            return;
        }
        self.push(Teed::Flow(Box::new(datagram.clone())));
    }

    fn push(&self, item: Teed) {
        if self.tx.try_send(item).is_err() {
            metrics::counter!("yagra_forward_dropped_total", "reason" => "inlet_full").increment(1);
        }
    }

    /// Runtime status of every running destination.
    #[must_use]
    pub fn status(&self) -> Vec<DestStatus> {
        self.registry.snapshot()
    }

    /// Ask the dispatcher to reload destinations now (called after a config change so an edit takes
    /// effect immediately rather than on the next drift tick).
    pub fn poke(&self) {
        self.reload.notify_one();
    }
}

// ── Dispatcher ───────────────────────────────────────────────────────────────────────────────

struct LiveDest {
    id: Uuid,
    name: String,
    source_kind: SourceKind,
    dest_kind: DestKind,
    target: String,
    pool: Option<String>,
    verbatim: bool,
    rate_limit_per_sec: Option<u32>,
    ca_cert: Option<String>,
    community: String,
    /// Fingerprint of the credential the *sender* holds (a BigQuery service-account key), so a
    /// rotated key rebuilds the sender. The key itself is deliberately not kept here: the sender
    /// needs it for the life of the task anyway, and a second plaintext copy in the dispatcher would
    /// buy nothing.
    secret_fp: u64,
    filter: CompiledFilter,
    tx: mpsc::Sender<Vec<u8>>,
    counters: Arc<DestCounters>,
    shutdown: CancellationToken,
}

impl LiveDest {
    /// Whether a config change requires tearing the sender down (transport-level settings) rather
    /// than just swapping the filter.
    fn transport_matches(&self, open: &OpenDestination) -> bool {
        self.dest_kind == open.dest.dest_kind
            && self.target == open.dest.target
            && self.rate_limit_per_sec == open.dest.rate_limit_per_sec
            && self.ca_cert == open.dest.ca_cert
            && self.secret_fp == sender_secret_fingerprint(open.secret.as_ref())
            // The stream decides a BigQuery sender's table schema, so it is transport-level there.
            && self.source_kind == open.dest.source_kind
    }
}

/// Fingerprint of the part of a destination's secret the **sender** owns.
///
/// Only the BigQuery key counts: the SNMP community is used by the dispatcher when it re-encodes a
/// trap, so changing it must *not* tear down a working socket. Hashing rather than comparing keeps
/// the credential out of the dispatcher's long-lived state.
fn sender_secret_fingerprint(secret: Option<&DestSecret>) -> u64 {
    use std::hash::{Hash, Hasher};
    match secret {
        Some(DestSecret::GcpServiceAccount { json }) => {
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            json.hash(&mut hasher);
            // 0 means "no sender credential", so a key that happens to hash to it must not collide.
            hasher.finish() | 1
        }
        Some(DestSecret::SnmpCommunity { .. }) | None => 0,
    }
}

/// The dispatcher's owned half, spawned separately from the handle.
pub struct ForwardRunner {
    rx: mpsc::Receiver<Teed>,
    store: Arc<ForwardStore>,
    registry: Arc<ForwardRegistry>,
    reload: Arc<Notify>,
}

impl ForwardRunner {
    /// Run the dispatcher until `shutdown`. **Leader-only** — spawned from `leader_work`, because a
    /// passive core in an HA pair must not double-send. The handle exists on every core so the API
    /// can serve destination CRUD from either; on a passive core nothing is ever offered to it.
    pub async fn run(self, shutdown: CancellationToken) {
        run_dispatcher(self.rx, self.store, self.registry, self.reload, shutdown).await;
    }
}

/// Build the tee inlet. The returned handle goes to the event consumer and the API; the runner is
/// spawned by whichever core holds leadership.
#[must_use]
pub fn prepare(store: Arc<ForwardStore>) -> (ForwardHandle, ForwardRunner) {
    let (tx, rx) = mpsc::channel::<Teed>(FORWARD_CHANNEL_CAP);
    let registry = Arc::new(ForwardRegistry::default());
    let reload = Arc::new(Notify::new());
    (
        ForwardHandle {
            tx,
            registry: registry.clone(),
            reload: reload.clone(),
        },
        ForwardRunner {
            rx,
            store,
            registry,
            reload,
        },
    )
}

async fn run_dispatcher(
    mut rx: mpsc::Receiver<Teed>,
    store: Arc<ForwardStore>,
    registry: Arc<ForwardRegistry>,
    reload: Arc<Notify>,
    shutdown: CancellationToken,
) {
    let mut dests: Vec<LiveDest> = Vec::new();
    // Exporter template state for decoding relayed flow datagrams. Owned by this task (never shared,
    // never locked) and bounded/FIFO-evicting by construction, so a template-churning exporter
    // cannot grow it without limit.
    let mut templates = FlowTemplates::new();
    let mut ticker = tokio::time::interval(RELOAD_TICK);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    reconcile(&mut dests, &store, &registry, &shutdown).await;

    loop {
        tokio::select! {
            biased;
            () = shutdown.cancelled() => break,
            () = reload.notified() => reconcile(&mut dests, &store, &registry, &shutdown).await,
            _ = ticker.tick() => reconcile(&mut dests, &store, &registry, &shutdown).await,
            msg = rx.recv() => {
                match msg {
                    Some(Teed::Event(msg)) => dispatch(&dests, &msg),
                    Some(Teed::Flow(dg)) => dispatch_flow(&mut templates, &dests, &dg),
                    None => break,
                }
                metrics::gauge!("yagra_forward_inlet_depth").set(rx.len() as f64);
            }
        }
    }
    for dest in dests {
        dest.shutdown.cancel();
    }
}

/// Reload enabled destinations and bring the running senders in line with them. A destination whose
/// filter changed keeps its sender (and therefore its queue and connection); one whose transport
/// changed is torn down and rebuilt.
async fn reconcile(
    dests: &mut Vec<LiveDest>,
    store: &Arc<ForwardStore>,
    registry: &Arc<ForwardRegistry>,
    shutdown: &CancellationToken,
) {
    let open = match store.list_open().await {
        Ok(open) => open,
        Err(e) => {
            tracing::warn!(error = %e, "failed to load forwarding destinations; keeping current set");
            return;
        }
    };

    let mut previous: HashMap<Uuid, LiveDest> = dests.drain(..).map(|d| (d.id, d)).collect();
    for entry in open {
        // A filter that no longer compiles would have been rejected at the API edge; if one somehow
        // reaches here, skip the destination rather than forwarding the unfiltered firehose to it.
        let filter = match yagra_forward::compile(&entry.dest.filter) {
            Ok(filter) => filter,
            Err(e) => {
                tracing::warn!(destination = %entry.dest.name, error = %e, "forwarding destination has an invalid filter; skipping");
                continue;
            }
        };
        let community = match &entry.secret {
            Some(DestSecret::SnmpCommunity { community }) => community.clone(),
            Some(DestSecret::GcpServiceAccount { .. }) | None => DEFAULT_TRAP_COMMUNITY.to_owned(),
        };

        match previous.remove(&entry.dest.id) {
            Some(live) if live.transport_matches(&entry) => {
                // Reuse the sender; only the routing-side config is swapped.
                dests.push(LiveDest {
                    name: entry.dest.name.clone(),
                    pool: entry.dest.pool.clone(),
                    verbatim: entry.dest.verbatim,
                    community,
                    filter,
                    ..live
                });
            }

            other => {
                if let Some(stale) = other {
                    stale.shutdown.cancel();
                }
                dests.push(start_dest(&entry, filter, community, shutdown));
            }
        }
    }
    for (_, removed) in previous {
        removed.shutdown.cancel();
    }

    {
        let mut entries = registry
            .entries
            .write()
            .unwrap_or_else(PoisonError::into_inner);
        *entries = dests
            .iter()
            .map(|d| RegistryEntry {
                id: d.id,
                name: d.name.clone(),
                counters: d.counters.clone(),
            })
            .collect();
        // Published after `entries`, and while the write lock is held, so a reader that sees a
        // non-zero count always finds the rows behind it.
        let flow = dests
            .iter()
            .filter(|d| d.source_kind == SourceKind::Flow)
            .count();
        registry.active_flow.store(flow, Ordering::Release);
        registry
            .active_event
            .store(entries.len() - flow, Ordering::Release);
    }
    metrics::gauge!("yagra_forward_destinations").set(dests.len() as f64);
}

fn start_dest(
    entry: &OpenDestination,
    filter: CompiledFilter,
    community: String,
    parent: &CancellationToken,
) -> LiveDest {
    let (tx, rx) = mpsc::channel::<Vec<u8>>(DEST_QUEUE_CAP);
    let counters = Arc::new(DestCounters::default());
    let shutdown = parent.child_token();
    let spec = SenderSpec {
        kind: entry.dest.dest_kind,
        source_kind: entry.dest.source_kind,
        target: entry.dest.target.clone(),
        name: entry.dest.name.clone(),
        rate_limit_per_sec: entry.dest.rate_limit_per_sec,
        ca_cert: entry.dest.ca_cert.clone(),
        service_account: match &entry.secret {
            Some(DestSecret::GcpServiceAccount { json }) => Some(json.clone()),
            Some(DestSecret::SnmpCommunity { .. }) | None => None,
        },
    };
    // BigQuery accumulates rows into batched HTTP requests; every other kind writes one payload per
    // message to a socket. Two loops rather than one with a mode flag, because they share nothing
    // beyond the counters, the rate limiter and the breaker.
    if entry.dest.dest_kind == DestKind::BigQuery {
        tokio::spawn(run_bq_sender(spec, rx, counters.clone(), shutdown.clone()));
    } else {
        tokio::spawn(run_sender(spec, rx, counters.clone(), shutdown.clone()));
    }
    LiveDest {
        id: entry.dest.id,
        name: entry.dest.name.clone(),
        source_kind: entry.dest.source_kind,
        dest_kind: entry.dest.dest_kind,
        target: entry.dest.target.clone(),
        pool: entry.dest.pool.clone(),
        verbatim: entry.dest.verbatim,
        rate_limit_per_sec: entry.dest.rate_limit_per_sec,
        ca_cert: entry.dest.ca_cert.clone(),
        community,
        secret_fp: sender_secret_fingerprint(entry.secret.as_ref()),
        filter,
        tx,
        counters,
        shutdown,
    }
}

/// Route one received event to every destination that wants it. Pure fan-out — the only work done
/// on this task is filtering and rendering, so a slow collector can never stall the dispatcher.
fn dispatch(dests: &[LiveDest], msg: &EventMsg) {
    if dests.is_empty() {
        return;
    }
    let view = FilterView::from(msg);
    // Rendered once and shared: several destinations commonly want the same normalized line.
    let mut raw: Option<Option<Vec<u8>>> = None;
    let mut syslog: Option<Vec<u8>> = None;
    let mut bq_row: Option<Vec<u8>> = None;

    for dest in dests {
        if !dest.source_kind.accepts(msg.kind) {
            continue;
        }
        if let Some(pool) = dest.pool.as_deref() {
            if msg.pool.as_deref() != Some(pool) {
                continue;
            }
        }
        if !dest.filter.matches(&view) {
            dest.counters.filtered.fetch_add(1, Ordering::Relaxed);
            continue;
        }

        let want_verbatim = dest.verbatim && dest.dest_kind.supports_verbatim(dest.source_kind);
        // Decoding the raw payload is only worth doing for a destination that can use it — a
        // BigQuery-only installation never touches it.
        let raw_bytes = if want_verbatim {
            raw.get_or_insert_with(|| msg.raw_bytes()).as_deref()
        } else {
            None
        };
        let payload = match (want_verbatim, raw_bytes) {
            (true, Some(bytes)) => Some(bytes.to_vec()),
            _ => {
                if want_verbatim {
                    // Asked for byte-exact, got an event with no raw payload (N-1 poller, or a
                    // webhook). Sending the rendered form is better than sending nothing, but it is
                    // counted so the UI can say the promise was downgraded.
                    dest.counters.rendered.fetch_add(1, Ordering::Relaxed);
                }
                match dest.dest_kind {
                    DestKind::SyslogUdp | DestKind::SyslogTcp | DestKind::SyslogTls => Some(
                        syslog
                            .get_or_insert_with(|| render_syslog_5424(msg))
                            .clone(),
                    ),
                    DestKind::SnmpTrapUdp => render_trap_v2c(msg, &dest.community),
                    DestKind::BigQuery => Some(
                        bq_row
                            .get_or_insert_with(|| encode_row(&yagra_forward::event_row(msg)))
                            .clone(),
                    ),
                    // Unreachable: a flow destination's `source_kind` never accepts an `EventKind`,
                    // so it is filtered out above. Dropping is the safe answer if that ever changes.
                    DestKind::FlowUdp => None,
                }
            }
        };
        let Some(payload) = payload else {
            // Only reachable for a trap with no usable identity OID.
            dest.counters.dropped.fetch_add(1, Ordering::Relaxed);
            metrics::counter!("yagra_forward_dropped_total", "reason" => "unrenderable")
                .increment(1);
            continue;
        };

        enqueue(dest, payload);
    }
}

/// Route one relayed flow datagram. Unlike an event it is never re-rendered: the datagram goes out
/// exactly as it arrived, or not at all.
///
/// Decoding happens **once**, and only when some destination actually needs it. Feeding `templates`
/// on every datagram (rather than lazily, on first filtered destination) is deliberate: templates
/// arrive in their own datagrams and are re-sent only every few minutes, so a cache warmed lazily
/// would silently drop everything from an exporter until its next template refresh — minutes of
/// invisible data loss right after an operator adds a filter.
fn dispatch_flow(templates: &mut FlowTemplates, dests: &[LiveDest], dg: &RawFlowDatagram) {
    let mut wanted: Vec<&LiveDest> = Vec::new();
    for dest in dests {
        if dest.source_kind != SourceKind::Flow {
            continue;
        }
        if let Some(pool) = dest.pool.as_deref() {
            if dg.pool.as_deref() != Some(pool) {
                continue;
            }
        }
        wanted.push(dest);
    }
    if wanted.is_empty() {
        return;
    }
    let Some(bytes) = dg.datagram() else {
        // Corrupt base64 on the wire: nothing to relay, and nothing worth logging per datagram.
        for dest in &wanted {
            dest.counters.dropped.fetch_add(1, Ordering::Relaxed);
        }
        metrics::counter!("yagra_forward_dropped_total", "reason" => "undecodable")
            .increment(wanted.len() as u64);
        return;
    };

    // Templates must be learned whether or not anyone filters today (see above), so decode runs for
    // every datagram once a flow destination exists. The records are only *used* when some filter
    // needs them (or when a BigQuery destination needs a row per record).
    let records = decode_records(templates, dg, &bytes);
    // A datagram with template definitions and no flow records is *only* templates. Filtering it is
    // both meaningless (there is no flow data in it to exclude) and harmful: an exporter that
    // refreshes templates in their own record-free datagrams — the usual NetFlow v9 behaviour —
    // would leave a filtered collector holding data sets it can never decode, silently and forever.
    // So it bypasses the filter. Exporters that inline templates in every export instead are already
    // covered, because the datagrams that *do* match carry the template with them.
    let templates_only =
        records.as_ref().is_some_and(Vec::is_empty) && carries_template_set(&bytes);
    let kind = dg.proto.as_str();
    let pool = dg.pool.as_deref();

    for dest in wanted {
        // BigQuery writes a row per record, so its filter is exact: a non-matching record is simply
        // not written. This is the one place the two flow destination kinds genuinely differ, and
        // it is the reason to choose one over the other.
        if dest.dest_kind == DestKind::BigQuery {
            let Some(records) = records.as_ref() else {
                dest.counters.dropped.fetch_add(1, Ordering::Relaxed);
                metrics::counter!("yagra_forward_dropped_total", "reason" => "unfilterable")
                    .increment(1);
                continue;
            };
            for (seq, rec) in records.iter().enumerate().take(MAX_FILTERED_RECORDS) {
                if !dest.filter.is_empty()
                    && !dest.filter.matches(&FilterView::for_flow(
                        dg.exporter_ip,
                        pool,
                        kind,
                        &flow_fields(rec),
                    ))
                {
                    dest.counters.filtered.fetch_add(1, Ordering::Relaxed);
                    continue;
                }
                enqueue(
                    dest,
                    encode_row(&yagra_forward::flow_row(&bq_row(dg, rec, seq))),
                );
            }
            continue;
        }

        if !dest.filter.is_empty() && !templates_only {
            let Some(records) = records.as_ref() else {
                // Undecodable (or a template we have not seen yet): a filter cannot be evaluated, so
                // the honest answer is to drop rather than forward an unfiltered datagram to a
                // destination that asked for a subset.
                dest.counters.dropped.fetch_add(1, Ordering::Relaxed);
                metrics::counter!("yagra_forward_dropped_total", "reason" => "unfilterable")
                    .increment(1);
                continue;
            };
            // Any-record semantics: one matching record carries the whole datagram.
            let hit = records.iter().take(MAX_FILTERED_RECORDS).any(|rec| {
                dest.filter.matches(&FilterView::for_flow(
                    dg.exporter_ip,
                    pool,
                    kind,
                    &flow_fields(rec),
                ))
            });
            if !hit {
                dest.counters.filtered.fetch_add(1, Ordering::Relaxed);
                continue;
            }
        }
        enqueue(dest, bytes.clone());
    }
}

/// Decode a relayed datagram's records with the exporter's templates. `None` when the datagram could
/// not be parsed at all; `Some(vec![])` when it parsed but carried no data records (a template-only
/// export) — the two are different, and only the first makes a filter unevaluable.
///
/// The full parser record is kept rather than the filter's projection of it, because a BigQuery row
/// carries counters and interface context no filter has an operator for.
fn decode_records(
    templates: &mut FlowTemplates,
    dg: &RawFlowDatagram,
    bytes: &[u8],
) -> Option<Vec<RawFlow>> {
    let parsed = match dg.proto {
        RawFlowProto::Netflow => parse_flow_export(templates, dg.exporter_ip, bytes),
        RawFlowProto::Sflow => parse_sflow(bytes),
    };
    match parsed {
        Ok(flows) => Some(flows),
        Err(e) => {
            metrics::counter!("yagra_forward_flow_decode_errors_total").increment(1);
            tracing::debug!(exporter = %dg.exporter_ip, error = %e, "relayed flow datagram did not decode");
            None
        }
    }
}

/// The filterable projection of a decoded record.
const fn flow_fields(f: &RawFlow) -> FlowFields {
    FlowFields {
        src_addr: f.src_ip,
        dst_addr: f.dst_ip,
        proto: f.proto,
        src_port: f.src_port,
        dst_port: f.dst_port,
        src_as: f.src_as,
        dst_as: f.dst_as,
    }
}

/// The BigQuery row view of a decoded record, with its datagram's context.
fn bq_row<'a>(dg: &'a RawFlowDatagram, f: &RawFlow, seq: usize) -> yagra_forward::FlowRow<'a> {
    yagra_forward::FlowRow {
        observed_unix_ms: dg.at_unix_ms,
        exporter_ip: dg.exporter_ip,
        pool: dg.pool.as_deref(),
        export_proto: dg.proto.as_str(),
        src_addr: f.src_ip,
        dst_addr: f.dst_ip,
        src_port: f.src_port,
        dst_port: f.dst_port,
        proto: f.proto,
        tos: f.tos,
        if_index: f.if_index,
        src_as: f.src_as,
        dst_as: f.dst_as,
        bytes: f.bytes,
        packets: f.packets,
        seq,
    }
}

/// Serialize a row for the per-destination queue. Infallible in practice — the value comes from
/// `serde_json`'s own builders — and an empty payload is skipped by the sender if it ever were not.
fn encode_row(row: &Value) -> Vec<u8> {
    serde_json::to_vec(row).unwrap_or_default()
}

/// Hand a rendered payload to a destination's sender without ever awaiting it.
fn enqueue(dest: &LiveDest, payload: Vec<u8>) {
    match dest.tx.try_send(payload) {
        Ok(()) => dest.counters.queue_depth.store(
            (dest.tx.max_capacity() - dest.tx.capacity()) as u64,
            Ordering::Relaxed,
        ),
        Err(mpsc::error::TrySendError::Full(_)) => {
            dest.counters.dropped.fetch_add(1, Ordering::Relaxed);
            metrics::counter!("yagra_forward_dropped_total", "reason" => "queue_full").increment(1);
        }
        Err(mpsc::error::TrySendError::Closed(_)) => {
            dest.counters.dropped.fetch_add(1, Ordering::Relaxed);
            metrics::counter!("yagra_forward_dropped_total", "reason" => "sender_gone")
                .increment(1);
        }
    }
}

// ── Senders ──────────────────────────────────────────────────────────────────────────────────

/// Simple token bucket: `per_sec` tokens refilled continuously, burst capped at one second's worth.
struct RateLimit {
    per_sec: f64,
    tokens: f64,
    last: Instant,
}

impl RateLimit {
    fn new(per_sec: u32) -> Self {
        let per_sec = f64::from(per_sec);
        Self {
            per_sec,
            tokens: per_sec,
            last: Instant::now(),
        }
    }

    fn allow(&mut self) -> bool {
        let now = Instant::now();
        self.tokens = (self.tokens + now.duration_since(self.last).as_secs_f64() * self.per_sec)
            .min(self.per_sec);
        self.last = now;
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

/// Trips after [`CIRCUIT_FAILURES`] consecutive failures and stays open for [`CIRCUIT_OPEN`], so a
/// dead collector costs one probe per window instead of a connect attempt per message.
struct Circuit {
    failures: u32,
    open_until: Option<Instant>,
}

impl Circuit {
    const fn new() -> Self {
        Self {
            failures: 0,
            open_until: None,
        }
    }

    fn is_open(&mut self) -> bool {
        match self.open_until {
            Some(until) if Instant::now() < until => true,
            Some(_) => {
                // Window elapsed: let one message through to probe.
                self.open_until = None;
                false
            }
            None => false,
        }
    }

    fn record(&mut self, ok: bool) -> bool {
        if ok {
            let was_open = self.failures >= CIRCUIT_FAILURES;
            self.failures = 0;
            self.open_until = None;
            return was_open;
        }
        self.failures = self.failures.saturating_add(1);
        if self.failures >= CIRCUIT_FAILURES {
            self.open_until = Some(Instant::now() + CIRCUIT_OPEN);
            return true;
        }
        false
    }
}

/// Everything a sender task needs about its destination. Grouped rather than passed as loose
/// arguments so the transport-shaping config stays one thing.
struct SenderSpec {
    kind: DestKind,
    /// Which stream feeds it — only BigQuery cares (it decides the table schema).
    source_kind: SourceKind,
    target: String,
    name: String,
    rate_limit_per_sec: Option<u32>,
    ca_cert: Option<String>,
    /// Google service-account key JSON for a BigQuery destination. `None` selects Workload Identity.
    /// A credential: held only for this task's lifetime, never logged.
    service_account: Option<String>,
}

async fn run_sender(
    spec: SenderSpec,
    mut rx: mpsc::Receiver<Vec<u8>>,
    counters: Arc<DestCounters>,
    shutdown: CancellationToken,
) {
    let SenderSpec {
        kind,
        target,
        name,
        rate_limit_per_sec,
        ca_cert,
        ..
    } = spec;
    let mut transport = match Transport::new(kind, ca_cert.as_deref()) {
        Ok(t) => t,
        Err(e) => {
            // A TLS destination whose trust configuration cannot be built will never connect, so
            // fail loudly once and drain the queue rather than retrying a hopeless handshake per
            // message. Config edits rebuild the sender, so fixing the CA recovers without a restart.
            counters.record_error(&e);
            counters.circuit_open.store(true, Ordering::Relaxed);
            tracing::warn!(destination = %name, error = %e, "forwarding destination TLS setup failed; not sending");
            while let Some(_payload) = rx.recv().await {
                counters.dropped.fetch_add(1, Ordering::Relaxed);
                metrics::counter!("yagra_forward_dropped_total", "reason" => "tls_config")
                    .increment(1);
            }
            return;
        }
    };
    let mut limiter = rate_limit_per_sec.map(RateLimit::new);
    let mut circuit = Circuit::new();

    loop {
        let payload = tokio::select! {
            biased;
            () = shutdown.cancelled() => break,
            msg = rx.recv() => match msg {
                Some(payload) => payload,
                None => break,
            },
        };
        counters
            .queue_depth
            .store(rx.len() as u64, Ordering::Relaxed);

        if limiter.as_mut().is_some_and(|l| !l.allow()) {
            counters.dropped.fetch_add(1, Ordering::Relaxed);
            metrics::counter!("yagra_forward_dropped_total", "reason" => "rate_limit").increment(1);
            continue;
        }
        if circuit.is_open() {
            counters.dropped.fetch_add(1, Ordering::Relaxed);
            metrics::counter!("yagra_forward_dropped_total", "reason" => "circuit_open")
                .increment(1);
            continue;
        }

        let bytes = payload.len() as u64;
        match transport.send(&target, &payload).await {
            Ok(()) => {
                counters.sent.fetch_add(1, Ordering::Relaxed);
                counters
                    .last_success_unix_ms
                    .store(now_unix_ms(), Ordering::Relaxed);
                metrics::counter!("yagra_forward_sent_total", "kind" => kind.as_str()).increment(1);
                metrics::counter!("yagra_forward_bytes_total", "kind" => kind.as_str())
                    .increment(bytes);
                if circuit.record(true) {
                    counters.circuit_open.store(false, Ordering::Relaxed);
                    tracing::info!(destination = %name, "forwarding destination recovered");
                }
            }
            Err(e) => {
                counters.record_error(&e);
                metrics::counter!("yagra_forward_errors_total", "kind" => kind.as_str())
                    .increment(1);
                transport.reset();
                if circuit.record(false) {
                    counters.circuit_open.store(true, Ordering::Relaxed);
                    // The payload is never logged — syslog bodies carry credentials (security.md).
                    tracing::warn!(destination = %name, error = %e, "forwarding destination failing; pausing sends");
                }
            }
        }
    }
}

/// The BigQuery sender. Unlike [`run_sender`] it accumulates rows and posts them in batches, because
/// `insertAll` is an HTTPS round trip — one request per syslog line would be both far slower than
/// intake and a quota problem.
///
/// A batch goes out when it reaches [`MAX_ROWS_PER_INSERT`] rows, [`MAX_INSERT_BYTES`] bytes, or
/// [`FLUSH_INTERVAL`] elapses — so a quiet destination still lands its rows within seconds rather
/// than waiting for company that may never arrive.
async fn run_bq_sender(
    spec: SenderSpec,
    mut rx: mpsc::Receiver<Vec<u8>>,
    counters: Arc<DestCounters>,
    shutdown: CancellationToken,
) {
    let SenderSpec {
        source_kind,
        target,
        name,
        rate_limit_per_sec,
        service_account,
        ..
    } = spec;
    let mut client = match BigQueryClient::new(&target, service_account.as_deref()) {
        Ok(client) => client,
        Err(e) => {
            // A key that cannot be parsed, or a malformed table name, will never work. Fail loudly
            // once and drain, exactly as an unusable TLS trust configuration does — an edit rebuilds
            // the sender, so fixing it recovers without a restart.
            counters.record_error(&e);
            counters.circuit_open.store(true, Ordering::Relaxed);
            tracing::warn!(destination = %name, error = %e, "BigQuery destination setup failed; not sending");
            while rx.recv().await.is_some() {
                counters.dropped.fetch_add(1, Ordering::Relaxed);
                metrics::counter!("yagra_forward_dropped_total", "reason" => "bq_config")
                    .increment(1);
            }
            return;
        }
    };

    let mut limiter = rate_limit_per_sec.map(RateLimit::new);
    let mut circuit = Circuit::new();
    let mut batch: Vec<Value> = Vec::with_capacity(MAX_ROWS_PER_INSERT);
    let mut batch_bytes = 0usize;
    let mut ticker = tokio::time::interval(FLUSH_INTERVAL);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        let payload = tokio::select! {
            biased;
            () = shutdown.cancelled() => break,
            _ = ticker.tick() => {
                flush_bq(&mut client, source_kind, &mut batch, &mut batch_bytes,
                         &counters, &mut circuit, &name).await;
                continue;
            }
            msg = rx.recv() => match msg {
                Some(payload) => payload,
                None => break,
            },
        };
        counters
            .queue_depth
            .store(rx.len() as u64, Ordering::Relaxed);

        // For a row destination the limiter's unit is the row, which is what an operator setting a
        // ceiling on a BigQuery table means.
        if limiter.as_mut().is_some_and(|l| !l.allow()) {
            counters.dropped.fetch_add(1, Ordering::Relaxed);
            metrics::counter!("yagra_forward_dropped_total", "reason" => "rate_limit").increment(1);
            continue;
        }
        // Dropping while the breaker is open (rather than accumulating) is the point: a dead
        // destination must not grow a batch that will be thrown away anyway.
        if circuit.is_open() {
            counters.dropped.fetch_add(1, Ordering::Relaxed);
            metrics::counter!("yagra_forward_dropped_total", "reason" => "circuit_open")
                .increment(1);
            continue;
        }
        let Ok(row) = serde_json::from_slice::<Value>(&payload) else {
            counters.dropped.fetch_add(1, Ordering::Relaxed);
            metrics::counter!("yagra_forward_dropped_total", "reason" => "unrenderable")
                .increment(1);
            continue;
        };
        batch_bytes += payload.len();
        batch.push(row);
        if batch.len() >= MAX_ROWS_PER_INSERT || batch_bytes >= MAX_INSERT_BYTES {
            flush_bq(
                &mut client,
                source_kind,
                &mut batch,
                &mut batch_bytes,
                &counters,
                &mut circuit,
                &name,
            )
            .await;
        }
    }
    // Land whatever is in hand before the task ends — on a config edit or a graceful shutdown the
    // rows are already accepted, and dropping them would be a silent loss the counters would not
    // even show as a drop.
    flush_bq(
        &mut client,
        source_kind,
        &mut batch,
        &mut batch_bytes,
        &counters,
        &mut circuit,
        &name,
    )
    .await;
}

/// Post one batch, creating the table first if this is the client's first successful call.
async fn flush_bq(
    client: &mut BigQueryClient,
    source_kind: SourceKind,
    batch: &mut Vec<Value>,
    batch_bytes: &mut usize,
    counters: &DestCounters,
    circuit: &mut Circuit,
    name: &str,
) {
    if batch.is_empty() {
        return;
    }
    let rows = batch.len() as u64;
    let bytes = *batch_bytes as u64;
    let result = match client.ensure_table(source_kind).await {
        Ok(()) => client.insert_rows(batch).await,
        Err(e) => Err(e),
    };
    batch.clear();
    *batch_bytes = 0;

    match result {
        Ok(rejected) => {
            let accepted = rows - rejected as u64;
            counters.sent.fetch_add(accepted, Ordering::Relaxed);
            counters
                .last_success_unix_ms
                .store(now_unix_ms(), Ordering::Relaxed);
            metrics::counter!("yagra_forward_sent_total", "kind" => "bigquery").increment(accepted);
            metrics::counter!("yagra_forward_bytes_total", "kind" => "bigquery").increment(bytes);
            metrics::counter!("yagra_bq_rows_inserted_total").increment(accepted);
            if rejected > 0 {
                // The request succeeded; these rows were rejected individually (`skipInvalidRows`),
                // which is a schema problem, not a transport one — so it must not trip the breaker.
                counters
                    .dropped
                    .fetch_add(rejected as u64, Ordering::Relaxed);
                counters.record_error(&format!("BigQuery rejected {rejected} row(s)"));
                metrics::counter!("yagra_bq_insert_errors_total").increment(rejected as u64);
                metrics::counter!("yagra_forward_dropped_total", "reason" => "bq_row_rejected")
                    .increment(rejected as u64);
            }
            if circuit.record(true) {
                counters.circuit_open.store(false, Ordering::Relaxed);
                tracing::info!(destination = %name, "forwarding destination recovered");
            }
        }
        Err(e) => {
            counters.record_error(&e);
            counters.dropped.fetch_add(rows, Ordering::Relaxed);
            metrics::counter!("yagra_forward_errors_total", "kind" => "bigquery").increment(1);
            metrics::counter!("yagra_bq_insert_errors_total").increment(rows);
            metrics::counter!("yagra_forward_dropped_total", "reason" => "send_error")
                .increment(rows);
            if circuit.record(false) {
                counters.circuit_open.store(true, Ordering::Relaxed);
                // Never the rows themselves — they can carry a syslog body (security.md).
                tracing::warn!(destination = %name, error = %e, "forwarding destination failing; pausing sends");
            }
        }
    }
}

/// The socket/connection a sender owns. Kept across messages so UDP does not re-bind and TCP/TLS do
/// not re-connect (or re-handshake) per message; [`Transport::reset`] drops it after an error so the
/// next attempt re-resolves and reconnects.
enum Transport {
    Udp {
        sock: Option<UdpSocket>,
        addr: Option<SocketAddr>,
        resolved_at: Option<Instant>,
    },
    Tcp {
        stream: Option<TcpStream>,
    },
    Tls {
        stream: Option<Box<tokio_rustls::client::TlsStream<TcpStream>>>,
        connector: tokio_rustls::TlsConnector,
    },
}

impl Transport {
    /// Build the transport for `kind`. Fails only for TLS, and only when the trust configuration is
    /// unusable (no system roots readable, or an operator-supplied CA that is not a certificate).
    fn new(kind: DestKind, ca_cert: Option<&str>) -> Result<Self, String> {
        Ok(match kind {
            DestKind::SyslogUdp | DestKind::SnmpTrapUdp | DestKind::FlowUdp => Self::Udp {
                sock: None,
                addr: None,
                resolved_at: None,
            },
            DestKind::SyslogTcp => Self::Tcp { stream: None },
            DestKind::SyslogTls => Self::Tls {
                stream: None,
                connector: tls_connector(ca_cert)?,
            },
            // BigQuery owns no socket — it batches rows over HTTPS through `BigQueryClient`, and
            // both `start_dest` and `send_test` route it there before reaching this.
            DestKind::BigQuery => {
                return Err("BigQuery destinations do not use a socket transport".to_owned())
            }
        })
    }

    fn reset(&mut self) {
        match self {
            Self::Udp {
                sock,
                addr,
                resolved_at,
            } => {
                *sock = None;
                *addr = None;
                *resolved_at = None;
            }
            Self::Tcp { stream } => *stream = None,
            Self::Tls { stream, .. } => *stream = None,
        }
    }

    async fn send(&mut self, target: &str, payload: &[u8]) -> Result<(), String> {
        match self {
            Self::Udp {
                sock,
                addr,
                resolved_at,
            } => {
                let stale = resolved_at.is_none_or(|t| t.elapsed() > RESOLVE_TTL);
                if addr.is_none() || stale {
                    let resolved = resolve(target).await?;
                    // Re-bind when the family changed (a target that moved from A to AAAA).
                    if sock.as_ref().is_none_or(|s| {
                        s.local_addr()
                            .is_ok_and(|l| l.is_ipv4() != resolved.is_ipv4())
                    }) {
                        let bind = if resolved.is_ipv4() {
                            "0.0.0.0:0"
                        } else {
                            "[::]:0"
                        };
                        // A dedicated ephemeral socket, never the listener's — otherwise the
                        // collector would see traffic sourced from port 514/162.
                        *sock = Some(UdpSocket::bind(bind).await.map_err(|e| e.to_string())?);
                    }
                    *addr = Some(resolved);
                    *resolved_at = Some(Instant::now());
                }
                let (Some(sock), Some(addr)) = (sock.as_ref(), *addr) else {
                    return Err("no resolved target".to_owned());
                };
                with_timeout(sock.send_to(payload, addr)).await.map(drop)
            }
            Self::Tcp { stream } => {
                if stream.is_none() {
                    let addr = resolve(target).await?;
                    *stream = Some(
                        with_timeout(TcpStream::connect(addr))
                            .await
                            .map_err(|e| format!("connect: {e}"))?,
                    );
                }
                let Some(conn) = stream.as_mut() else {
                    return Err("no connection".to_owned());
                };
                with_timeout(conn.write_all(&framed(payload))).await
            }
            Self::Tls { stream, connector } => {
                if stream.is_none() {
                    let (host, _) = split_target(target)?;
                    let addr = resolve(target).await?;
                    // The certificate is verified against `host` as written by the operator — for an
                    // IP literal that means the certificate must carry an iPAddress SAN, which is
                    // correct and is why the target's host half (not the resolved address) is used.
                    let server_name = rustls::pki_types::ServerName::try_from(host.clone())
                        .map_err(|_| format!("{host} is not a valid TLS server name"))?;
                    let tcp = with_timeout(TcpStream::connect(addr))
                        .await
                        .map_err(|e| format!("connect: {e}"))?;
                    let tls = with_timeout(connector.connect(server_name, tcp))
                        .await
                        .map_err(|e| format!("tls handshake: {e}"))?;
                    *stream = Some(Box::new(tls));
                }
                let Some(conn) = stream.as_mut() else {
                    return Err("no connection".to_owned());
                };
                // RFC 5425 mandates the same octet-counting framing as RFC 6587, inside the session.
                with_timeout(conn.write_all(&framed(payload))).await
            }
        }
    }
}

/// RFC 6587 / 5425 octet counting: `MSG-LEN SP SYSLOG-MSG`. Non-transparent framing (a trailing
/// newline) would corrupt any message containing one — and device log lines do contain them.
fn framed(payload: &[u8]) -> Vec<u8> {
    let mut out = format!("{} ", payload.len()).into_bytes();
    out.extend_from_slice(payload);
    out
}

/// Split a validated `host:port` target back into its halves. The API rejects anything else at write
/// time, so a failure here means a row written by a different binary.
fn split_target(target: &str) -> Result<(String, u16), String> {
    let (host, port) = if let Some(rest) = target.strip_prefix('[') {
        let (h, rest) = rest
            .split_once(']')
            .ok_or_else(|| format!("malformed target {target}"))?;
        (
            h.to_owned(),
            rest.strip_prefix(':')
                .ok_or_else(|| format!("malformed target {target}"))?,
        )
    } else {
        let (h, p) = target
            .rsplit_once(':')
            .ok_or_else(|| format!("malformed target {target}"))?;
        (h.to_owned(), p)
    };
    let port = port
        .parse()
        .map_err(|_| format!("malformed target {target}"))?;
    Ok((host, port))
}

/// Build the TLS client configuration for a destination: the container's system trust anchors, plus
/// any operator-supplied CA certificate.
///
/// Verification is **never** disabled and there is no flag to disable it (security.md). Forwarding
/// exists to send credential-bearing log bodies to another system; an unauthenticated peer is
/// precisely the failure TLS is here to prevent, so a private CA is configured rather than skipped.
fn tls_connector(ca_cert: Option<&str>) -> Result<tokio_rustls::TlsConnector, String> {
    let mut roots = rustls::RootCertStore::empty();
    let native = rustls_native_certs::load_native_certs();
    for cert in native.certs {
        // Individual malformed anchors in a system bundle are skipped by `add`; that is not fatal.
        let _ = roots.add(cert);
    }
    let mut added = 0usize;
    if let Some(pem) = ca_cert.map(str::trim).filter(|p| !p.is_empty()) {
        use rustls::pki_types::{pem::PemObject, CertificateDer};
        for cert in CertificateDer::pem_slice_iter(pem.as_bytes()) {
            let cert = cert.map_err(|e| format!("CA certificate is not valid PEM: {e}"))?;
            roots
                .add(cert)
                .map_err(|e| format!("CA certificate rejected: {e}"))?;
            added += 1;
        }
        if added == 0 {
            return Err("CA certificate contained no CERTIFICATE block".to_owned());
        }
    }
    if roots.is_empty() {
        return Err(
            "no trust anchors available (no system CA bundle and no CA certificate set)".to_owned(),
        );
    }
    // `ring` is named explicitly: both crypto providers end up enabled in this dependency graph, and
    // with both on rustls installs no process default — `ClientConfig::builder()` would panic.
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(|e| format!("TLS configuration: {e}"))?
    .with_root_certificates(roots)
    .with_no_client_auth();
    Ok(tokio_rustls::TlsConnector::from(Arc::new(config)))
}

async fn with_timeout<T, F>(fut: F) -> Result<T, String>
where
    F: std::future::Future<Output = std::io::Result<T>>,
{
    match tokio::time::timeout(IO_TIMEOUT, fut).await {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(e)) => Err(e.to_string()),
        Err(_) => Err("timed out".to_owned()),
    }
}

async fn resolve(target: &str) -> Result<SocketAddr, String> {
    lookup_host(target)
        .await
        .map_err(|e| format!("resolve {target}: {e}"))?
        .next()
        .ok_or_else(|| format!("resolve {target}: no addresses"))
}

fn now_unix_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

// ── One-shot delivery, for the "Test" button ────────────────────────────────────────────────

/// Send a single synthetic message to `dest` without touching the running senders, so an admin can
/// validate a destination that is still disabled and get the transport error verbatim.
///
/// # Errors
/// Returns the transport error text on resolve/connect/send failure, or when the destination's
/// configuration cannot produce a payload.
pub async fn send_test(dest: &OpenDestination) -> Result<(), String> {
    let msg = test_event(dest.dest.source_kind);
    // BigQuery's test is the most useful of the lot: one round trip proves the credential, the IAM
    // binding, the dataset's existence and the table's schema, all of which are otherwise only
    // discoverable from the status page after enabling the destination.
    if dest.dest.dest_kind == DestKind::BigQuery {
        let key = match &dest.secret {
            Some(DestSecret::GcpServiceAccount { json }) => Some(json.as_str()),
            Some(DestSecret::SnmpCommunity { .. }) | None => None,
        };
        let mut client = BigQueryClient::new(&dest.dest.target, key)?;
        client.ensure_table(dest.dest.source_kind).await?;
        let row = match dest.dest.source_kind {
            SourceKind::Flow => yagra_forward::flow_row(&test_flow_row()),
            SourceKind::Syslog | SourceKind::Trap => yagra_forward::event_row(&msg),
        };
        let rejected = client.insert_rows(std::slice::from_ref(&row)).await?;
        return if rejected == 0 {
            Ok(())
        } else {
            Err("BigQuery accepted the request but rejected the test row".to_owned())
        };
    }

    let community = match &dest.secret {
        Some(DestSecret::SnmpCommunity { community }) => community.as_str(),
        Some(DestSecret::GcpServiceAccount { .. }) | None => DEFAULT_TRAP_COMMUNITY,
    };
    let payload = match dest.dest.dest_kind {
        DestKind::SyslogUdp | DestKind::SyslogTcp | DestKind::SyslogTls => render_syslog_5424(&msg),
        DestKind::SnmpTrapUdp => render_trap_v2c(&msg, community)
            .ok_or_else(|| "could not build a test trap".to_owned())?,
        DestKind::FlowUdp => test_flow_datagram(),
        // Handled above; `Transport::new` would refuse it.
        DestKind::BigQuery => return Err("unreachable BigQuery transport".to_owned()),
    };
    let mut transport = Transport::new(dest.dest.dest_kind, dest.dest.ca_cert.as_deref())?;
    transport.send(&dest.dest.target, &payload).await
}

/// The synthetic flow record the Test button writes to a BigQuery flow table. Documentation
/// addresses (RFC 5737 TEST-NET-1), so the row is obviously a probe in a query.
fn test_flow_row() -> yagra_forward::FlowRow<'static> {
    yagra_forward::FlowRow {
        observed_unix_ms: now_unix_ms(),
        exporter_ip: std::net::Ipv4Addr::new(192, 0, 2, 1).into(),
        pool: None,
        export_proto: "netflow",
        src_addr: std::net::Ipv4Addr::new(192, 0, 2, 1).into(),
        dst_addr: std::net::Ipv4Addr::new(192, 0, 2, 2).into(),
        src_port: 40_000,
        dst_port: 514,
        proto: 6,
        tos: 0,
        if_index: 0,
        src_as: 0,
        dst_as: 0,
        bytes: 1024,
        packets: 1,
        seq: 0,
    }
}

/// A syntactically valid NetFlow v9 export (one template + one data record) for the Test button, so
/// a real collector decodes and displays it rather than counting a malformed packet. Deliberately
/// uses documentation addresses (RFC 5737 TEST-NET-1) so the row is recognisable as a probe.
fn test_flow_datagram() -> Vec<u8> {
    // IPV4_SRC_ADDR(8,4) IPV4_DST_ADDR(12,4) L4_SRC_PORT(7,2) L4_DST_PORT(11,2)
    // PROTOCOL(4,1) IN_BYTES(1,4) IN_PKTS(2,4)
    const FIELDS: [(u16, u16); 7] = [(8, 4), (12, 4), (7, 2), (11, 2), (4, 1), (1, 4), (2, 4)];
    const TEMPLATE_ID: u16 = 256;

    let mut template = Vec::new();
    template.extend_from_slice(&TEMPLATE_ID.to_be_bytes());
    template.extend_from_slice(&(FIELDS.len() as u16).to_be_bytes());
    for (ie, len) in FIELDS {
        template.extend_from_slice(&ie.to_be_bytes());
        template.extend_from_slice(&len.to_be_bytes());
    }
    let mut template_set = Vec::new();
    template_set.extend_from_slice(&0u16.to_be_bytes()); // set id 0 = template set
    template_set.extend_from_slice(&((4 + template.len()) as u16).to_be_bytes());
    template_set.extend_from_slice(&template);

    let mut record = Vec::new();
    record.extend_from_slice(&[192, 0, 2, 1]); // TEST-NET-1
    record.extend_from_slice(&[192, 0, 2, 2]);
    record.extend_from_slice(&40_000u16.to_be_bytes());
    record.extend_from_slice(&514u16.to_be_bytes());
    record.push(6); // TCP
    record.extend_from_slice(&1024u32.to_be_bytes());
    record.extend_from_slice(&1u32.to_be_bytes());
    let mut data_set = Vec::new();
    data_set.extend_from_slice(&TEMPLATE_ID.to_be_bytes());
    data_set.extend_from_slice(&((4 + record.len()) as u16).to_be_bytes());
    data_set.extend_from_slice(&record);

    let secs = u32::try_from(now_unix_ms() / 1000).unwrap_or(0);
    let mut pkt = Vec::new();
    pkt.extend_from_slice(&9u16.to_be_bytes()); // version
    pkt.extend_from_slice(&2u16.to_be_bytes()); // flowset count (template + data)
    pkt.extend_from_slice(&0u32.to_be_bytes()); // sysUpTime
    pkt.extend_from_slice(&secs.to_be_bytes()); // unix seconds
    pkt.extend_from_slice(&0u32.to_be_bytes()); // sequence
    pkt.extend_from_slice(&0u32.to_be_bytes()); // source id
    pkt.extend_from_slice(&template_set);
    pkt.extend_from_slice(&data_set);
    pkt
}

/// The synthetic message the Test button sends. Clearly labelled so it is obvious in a collector.
fn test_event(source: SourceKind) -> EventMsg {
    let kind = match source {
        SourceKind::Trap => EventKind::Trap,
        // Flow never renders from an event — `send_test` builds a real export datagram instead — so
        // the kind here is only a placeholder for that case.
        SourceKind::Syslog | SourceKind::Flow => EventKind::Syslog,
    };
    EventMsg {
        schema_version: yagra_bus::BUS_SCHEMA_VERSION,
        event_id: Uuid::new_v4(),
        kind,
        at_unix_ms: now_unix_ms(),
        source_ip: None,
        pool: None,
        message: "Yagra forwarding test message".to_owned(),
        facility: Some(23),
        syslog_severity: Some(5),
        hostname: Some("yagra".to_owned()),
        app_name: Some("yagra-forward".to_owned()),
        trap_oid: matches!(source, SourceKind::Trap).then(|| "1.3.6.1.6.3.1.1.5.1".to_owned()),
        varbinds: Vec::new(),
        truncated: false,
        raw: None,
        src_port: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use yagra_forward::{Condition, FilterExpr, FilterField, FilterMode, FilterOp};

    // rustls requires no process-wide default provider here: `tls_connector` names `ring` at the
    // call site precisely because both providers are enabled in this dependency graph.

    fn counters() -> Arc<DestCounters> {
        Arc::new(DestCounters::default())
    }

    /// A socket-transport sender spec. `source_kind` and `service_account` only matter to the
    /// BigQuery sender, which has its own tests.
    fn spec(kind: DestKind, target: String, name: &str) -> SenderSpec {
        SenderSpec {
            kind,
            source_kind: SourceKind::Syslog,
            target,
            name: name.to_owned(),
            rate_limit_per_sec: None,
            ca_cert: None,
            service_account: None,
        }
    }

    fn live(
        source_kind: SourceKind,
        dest_kind: DestKind,
        verbatim: bool,
        pool: Option<&str>,
        filter: FilterExpr,
    ) -> (LiveDest, mpsc::Receiver<Vec<u8>>) {
        let (tx, rx) = mpsc::channel::<Vec<u8>>(8);
        let dest = LiveDest {
            id: Uuid::new_v4(),
            name: "test".to_owned(),
            source_kind,
            dest_kind,
            target: "127.0.0.1:1".to_owned(),
            pool: pool.map(str::to_owned),
            verbatim,
            rate_limit_per_sec: None,
            ca_cert: None,
            community: DEFAULT_TRAP_COMMUNITY.to_owned(),
            secret_fp: 0,
            filter: yagra_forward::compile(&filter).unwrap(),
            tx,
            counters: counters(),
            shutdown: CancellationToken::new(),
        };
        (dest, rx)
    }

    fn syslog_event(pool: Option<&str>, raw: Option<&[u8]>) -> EventMsg {
        EventMsg {
            schema_version: yagra_bus::BUS_SCHEMA_VERSION,
            event_id: Uuid::new_v4(),
            kind: EventKind::Syslog,
            at_unix_ms: 1_700_000_000_000,
            source_ip: Some("10.0.0.1".parse().unwrap()),
            pool: pool.map(str::to_owned),
            message: "link down".to_owned(),
            facility: Some(23),
            syslog_severity: Some(4),
            hostname: Some("rtr1".to_owned()),
            app_name: None,
            trap_oid: None,
            varbinds: Vec::new(),
            truncated: false,
            raw: raw.map(yagra_bus::encode_raw),
            src_port: Some(514),
        }
    }

    #[tokio::test]
    async fn verbatim_destination_relays_the_original_bytes_untouched() {
        let original = b"<188>oct 10 12:00:00 rtr1 weird\xff-not-utf8";
        let (dest, mut rx) = live(
            SourceKind::Syslog,
            DestKind::SyslogUdp,
            true,
            None,
            FilterExpr::default(),
        );
        dispatch(&[dest], &syslog_event(None, Some(original)));
        assert_eq!(rx.try_recv().unwrap(), original.to_vec());
    }

    #[tokio::test]
    async fn rendered_destination_sends_a_normalized_line_not_the_original() {
        let original = b"<188>legacy 3164 form";
        let (dest, mut rx) = live(
            SourceKind::Syslog,
            DestKind::SyslogUdp,
            false,
            None,
            FilterExpr::default(),
        );
        let counters = dest.counters.clone();
        dispatch(&[dest], &syslog_event(None, Some(original)));
        let out = rx.try_recv().unwrap();
        assert_ne!(out, original.to_vec());
        assert!(
            out.starts_with(b"<188>1 "),
            "{:?}",
            String::from_utf8_lossy(&out)
        );
        // Rendering was the configured choice, not a downgrade.
        assert_eq!(counters.rendered.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn verbatim_without_a_raw_payload_degrades_to_rendering_and_is_counted() {
        let (dest, mut rx) = live(
            SourceKind::Syslog,
            DestKind::SyslogUdp,
            true,
            None,
            FilterExpr::default(),
        );
        let counters = dest.counters.clone();
        dispatch(&[dest], &syslog_event(None, None));
        assert!(rx.try_recv().is_ok(), "must still forward, just rendered");
        assert_eq!(
            counters.rendered.load(Ordering::Relaxed),
            1,
            "the downgrade has to be visible in the UI"
        );
    }

    #[tokio::test]
    async fn filtered_out_messages_are_counted_and_not_queued() {
        let filter = FilterExpr {
            mode: FilterMode::All,
            conditions: vec![Condition {
                field: FilterField::Severity,
                op: FilterOp::Lte,
                value: "2".to_owned(),
            }],
        };
        let (dest, mut rx) = live(SourceKind::Syslog, DestKind::SyslogUdp, true, None, filter);
        let counters = dest.counters.clone();
        dispatch(&[dest], &syslog_event(None, Some(b"x"))); // severity 4 > 2
        assert!(rx.try_recv().is_err());
        assert_eq!(counters.filtered.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn pool_scope_and_stream_selection_route_the_message() {
        // Wrong pool: skipped entirely (not even counted as filtered — it is not this poller's).
        let (dest, mut rx) = live(
            SourceKind::Syslog,
            DestKind::SyslogUdp,
            true,
            Some("osaka"),
            FilterExpr::default(),
        );
        dispatch(&[dest], &syslog_event(Some("tokyo"), Some(b"x")));
        assert!(rx.try_recv().is_err());

        // A trap destination ignores syslog.
        let (dest, mut rx) = live(
            SourceKind::Trap,
            DestKind::SnmpTrapUdp,
            true,
            None,
            FilterExpr::default(),
        );
        dispatch(&[dest], &syslog_event(None, Some(b"x")));
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn a_full_destination_queue_drops_rather_than_blocking_the_dispatcher() {
        let (tx, rx) = mpsc::channel::<Vec<u8>>(1);
        let dest = LiveDest {
            id: Uuid::new_v4(),
            name: "slow".to_owned(),
            source_kind: SourceKind::Syslog,
            dest_kind: DestKind::SyslogUdp,
            target: "127.0.0.1:1".to_owned(),
            pool: None,
            verbatim: true,
            rate_limit_per_sec: None,
            ca_cert: None,
            community: DEFAULT_TRAP_COMMUNITY.to_owned(),
            secret_fp: 0,
            filter: CompiledFilter::allow_all(),
            tx,
            counters: counters(),
            shutdown: CancellationToken::new(),
        };
        let counters = dest.counters.clone();
        let ev = syslog_event(None, Some(b"x"));
        // The receiver never drains: the second message must be dropped, not awaited.
        for _ in 0..5 {
            dispatch(std::slice::from_ref(&dest), &ev);
        }
        drop(rx);
        assert_eq!(counters.dropped.load(Ordering::Relaxed), 4);
    }

    #[tokio::test]
    async fn verbatim_is_ignored_for_a_pairing_that_cannot_carry_it() {
        // A trap relayed to a syslog collector has to be rendered — a PDU on :514 is undecodable.
        let (dest, mut rx) = live(
            SourceKind::Trap,
            DestKind::SyslogUdp,
            true,
            None,
            FilterExpr::default(),
        );
        let counters = dest.counters.clone();
        let mut ev = syslog_event(None, Some(b"\x30\x82raw pdu"));
        ev.kind = EventKind::Trap;
        ev.trap_oid = Some("1.3.6.1.6.3.1.1.5.3".to_owned());
        dispatch(&[dest], &ev);
        let out = rx.try_recv().unwrap();
        assert!(out.starts_with(b"<"), "must be a syslog line");
        // Not a degradation — this pairing never supported verbatim.
        assert_eq!(counters.rendered.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn circuit_opens_after_consecutive_failures_and_closes_on_success() {
        let mut c = Circuit::new();
        for _ in 0..CIRCUIT_FAILURES - 1 {
            assert!(!c.record(false));
            assert!(!c.is_open());
        }
        assert!(c.record(false), "the threshold failure trips the breaker");
        assert!(c.is_open());
        // A success while open reports the recovery once.
        assert!(c.record(true));
        assert!(!c.is_open());
        assert!(!c.record(true), "no repeat recovery log");
    }

    #[test]
    fn rate_limiter_admits_a_burst_then_throttles() {
        let mut l = RateLimit::new(3);
        assert!(l.allow() && l.allow() && l.allow());
        assert!(!l.allow(), "burst exhausted within the same instant");
    }

    #[tokio::test]
    async fn udp_sender_delivers_bytes_verbatim_from_an_ephemeral_port() {
        let collector = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let addr = collector.local_addr().unwrap();
        let (tx, rx) = mpsc::channel::<Vec<u8>>(8);
        let counters = counters();
        let shutdown = CancellationToken::new();
        tokio::spawn(run_sender(
            spec(DestKind::SyslogUdp, addr.to_string(), "collector"),
            rx,
            counters.clone(),
            shutdown.clone(),
        ));

        let payload = b"<188>1 2023-11-14T22:13:20.123Z rtr1 - - - link down".to_vec();
        tx.send(payload.clone()).await.unwrap();

        let mut buf = [0u8; 512];
        let (n, peer) = tokio::time::timeout(Duration::from_secs(2), collector.recv_from(&mut buf))
            .await
            .expect("collector timed out")
            .unwrap();
        assert_eq!(&buf[..n], &payload[..], "bytes must arrive untouched");
        // A dedicated ephemeral socket, not the listener's — otherwise the collector would see
        // traffic sourced from 514/162 and could mistake Yagra for the originating device.
        assert_ne!(peer.port(), addr.port());
        assert_ne!(peer.port(), 514);

        shutdown.cancel();
        assert_eq!(counters.sent.load(Ordering::Relaxed), 1);
        assert_eq!(counters.errors.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn tcp_sender_uses_rfc6587_octet_counting_so_a_newline_cannot_split_a_message() {
        use tokio::io::AsyncReadExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        // A body containing a newline is exactly what non-transparent framing would corrupt.
        let body = b"<13>1 - - - - - - line\nbreak";
        let expected = format!("{} ", body.len()).into_bytes();
        let want = expected.len() + body.len();
        let accept = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = Vec::new();
            // The peer stays open, so read until the framed length arrives rather than to EOF.
            let mut chunk = [0u8; 128];
            while buf.len() < want {
                let n = sock.read(&mut chunk).await.unwrap();
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&chunk[..n]);
            }
            buf
        });

        let (tx, rx) = mpsc::channel::<Vec<u8>>(8);
        let shutdown = CancellationToken::new();
        tokio::spawn(run_sender(
            spec(DestKind::SyslogTcp, addr.to_string(), "siem"),
            rx,
            counters(),
            shutdown.clone(),
        ));
        tx.send(body.to_vec()).await.unwrap();

        let got = tokio::time::timeout(Duration::from_secs(5), accept)
            .await
            .expect("collector timed out")
            .unwrap();
        let mut framed = expected.clone();
        framed.extend_from_slice(body);
        assert_eq!(got, framed, "{:?}", String::from_utf8_lossy(&got));
        shutdown.cancel();
    }

    #[tokio::test]
    async fn a_failing_destination_trips_the_breaker_without_stalling_the_sender() {
        // An unresolvable target fails deterministically and without touching the network stack's
        // connect timeouts, which differ per platform.
        let (tx, rx) = mpsc::channel::<Vec<u8>>(64);
        let counters = counters();
        let shutdown = CancellationToken::new();
        tokio::spawn(run_sender(
            spec(
                DestKind::SyslogUdp,
                "no-such-host.invalid:514".to_owned(),
                "dead",
            ),
            rx,
            counters.clone(),
            shutdown.clone(),
        ));
        // The sender must keep draining: if it blocked on a dead collector, the channel would fill
        // and these sends would hang instead of completing.
        for _ in 0..20 {
            tokio::time::timeout(Duration::from_secs(5), tx.send(b"x".to_vec()))
                .await
                .expect("sender stalled — a dead collector must not block the queue")
                .unwrap();
        }
        for _ in 0..100 {
            if counters.circuit_open.load(Ordering::Relaxed) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(
            counters.circuit_open.load(Ordering::Relaxed),
            "breaker should have opened after repeated failures (errors={})",
            counters.errors.load(Ordering::Relaxed)
        );
        assert!(counters.errors.load(Ordering::Relaxed) >= u64::from(CIRCUIT_FAILURES));
        assert_eq!(counters.sent.load(Ordering::Relaxed), 0);
        shutdown.cancel();
    }

    #[tokio::test]
    async fn send_test_delivers_a_labelled_probe_and_reports_transport_errors() {
        use crate::forward_store::ForwardDestination;
        let collector = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let addr = collector.local_addr().unwrap();
        let dest = |target: String| OpenDestination {
            dest: ForwardDestination {
                id: Uuid::nil(),
                name: "probe".to_owned(),
                enabled: false,
                source_kind: SourceKind::Syslog,
                dest_kind: DestKind::SyslogUdp,
                target,
                pool: None,
                verbatim: true,
                filter: yagra_forward::FilterExpr::default(),
                rate_limit_per_sec: None,
                ca_cert: None,
                has_secret: false,
            },
            secret: None,
        };

        send_test(&dest(addr.to_string())).await.unwrap();
        let mut buf = [0u8; 512];
        let (n, _) = tokio::time::timeout(Duration::from_secs(2), collector.recv_from(&mut buf))
            .await
            .expect("collector timed out")
            .unwrap();
        let text = String::from_utf8_lossy(&buf[..n]).into_owned();
        assert!(text.contains("Yagra forwarding test message"), "{text}");

        // An unresolvable target reports the transport error to the caller rather than 500ing.
        let err = send_test(&dest("no-such-host.invalid:514".to_owned()))
            .await
            .unwrap_err();
        assert!(!err.is_empty());
    }

    #[tokio::test]
    async fn offer_costs_nothing_until_a_destination_of_that_stream_exists() {
        let (tx, mut rx) = mpsc::channel::<Teed>(4);
        let registry = Arc::new(ForwardRegistry::default());
        let handle = ForwardHandle {
            tx,
            registry: registry.clone(),
            reload: Arc::new(Notify::new()),
        };
        let ev = syslog_event(None, Some(b"x"));
        let dg = raw_flow(nf9_datagram(), None);

        // No destinations: neither stream is even cloned onto the inlet.
        handle.offer(&ev);
        handle.offer_flow(&dg);
        assert!(rx.try_recv().is_err());

        // A syslog destination must not switch on the flow tee — the poller relays flow
        // unconditionally, so a syslog-only installation would otherwise pay for every datagram.
        registry.active_event.store(1, Ordering::Release);
        handle.offer_flow(&dg);
        assert!(rx.try_recv().is_err());
        handle.offer(&ev);
        assert!(matches!(rx.try_recv(), Ok(Teed::Event(_))));

        registry.active_flow.store(1, Ordering::Release);
        handle.offer_flow(&dg);
        assert!(matches!(rx.try_recv(), Ok(Teed::Flow(_))));
    }

    #[test]
    fn test_event_is_labelled_and_renderable_for_both_streams() {
        let syslog = test_event(SourceKind::Syslog);
        assert!(syslog.message.contains("test"));
        assert!(!render_syslog_5424(&syslog).is_empty());
        let trap = test_event(SourceKind::Trap);
        assert!(render_trap_v2c(&trap, DEFAULT_TRAP_COMMUNITY).is_some());
    }

    // ── Flow forwarding (ADR-034 Increment 2) ────────────────────────────────────────────────

    /// A NetFlow v9 export with one template + one data record: 10.0.0.5:40000 → 8.8.8.8:443 TCP,
    /// dst AS 15169.
    fn nf9_datagram() -> Vec<u8> {
        nf9_with(&[(([10, 0, 0, 5], [8, 8, 8, 8]), 443, 6, 15169)])
    }

    /// One test record: `((src, dst), dst_port, proto, dst_as)`. Source port and byte count are
    /// fixed — nothing in these tests turns on them.
    type TestRecord = (([u8; 4], [u8; 4]), u16, u8, u32);

    /// Build a v9 export carrying `records`.
    fn nf9_with(records: &[TestRecord]) -> Vec<u8> {
        nf9_packet(&[&nf9_template_set(), &nf9_data_set(records)])
    }

    // IPV4_SRC(8,4) IPV4_DST(12,4) L4_SRC_PORT(7,2) L4_DST_PORT(11,2) PROTOCOL(4,1)
    // IN_BYTES(1,4) DST_AS(17,4)
    const NF9_FIELDS: [(u16, u16); 7] = [(8, 4), (12, 4), (7, 2), (11, 2), (4, 1), (1, 4), (17, 4)];

    fn nf9_template_set() -> Vec<u8> {
        let mut tmpl = Vec::new();
        tmpl.extend_from_slice(&256u16.to_be_bytes());
        tmpl.extend_from_slice(&(NF9_FIELDS.len() as u16).to_be_bytes());
        for (ie, len) in NF9_FIELDS {
            tmpl.extend_from_slice(&ie.to_be_bytes());
            tmpl.extend_from_slice(&len.to_be_bytes());
        }
        let mut set = Vec::new();
        set.extend_from_slice(&0u16.to_be_bytes()); // set id 0 = template set
        set.extend_from_slice(&((4 + tmpl.len()) as u16).to_be_bytes());
        set.extend_from_slice(&tmpl);
        set
    }

    fn nf9_data_set(records: &[TestRecord]) -> Vec<u8> {
        let mut data = Vec::new();
        for ((src, dst), dport, proto, dst_as) in records {
            data.extend_from_slice(src);
            data.extend_from_slice(dst);
            data.extend_from_slice(&40_000u16.to_be_bytes());
            data.extend_from_slice(&dport.to_be_bytes());
            data.push(*proto);
            data.extend_from_slice(&4096u32.to_be_bytes());
            data.extend_from_slice(&dst_as.to_be_bytes());
        }
        let mut set = Vec::new();
        set.extend_from_slice(&256u16.to_be_bytes());
        set.extend_from_slice(&((4 + data.len()) as u16).to_be_bytes());
        set.extend_from_slice(&data);
        set
    }

    /// Wrap flowsets in a v9 header. Assembling from parts (rather than slicing a full packet at a
    /// magic offset) is what lets the template/data split test stay readable.
    fn nf9_packet(sets: &[&[u8]]) -> Vec<u8> {
        let mut pkt = Vec::new();
        pkt.extend_from_slice(&9u16.to_be_bytes()); // version
        pkt.extend_from_slice(&(sets.len() as u16).to_be_bytes()); // flowset count
        pkt.extend_from_slice(&[0u8; 12]); // sysUpTime + unix secs + sequence
        pkt.extend_from_slice(&7u32.to_be_bytes()); // source id
        for set in sets {
            pkt.extend_from_slice(set);
        }
        pkt
    }

    fn raw_flow(bytes: Vec<u8>, pool: Option<&str>) -> RawFlowDatagram {
        RawFlowDatagram {
            schema_version: yagra_bus::BUS_SCHEMA_VERSION,
            poller_id: "edge-1".to_owned(),
            pool: pool.map(str::to_owned),
            exporter_ip: "192.168.1.1".parse().unwrap(),
            src_port: 51_234,
            proto: RawFlowProto::Netflow,
            at_unix_ms: 1_700_000_000_000,
            bytes: yagra_bus::encode_raw(&bytes),
        }
    }

    fn flow_dest(pool: Option<&str>, filter: FilterExpr) -> (LiveDest, mpsc::Receiver<Vec<u8>>) {
        let (dest, rx) = live(SourceKind::Flow, DestKind::FlowUdp, true, pool, filter);
        (dest, rx)
    }

    fn flow_filter(field: FilterField, op: FilterOp, value: &str) -> FilterExpr {
        FilterExpr {
            mode: FilterMode::All,
            conditions: vec![Condition {
                field,
                op,
                value: value.to_owned(),
            }],
        }
    }

    #[tokio::test]
    async fn unfiltered_flow_destination_relays_the_datagram_untouched() {
        let pkt = nf9_datagram();
        let (dest, mut rx) = flow_dest(None, FilterExpr::default());
        dispatch_flow(
            &mut FlowTemplates::new(),
            &[dest],
            &raw_flow(pkt.clone(), None),
        );
        assert_eq!(rx.try_recv().unwrap(), pkt, "bytes must be untouched");
    }

    #[tokio::test]
    async fn flow_filter_is_any_record_and_relays_the_whole_datagram() {
        // Two records in one export: one to 8.8.8.8, one to 1.1.1.1. A filter naming either one has
        // to carry the whole datagram — records cannot be removed from a template-bound bundle.
        let pkt = nf9_with(&[
            (([10, 0, 0, 5], [8, 8, 8, 8]), 443, 6, 15169),
            (([10, 0, 0, 6], [1, 1, 1, 1]), 53, 17, 13335),
        ]);
        let mut templates = FlowTemplates::new();

        let (dest, mut rx) = flow_dest(
            None,
            flow_filter(FilterField::DstAddr, FilterOp::Eq, "1.1.1.1"),
        );
        dispatch_flow(&mut templates, &[dest], &raw_flow(pkt.clone(), None));
        assert_eq!(
            rx.try_recv().unwrap(),
            pkt,
            "the non-matching 8.8.8.8 record rides along — that is the documented semantics"
        );

        // A filter matching neither record drops the datagram and counts it as filtered.
        let (dest, mut rx) = flow_dest(
            None,
            flow_filter(FilterField::DstAddr, FilterOp::Eq, "9.9.9.9"),
        );
        let counters = dest.counters.clone();
        dispatch_flow(&mut templates, &[dest], &raw_flow(pkt, None));
        assert!(rx.try_recv().is_err());
        assert_eq!(counters.filtered.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn flow_filter_reads_the_exporter_separately_from_the_record_addresses() {
        let pkt = nf9_datagram();
        let mut templates = FlowTemplates::new();
        // `source_ip` is the exporter (192.168.1.1), never a flow endpoint.
        let (dest, mut rx) = flow_dest(
            None,
            flow_filter(FilterField::SourceIp, FilterOp::Eq, "192.168.1.1"),
        );
        dispatch_flow(&mut templates, &[dest], &raw_flow(pkt.clone(), None));
        assert!(rx.try_recv().is_ok());

        let (dest, mut rx) = flow_dest(
            None,
            flow_filter(FilterField::SrcAddr, FilterOp::Eq, "192.168.1.1"),
        );
        dispatch_flow(&mut templates, &[dest], &raw_flow(pkt, None));
        assert!(rx.try_recv().is_err(), "the exporter is not a flow source");
    }

    #[tokio::test]
    async fn a_datagram_that_cannot_be_decoded_is_dropped_only_for_filtered_destinations() {
        // Garbage: an unfiltered destination still gets it (the tee promised "everything"), but a
        // filtered one must not — forwarding an unfiltered datagram to a destination that asked for
        // a subset would be worse than dropping it.
        let junk = vec![0xFFu8, 0x00, 0x13, 0x37];
        let mut templates = FlowTemplates::new();

        let (open, mut open_rx) = flow_dest(None, FilterExpr::default());
        let (picky, mut picky_rx) =
            flow_dest(None, flow_filter(FilterField::DstPort, FilterOp::Eq, "443"));
        let picky_counters = picky.counters.clone();
        dispatch_flow(
            &mut templates,
            &[open, picky],
            &raw_flow(junk.clone(), None),
        );

        assert_eq!(open_rx.try_recv().unwrap(), junk);
        assert!(picky_rx.try_recv().is_err());
        assert_eq!(picky_counters.dropped.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn flow_pool_scope_and_stream_isolation_hold() {
        let pkt = nf9_datagram();
        let mut templates = FlowTemplates::new();

        // Wrong pool: skipped entirely.
        let (dest, mut rx) = flow_dest(Some("osaka"), FilterExpr::default());
        dispatch_flow(
            &mut templates,
            &[dest],
            &raw_flow(pkt.clone(), Some("tokyo")),
        );
        assert!(rx.try_recv().is_err());

        // A syslog destination never receives a flow datagram...
        let (dest, mut rx) = live(
            SourceKind::Syslog,
            DestKind::SyslogUdp,
            true,
            None,
            FilterExpr::default(),
        );
        dispatch_flow(&mut templates, &[dest], &raw_flow(pkt, None));
        assert!(rx.try_recv().is_err());

        // ...and a flow destination never receives an event.
        let (dest, mut rx) = flow_dest(None, FilterExpr::default());
        dispatch(&[dest], &syslog_event(None, Some(b"x")));
        assert!(rx.try_recv().is_err());
    }

    /// The relay carries template sets as their own datagrams, so the dispatcher's cache has to
    /// survive across datagrams or every filtered destination would go permanently silent.
    #[tokio::test]
    async fn templates_learned_from_one_datagram_decode_the_next() {
        let template_only = nf9_packet(&[&nf9_template_set()]);
        let data_only = nf9_packet(&[&nf9_data_set(&[(
            ([10, 0, 0, 5], [8, 8, 8, 8]),
            443,
            6,
            15169,
        )])]);
        let mut templates = FlowTemplates::new();
        let filter = flow_filter(FilterField::DstPort, FilterOp::Eq, "443");

        // Data first, with no template learned yet: undecodable, so a filtered destination drops.
        let (dest, mut rx) = flow_dest(None, filter.clone());
        dispatch_flow(&mut templates, &[dest], &raw_flow(data_only.clone(), None));
        assert!(rx.try_recv().is_err());

        // Feed the template datagram. It carries no records, so no filter can match it — and it is
        // relayed anyway, because the collector needs it to decode anything that follows.
        let (dest, mut rx) = flow_dest(None, filter.clone());
        dispatch_flow(
            &mut templates,
            &[dest],
            &raw_flow(template_only.clone(), None),
        );
        assert_eq!(rx.try_recv().unwrap(), template_only);

        // ...and the same data datagram now decodes and matches.
        let (dest, mut rx) = flow_dest(None, filter);
        dispatch_flow(&mut templates, &[dest], &raw_flow(data_only.clone(), None));
        assert_eq!(rx.try_recv().unwrap(), data_only);
    }

    /// A filter must not be able to starve a collector of the templates it needs to decode what the
    /// filter *does* let through — but it must still exclude the flow data the operator excluded.
    #[tokio::test]
    async fn filtered_flow_destination_gets_templates_but_not_unmatched_records() {
        let template_only = nf9_packet(&[&nf9_template_set()]);
        let matching = nf9_packet(&[&nf9_data_set(&[(
            ([10, 0, 0, 5], [8, 8, 8, 8]),
            443,
            6,
            15169,
        )])]);
        let unmatched = nf9_packet(&[&nf9_data_set(&[(
            ([10, 0, 0, 6], [1, 1, 1, 1]),
            53,
            17,
            13335,
        )])]);
        let mut templates = FlowTemplates::new();
        let (dest, mut rx) =
            flow_dest(None, flow_filter(FilterField::DstPort, FilterOp::Eq, "443"));

        let dests = [dest];
        for pkt in [&template_only, &matching, &unmatched] {
            dispatch_flow(&mut templates, &dests, &raw_flow(pkt.clone(), None));
        }

        assert_eq!(
            rx.try_recv().unwrap(),
            template_only,
            "templates bypass the filter"
        );
        assert_eq!(
            rx.try_recv().unwrap(),
            matching,
            "a matching record still carries its datagram"
        );
        assert!(
            rx.try_recv().is_err(),
            "a datagram with no matching record must still be filtered out"
        );
    }

    /// The escape hatch is templates, not emptiness: a datagram that decodes to no records because
    /// its template is unknown teaches a collector nothing, so the filter still decides.
    #[tokio::test]
    async fn filtered_flow_destination_still_drops_recordless_data_without_templates() {
        let orphan = nf9_packet(&[&nf9_data_set(&[(
            ([10, 0, 0, 5], [8, 8, 8, 8]),
            443,
            6,
            15169,
        )])]);
        let (dest, mut rx) =
            flow_dest(None, flow_filter(FilterField::DstPort, FilterOp::Eq, "443"));
        // Fresh cache: the data set references a template this dispatcher has never seen.
        dispatch_flow(&mut FlowTemplates::new(), &[dest], &raw_flow(orphan, None));
        assert!(rx.try_recv().is_err());
    }

    /// The bypass must not become a filter bypass. Exporters that inline a template set in *every*
    /// export (the real USG on the test bench does) would defeat filtering entirely if carrying
    /// templates were enough — the datagram has to be records-free to skip the filter.
    #[tokio::test]
    async fn an_inline_template_does_not_exempt_a_datagram_that_carries_records() {
        let inline = nf9_with(&[(([10, 0, 0, 6], [1, 1, 1, 1]), 53, 17, 13335)]);
        assert!(
            yagra_ingest::carries_template_set(&inline),
            "fixture must inline a template set"
        );
        let (dest, mut rx) =
            flow_dest(None, flow_filter(FilterField::DstPort, FilterOp::Eq, "443"));
        dispatch_flow(&mut FlowTemplates::new(), &[dest], &raw_flow(inline, None));
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn flow_sender_delivers_the_datagram_over_udp() {
        let collector = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let addr = collector.local_addr().unwrap();
        let (tx, rx) = mpsc::channel::<Vec<u8>>(8);
        let shutdown = CancellationToken::new();
        tokio::spawn(run_sender(
            spec(DestKind::FlowUdp, addr.to_string(), "flowcoll"),
            rx,
            counters(),
            shutdown.clone(),
        ));
        let pkt = nf9_datagram();
        tx.send(pkt.clone()).await.unwrap();

        let mut buf = [0u8; 2048];
        let (n, _) = tokio::time::timeout(Duration::from_secs(2), collector.recv_from(&mut buf))
            .await
            .expect("collector timed out")
            .unwrap();
        assert_eq!(&buf[..n], &pkt[..]);
        shutdown.cancel();
    }

    #[test]
    fn the_test_button_builds_a_decodable_netflow_export() {
        // A collector has to be able to parse the probe, or "delivered" would mean nothing.
        let pkt = test_flow_datagram();
        let mut templates = FlowTemplates::new();
        let flows =
            parse_flow_export(&mut templates, "127.0.0.1".parse().unwrap(), &pkt).expect("parses");
        assert_eq!(flows.len(), 1);
        assert_eq!(
            flows[0].dst_ip,
            "192.0.2.2".parse::<std::net::IpAddr>().unwrap()
        );
        assert_eq!(flows[0].dst_port, 514);
        assert_eq!(flows[0].proto, 6);
    }

    // ── Syslog over TLS (RFC 5425) ───────────────────────────────────────────────────────────

    #[test]
    fn tls_transport_accepts_system_roots_and_rejects_a_bogus_ca() {
        // No CA supplied: the container's system bundle is the trust set.
        assert!(Transport::new(DestKind::SyslogTls, None).is_ok());
        // A CA that is not a certificate must fail at setup, not at handshake time — the sender
        // reports it once and stops, rather than retrying a hopeless connection per message.
        let err = Transport::new(DestKind::SyslogTls, Some("-----BEGIN NONSENSE-----\nzz\n"))
            .err()
            .expect("a non-certificate CA must be rejected at setup");
        assert!(!err.is_empty(), "{err}");
        // Blank/whitespace is treated as "not set" rather than as a parse failure.
        assert!(Transport::new(DestKind::SyslogTls, Some("   ")).is_ok());
        // Non-TLS kinds never touch the trust store.
        assert!(Transport::new(DestKind::SyslogUdp, None).is_ok());
        assert!(Transport::new(DestKind::FlowUdp, None).is_ok());
    }

    #[tokio::test]
    async fn a_tls_destination_never_falls_back_to_plaintext() {
        // A plain TCP listener answers the handshake with nothing usable, so the send must fail —
        // silently downgrading would ship credential-bearing log bodies in the clear.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            // Accept and close without ever speaking TLS.
            let _ = listener.accept().await;
        });
        let mut transport = Transport::new(DestKind::SyslogTls, None).unwrap();
        // A literal address is used rather than `localhost` so the test cannot fail on a host that
        // resolves it to ::1 first; rustls verifies an IP target against an iPAddress SAN.
        let err = transport
            .send(&addr.to_string(), b"<13>1 - - - - - - x")
            .await
            .expect_err("a plaintext peer must not be accepted by a TLS destination");
        assert!(
            err.contains("tls handshake") || err.contains("timed out"),
            "{err}"
        );
    }

    #[test]
    fn octet_counting_frames_the_payload_by_byte_length() {
        // A multi-byte character must be counted in bytes, not chars — a collector reading the
        // count as a char length would desynchronize the stream permanently.
        let body = "<13>1 - - - - - - 日本語".as_bytes();
        let out = framed(body);
        let prefix = format!("{} ", body.len());
        assert!(out.starts_with(prefix.as_bytes()), "{out:?}");
        assert_eq!(out.len(), prefix.len() + body.len());
    }

    #[test]
    fn target_splitting_handles_ipv6_literals() {
        assert_eq!(
            split_target("collector.example.com:6514").unwrap(),
            ("collector.example.com".to_owned(), 6514)
        );
        assert_eq!(
            split_target("[2001:db8::1]:6514").unwrap(),
            ("2001:db8::1".to_owned(), 6514)
        );
        assert!(split_target("no-port").is_err());
    }

    // ── BigQuery destinations (ADR-034 Increment 3) ──────────────────────────────────────────
    //
    // The HTTP contract (auth, table creation, `insertAll` shape) is covered in `crate::bigquery`
    // against a fake Google. What is unique to the forwarder is what it *puts on the queue*, and
    // that is what these cover.

    fn bq_dest(source: SourceKind, filter: FilterExpr) -> (LiveDest, mpsc::Receiver<Vec<u8>>) {
        live(source, DestKind::BigQuery, false, None, filter)
    }

    /// The queued payload, parsed back into the row envelope the sender will batch.
    fn queued_row(rx: &mut mpsc::Receiver<Vec<u8>>) -> Value {
        serde_json::from_slice(&rx.try_recv().expect("a row should be queued")).unwrap()
    }

    #[tokio::test]
    async fn a_bigquery_event_destination_queues_a_row_and_never_the_raw_payload() {
        let (dest, mut rx) = bq_dest(SourceKind::Syslog, FilterExpr::default());
        let ev = syslog_event(None, Some(b"<134>1 - host app - - - secret=hunter2"));
        dispatch(&[dest], &ev);

        let row = queued_row(&mut rx);
        assert_eq!(row["insertId"], serde_json::json!(ev.event_id.to_string()));
        assert_eq!(row["json"]["kind"], serde_json::json!("syslog"));
        assert_eq!(row["json"]["message"], serde_json::json!(ev.message));
        // A BigQuery destination is normalized rows *only* — putting the original bytes in a column
        // would make the credential exposure permanent and queryable off-box.
        let text = row.to_string();
        assert!(!text.contains("hunter2"), "raw payload leaked into the row");
        assert!(!text.contains("\"raw\""), "raw payload leaked into the row");
    }

    #[tokio::test]
    async fn a_bigquery_flow_destination_writes_one_row_per_record() {
        let pkt = nf9_with(&[
            (([10, 0, 0, 5], [8, 8, 8, 8]), 443, 6, 15169),
            (([10, 0, 0, 6], [1, 1, 1, 1]), 53, 17, 13335),
        ]);
        let (dest, mut rx) = bq_dest(SourceKind::Flow, FilterExpr::default());
        dispatch_flow(&mut FlowTemplates::new(), &[dest], &raw_flow(pkt, None));

        let first = queued_row(&mut rx);
        let second = queued_row(&mut rx);
        assert_eq!(first["json"]["dst_addr"], serde_json::json!("8.8.8.8"));
        assert_eq!(second["json"]["dst_addr"], serde_json::json!("1.1.1.1"));
        // Derived ids, so re-sending the datagram de-duplicates rather than doubling the table.
        assert_ne!(first["insertId"], second["insertId"]);
        assert!(
            rx.try_recv().is_err(),
            "only two records were in the export"
        );
    }

    #[tokio::test]
    async fn a_bigquery_flow_filter_is_exact_per_record_unlike_the_relay() {
        // This is the whole reason to choose BigQuery over `flow_udp` for a filtered flow feed:
        // rows are independent, so the non-matching record is simply not written — where the relay
        // has to carry its whole bundle.
        let pkt = nf9_with(&[
            (([10, 0, 0, 5], [8, 8, 8, 8]), 443, 6, 15169),
            (([10, 0, 0, 6], [1, 1, 1, 1]), 53, 17, 13335),
        ]);
        let filter = flow_filter(FilterField::DstAddr, FilterOp::Eq, "1.1.1.1");
        let mut templates = FlowTemplates::new();

        let (bq, mut bq_rx) = bq_dest(SourceKind::Flow, filter.clone());
        let bq_counters = bq.counters.clone();
        let (relay, mut relay_rx) = flow_dest(None, filter);
        dispatch_flow(&mut templates, &[bq, relay], &raw_flow(pkt.clone(), None));

        // BigQuery: exactly the matching record, and the other counted as filtered.
        let row = queued_row(&mut bq_rx);
        assert_eq!(row["json"]["dst_addr"], serde_json::json!("1.1.1.1"));
        assert!(
            bq_rx.try_recv().is_err(),
            "the non-matching record must not be written"
        );
        assert_eq!(bq_counters.filtered.load(Ordering::Relaxed), 1);
        // The relay: the whole datagram, non-matching record included.
        assert_eq!(relay_rx.try_recv().unwrap(), pkt);
    }

    #[tokio::test]
    async fn an_undecodable_datagram_is_dropped_for_bigquery_even_without_a_filter() {
        // A relay can still forward bytes it cannot decode; rows cannot be built from them at all,
        // so an unfiltered BigQuery destination drops where an unfiltered relay forwards.
        let junk = vec![0xFFu8; 32];
        let (bq, mut bq_rx) = bq_dest(SourceKind::Flow, FilterExpr::default());
        let bq_counters = bq.counters.clone();
        let (relay, mut relay_rx) = flow_dest(None, FilterExpr::default());
        dispatch_flow(
            &mut FlowTemplates::new(),
            &[bq, relay],
            &raw_flow(junk.clone(), None),
        );

        assert!(bq_rx.try_recv().is_err());
        assert_eq!(bq_counters.dropped.load(Ordering::Relaxed), 1);
        assert_eq!(relay_rx.try_recv().unwrap(), junk);
    }

    #[test]
    fn only_a_rotated_service_account_key_rebuilds_the_sender() {
        // The community is used by the *dispatcher* when it re-encodes a trap, so editing it must
        // not tear down a working socket. The BigQuery key is held by the *sender*, so rotating it
        // must. Getting this backwards means either a dropped connection per edit or a sender that
        // keeps using a revoked key until core restarts.
        let key = |json: &str| {
            sender_secret_fingerprint(Some(&DestSecret::GcpServiceAccount {
                json: json.to_owned(),
            }))
        };
        assert_eq!(key("{\"a\":1}"), key("{\"a\":1}"));
        assert_ne!(key("{\"a\":1}"), key("{\"a\":2}"));
        // A community is not a sender credential, so it fingerprints as "none" — same as no secret.
        assert_eq!(
            sender_secret_fingerprint(Some(&DestSecret::SnmpCommunity {
                community: "private".to_owned()
            })),
            sender_secret_fingerprint(None)
        );
        // ...and a real key never collides with "none".
        assert_ne!(key("{}"), 0);
    }

    #[tokio::test]
    async fn a_bigquery_sender_that_cannot_be_configured_drains_instead_of_blocking_the_dispatcher()
    {
        // Same contract as an unusable TLS trust store: a destination that can never work must not
        // let its queue back up into the dispatcher.
        let (tx, rx) = mpsc::channel::<Vec<u8>>(4);
        let counters = counters();
        let shutdown = CancellationToken::new();
        tokio::spawn(run_bq_sender(
            SenderSpec {
                kind: DestKind::BigQuery,
                source_kind: SourceKind::Syslog,
                // Not a `project.dataset.table` — the client refuses to build.
                target: "nonsense".to_owned(),
                name: "bq".to_owned(),
                rate_limit_per_sec: None,
                ca_cert: None,
                service_account: None,
            },
            rx,
            counters.clone(),
            shutdown.clone(),
        ));

        for _ in 0..8 {
            // `send` (not `try_send`) is the point: it would hang forever against a stalled sender.
            tokio::time::timeout(Duration::from_secs(2), tx.send(b"{}".to_vec()))
                .await
                .expect("the sender must keep draining")
                .expect("the sender must stay alive");
        }
        // Give the drain loop a moment to account for what it threw away.
        for _ in 0..50 {
            if counters.dropped.load(Ordering::Relaxed) >= 8 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(counters.dropped.load(Ordering::Relaxed) >= 8);
        assert!(counters.circuit_open.load(Ordering::Relaxed));
        assert!(counters
            .last_error
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|e| e.contains("project.dataset.table")));
        shutdown.cancel();
    }
}
