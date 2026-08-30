// SPDX-License-Identifier: AGPL-3.0-only
//! What happens to a poll result after it arrives on the bus (ADR-025).
//!
//! One logical consumer drains `yagra.results`, matches each result **in memory and
//! synchronously** — `AlertManager::observe` does no I/O — and then *hands off* every kind of
//! persistence over a bounded channel: samples to the VictoriaMetrics writers, interface and
//! identity metadata to the PostgreSQL writer, alert transitions to the same writer with an inline
//! fallback. The matcher never blocks on a store, which is what keeps a slow database from
//! becoming a slow poller.
//!
//! Lived in `main.rs` until ADR-090; none of it is part of booting.
//!
//! ## The three tiers, and why they shed differently
//!
//! | queue | on overload | why |
//! |---|---|---|
//! | metrics → VM | sheds the newest | a lost sample is a gap; the next poll refills the series |
//! | metadata → PG | sheds the newest | re-emitted every poll, so it is self-healing |
//! | history → PG | **never sheds** — writes inline instead | the audit trail, and fire→resolve order |
//!
//! ⚠️ **The backfill consumer must never run alert evaluation.** Store-and-forward replays a remote
//! site's buffered results at their original timestamps; feeding those to the matcher would re-fire
//! every dwell-based alert as a flood. It therefore reaches the VM and metadata writers only, and
//! that is the property [`consume_results_backfill`]'s own test pins.

use std::sync::Arc;

use futures::{Stream, StreamExt};
use uuid::Uuid;
use yagra_alert::Alert;
use yagra_bus::PollResult;
use yagra_telemetry::CancellationToken;

use tokio::sync::mpsc::error::TrySendError;

use crate::alerts::AlertManager;
use crate::coordinator::Coordinator;
use crate::history::AlertHistoryStore;
use crate::repo::{self, NodeRepo};
use crate::store::MetricStore;
use crate::{arp, dns_check, l3, l3_routing, meraki, neighbors, scheduler};

/// Bounded queue between the single result matcher and each async batch persist writer (ADR-025,
/// mirroring the event pipeline's ADR-024 split). Like events, sustained overload sheds the newest
/// record rather than blocking the matcher or growing memory unbounded.
pub(crate) const RESULT_PERSIST_CHANNEL_CAP: usize = 8192;
/// Largest batch a result persist writer flushes at once (PG param ceiling: history is 11 cols/row,
/// well under 65535 at this cap; interface/identity use array `unnest`, unbounded by params).
const RESULT_PERSIST_BATCH_MAX: usize = 500;
/// Max poll results whose samples one VictoriaMetrics bulk import POST coalesces.
const VM_BATCH_MAX_RESULTS: usize = 200;
/// Bounded VM spill: batches that failed every retry are held here and retried on the next flush,
/// so a brief VM hiccup rides through. Capped so a *sustained* outage sheds the oldest batch rather
/// than growing memory (best-effort tier, ADR-025 — a shed metric never loses an alert).
const VM_SPILL_MAX_BATCHES: usize = 64;
/// Retry attempts for a VM bulk POST before it spills.
const VM_WRITE_RETRIES: usize = 2;
/// How long a writer waits for its batch to fill before posting a partial one.
///
/// **A bulk import costs about 2 ms before it carries any data at all** (measured at 50,000 nodes
/// x 24 ports: fitting post time against body size across two runs gives ~2.2 ms fixed plus
/// ~8.3 ms/MB). Draining only what is already queued makes that fixed cost dominate — one writer
/// averaged 4,410 samples per POST and four averaged **656**, against a cap of 52,800, and 82% of
/// the four writers' total post time went on per-request overhead rather than on data. Sharding
/// without this shrinks the batches by exactly the factor it multiplies the writers, which is why
/// the split alone did not move the ceiling.
///
/// 100 ms is chosen against the two things it trades between: long enough to collect tens of
/// results per writer at fleet rates, and irrelevant beside a poll interval of 30 s or more.
/// It delays *writing* a sample, never its timestamp — an exposition line carries the poll's own
/// at_unix_ms, so a lingered batch lands at the same place in the series.
const VM_BATCH_LINGER: std::time::Duration = std::time::Duration::from_millis(100);
/// Most VictoriaMetrics writer tasks the metrics tier will run, however many cores it is given
/// or an operator asks for. Past this the per-shard bounds below get small enough that batch
/// coalescing degrades — each shard sees 1/N of the stream, so it takes N times as long to fill
/// a batch — and four already covers a machine whose `run_vm_writer` was the constraint.
const VM_WRITERS_MAX: usize = 4;

/// How many writer tasks to run: `YAGRA_VM_WRITERS` if set, otherwise one per core up to
/// [`VM_WRITERS_MAX`]. A request above the cap is clamped **and logged** — silently ignoring a
/// number an operator typed is worse than refusing it.
fn vm_writer_count() -> usize {
    writer_count_from(
        std::env::var("YAGRA_VM_WRITERS")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|&n| n > 0),
        std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get),
    )
}

/// The decision itself, separated from where its two inputs come from so it can be tested: reading
/// `YAGRA_VM_WRITERS` is a process-global mutation that parallel tests cannot do safely.
fn writer_count_from(requested: Option<usize>, cores: usize) -> usize {
    match requested {
        Some(n) if n > VM_WRITERS_MAX => {
            tracing::warn!(
                requested = n,
                max = VM_WRITERS_MAX,
                "YAGRA_VM_WRITERS above the cap; using the cap"
            );
            VM_WRITERS_MAX
        }
        Some(n) => n,
        None => cores.clamp(1, VM_WRITERS_MAX),
    }
}

/// One shard's share of the metrics queue. **The tier's total is what is bounded, not each
/// shard's**: at 24 ports a queued result is ~21 KB, so giving every shard the full cap would
/// multiply the tier's worst-case memory by the writer count. Dividing keeps `N = 1` identical
/// to the single-writer design, byte for byte.
const fn per_shard_channel_cap(shards: usize) -> usize {
    let n = RESULT_PERSIST_CHANNEL_CAP / shards;
    if n == 0 {
        1
    } else {
        n
    }
}

/// One shard's share of the spill, for the same reason and with the same arithmetic. A spilled
/// batch is the expensive one — up to 200 results — so this is the division that matters most.
const fn per_shard_spill_cap(shards: usize) -> usize {
    let n = VM_SPILL_MAX_BATCHES / shards;
    if n == 0 {
        1
    } else {
        n
    }
}

/// Which writer owns a node's samples.
///
/// Sharding by **node** rather than letting N tasks race one queue is what keeps a series'
/// samples in order: every sample of a series carries that node's id, so one node's batches are
/// always built and posted by the same task, in arrival order. A shared queue would let two
/// batches for the same series be posted concurrently by different tasks.
///
/// The two halves are folded together because the entropy sits in a different half in each id
/// scheme this sees: a v4 UUID is random throughout, while the load rig's ids are a constant
/// namespace tag in the high half and a sequential index in the low one.
fn shard_of(node: yagra_common::NodeId, shards: usize) -> usize {
    if shards <= 1 {
        return 0;
    }
    let v = node.as_uuid().as_u128();
    ((((v >> 64) as u64) ^ (v as u64)) % shards as u64) as usize
}

/// The metrics tier's sender side: one bounded channel per writer task, picked by node.
///
/// Cloneable and cheap — the matcher and the store-and-forward backfill consumer each hold one,
/// exactly as they held one `Sender` before. The modulus is `txs.len()` rather than a stored
/// count so the routing and the set of live channels cannot disagree.
#[derive(Clone)]
pub(crate) struct VmWriters {
    txs: Arc<[tokio::sync::mpsc::Sender<Arc<PollResult>>]>,
}

impl VmWriters {
    /// Hand one result to its shard, shedding it if that shard is full (best-effort tier,
    /// ADR-025). Publishes the tier's **total** queue depth under the name it has always had:
    /// the per-shard breakdown is a separate metric, because a gauge published under one name
    /// with two different label sets is double-counted by any `sum by()` over it.
    fn try_send(&self, result: &Arc<PollResult>) -> Result<(), TrySendError<Arc<PollResult>>> {
        let shard = shard_of(result.node_id, self.txs.len());
        let sent = self.txs[shard].try_send(Arc::clone(result));
        metrics::gauge!("yagra_persist_queue_depth", "stream" => "metrics")
            .set(self.depth() as f64);
        sent
    }

    /// Results queued across every shard.
    fn depth(&self) -> usize {
        self.txs
            .iter()
            .map(|tx| tx.max_capacity().saturating_sub(tx.capacity()))
            .sum()
    }

    /// How many writer tasks this handle feeds.
    #[cfg(test)]
    fn shards(&self) -> usize {
        self.txs.len()
    }

    /// Build a handle over channels the caller already owns, so a test can drain them without
    /// running a writer task.
    #[cfg(test)]
    fn from_senders(txs: Vec<tokio::sync::mpsc::Sender<Arc<PollResult>>>) -> Self {
        Self { txs: txs.into() }
    }
}

/// Start the metrics tier's writer tasks and return the handle that feeds them.
///
/// Leader-only, like everything `spawn_result_ingest` starts. The channel count, the per-shard
/// bounds and the spawn live together here rather than at the call site because they are one
/// decision: a cap divided by a number of shards that no longer matches the number of tasks is
/// the whole failure this arrangement has to avoid.
pub(crate) fn start_vm_writers(
    store: Arc<dyn MetricStore>,
    shutdown: CancellationToken,
) -> VmWriters {
    let shards = vm_writer_count();
    let channel_cap = per_shard_channel_cap(shards);
    let spill_cap = per_shard_spill_cap(shards);
    let mut txs = Vec::with_capacity(shards);
    for shard in 0..shards {
        let (tx, rx) = tokio::sync::mpsc::channel::<Arc<PollResult>>(channel_cap);
        txs.push(tx);
        tokio::spawn(run_vm_writer(
            rx,
            store.clone(),
            shutdown.clone(),
            shard,
            spill_cap,
        ));
    }
    tracing::info!(
        shards,
        channel_cap,
        spill_cap,
        "VictoriaMetrics writers started"
    );
    VmWriters { txs: txs.into() }
}

/// One interface row for the batched metadata upsert (matcher extracts it from the result so the
/// writer re-derives nothing): `(ifindex, if_name, if_alias, if_speed)`.
/// What one poll learned about one interface, before the node id is attached.
///
/// The same struct the repo writes (`repo::InterfaceUpsert`) rather than a local tuple: it holds
/// four `Option<f64>` optical bounds in a row, and a positional form would let a transposed pair
/// paint a receive window around the transmit line with nothing to catch it.
type OwnedIface = repo::InterfaceUpsert;

/// One result's metadata for the async PG writer: discovered interfaces to upsert plus an optional
/// `(vendor, model)` identity classified from `sysDescr`. Shed-able and self-healing — re-emitted on
/// every poll, so a dropped record is re-upserted next cycle.
pub(crate) struct MetaRecord {
    node_id: Uuid,
    interfaces: Vec<OwnedIface>,
    identity: Option<(Option<String>, Option<String>)>,
    /// The DNS resolution chain observed on this poll (DNS monitors only, ADR-033). Same tier as
    /// the fields above: poller-returned structured strings that belong in PostgreSQL, never in the
    /// TSDB.
    dns_chain: Option<yagra_common::DnsChain>,
    /// The CDP/LLDP neighbour set observed on this poll (neighbour walks only, ADR-038). Same tier
    /// again. `None` means no set was observed and nothing is written — never "no neighbours".
    neighbors: Option<yagra_common::NeighborSet>,
    /// The interface addresses observed on this poll (L3 walks only, ADR-043). Same tier again.
    /// `None` means no snapshot was observed and nothing is written — never "no addresses".
    l3: Option<yagra_common::L3Snapshot>,
    /// The ARP/ND cache observed on this poll (ARP walks only, ADR-043 Increment 3). Same tier
    /// again. `None` means no summary was observed and nothing is written — never "no endpoints".
    arp: Option<yagra_common::ArpSummary>,
    /// The routing adjacency observed on this poll (routing walks only, ADR-043 Increment 4). Same
    /// tier again. `None` means nothing was observed and nothing is written — never "no peers".
    routing: Option<yagra_common::RoutingSnapshot>,
}

/// One alert-lifecycle transition for the async PG writer's history batch. Never shed — the matcher
/// falls back to an inline write when this channel is full (the audit trail must not be lost).
pub(crate) struct HistoryRecord {
    alert: Alert,
    resolved: bool,
}

/// Drain poll results off the bus, match them in-memory (single logical consumer), and hand all
/// persistence to the async batch writers over bounded channels (ADR-025). Returns when the stream
/// ends. The matcher does no blocking I/O — `alerts.observe` is synchronous and in-memory, and every
/// persist step is a non-blocking `try_send` (history has an inline fallback).
#[allow(clippy::too_many_arguments)]
pub(crate) async fn consume_results<S>(
    mut results: S,
    alerts: Arc<AlertManager>,
    notify_tx: tokio::sync::mpsc::Sender<crate::alerts::NotifyAction>,
    vm: VmWriters,
    meta_tx: tokio::sync::mpsc::Sender<MetaRecord>,
    history_tx: tokio::sync::mpsc::Sender<HistoryRecord>,
    history: Arc<AlertHistoryStore>,
    stats: Arc<scheduler::SchedulerStats>,
    meraki_inflight: Arc<meraki::MerakiInflight>,
    coordinator: Arc<Coordinator>,
) where
    S: Stream<Item = PollResult> + Unpin,
{
    use tracing::Instrument as _;
    while let Some(result) = results.next().await {
        let result = Arc::new(result);
        // Result-ingest span: child of the poller's poll span (via the result's carried trace
        // context), completing the poll's end-to-end distributed trace. Secret-free fields only.
        let ingest_span = tracing::info_span!(
            "poll.ingest",
            node_id = %result.node_id,
            job_id = %result.job_id,
        );
        yagra_telemetry::set_span_parent(&ingest_span, &result.trace_context);
        ingest_result(
            result,
            &alerts,
            &notify_tx,
            &vm,
            &meta_tx,
            &history_tx,
            &history,
            &stats,
            &meraki_inflight,
            &coordinator,
        )
        .instrument(ingest_span)
        .await;
    }
    tracing::warn!("result stream ended");
}

/// Drain **backfilled** poll results (store-and-forward replay, Phase 3) and persist only their
/// metrics + interface metadata — **never** alert evaluation. A poller replays a partition's buffered
/// results here on reconnect; core imports them to the TSDB at their original `at_unix_ms` so history
/// fills at true time. Re-running the sample-count dwell machine over a stale burst would re-fire
/// resolved alerts, so the alert/notify/history path is deliberately skipped (the separate subject is
/// what routes them here — see [`yagra_bus::subjects::results_backfill`]). Leader-only, like the live
/// consumer (fan-out subscribe; only the leader ingests). Returns when the stream ends.
pub(crate) async fn consume_results_backfill<S>(
    mut results: S,
    vm: VmWriters,
    meta_tx: tokio::sync::mpsc::Sender<MetaRecord>,
) where
    S: Stream<Item = PollResult> + Unpin,
{
    while let Some(result) = results.next().await {
        metrics::counter!("yagra_core_backfill_results_total").increment(1);
        persist_metrics_and_meta(&Arc::new(result), &vm, &meta_tx);
    }
    tracing::warn!("backfill result stream ended");
}

/// Persist a result's observational tiers: metric samples → the VM writer, interface + `sysDescr`
/// identity metadata → the PG writer. Both are best-effort `try_send`s (shed-able, self-healing) and
/// touch **no** alert state, so this runs for backfilled results too (store-and-forward, Phase 3) —
/// the same metrics land at their original timestamp without re-driving the alert machine. Metadata
/// only — names/aliases live in PostgreSQL, joined at query time (ADR-011).
fn persist_metrics_and_meta(
    result: &Arc<PollResult>,
    vm: &VmWriters,
    meta_tx: &tokio::sync::mpsc::Sender<MetaRecord>,
) {
    // Metrics → VM writer. Shed-able: alerts are computed in-memory and never read VM back, so a
    // dropped sample never loses an alert (best-effort observational tier, ADR-025).
    if !result.samples.is_empty() {
        match vm.try_send(result) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                metrics::counter!("yagra_result_metrics_persist_dropped_total", "reason" => "channel_full")
                    .increment(1);
            }
            Err(TrySendError::Closed(_)) => {}
        }
    }

    // Interface metadata + `sysDescr` identity → PG meta writer. Shed-able and self-healing (both are
    // re-emitted every poll). `identify()` is cheap in-memory work; only the PG UPDATE is offloaded.
    let interfaces: Vec<OwnedIface> = result
        .interfaces
        .iter()
        .map(|iface| repo::InterfaceUpsert {
            ifindex: i32::try_from(iface.ifindex.0).unwrap_or(i32::MAX),
            if_name: iface.if_name.clone(),
            if_alias: iface.if_alias.clone(),
            if_speed: iface.if_speed,
            // The enum crosses the bus; the DB column holds its token. `as_str()` is the one
            // conversion, and a test pins it to the serde tag so the two cannot drift.
            if_duplex: iface.if_duplex.map(|d| d.as_str().to_owned()),
            if_type: iface.if_type,
            if_media: iface.if_media.clone(),
            transceiver_model: iface.transceiver_model.clone(),
            rx_power_low_dbm: iface.rx_power_low_dbm,
            rx_power_high_dbm: iface.rx_power_high_dbm,
            tx_power_low_dbm: iface.tx_power_low_dbm,
            tx_power_high_dbm: iface.tx_power_high_dbm,
        })
        .collect();
    let identity = result.sys_descr.as_deref().and_then(|descr| {
        let id = yagra_discovery::identify(descr);
        (id.vendor.is_some() || id.model.is_some()).then_some((id.vendor, id.model))
    });
    // A DNS chain rides the same shed-able meta tier. Dropping one only defers recording a change
    // by a poll — the next observation re-reports the current chain — so the only thing genuinely
    // lost is a transient change that reverts before the next poll.
    let dns_chain = result.dns_chain.clone();
    // Neighbours ride the same shed-able tier for the same reason: dropping one only defers
    // recording a change by a poll, since the next walk re-reports the current set.
    let neighbors = result.neighbors.clone();
    // Interface addresses ride the same shed-able tier, for the same reason again.
    let l3 = result.l3.clone();
    // And the ARP summary, for the fourth time: an endpoint dropped here is re-observed on the next
    // walk, and the endpoint table's `last_seen` simply does not advance in the meantime.
    let arp = result.arp.clone();
    // And the routing adjacency, for the fifth time: a snapshot dropped here is re-observed on the
    // next collection, and the stored one simply does not advance in the meantime.
    let routing = result.routing.clone();
    if !interfaces.is_empty()
        || identity.is_some()
        || dns_chain.is_some()
        || neighbors.is_some()
        || l3.is_some()
        || arp.is_some()
        || routing.is_some()
    {
        let rec = MetaRecord {
            node_id: result.node_id.as_uuid(),
            interfaces,
            identity,
            dns_chain,
            neighbors,
            l3,
            arp,
            routing,
        };
        match meta_tx.try_send(rec) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                metrics::counter!("yagra_result_meta_persist_dropped_total", "reason" => "channel_full")
                    .increment(1);
            }
            Err(TrySendError::Closed(_)) => {}
        }
    }
}

/// Match one poll result on the single in-memory matcher: count it, attribute provenance, then
/// **hand off** persistence — metrics to the VM writer, interface/identity metadata to the PG writer,
/// alert history to the PG writer (inline fallback if full) — and evaluate alerts synchronously
/// (`alerts.observe`, no I/O). The only blocking await is `notify_tx.send` onto the already-bounded
/// notification queue. Split out of [`consume_results`] so the per-result work is one `.instrument`-able
/// unit. Every persist step is best-effort; only alert evaluation and history are loss-free.
#[allow(clippy::too_many_arguments)]
async fn ingest_result(
    result: Arc<PollResult>,
    alerts: &Arc<AlertManager>,
    notify_tx: &tokio::sync::mpsc::Sender<crate::alerts::NotifyAction>,
    vm: &VmWriters,
    meta_tx: &tokio::sync::mpsc::Sender<MetaRecord>,
    history_tx: &tokio::sync::mpsc::Sender<HistoryRecord>,
    history: &Arc<AlertHistoryStore>,
    stats: &Arc<scheduler::SchedulerStats>,
    meraki_inflight: &Arc<meraki::MerakiInflight>,
    coordinator: &Arc<Coordinator>,
) {
    metrics::counter!("yagra_poll_results_total").increment(1);
    stats.record_result();
    // Attribute the result to its producing poller for the Pollers view (provenance only;
    // `None` from a legacy/central poller is simply not counted).
    if let Some(pid) = &result.poller_id {
        coordinator.record_result(pid);
    }
    // Clear this org's Meraki single-flight on the collect's first returning result (all fan-out
    // results share the job id; a no-op for non-Meraki jobs).
    meraki_inflight.complete(result.job_id);

    // End-to-end ingest lag (poll timestamp → matcher entry) — the primary scale health signal.
    if result.at_unix_ms > 0 {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
        metrics::gauge!("yagra_result_ingest_lag_ms")
            .set((now_ms - result.at_unix_ms).max(0) as f64);
    }

    // Metrics → VM writer + interface/identity metadata → PG writer. Shed-able/self-healing and
    // alert-independent, so it's shared with the backfill path (`consume_results_backfill`).
    persist_metrics_and_meta(&result, vm, meta_tx);

    // An observational result (today: the CDP/LLDP neighbour walk, ADR-038) states nothing about
    // the node's reachability, so it must not reach the alert engine at all. `observe` derives
    // liveness from `outcome` on *every* result, and both ways of pretending otherwise are bugs:
    // an hourly walk that timed out would push `Unreachable` into the dwell window ICMP owns and
    // page someone for a healthy device, while a hard-coded `Reachable` would cancel a genuine
    // outage. Same shape as `consume_results_backfill` — persist the observation, touch no alert
    // state. The counters above still run: the result did arrive, from a real poller.
    if result.observational {
        return;
    }

    // Alerts: evaluate synchronously in-memory (never shed — the loss-free matcher core), record each
    // lifecycle transition (batched via `history_tx`, inline fallback), and hand delivery to the
    // notification task (bounded queue) so a slow vendor endpoint can't stall ingest.
    for action in alerts.observe(&result) {
        // Which row an action produces is one rule for the whole crate (ADR-092); what is this
        // path's own is the channel — a roll-up persists nothing, and the eventual real recovery
        // is what records.
        if let Some((alert, resolved)) = crate::alerts::history_row(&action) {
            enqueue_history(history_tx, history, alert, resolved).await;
        }
        if notify_tx.send(action).await.is_err() {
            tracing::debug!("notification channel closed (shutdown); dropping delivery");
        }
    }
}

/// Enqueue an alert-history row for the batch writer; if the (never-shed) channel is full, fall back
/// to an inline write so the audit trail is never lost. A brief synchronous stall under a genuine
/// alert flood is acceptable — no-loss is the invariant, never-block is not (ADR-025).
async fn enqueue_history(
    history_tx: &tokio::sync::mpsc::Sender<HistoryRecord>,
    history: &Arc<AlertHistoryStore>,
    alert: &Alert,
    resolved: bool,
) {
    let rec = HistoryRecord {
        alert: alert.clone(),
        resolved,
    };
    match history_tx.try_send(rec) {
        Ok(()) => {}
        Err(TrySendError::Full(rec)) => {
            metrics::counter!("yagra_result_history_persist_fallback_total").increment(1);
            if let Err(e) = history.record(&rec.alert, rec.resolved).await {
                tracing::warn!(error = %e, "inline alert-history fallback write failed");
            }
        }
        Err(TrySendError::Closed(rec)) => {
            // Shutdown (writer gone): still write inline so the transition isn't lost.
            if let Err(e) = history.record(&rec.alert, rec.resolved).await {
                tracing::debug!(error = %e, "alert-history write on closed channel failed");
            }
        }
    }
}

/// Async VictoriaMetrics batch writer (ADR-025): drains the bounded metrics queue and coalesces many
/// poll results' samples into few bulk import POSTs, off the matcher's hot path. Takes the shutdown
/// token directly (not `spawn_cancellable`) so it can do a best-effort final flush on cancel.
///
/// **How busy this task is** is read from `yagra_vm_import_seconds` (`store.rs`), whose two phases
/// sum to the flush's own cost: while nothing is spilling, that sum over a window *is* this task's
/// occupancy. One task can use one core, so an occupancy near 1.0 means the queue behind it is
/// pinned because this is the constraint — and an occupancy well under 1.0 while the queue is
/// pinned means it is not (ADR-110 Increment 2).
pub(crate) async fn run_vm_writer(
    mut rx: tokio::sync::mpsc::Receiver<Arc<PollResult>>,
    store: Arc<dyn MetricStore>,
    shutdown: CancellationToken,
    shard: usize,
    spill_cap: usize,
) {
    let mut spill: std::collections::VecDeque<Vec<Arc<PollResult>>> =
        std::collections::VecDeque::new();
    let mut buf: Vec<Arc<PollResult>> = Vec::with_capacity(VM_BATCH_MAX_RESULTS);
    loop {
        tokio::select! {
            biased;
            () = shutdown.cancelled() => {
                while let Ok(r) = rx.try_recv() {
                    buf.push(r);
                    if buf.len() >= VM_BATCH_MAX_RESULTS {
                        flush_vm(&store, &mut buf, &mut spill, spill_cap).await;
                    }
                }
                flush_vm(&store, &mut buf, &mut spill, spill_cap).await;
                break;
            }
            first = rx.recv() => {
                match first {
                    None => { flush_vm(&store, &mut buf, &mut spill, spill_cap).await; break; }
                    Some(r) => {
                        buf.push(r);
                        while buf.len() < VM_BATCH_MAX_RESULTS {
                            match rx.try_recv() {
                                Ok(r) => buf.push(r),
                                Err(_) => break,
                            }
                        }
                        // Then wait, briefly, for the rest of a batch. Without this the writer
                        // posts whatever happened to be queued — 2.5 results at fleet rates — and
                        // pays the fixed per-request cost on each (see VM_BATCH_LINGER).
                        // Cancellable: shutdown must not have to wait out the linger.
                        if buf.len() < VM_BATCH_MAX_RESULTS {
                            let linger = tokio::time::sleep(VM_BATCH_LINGER);
                            tokio::pin!(linger);
                            loop {
                                tokio::select! {
                                    biased;
                                    () = shutdown.cancelled() => break,
                                    () = &mut linger => break,
                                    more = rx.recv() => match more {
                                        Some(r) => {
                                            buf.push(r);
                                            if buf.len() >= VM_BATCH_MAX_RESULTS {
                                                break;
                                            }
                                        }
                                        None => break,
                                    },
                                }
                            }
                        }
                        flush_vm(&store, &mut buf, &mut spill, spill_cap).await;
                        // The per-shard depth, under its own name: the tier's total is published
                        // by `VmWriters::try_send`, and one name carrying both would be summed
                        // twice by anything aggregating over it.
                        metrics::gauge!("yagra_vm_writer_queue_depth", "shard" => shard.to_string())
                            .set(rx.len() as f64);
                    }
                }
            }
        }
    }
}

/// Flush one VM batch: retry any spilled batches first (oldest-first), then the fresh batch. A batch
/// that still fails after retries spills (bounded — the oldest is dropped, counted, when full).
async fn flush_vm(
    store: &Arc<dyn MetricStore>,
    buf: &mut Vec<Arc<PollResult>>,
    spill: &mut std::collections::VecDeque<Vec<Arc<PollResult>>>,
    spill_cap: usize,
) {
    let fresh = std::mem::take(buf);
    // Drain spilled batches oldest-first; stop at the first failure (VM still down).
    let mut vm_down = false;
    while let Some(batch) = spill.pop_front() {
        if write_vm_with_retry(store, &batch).await {
            metrics::counter!("yagra_vm_batch_flush_total").increment(1);
        } else {
            spill.push_front(batch);
            vm_down = true;
            break;
        }
    }
    if !fresh.is_empty() {
        if !vm_down && write_vm_with_retry(store, &fresh).await {
            metrics::counter!("yagra_vm_batch_flush_total").increment(1);
        } else {
            if spill.len() >= spill_cap {
                if let Some(dropped) = spill.pop_front() {
                    let n: u64 = dropped.iter().map(|r| r.samples.len() as u64).sum();
                    metrics::counter!("yagra_vm_samples_dropped_total", "reason" => "spill_full")
                        .increment(n);
                }
            }
            spill.push_back(fresh);
        }
    }
    metrics::gauge!("yagra_vm_spill_depth").set(spill.len() as f64);
}

/// Attempt a VM bulk write with bounded retries + short backoff. Returns whether it was accepted.
async fn write_vm_with_retry(store: &Arc<dyn MetricStore>, batch: &[Arc<PollResult>]) -> bool {
    for attempt in 0..=VM_WRITE_RETRIES {
        if store.write_batch(batch).await {
            return true;
        }
        if attempt < VM_WRITE_RETRIES {
            metrics::counter!("yagra_vm_write_retries_total").increment(1);
            tokio::time::sleep(std::time::Duration::from_millis(100 * (attempt as u64 + 1))).await;
        }
    }
    false
}

/// Async PostgreSQL batch writer for result metadata + alert history (ADR-025). Interface upserts and
/// identity fills are best-effort (shed at the matcher); alert history is preserved (inline fallback
/// at the matcher). Batches each stream independently and does a best-effort final flush on shutdown.
/// Where a batch of result metadata lands. One struct rather than four parameters: these are not
/// independent knobs, they are "the stores the metadata tier writes to", and every one of them is
/// needed by both `run_pg_writer` and `flush_meta`.
#[derive(Clone)]
pub(crate) struct MetaStores {
    pub(crate) repo: Arc<NodeRepo>,
    pub(crate) dns: Arc<dns_check::DnsCheckRepo>,
    pub(crate) neighbors: Arc<neighbors::NeighborRepo>,
    pub(crate) l3: Arc<l3::L3Repo>,
    pub(crate) arp: Arc<arp::ArpRepo>,
    pub(crate) routing: Arc<l3_routing::RoutingRepo>,
}

pub(crate) async fn run_pg_writer(
    mut meta_rx: tokio::sync::mpsc::Receiver<MetaRecord>,
    mut history_rx: tokio::sync::mpsc::Receiver<HistoryRecord>,
    stores: MetaStores,
    history: Arc<AlertHistoryStore>,
    shutdown: CancellationToken,
) {
    let mut meta_buf: Vec<MetaRecord> = Vec::with_capacity(RESULT_PERSIST_BATCH_MAX);
    let mut hist_buf: Vec<HistoryRecord> = Vec::with_capacity(RESULT_PERSIST_BATCH_MAX);
    loop {
        tokio::select! {
            biased;
            () = shutdown.cancelled() => {
                while let Ok(m) = meta_rx.try_recv() {
                    meta_buf.push(m);
                    if meta_buf.len() >= RESULT_PERSIST_BATCH_MAX {
                        flush_meta(&stores, &mut meta_buf).await;
                    }
                }
                while let Ok(h) = history_rx.try_recv() {
                    hist_buf.push(h);
                    if hist_buf.len() >= RESULT_PERSIST_BATCH_MAX {
                        flush_history(&history, &mut hist_buf).await;
                    }
                }
                flush_meta(&stores, &mut meta_buf).await;
                flush_history(&history, &mut hist_buf).await;
                break;
            }
            m = meta_rx.recv() => {
                let Some(m) = m else {
                    flush_meta(&stores, &mut meta_buf).await;
                    flush_history(&history, &mut hist_buf).await;
                    break;
                };
                meta_buf.push(m);
                while meta_buf.len() < RESULT_PERSIST_BATCH_MAX {
                    match meta_rx.try_recv() {
                        Ok(m) => meta_buf.push(m),
                        Err(_) => break,
                    }
                }
                flush_meta(&stores, &mut meta_buf).await;
                metrics::gauge!("yagra_persist_queue_depth", "stream" => "meta")
                    .set(meta_rx.len() as f64);
            }
            h = history_rx.recv() => {
                let Some(h) = h else {
                    flush_meta(&stores, &mut meta_buf).await;
                    flush_history(&history, &mut hist_buf).await;
                    break;
                };
                hist_buf.push(h);
                while hist_buf.len() < RESULT_PERSIST_BATCH_MAX {
                    match history_rx.try_recv() {
                        Ok(h) => hist_buf.push(h),
                        Err(_) => break,
                    }
                }
                flush_history(&history, &mut hist_buf).await;
                metrics::gauge!("yagra_persist_queue_depth", "stream" => "history")
                    .set(history_rx.len() as f64);
            }
        }
    }
}

/// Flush buffered metadata: coalesce every buffered result's interfaces into one cross-node upsert
/// and every identity into one cross-node fill. Best-effort — a DB error is logged, never propagated.
async fn flush_meta(stores: &MetaStores, buf: &mut Vec<MetaRecord>) {
    let MetaStores {
        repo,
        dns,
        neighbors,
        l3,
        arp,
        routing,
    } = stores;
    if buf.is_empty() {
        return;
    }
    let count = buf.len() as u64;
    let mut iface_rows: Vec<repo::InterfaceBatchRow> = Vec::new();
    let mut ident_rows: Vec<(Uuid, Option<String>, Option<String>)> = Vec::new();
    let mut dns_rows: Vec<(Uuid, yagra_common::DnsChain)> = Vec::new();
    let mut neighbor_rows: Vec<(Uuid, yagra_common::NeighborSet)> = Vec::new();
    let mut l3_rows: Vec<(Uuid, yagra_common::L3Snapshot)> = Vec::new();
    let mut arp_rows: Vec<(Uuid, yagra_common::ArpSummary)> = Vec::new();
    let mut routing_rows: Vec<(Uuid, yagra_common::RoutingSnapshot)> = Vec::new();
    for rec in buf.drain(..) {
        for iface in rec.interfaces {
            iface_rows.push((rec.node_id, iface));
        }
        if let Some((vendor, model)) = rec.identity {
            ident_rows.push((rec.node_id, vendor, model));
        }
        if let Some(chain) = rec.dns_chain {
            dns_rows.push((rec.node_id, chain));
        }
        if let Some(set) = rec.neighbors {
            neighbor_rows.push((rec.node_id, set));
        }
        if let Some(snapshot) = rec.l3 {
            l3_rows.push((rec.node_id, snapshot));
        }
        if let Some(summary) = rec.arp {
            arp_rows.push((rec.node_id, summary));
        }
        if let Some(snapshot) = rec.routing {
            routing_rows.push((rec.node_id, snapshot));
        }
    }
    if !iface_rows.is_empty() {
        if let Err(e) = repo.upsert_interfaces_batch(&iface_rows).await {
            tracing::warn!(error = %e, "batch interface upsert failed");
        }
    }
    if !ident_rows.is_empty() {
        if let Err(e) = repo.fill_node_identity_batch(&ident_rows).await {
            tracing::warn!(error = %e, "batch node-identity fill failed");
        }
    }
    // One statement per observation, in arrival order. Deliberately NOT coalesced per node the way
    // interfaces are: interfaces are idempotent current state, but DNS observations are a
    // *sequence*, and collapsing two of them for one node would erase a real A→B→A transition. The
    // loop is bounded by the number of DNS monitors in the batch, which is tens, not thousands.
    for (node_id, chain) in &dns_rows {
        if let Err(e) = dns.record_observation(*node_id, chain).await {
            tracing::warn!(node = %node_id, error = %e, "dns chain observation failed");
        }
    }
    if !dns_rows.is_empty() {
        metrics::counter!("yagra_dns_chain_persisted_total").increment(dns_rows.len() as u64);
    }
    // Also one statement per observation and deliberately NOT coalesced per node, for the same
    // reason as the DNS loop above: adjacency observations are a *sequence*, and collapsing two of
    // them for one node would erase a real A→B→A transition. Bounded by the number of neighbour
    // walks in the batch, which at an hourly cadence is a trickle even at fleet scale.
    for (node_id, set) in &neighbor_rows {
        if let Err(e) = neighbors.record_observation(*node_id, set).await {
            tracing::warn!(node = %node_id, error = %e, "neighbour observation failed");
        }
    }
    if !neighbor_rows.is_empty() {
        metrics::counter!("yagra_neighbors_persisted_total").increment(neighbor_rows.len() as u64);
    }
    // One statement per observation and deliberately NOT coalesced per node, for the third time and
    // the same reason: address observations are a *sequence*, and collapsing two of them for one
    // node would erase a real A→B→A transition — an interface that was renumbered and put back.
    for (node_id, snapshot) in &l3_rows {
        if let Err(e) = l3.record_observation(*node_id, snapshot).await {
            tracing::warn!(node = %node_id, error = %e, "interface-address observation failed");
        }
    }
    if !l3_rows.is_empty() {
        metrics::counter!("yagra_l3_persisted_total").increment(l3_rows.len() as u64);
    }
    // ARP is the one member of this tier whose observations are *not* a sequence — an ARP cache is
    // current state and the previous read is worthless — but it is still written one statement per
    // observation, because two summaries in one batch belong to two different nodes and coalescing
    // buys nothing at the volume an hourly-or-slower walk produces.
    for (node_id, summary) in &arp_rows {
        if let Err(e) = arp.record_observation(*node_id, summary).await {
            tracing::warn!(node = %node_id, error = %e, "ARP observation failed");
        }
    }
    if !arp_rows.is_empty() {
        metrics::counter!("yagra_arp_persisted_total").increment(arp_rows.len() as u64);
    }
    // Routing adjacency is current state like ARP, not a sequence, and is written the same way and
    // for the same reason.
    for (node_id, snapshot) in &routing_rows {
        if let Err(e) = routing.record_observation(*node_id, snapshot).await {
            tracing::warn!(node = %node_id, error = %e, "routing adjacency observation failed");
        }
    }
    if !routing_rows.is_empty() {
        metrics::counter!("yagra_routing_persisted_total").increment(routing_rows.len() as u64);
    }
    metrics::counter!("yagra_result_meta_persisted_total").increment(count);
}

/// Flush buffered alert-history transitions in one multi-row INSERT. Best-effort at this layer — the
/// matcher already guaranteed no-loss via its inline fallback when the channel was full.
async fn flush_history(history: &Arc<AlertHistoryStore>, buf: &mut Vec<HistoryRecord>) {
    if buf.is_empty() {
        return;
    }
    let rows: Vec<(Alert, bool)> = buf.drain(..).map(|r| (r.alert, r.resolved)).collect();
    match history.record_batch(&rows).await {
        Ok(n) => metrics::counter!("yagra_result_history_persisted_total").increment(n),
        Err(e) => tracing::warn!(error = %e, "batch alert-history insert failed"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{self, MetricPoint};
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicBool, Ordering};
    use yagra_bus::{CheckOutcome, Sample};
    use yagra_common::{NodeId, SeriesKey};

    #[tokio::test]
    async fn backfill_consumer_persists_metrics_and_meta_only() {
        // Store-and-forward (Phase 3): a backfilled result reaches the VM + meta writers (so history
        // fills at its original timestamp) but the backfill consumer has no access to alert state at
        // all — replaying a stale burst can never re-fire resolved alerts.
        use yagra_bus::DiscoveredInterface;
        use yagra_common::IfIndex;
        let (metrics_tx, mut metrics_rx) = tokio::sync::mpsc::channel::<Arc<PollResult>>(8);
        let vm = VmWriters::from_senders(vec![metrics_tx]);
        let (meta_tx, mut meta_rx) = tokio::sync::mpsc::channel::<MetaRecord>(8);
        let result = PollResult {
            job_id: uuid::Uuid::nil(),
            node_id: NodeId::from(uuid::Uuid::nil()),
            at_unix_ms: 1_000, // deliberately ancient — must NOT drive any "now" alert logic
            outcome: CheckOutcome::Reachable,
            samples: vec![Sample::gauge("icmp_rtt_ms", 3.0)],
            interfaces: vec![DiscoveredInterface {
                ifindex: IfIndex(1),
                if_name: Some("eth0".into()),
                if_alias: None,
                if_speed: None,
                if_duplex: None,
                if_type: None,
                if_media: None,
                transceiver_model: None,
                rx_power_low_dbm: None,
                rx_power_high_dbm: None,
                tx_power_low_dbm: None,
                tx_power_high_dbm: None,
            }],
            sys_descr: None,
            dns_chain: None,
            neighbors: None,
            l3: None,
            arp: None,
            routing: None,
            observational: false,
            poller_id: Some("edge-1".into()),
            trace_context: Default::default(),
        };
        consume_results_backfill(futures::stream::iter(vec![result]), vm, meta_tx).await;
        assert!(
            metrics_rx.try_recv().is_ok(),
            "backfilled metrics reach the VM writer"
        );
        assert!(
            meta_rx.try_recv().is_ok(),
            "backfilled interface metadata reaches the PG writer"
        );
    }

    /// One result through `ingest_result`, returning everything the alert engine produced.
    ///
    /// Built with a real `AlertManager` rather than a fake because the property under test is a
    /// property of the engine's own liveness machine — a stub would just assert the stub.
    async fn drive_ingest(results: Vec<PollResult>) -> Vec<crate::alerts::NotifyAction> {
        let alerts = Arc::new(AlertManager::new());
        // Up/down alerting is rule-driven (ADR-075), so a bare manager commits state and pages
        // nobody. Install what `repo.rs` seeds, or every liveness assertion below would pass for
        // the wrong reason — "no Fire" is what a missing rule and a working suppression look like.
        alerts.set_config(crate::alerts::AlertConfig::new(
            vec![crate::alerts::seeded_liveness_rule()],
            std::collections::HashMap::new(),
        ));
        let (notify_tx, mut notify_rx) = tokio::sync::mpsc::channel(64);
        let (metrics_tx, _metrics_rx) = tokio::sync::mpsc::channel::<Arc<PollResult>>(64);
        let vm = VmWriters::from_senders(vec![metrics_tx]);
        let (meta_tx, _meta_rx) = tokio::sync::mpsc::channel::<MetaRecord>(64);
        let (history_tx, _history_rx) = tokio::sync::mpsc::channel::<HistoryRecord>(64);
        // The history store is never touched: the channel is wide enough that `enqueue_history`
        // never takes its inline-write fallback. `connect_lazy` gives a handle that connects to
        // nothing (the same trick `events/engine.rs`'s planner tests use).
        let history = Arc::new(AlertHistoryStore::new(
            sqlx::postgres::PgPoolOptions::new()
                .connect_lazy("postgres://localhost/unused")
                .expect("a lazy pool needs no server"),
        ));
        let stats = Arc::new(scheduler::SchedulerStats::default());
        let inflight = Arc::new(meraki::MerakiInflight::default());
        let coordinator = Arc::new(Coordinator::new(
            Arc::new(yagra_bus::InMemoryBus::new(64)),
            Arc::new(crate::volatile::VolatileStore::disabled()),
            None,
            stats.clone(),
            None,
        ));
        for r in results {
            ingest_result(
                Arc::new(r),
                &alerts,
                &notify_tx,
                &vm,
                &meta_tx,
                &history_tx,
                &history,
                &stats,
                &inflight,
                &coordinator,
            )
            .await;
        }
        drop(notify_tx);
        let mut out = Vec::new();
        while let Ok(a) = notify_rx.try_recv() {
            out.push(a);
        }
        out
    }

    fn observational_result(node: NodeId, outcome: CheckOutcome, at: i64) -> PollResult {
        PollResult {
            job_id: uuid::Uuid::new_v4(),
            node_id: node,
            at_unix_ms: at,
            outcome,
            samples: Vec::new(),
            interfaces: Vec::new(),
            sys_descr: None,
            dns_chain: None,
            neighbors: Some(yagra_common::NeighborSet::default()),
            l3: None,
            arp: None,
            routing: None,
            observational: true,
            poller_id: None,
            trace_context: Default::default(),
        }
    }

    fn liveness_result(node: NodeId, outcome: CheckOutcome, at: i64) -> PollResult {
        PollResult {
            observational: false,
            neighbors: None,
            l3: None,
            arp: None,
            routing: None,
            ..observational_result(node, outcome, at)
        }
    }

    /// ADR-038's safety property, direction 1: a failed hourly neighbour walk must not page anyone.
    /// `observe` derives liveness from `outcome` on every result, so without the branch these three
    /// `Error` results would satisfy the dwell window and fire an alert for a device nothing else
    /// says is down.
    #[tokio::test]
    async fn an_observational_result_never_drives_the_node_down() {
        let node = NodeId::new();
        let failures: Vec<PollResult> = (0..6)
            .map(|i| observational_result(node, CheckOutcome::Error, 1_000 + i))
            .collect();
        assert!(
            drive_ingest(failures).await.is_empty(),
            "a neighbour walk that failed is not an outage"
        );
    }

    /// Direction 2, the one that would be missed by only testing direction 1: hard-coding
    /// `Reachable` on the neighbour result instead of skipping the engine would *cancel* a real
    /// outage, because a healthy-looking sample lands in the same dwell window ICMP is filling.
    #[tokio::test]
    async fn an_observational_result_cannot_cancel_a_real_outage() {
        let node = NodeId::new();
        // Real ICMP failures, interleaved with successful neighbour walks.
        let mut stream = Vec::new();
        for i in 0..6 {
            stream.push(liveness_result(
                node,
                CheckOutcome::Unreachable,
                1_000 + i * 2,
            ));
            stream.push(observational_result(
                node,
                CheckOutcome::Reachable,
                1_001 + i * 2,
            ));
        }
        let actions = drive_ingest(stream).await;
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, crate::alerts::NotifyAction::Fire(_))),
            "the genuine outage must still fire despite the interleaved observational results"
        );
    }

    /// A [`MetricStore`] whose `write_batch` succeeds or fails on demand, for exercising the VM
    /// writer's retry/spill bookkeeping without a network. The read methods are never hit here.
    struct FakeStore {
        fail: AtomicBool,
        /// Every node whose samples reached this store, in arrival order.
        seen: std::sync::Mutex<Vec<NodeId>>,
    }
    impl FakeStore {
        fn new(fail: bool) -> Self {
            Self {
                fail: AtomicBool::new(fail),
                seen: std::sync::Mutex::new(Vec::new()),
            }
        }
        fn seen_nodes(&self) -> Vec<NodeId> {
            self.seen.lock().expect("seen mutex poisoned").clone()
        }
    }
    #[async_trait::async_trait]
    impl MetricStore for FakeStore {
        async fn write(&self, _result: &PollResult) {}
        async fn write_batch(&self, results: &[Arc<PollResult>]) -> bool {
            if !self.fail.load(Ordering::SeqCst) {
                let mut seen = self.seen.lock().expect("seen mutex poisoned");
                seen.extend(results.iter().map(|r| r.node_id));
            }
            !self.fail.load(Ordering::SeqCst)
        }
        async fn latest(&self, _k: &SeriesKey) -> Option<f64> {
            None
        }
        async fn range(&self, _k: &SeriesKey, _f: i64, _t: i64, _s: u64) -> Vec<MetricPoint> {
            Vec::new()
        }
        async fn rate_range(
            &self,
            _k: &SeriesKey,
            _f: i64,
            _t: i64,
            _s: u64,
            _l: u64,
        ) -> Vec<MetricPoint> {
            Vec::new()
        }
        async fn aggregate_latest(&self, _k: &SeriesKey) -> Option<f64> {
            None
        }
        async fn aggregate_range(
            &self,
            _k: &SeriesKey,
            _f: i64,
            _t: i64,
            _s: u64,
        ) -> Vec<MetricPoint> {
            Vec::new()
        }
        async fn top_nodes(&self, _m: &str, _a: store::TopAgg, _l: usize) -> Vec<(Uuid, f64)> {
            Vec::new()
        }
        async fn top_interfaces(
            &self,
            _m: store::InterfaceTopMetric,
            _a: store::TopAgg,
            _l: usize,
        ) -> Vec<(Uuid, i32, f64)> {
            Vec::new()
        }
        async fn interface_candidates(
            &self,
            _m: store::InterfaceTopMetric,
            _floor_bps: f64,
            _nodes: Option<&[Uuid]>,
        ) -> Option<Vec<(Uuid, i32, f64)>> {
            // Answers, with nothing — the read paths are not what this fake exercises.
            Some(Vec::new())
        }
        async fn fresh_node_ids(&self, _m: &[&str], _w: u64) -> Vec<Uuid> {
            Vec::new()
        }
        async fn interface_delta(
            &self,
            _d: store::DeltaDirection,
            _w: u64,
            _l: usize,
        ) -> Vec<(Uuid, i32, f64)> {
            Vec::new()
        }
        async fn throughput_range(
            &self,
            _f: i64,
            _t: i64,
            _s: u64,
        ) -> (Vec<MetricPoint>, Vec<MetricPoint>) {
            (Vec::new(), Vec::new())
        }
        async fn interface_throughput_range(
            &self,
            _n: Uuid,
            _i: i32,
            _f: i64,
            _t: i64,
            _s: u64,
        ) -> Vec<MetricPoint> {
            Vec::new()
        }
        // `node_series` / `node_metric_names`: the trait defaults (empty) are what this fake wants.
    }

    fn sample_result() -> Arc<PollResult> {
        Arc::new(PollResult {
            job_id: Uuid::nil(),
            node_id: NodeId::new(),
            at_unix_ms: 1,
            outcome: CheckOutcome::Reachable,
            samples: vec![Sample::gauge("icmp_rtt_ms", 9.0)],
            interfaces: Vec::new(),
            sys_descr: None,
            dns_chain: None,
            neighbors: None,
            l3: None,
            arp: None,
            routing: None,
            observational: false,
            poller_id: None,
            trace_context: Default::default(),
        })
    }

    // A failed VM batch is held in the bounded spill and retried; on recovery the spill drains.
    #[tokio::test(start_paused = true)]
    async fn vm_flush_spills_failed_batch_then_drains_on_recovery() {
        let fake = Arc::new(FakeStore::new(true)); // VM "down"
        let store: Arc<dyn MetricStore> = fake.clone();
        let mut spill: VecDeque<Vec<Arc<PollResult>>> = VecDeque::new();

        let mut buf = vec![sample_result()];
        flush_vm(&store, &mut buf, &mut spill, VM_SPILL_MAX_BATCHES).await;
        assert!(buf.is_empty(), "fresh buffer is taken by the flush");
        assert_eq!(spill.len(), 1, "a batch that fails every retry is spilled");

        // Still down: a later flush retries the spilled batch (fails) and keeps it.
        flush_vm(&store, &mut buf, &mut spill, VM_SPILL_MAX_BATCHES).await;
        assert_eq!(spill.len(), 1, "spill retained while VM is down");

        // Recover: the next flush drains the spill.
        fake.fail.store(false, Ordering::SeqCst);
        flush_vm(&store, &mut buf, &mut spill, VM_SPILL_MAX_BATCHES).await;
        assert!(
            spill.is_empty(),
            "spill drains once VM accepts writes again"
        );
    }

    // The spill is bounded: past the cap the oldest batch is dropped rather than growing unbounded.
    #[tokio::test(start_paused = true)]
    async fn vm_flush_bounds_the_spill() {
        let fake = Arc::new(FakeStore::new(true)); // permanently "down"
        let store: Arc<dyn MetricStore> = fake.clone();
        let mut spill: VecDeque<Vec<Arc<PollResult>>> = VecDeque::new();

        for _ in 0..(VM_SPILL_MAX_BATCHES + 5) {
            let mut buf = vec![sample_result()];
            flush_vm(&store, &mut buf, &mut spill, VM_SPILL_MAX_BATCHES).await;
        }
        assert_eq!(
            spill.len(),
            VM_SPILL_MAX_BATCHES,
            "spill never exceeds its bound; the oldest is dropped"
        );
    }

    /// A node's samples must always take the same route (order within a series), and the routes
    /// must be used evenly (a shard nobody reaches is a writer task doing nothing while another
    /// is the constraint again).
    #[test]
    fn shard_of_is_stable_and_spreads() {
        // Both id schemes this sees in production: v4 UUIDs, and the load rig's
        // `Uuid::from_u128(TAG << 64 | i)`, whose high half is a constant.
        const TAG: u128 = 0xF17E_0000_5EED_0000;
        let mut ids: Vec<NodeId> = (0..50_000u128)
            .map(|i| NodeId::from(uuid::Uuid::from_u128((TAG << 64) | i)))
            .collect();
        ids.extend((0..50_000).map(|_| NodeId::new()));
        assert_eq!(ids.len(), 100_000, "the floor: this examined 100,000 ids");

        for shards in [2usize, 3, 4, 8] {
            let mut buckets = vec![0usize; shards];
            for id in &ids {
                let first = shard_of(*id, shards);
                assert!(first < shards, "a shard index must address a real channel");
                assert_eq!(
                    first,
                    shard_of(*id, shards),
                    "the same node must always route to the same writer"
                );
                buckets[first] += 1;
            }
            let even = ids.len() / shards;
            let (lo, hi) = (even * 9 / 10, even * 11 / 10);
            for (shard, &n) in buckets.iter().enumerate() {
                assert!(
                    (lo..=hi).contains(&n),
                    "shards={shards}: shard {shard} took {n} of {} ids, outside +/-10% of {even}",
                    ids.len()
                );
            }
        }
    }

    /// 🚨 The failure this exists for: a writer task that was never started. Nothing about it is
    /// loud — the send succeeds until that shard's channel fills, then every result for the
    /// nodes that hash to it is shed forever, while the other shards look perfectly healthy.
    /// So drive one result **per shard** all the way to the store and demand all of them arrive.
    #[tokio::test]
    async fn every_shard_has_a_running_writer() {
        let fake = Arc::new(FakeStore::new(false));
        let store: Arc<dyn MetricStore> = fake.clone();
        let shutdown = CancellationToken::new();
        let vm = start_vm_writers(store, shutdown.clone());
        let shards = vm.shards();
        assert!(shards >= 1, "the floor: at least one writer was started");

        // One node per shard, found by search — the ids are opaque, so the routing decides.
        let mut per_shard: Vec<Option<NodeId>> = vec![None; shards];
        while per_shard.iter().any(Option::is_none) {
            let node = NodeId::new();
            per_shard[shard_of(node, shards)].get_or_insert(node);
        }

        for node in per_shard.iter().flatten() {
            let mut r = (*sample_result()).clone();
            r.node_id = *node;
            vm.try_send(&Arc::new(r)).expect("a fresh shard has room");
        }

        // Each writer wakes, drains and flushes; give them a moment rather than a fixed sleep.
        for _ in 0..200 {
            if fake.seen_nodes().len() >= shards {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }

        let seen = fake.seen_nodes();
        for (shard, node) in per_shard.iter().enumerate() {
            let node = node.expect("every shard got a node");
            assert!(
                seen.contains(&node),
                "shard {shard} of {shards} never delivered its result: no writer is draining it"
            );
        }
        shutdown.cancel();
    }

    /// Splitting the tier must not multiply what it can hold. The bounds are divided, not
    /// repeated — at 24 ports a queued result is ~21 KB and a spilled batch up to 200 of them,
    /// so `N` copies of the single-writer bounds would be `N` times the tier's worst case.
    #[test]
    fn sharding_does_not_raise_the_metrics_tier_bound() {
        for shards in 1..=VM_WRITERS_MAX {
            assert!(
                per_shard_channel_cap(shards) * shards <= RESULT_PERSIST_CHANNEL_CAP,
                "shards={shards}: queued results across the tier exceed the single-writer bound"
            );
            assert!(
                per_shard_spill_cap(shards) * shards <= VM_SPILL_MAX_BATCHES,
                "shards={shards}: spilled batches across the tier exceed the single-writer bound"
            );
        }
        // And one writer is the old design exactly, so `YAGRA_VM_WRITERS=1` cannot regress.
        assert_eq!(per_shard_channel_cap(1), RESULT_PERSIST_CHANNEL_CAP);
        assert_eq!(per_shard_spill_cap(1), VM_SPILL_MAX_BATCHES);
    }

    /// The linger has to do both halves: hold a partial batch back long enough for the rest of it
    /// to arrive, and never hold it longer than that. Time is paused, so this is deterministic —
    /// the clock only moves when the test moves it.
    #[tokio::test(start_paused = true)]
    async fn a_partial_batch_waits_for_the_linger_and_no_longer() {
        let fake = Arc::new(FakeStore::new(false));
        let store: Arc<dyn MetricStore> = fake.clone();
        let shutdown = CancellationToken::new();
        let (tx, rx) = tokio::sync::mpsc::channel::<Arc<PollResult>>(8);
        tokio::spawn(run_vm_writer(rx, store, shutdown.clone(), 0, 8));

        tx.send(sample_result())
            .await
            .expect("the writer is listening");
        tokio::time::sleep(VM_BATCH_LINGER / 2).await;
        assert!(
            fake.seen_nodes().is_empty(),
            "a one-result batch must wait for company rather than pay a whole POST for itself"
        );

        tokio::time::sleep(VM_BATCH_LINGER).await;
        assert_eq!(
            fake.seen_nodes().len(),
            1,
            "and it must be posted when the linger expires, not held for a batch that never comes"
        );
        shutdown.cancel();
    }

    /// 🚨 The linger is inside the receive arm, so a naive implementation makes shutdown wait it
    /// out — once per writer, on every stop. The cancel branch is `biased` and first for that
    /// reason, and this is what says so.
    #[tokio::test(start_paused = true)]
    async fn shutdown_does_not_wait_out_the_linger() {
        let fake = Arc::new(FakeStore::new(false));
        let store: Arc<dyn MetricStore> = fake.clone();
        let shutdown = CancellationToken::new();
        let (tx, rx) = tokio::sync::mpsc::channel::<Arc<PollResult>>(8);
        tokio::spawn(run_vm_writer(rx, store, shutdown.clone(), 0, 8));

        tx.send(sample_result())
            .await
            .expect("the writer is listening");
        tokio::task::yield_now().await; // let the writer take it and start lingering
        let before = tokio::time::Instant::now();
        shutdown.cancel();
        for _ in 0..50 {
            if !fake.seen_nodes().is_empty() {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(
            fake.seen_nodes().len(),
            1,
            "the pending batch is flushed on cancel"
        );
        assert!(
            before.elapsed() < VM_BATCH_LINGER,
            "cancel must cut the linger short, not wait it out"
        );
    }

    /// How many writers to run, from the two inputs that decide it. The operator's number wins up
    /// to the cap; above it the cap wins and the log says so, because silently running 4 when the
    /// operator asked for 16 is the kind of thing found months later.
    #[test]
    fn writer_count_respects_the_operator_then_the_cores_then_the_cap() {
        assert_eq!(writer_count_from(Some(1), 32), 1, "1 is the old design");
        assert_eq!(
            writer_count_from(Some(3), 2),
            3,
            "the operator's number wins over the cores"
        );
        assert_eq!(
            writer_count_from(Some(64), 32),
            VM_WRITERS_MAX,
            "above the cap is clamped"
        );
        assert_eq!(
            writer_count_from(None, 1),
            1,
            "a one-core box gets one writer"
        );
        assert_eq!(writer_count_from(None, 2), 2, "and a two-core box two");
        assert_eq!(
            writer_count_from(None, 32),
            VM_WRITERS_MAX,
            "cores are capped too"
        );
    }

    /// `yagra_persist_queue_depth{stream="metrics"}` keeps meaning "results waiting for the
    /// metrics tier", which is now a sum rather than one channel's length.
    #[tokio::test]
    async fn queue_depth_is_the_sum_across_shards() {
        let (tx0, _rx0) = tokio::sync::mpsc::channel::<Arc<PollResult>>(8);
        let (tx1, _rx1) = tokio::sync::mpsc::channel::<Arc<PollResult>>(8);
        let vm = VmWriters::from_senders(vec![tx0, tx1]);
        assert_eq!(vm.depth(), 0, "nothing queued yet");

        // Two nodes that land on different shards, so both channels get something.
        let mut per_shard: Vec<Option<NodeId>> = vec![None; 2];
        while per_shard.iter().any(Option::is_none) {
            let node = NodeId::new();
            per_shard[shard_of(node, 2)].get_or_insert(node);
        }
        for node in per_shard.iter().flatten() {
            let mut r = (*sample_result()).clone();
            r.node_id = *node;
            vm.try_send(&Arc::new(r)).expect("room in both shards");
        }
        assert_eq!(vm.depth(), 2, "one queued in each shard, counted once each");
    }
}
