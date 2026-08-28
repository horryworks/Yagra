// SPDX-License-Identifier: AGPL-3.0-only
//! Live poller registry + working-set publisher (ADR-009/020).
//!
//! The coordinator is core's control plane for the distributed poller pool. It owns the
//! authoritative in-memory registry of which pollers are alive (the source of truth, ADR-009),
//! mirrors that liveness into Redis (best-effort observability) and PostgreSQL (durable inventory),
//! and — per pool, per sweep — computes each live poller's **working set** and distributes it as a
//! snapshot + incremental deltas over the bus (ADR-020). Failover is not a special path: when a
//! poller stops beating it drops out of its pool's hash ring, and its nodes reassign to survivors as
//! ordinary upsert/remove deltas on the next reconcile.
//!
//! Self-contained so ADR-016 can later gate the whole thing behind a leader lock without touching
//! the call sites: the scheduler asks [`Coordinator::live_pools`] which pools have a live poller and
//! calls [`Coordinator::reconcile_pool`]; the coordinator never decides legacy-vs-working-set (that
//! is the scheduler's per-pool call) and never publishes a legacy per-node job.
//!
//! Clock is injected everywhere as an [`Instant`] so the registry/failover logic is unit-testable
//! against [`yagra_bus::InMemoryBus`] without a running NATS or real time (mirrors
//! [`crate::meraki::MerakiInflight`]).
//!
//! Concurrency: the registry is a plain [`std::sync::Mutex`] held only for synchronous
//! read/mutate; every bus/Redis/PG await happens *after* the guard is dropped, so no lock is held
//! across `.await` (keeps the futures `Send` and the lock uncontended). Two deliberate refinements
//! keep that mutex off the fleet-scale paths: [`Coordinator::reconcile_pool`] takes it **twice, for
//! O(members) each time**, and does its O(fleet) assignment/fingerprint pass unlocked in between;
//! and the per-poller result counters sit on their own [`RwLock`] because they are bumped once per
//! poll result (~1.7k/s at 50k nodes) and must never queue behind a working-set diff.
//!
//! SECURITY (security.md): a [`JobSpec`] carries the node's **decrypted, inlined** monitoring
//! credential (SNMP community / v3 keys / API token). Working-set messages therefore must **never**
//! be logged by content — every log in this module records node/spec *counts* and poller ids only,
//! never the specs themselves. See the note on [`Coordinator::reconcile_pool`].

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use futures::stream::{Stream, StreamExt};
use uuid::Uuid;
use yagra_bus::{
    CheckSpec, HeartbeatMsg, JobSpec, NodeJobs, SyncBus, SyncMsg, SyncRequest, WorkingSetDelta,
    WorkingSetSnapshot, CAP_HTTP_AUTH, CAP_HTTP_BODY, OFFLINE_AFTER_SECS, SNAPSHOT_CHUNK_NODES,
};
use yagra_common::{HostSample, NodeId};

use crate::pollers::PollerRepo;
use crate::ring::HashRing;
use crate::scheduler::SchedulerStats;
use crate::store::MetricStore;
use crate::volatile::{PollerLiveDoc, VolatileStore};

/// A poller is "offline" once this long has elapsed since its last heartbeat (ADR-009). Sourced
/// from [`yagra_bus::OFFLINE_AFTER_SECS`] so core and poller share one definition.
const OFFLINE_AFTER: Duration = Duration::from_secs(OFFLINE_AFTER_SECS);

/// TTL for the Redis liveness mirror — matches the offline window so a dead poller's doc expires as
/// core forgets it (ADR-004/009).
const REDIS_TTL: Duration = OFFLINE_AFTER;

/// Throttle for the durable PostgreSQL inventory upsert so a 10s heartbeat isn't a write per beat
/// (ADR-009).
const PG_UPSERT_EVERY: Duration = Duration::from_secs(60);

/// A [`std::io::Write`] sink that folds bytes into a hasher, so a value can be fingerprinted by
/// streaming its serialization straight into the hash with no intermediate allocation.
struct HashWriter(std::collections::hash_map::DefaultHasher);

impl std::io::Write for HashWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        use std::hash::Hasher;
        self.0.write(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// A stable in-process fingerprint of a node's working-set specs, used only as the reconcile diff
/// baseline (S16) so core need not keep decrypted credentials resident between sweeps. Equal specs
/// ⇒ equal fingerprint: the specs contain no maps/sets, so their JSON serialization is canonical,
/// and every field serializes (no `skip_serializing_if`), so the fingerprint changes iff the spec
/// set does — exactly what the old `Vec<JobSpec>` equality check gave. The comparison is per node
/// against that same node's stored fingerprint (not all-pairs), so 64 bits is ample — no birthday
/// amplification. Not persisted: a core restart bumps the epoch and re-snapshots every poller, so
/// cross-version hash stability is irrelevant.
///
/// Generic over the element so the reconcile can fingerprint a **borrowed** `&[&JobSpec]` — serde
/// forwards `&T` to `T`, so the bytes fed to the hasher are identical to those of an owned
/// `&[JobSpec]` and the two spellings cannot disagree.
fn specs_fingerprint<T: serde::Serialize>(specs: &[T]) -> u64 {
    use std::hash::Hasher;
    let mut hw = HashWriter(std::collections::hash_map::DefaultHasher::new());
    // Serializing these plain (map-free) types into an infallible writer cannot fail.
    let _ = serde_json::to_writer(&mut hw, specs);
    hw.0.finish()
}

/// Registry entry for one poller (the authoritative liveness/telemetry record, ADR-009).
struct PollerEntry {
    /// Pool this poller serves (from its latest heartbeat / sync request).
    pool: String,
    /// Per-process incarnation — a change means the poller restarted (⇒ resync).
    incarnation: Uuid,
    /// When core last heard from this poller (heartbeat or sync request).
    last_seen: Instant,
    /// Build version from its latest heartbeat.
    version: String,
    /// Highest sync sequence core has *sent* this poller (monotonic per poller across its lifetime
    /// in this core process; snapshots carry an absolute seq the poller adopts wholesale).
    seq: u64,
    /// Whether the next [`Coordinator::reconcile_pool`] must send a full snapshot instead of a delta
    /// (new poller, restart, epoch mismatch, or a heartbeat echo revealing a gap).
    needs_snapshot: bool,
    /// Working-set node count the poller last reported (for the Pollers view).
    working_set_nodes: u32,
    /// Working-set spec count the poller last reported.
    working_set_specs: u32,
    /// Polls in flight the poller last reported.
    inflight: u32,
    /// Total results the poller last reported producing since its start.
    results_total: u64,
    /// The poller host's latest resource sample (CPU/load/mem/disk), from its heartbeat. `None`
    /// until the first beat carrying host telemetry (an N-1 poller never sets it).
    host: Option<HostSample>,
    /// Passive-event listeners it has bound, e.g. `syslog:0.0.0.0:1514` (ADR-024). Used by the
    /// forwarding loop guard (ADR-034) and shown on the Pollers view.
    listeners: Vec<String>,
    /// Capabilities its build advertises, e.g. `raw-capture` (ADR-034). Empty from an N-1 poller,
    /// which is exactly what lets the Forwarding page say "this poller cannot do byte-exact yet".
    caps: Vec<String>,
    /// What its site updater last reported about an upgrade (ADR-051 Inc.4). `None` from a
    /// poller with no site updater and from any N-1 build. Read by `poller_upgrade` to start
    /// an apply the moment the image is local rather than sleeping out the prefetch budget.
    upgrade: Option<yagra_bus::UpgradeReport>,
    /// When core last wrote this poller to the durable PG inventory (throttle bookkeeping).
    last_upserted: Option<Instant>,
}

/// The coordinator's mutable state, guarded by one [`Mutex`].
#[derive(Default)]
struct CoordState {
    /// Live registry: poller id → entry.
    pollers: HashMap<String, PollerEntry>,
    /// Diff baseline for reconcile: per poller, the **fingerprint** of the working set core last
    /// sent it (node → specs fingerprint), *not* the specs themselves (S16). A [`JobSpec`] inlines a
    /// decrypted monitoring credential, so keeping the full spec set resident here would leave
    /// secrets in core's memory sweep-to-sweep and re-clone them on every reconcile. The fingerprint
    /// is all the diff needs ("did this node's spec set change?"); the decrypted specs live only
    /// transiently inside [`Coordinator::reconcile_pool`] while a message is built, then drop.
    published: HashMap<String, HashMap<NodeId, u64>>,
}

/// A point-in-time view of one poller for the Pollers API (Step 6).
#[derive(Debug, Clone, serde::Serialize)]
pub struct PollerView {
    /// Sanitized poller id.
    pub id: String,
    /// Pool it serves.
    pub pool: String,
    /// Whether it is within the offline window.
    pub online: bool,
    /// Seconds since core last heard from it.
    pub seconds_since_seen: u64,
    /// Build version from its latest heartbeat.
    pub version: String,
    /// Per-process incarnation.
    pub incarnation: Uuid,
    /// Working-set node count it last reported.
    pub working_set_nodes: u32,
    /// Working-set spec count it last reported.
    pub working_set_specs: u32,
    /// Polls in flight it last reported.
    pub inflight: u32,
    /// Results core has consumed from it (via [`Coordinator::record_result`]).
    pub results_total: u64,
    /// The poller host's latest resource sample (CPU/load/mem/disk), if it reports host telemetry.
    pub host: Option<HostSample>,
    /// Passive-event listeners it has bound, e.g. `syslog:0.0.0.0:1514`.
    pub listeners: Vec<String>,
    /// Capabilities its build advertises (empty from an N-1 poller).
    pub caps: Vec<String>,
    /// What its site updater last reported about an upgrade (ADR-051 Inc.4); `None` when it
    /// has none, has never been asked, or predates the field.
    pub upgrade: Option<yagra_bus::UpgradeReport>,
}

/// Live poller registry + working-set publisher (ADR-009/020).
///
/// Takes the [`SyncBus`] as a trait object so it runs over NATS in production and over
/// [`yagra_bus::InMemoryBus`] in tests. It was generic over the bus instead, which is strictly
/// stronger *here* but leaked outward: every holder had to name the implementation, so
/// `ApiState.coordinator` spelled `Coordinator<NatsBus>` and the API layer — which never touches
/// the bus — depended on the production transport. Publishing a working set is per-poller
/// bookkeeping, not a hot loop, so the virtual call costs nothing worth measuring.
///
/// Holds the process `epoch` (a fresh UUID each start; a bump forces every poller to resync), the
/// guarded registry, the Redis mirror, the optional durable inventory (`None` where PG isn't wired,
/// e.g. tests), and the shared scheduler stats handle it bumps as it publishes.
pub struct Coordinator {
    /// Core-process epoch — stamped on every snapshot/delta; a restart bumps it and forces resync.
    epoch: Uuid,
    /// The control-plane bus (snapshot/delta publish).
    bus: Arc<dyn SyncBus>,
    /// Best-effort Redis liveness/assignment mirror (ADR-004).
    volatile: Arc<VolatileStore>,
    /// Durable poller inventory (`None` in skeleton/tests — no PG).
    pollers_repo: Option<Arc<PollerRepo>>,
    /// Shared self-monitoring counters (snapshots/deltas published).
    stats: Arc<SchedulerStats>,
    /// TSDB seam for writing each poller's host self-sample (self-observability). `None` in
    /// skeleton/tests — host telemetry is then simply not persisted. Core is the single writer of
    /// `yagra_host_*` series for every poller, since remote pollers can't reach the TSDB directly.
    store: Option<Arc<dyn MetricStore>>,
    /// The registry + published baselines.
    state: Mutex<CoordState>,
    /// Per-poller count of results core has consumed off the bus (provenance for the Pollers view).
    ///
    /// On its own lock rather than in [`CoordState`], and an atomic rather than a plain counter,
    /// because [`Coordinator::record_result`] runs once per poll result while `state` is also held
    /// by the working-set diff — sharing one mutex made result ingestion wait on a per-pool sweep.
    /// Kept separate from [`PollerEntry`] for the original reason too: a result that arrives before
    /// the first heartbeat is still counted.
    results_seen: RwLock<HashMap<String, AtomicU64>>,
    /// Nudge for the sweep loop, so a membership change is reconciled at once rather than at the
    /// next scheduled round. A [`tokio::sync::Notify`] rather than a channel because the signal
    /// carries nothing and coalescing several nudges into one sweep is exactly right.
    sweep_wake: tokio::sync::Notify,
}

/// Current Unix time in milliseconds (UTC), saturating on the impossible pre-epoch case.
fn now_unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

impl Coordinator {
    /// Build a coordinator with a fresh process epoch. `pollers_repo == None` disables durable
    /// inventory writes (skeleton/tests); `volatile` may be [`VolatileStore::disabled`] to disable
    /// the Redis mirror — both degrade to no-ops without affecting the in-memory source of truth.
    #[must_use]
    pub fn new(
        bus: Arc<dyn SyncBus>,
        volatile: Arc<VolatileStore>,
        pollers_repo: Option<Arc<PollerRepo>>,
        stats: Arc<SchedulerStats>,
        store: Option<Arc<dyn MetricStore>>,
    ) -> Self {
        Self {
            epoch: Uuid::new_v4(),
            bus,
            volatile,
            pollers_repo,
            stats,
            store,
            state: Mutex::new(CoordState::default()),
            results_seen: RwLock::new(HashMap::new()),
            sweep_wake: tokio::sync::Notify::new(),
        }
    }

    /// Observe a heartbeat: upsert the registry entry, decide whether the poller needs a fresh
    /// snapshot, and mirror liveness to Redis (always) + PostgreSQL (throttled). Clock injected.
    ///
    /// `needs_snapshot` is (re)armed when the poller is new, has a changed incarnation (restart),
    /// echoes an epoch that isn't ours, or echoes a `last_seq` that doesn't match what we've sent —
    /// each meaning the poller's set may be stale/gapped.
    ///
    /// Returns `true` iff this beat closed a **monitoring gap** — a known poller reappearing after
    /// its heartbeats had lapsed past the offline window (Phase 3, store-and-forward).
    pub async fn observe_heartbeat(&self, hb: HeartbeatMsg, now: Instant) -> bool {
        // A departing poller announces itself on its way out (SIGTERM). Drop it now rather than
        // waiting out three missed beats: for that window its nodes are assigned to a process that
        // is already gone, and if the restart completes inside the window the ring never changes at
        // all, so the nodes go unpolled for the whole restart instead of moving to a live poller.
        if hb.leaving {
            let mut st = self.state.lock().expect("coordinator state poisoned");
            st.pollers.remove(&hb.poller_id);
            st.published.remove(&hb.poller_id);
            drop(st);
            tracing::info!(
                poller = %hb.poller_id,
                pool = %hb.pool,
                "poller announced it is leaving — reassigning its nodes now"
            );
            metrics::counter!("yagra_poller_graceful_leave_total").increment(1);
            // Ask for a sweep instead of reconciling here: the coordinator holds no desired set,
            // and the sweep is the one place that builds it.
            self.wake_sweep();
            return false;
        }
        let (doc, do_pg, gap, gap_listeners, is_new) = {
            let mut st = self.state.lock().expect("coordinator state poisoned");
            let epoch = self.epoch;
            let known = st.pollers.contains_key(&hb.poller_id);
            let entry = st
                .pollers
                .entry(hb.poller_id.clone())
                .or_insert_with(|| PollerEntry {
                    // First contact: the poller's own environment is the only answer that exists
                    // (ADR-107 Inc.2). Every later beat is a report, not an instruction.
                    pool: hb.pool.clone(),
                    incarnation: hb.incarnation,
                    last_seen: now,
                    version: hb.version.clone(),
                    seq: 0,
                    needs_snapshot: true, // a newly-seen poller always gets a full snapshot first
                    working_set_nodes: 0,
                    working_set_specs: 0,
                    inflight: 0,
                    results_total: 0,
                    host: None,
                    listeners: Vec::new(),
                    caps: Vec::new(),
                    upgrade: None,
                    last_upserted: None,
                });
            if known {
                if entry.incarnation != hb.incarnation {
                    entry.needs_snapshot = true; // the poller restarted
                    entry.incarnation = hb.incarnation;
                }
                if hb.epoch != Some(epoch) {
                    entry.needs_snapshot = true; // stale/absent core epoch
                }
                if hb.last_seq != entry.seq {
                    entry.needs_snapshot = true; // the poller is behind / gapped
                }
                // 🚨 **The reported pool is deliberately NOT adopted here** (ADR-107 Inc.2).
                // `entry.pool` is what core has decided this poller serves, and the ring is built
                // from it — so taking the beat's value back would revert an operator's move within
                // one beat, with no error anywhere. A poller learns core's answer from
                // `WorkingSetSnapshot::pool` and starts reporting it; until it does, the two
                // disagree and core's side is the right one. The one place the beat still decides
                // is first contact, below, where nothing else has an answer.
                entry.version = hb.version.clone();
            }
            // Detect an offline→online transition: a *known* poller whose last contact predates the
            // offline window is reappearing, so the span since then was a monitoring gap. Computed
            // before we overwrite `last_seen` below; recorded once per transition (one row per gap).
            let offline_gap = if known {
                let since = now.saturating_duration_since(entry.last_seen);
                (since >= OFFLINE_AFTER).then_some(since)
            } else {
                None
            };
            // The listener set the poller had *going into* the gap — captured before the refresh
            // below overwrites it. That is the set whose datagrams were dropped on the floor, and
            // it is not necessarily what the poller reports now (a restart can change its binds).
            let gap_listeners = entry.listeners.clone();
            // Liveness + telemetry refresh regardless of new/known.
            entry.last_seen = now;
            entry.working_set_nodes = hb.working_set_nodes;
            entry.working_set_specs = hb.working_set_specs;
            entry.inflight = hb.inflight;
            entry.results_total = hb.results_total;
            entry.listeners.clone_from(&hb.listeners);
            entry.caps.clone_from(&hb.caps);
            entry.upgrade.clone_from(&hb.upgrade);
            if hb.host.is_some() {
                entry.host = hb.host.clone(); // keep the last known sample if a beat omits it
            }
            // Throttle the durable inventory write (a 10s beat must not be a write per beat).
            let do_pg = match entry.last_upserted {
                None => true,
                Some(t) => now.saturating_duration_since(t) >= PG_UPSERT_EVERY,
            };
            if do_pg {
                entry.last_upserted = Some(now);
            }
            let doc = PollerLiveDoc {
                pool: hb.pool.clone(),
                incarnation: hb.incarnation,
                version: hb.version.clone(),
                last_seq: hb.last_seq,
                working_set_nodes: hb.working_set_nodes,
                working_set_specs: hb.working_set_specs,
                inflight: hb.inflight,
                results_total: hb.results_total,
                seen_unix_ms: now_unix_ms(),
            };
            (doc, do_pg, offline_gap, gap_listeners, !known)
        };
        // A poller we did not have joined the ring — nudge the sweep instead of letting its share sit
        // with whoever covered for it until the next tick. The departure side has done this since the
        // `leaving` beat landed; the arrival side had not, so a poller coming back from an upgrade or
        // a restart waited out one sweep period (the fleet-minimum interval, 30s by default) before
        // core even told it what to poll — and then re-jittered everything on top of that (ADR-051).
        if is_new {
            self.wake_sweep();
        }
        // A monitoring gap healed: record the window (store-and-forward backfills its metrics; alerts
        // resume from "now"). One durable row per offline→online transition (Phase 3).
        if let Some(since) = gap {
            let ended_ms = now_unix_ms();
            let started_ms =
                ended_ms.saturating_sub(i64::try_from(since.as_millis()).unwrap_or(i64::MAX));
            metrics::counter!("yagra_core_poller_gap_total").increment(1);
            tracing::info!(
                poller = %hb.poller_id,
                pool = %hb.pool,
                gap_secs = since.as_secs(),
                "poller reappeared after a monitoring gap — store-and-forward backfills the window"
            );
            if let Some(repo) = &self.pollers_repo {
                if let Err(e) = repo
                    .insert_monitoring_gap(
                        &hb.poller_id,
                        &hb.pool,
                        started_ms,
                        ended_ms,
                        &gap_listeners,
                    )
                    .await
                {
                    tracing::warn!(error = %e, poller = %hb.poller_id, "failed to record monitoring gap");
                }
            }
        }
        // Best-effort mirrors, outside the lock.
        self.volatile
            .record_poller(&hb.poller_id, &doc, REDIS_TTL)
            .await;
        // Persist the poller host self-sample to the TSDB. Core is the single writer for remote
        // pollers (they can't reach VictoriaMetrics), so this is the only path for their trends.
        if let (Some(store), Some(host)) = (&self.store, &hb.host) {
            store
                .write_host_sample(&hb.poller_id, "poller", Some(&hb.pool), host, now_unix_ms())
                .await;
        }
        if do_pg {
            if let Some(repo) = &self.pollers_repo {
                // Where the poller says it is, for ADR-043 anchor resolution. Rendered here rather
                // than in the repo so the store keeps taking plain bound values.
                let mgmt: Vec<String> = hb.mgmt_addrs.iter().map(ToString::to_string).collect();
                match repo
                    .upsert_seen(&hb.poller_id, &hb.pool, &hb.version, hb.incarnation, &mgmt)
                    .await
                {
                    // The inventory is the durable answer, and on an HA pair it is the **only**
                    // thing the two cores share: a move taken on the follower reaches the leader
                    // here and nowhere else (ADR-107 Inc.2). That makes convergence bounded by
                    // `PG_UPSERT_EVERY`, not instant — the single-core case is instant because
                    // `set_pool` below updates this registry directly.
                    Ok(Some(pool)) => self.adopt_pool(&hb.poller_id, &pool),
                    Ok(None) => {}
                    Err(e) => {
                        tracing::warn!(error = %e, poller = %hb.poller_id, "poller inventory upsert failed");
                    }
                }
            }
        }
        gap.is_some()
    }

    /// Observe an explicit snapshot request: upsert-or-create the entry and arm `needs_snapshot`
    /// so the next reconcile sends a full snapshot. Sent by a poller at startup or after it detects
    /// a gap. Clock injected.
    pub async fn observe_sync_request(&self, req: SyncRequest, now: Instant) {
        let doc = {
            let mut st = self.state.lock().expect("coordinator state poisoned");
            let entry = st
                .pollers
                .entry(req.poller_id.clone())
                .or_insert_with(|| PollerEntry {
                    pool: req.pool.clone(),
                    incarnation: req.incarnation,
                    last_seen: now,
                    version: String::new(),
                    seq: 0,
                    needs_snapshot: true,
                    working_set_nodes: 0,
                    working_set_specs: 0,
                    inflight: 0,
                    results_total: 0,
                    host: None,
                    listeners: Vec::new(),
                    caps: Vec::new(),
                    upgrade: None,
                    last_upserted: None,
                });
            entry.needs_snapshot = true;
            entry.last_seen = now;
            // Not `req.pool`, for the reason in `observe_heartbeat`: a resync must not be a way
            // for a poller to put itself back in the pool it used to serve. A newly-created entry
            // above did take it, because there was nothing else to take.
            entry.incarnation = req.incarnation;
            let pool = entry.pool.clone();
            PollerLiveDoc {
                pool: pool.clone(),
                incarnation: req.incarnation,
                version: entry.version.clone(),
                last_seq: entry.seq,
                working_set_nodes: entry.working_set_nodes,
                working_set_specs: entry.working_set_specs,
                inflight: entry.inflight,
                results_total: entry.results_total,
                seen_unix_ms: now_unix_ms(),
            }
        };
        self.volatile
            .record_poller(&req.poller_id, &doc, REDIS_TTL)
            .await;
        // A poller asking for its working set is waiting on the sweep to build one, and this is the
        // first thing a poller does on boot — so it is the earliest moment core can act on a return
        // (the heartbeat is up to `HEARTBEAT_SECS` behind it). Nudging here is what makes a restart
        // symmetric with a departure; both are idempotent, so the two nudges coalesce into one sweep.
        self.wake_sweep();
    }

    /// Drain the heartbeat stream into [`Self::observe_heartbeat`] using the real clock. Thin so the
    /// decision logic stays testable; returns when the stream ends (shutdown = bus close).
    pub async fn run_heartbeat_consumer<S>(self: Arc<Self>, mut stream: S)
    where
        S: Stream<Item = HeartbeatMsg> + Unpin,
    {
        while let Some(hb) = stream.next().await {
            self.observe_heartbeat(hb, Instant::now()).await;
        }
        tracing::warn!("coordinator heartbeat stream ended");
    }

    /// Drain the sync-request stream into [`Self::observe_sync_request`] using the real clock.
    /// Returns when the stream ends (shutdown = bus close).
    pub async fn run_sync_request_consumer<S>(self: Arc<Self>, mut stream: S)
    where
        S: Stream<Item = SyncRequest> + Unpin,
    {
        while let Some(req) = stream.next().await {
            self.observe_sync_request(req, Instant::now()).await;
        }
        tracing::warn!("coordinator sync-request stream ended");
    }

    /// The set of pools that currently have at least one live poller (within [`OFFLINE_AFTER`] of
    /// `now`). The scheduler uses this to pick working-set mode vs legacy per pool each sweep.
    #[must_use]
    pub fn live_pools(&self, now: Instant) -> HashSet<String> {
        let st = self.state.lock().expect("coordinator state poisoned");
        st.pollers
            .values()
            .filter(|e| now.saturating_duration_since(e.last_seen) < OFFLINE_AFTER)
            .map(|e| e.pool.clone())
            .collect()
    }

    /// Whether **every** live poller that could be running work on `pool` advertises `cap`.
    ///
    /// `pool: None` asks about the whole live fleet, which is what a sweep published on the global
    /// subject needs: it is queue-delivered, so any live poller might hold it.
    ///
    /// ⚠️ **This answers "would it work", never "should I send it".** Unlike
    /// [`spec_required_caps`], which withholds a check from a poller that cannot run it, there is
    /// nothing to withhold here — core does not know which poller took a queue-delivered job. The
    /// caller uses this to *tell the operator* what to expect, and the authoritative answer still
    /// comes later from whatever the poller reports.
    ///
    /// **No live poller ⇒ `false`.** Vacuous truth would be the wrong reading: "every poller
    /// supports this" said of an empty set would tell an operator their stop will land when nothing
    /// is listening at all.
    #[must_use]
    pub fn pollers_support(&self, pool: Option<&str>, cap: &str, now: Instant) -> bool {
        let st = self.state.lock().expect("coordinator state poisoned");
        let mut live = st
            .pollers
            .values()
            .filter(|e| now.saturating_duration_since(e.last_seen) < OFFLINE_AFTER)
            .filter(|e| pool.is_none_or(|p| e.pool == p))
            .peekable();
        live.peek().is_some() && live.all(|e| e.caps.iter().any(|c| c == cap))
    }

    /// Reconcile one pool's desired working set to its live pollers (ADR-009/020).
    ///
    /// Builds the pool's consistent-hash ring from its (sorted, for determinism) live members,
    /// assigns each desired node to its owner, diffs each poller's assigned set against what we last
    /// sent, and publishes the minimal update: a chunked **snapshot** when the poller needs one
    /// (`needs_snapshot`) or the diff touches more than half the working set, otherwise a **delta**
    /// (`seq + 1`). An empty target yields a single empty snapshot chunk so the poller learns it owns
    /// nothing. A poller with no change (and no pending snapshot) is skipped entirely. The Redis
    /// assignment mirror and the snapshot/delta counters are updated too. Clock injected.
    ///
    /// Failover falls out for free: a dead poller isn't in `members`, so its nodes land on survivors
    /// and appear as their upserts here — no dedicated failover code.
    ///
    /// SECURITY (security.md): the published messages carry inlined decrypted credentials inside
    /// each [`JobSpec`]. This method logs **counts and poller ids only** — never spec contents. Keep
    /// it that way when editing.
    ///
    /// PERFORMANCE: `desired` is **borrowed**, and so are the specs inside the per-member target
    /// sets — an owned copy is taken only for the nodes that actually go into a message. The steady
    /// state (nothing changed) therefore deep-clones nothing at all, where it used to clone the
    /// fleet's entire decrypted spec set twice per sweep: once for the capability filter and once
    /// for the caller's cached copy. The three-phase shape below exists for the same reason — the
    /// O(fleet) assignment + fingerprint pass (which serializes every spec) runs with **no lock
    /// held**, so a sweep no longer stalls heartbeats and the Pollers view once per pool per round.
    pub async fn reconcile_pool(
        &self,
        pool: &str,
        desired: &HashMap<NodeId, Vec<JobSpec>>,
        now: Instant,
    ) {
        // ── Phase 1 (locked, O(members)): who is live, and what can each of them honour? ─────────
        // Snapshotting the capability sets here is what lets the fleet-scale pass below run
        // unlocked. Membership can move between the phases; that is benign — a poller that left is
        // skipped in phase 3 (its registry entry is gone) and one that joined is picked up by the
        // next sweep, which its own registration already nudges.
        let members: Vec<String>;
        let caps: HashMap<String, Vec<String>>;
        {
            let st = self.state.lock().expect("coordinator state poisoned");
            members = live_members(&st, pool, now);
            if members.is_empty() {
                // Defensive: the scheduler only calls us for a pool in `live_pools`. A no-op keeps
                // the last working set intact rather than tearing everything down on a race.
                return;
            }
            caps = members
                .iter()
                .map(|m| {
                    let advertised = st
                        .pollers
                        .get(m)
                        .map(|e| e.caps.clone())
                        .unwrap_or_default();
                    (m.clone(), advertised)
                })
                .collect();
        }

        // ── Phase 2 (unlocked, O(fleet)): assign, filter, fingerprint ───────────────────────────
        let ring = HashRing::new(members.iter().cloned());
        // Per-member assigned target set (start every live member at empty so a member that loses
        // all its nodes still gets a diff computed). The specs are references into `desired`: in the
        // steady state the fingerprints below decide nothing changed, and a clone would have been
        // built and dropped for the whole fleet with nothing sent.
        let mut targets: HashMap<&str, HashMap<NodeId, Vec<&JobSpec>>> = members
            .iter()
            .map(|m| (m.as_str(), HashMap::new()))
            .collect();
        for (node, specs) in desired {
            let Some(owner) = ring.assign(*node) else {
                continue;
            };
            let Some(set) = targets.get_mut(owner) else {
                continue;
            };
            // Withhold a spec its assigned poller cannot honour. Every entry in
            // `spec_required_caps` exists because an N-1 poller drops the field it does not know and
            // then reports a *confident wrong answer* — a 401 read as an outage, or a page it never
            // read reported as content-checked. Withholding produces a monitoring gap instead, which
            // is recoverable and wakes no one.
            //
            // Still evaluated for every node every round, deliberately: the fingerprint is taken
            // over the *kept* set, so a poller that gains a capability must show up as a change.
            let kept: Vec<&JobSpec> = specs
                .iter()
                .filter(|s| {
                    let Some(missing) = spec_required_caps(s)
                        .into_iter()
                        .find(|cap| !has_cap(&caps, owner, cap))
                    else {
                        return true;
                    };
                    tracing::warn!(
                        poller = %owner,
                        node = %node,
                        cap = missing,
                        "poller does not advertise this capability; withholding the check that needs it until the poller is upgraded"
                    );
                    metrics::counter!("yagra_specs_withheld_total", "cap" => missing).increment(1);
                    false
                })
                .collect();
            // An empty set still registers the node as assigned, so the sweep does not treat it as
            // unowned and hand it to a second poller.
            set.insert(*node, kept);
        }
        // Fingerprint each member's target once — it is both the change signal against the stored
        // baseline and the next baseline. This serializes every spec in the pool, which is precisely
        // why it sits outside the lock.
        let mut fps: HashMap<&str, HashMap<NodeId, u64>> = targets
            .iter()
            .map(|(m, target)| {
                let per_node = target
                    .iter()
                    .map(|(node, specs)| (*node, specs_fingerprint(specs)))
                    .collect();
                (*m, per_node)
            })
            .collect();

        // ── Phase 3 (locked, O(changed)): diff against the baseline and build the messages ──────
        let mut to_send: Vec<(String, SyncMsg)> = Vec::new();
        let mut snapshots = 0u64;
        let mut deltas = 0u64;
        let epoch = self.epoch;
        {
            let mut guard = self.state.lock().expect("coordinator state poisoned");
            // Reborrowed so `published` and `pollers` can be borrowed as the disjoint fields they
            // are — that is what lets the baseline be read *in place* instead of cloned per member
            // per round.
            let st = &mut *guard;
            for m in &members {
                let (Some(target), Some(target_fps)) =
                    (targets.get(m.as_str()), fps.remove(m.as_str()))
                else {
                    continue;
                };

                // Diff: a node is an upsert if it is new or its spec set changed; a node in the
                // previous set but not the target is a remove. Both sorted for deterministic output.
                // The baseline holds only per-node fingerprints (S16), never the decrypted specs.
                let (upserted, removes, prev_len) = {
                    let prev = st.published.get(m);
                    let mut upserted: Vec<NodeId> = target_fps
                        .iter()
                        .filter(|(node, fp)| prev.and_then(|p| p.get(*node)).copied() != Some(**fp))
                        .map(|(node, _)| *node)
                        .collect();
                    upserted.sort_unstable();
                    let mut removes: Vec<NodeId> = prev
                        .map(|p| {
                            p.keys()
                                .filter(|node| !target_fps.contains_key(*node))
                                .copied()
                                .collect()
                        })
                        .unwrap_or_default();
                    removes.sort_unstable();
                    (upserted, removes, prev.map_or(0, HashMap::len))
                };

                let Some(entry) = st.pollers.get_mut(m) else {
                    continue;
                };
                let needs_snapshot = entry.needs_snapshot;
                let changed = upserted.len() + removes.len();
                if changed == 0 && !needs_snapshot {
                    continue; // nothing to send; keep the baseline as-is
                }
                let new_seq = entry.seq + 1;
                // Snapshot when forced, or when the change touches > 50% of the working set (delta
                // would be nearly a full resend anyway). `base` is the larger of old/new so an empty
                // target (everything moved away) snapshots rather than sending a giant delta.
                let base = target_fps.len().max(prev_len);
                let use_snapshot = needs_snapshot || changed * 2 > base;
                entry.seq = new_seq;
                if use_snapshot {
                    entry.needs_snapshot = false;
                    let mut nodes: Vec<NodeJobs> = target
                        .iter()
                        .map(|(node, specs)| NodeJobs {
                            node_id: *node,
                            specs: owned(specs),
                        })
                        .collect();
                    nodes.sort_by_key(|nj| nj.node_id); // deterministic chunking
                    let total_nodes = u32::try_from(nodes.len()).unwrap_or(u32::MAX);
                    // Empty target ⇒ one empty chunk (the poller learns "you own nothing").
                    let chunks: Vec<&[NodeJobs]> = if nodes.is_empty() {
                        vec![&[][..]]
                    } else {
                        nodes.chunks(SNAPSHOT_CHUNK_NODES).collect()
                    };
                    let chunk_total = u32::try_from(chunks.len()).unwrap_or(u32::MAX);
                    for (i, chunk) in chunks.iter().enumerate() {
                        to_send.push((
                            m.clone(),
                            SyncMsg::SnapshotChunk(WorkingSetSnapshot {
                                poller_id: m.clone(),
                                epoch,
                                seq: new_seq,
                                chunk_index: u32::try_from(i).unwrap_or(u32::MAX),
                                chunk_total,
                                nodes: chunk.to_vec(),
                                total_nodes,
                                // What this poller must serve (ADR-107 Inc.2). On every chunk, not
                                // just the first: chunks are independent messages and the poller
                                // may apply them in any order.
                                pool: Some(pool.to_owned()),
                            }),
                        ));
                    }
                    snapshots += 1;
                } else {
                    // Only here does a spec get copied: the nodes whose set actually moved.
                    // `upserted` is derived from `target_fps`, whose keys are exactly `target`'s, so
                    // the lookup always hits — `filter_map` is the total-function spelling of that.
                    let upserts: Vec<NodeJobs> = upserted
                        .iter()
                        .filter_map(|node| {
                            target.get(node).map(|specs| NodeJobs {
                                node_id: *node,
                                specs: owned(specs),
                            })
                        })
                        .collect();
                    to_send.push((
                        m.clone(),
                        SyncMsg::Delta(WorkingSetDelta {
                            poller_id: m.clone(),
                            epoch,
                            seq: new_seq,
                            upserts,
                            removes,
                        }),
                    ));
                    deltas += 1;
                }
                st.published.insert(m.clone(), target_fps);
            }
        }

        // Publish + mirror outside the lock (best-effort). Never log spec contents (credentials).
        for (poller_id, msg) in to_send {
            if let Err(e) = self.bus.publish_sync(&poller_id, msg).await {
                tracing::warn!(error = %e, poller = %poller_id, "failed to publish working-set sync");
            }
        }
        // S18: the Redis assignment mirror only changes when the working set does. If this sweep
        // sent nothing (steady state), the map is byte-for-byte what Redis already holds — any node
        // add/remove or ring-membership change produces at least one snapshot or delta above — so
        // skip the O(fleet) DEL+HSET rewrite. A real change still rewrites the full map.
        //
        // Built here rather than during assignment for the same reason: a steady-state round must
        // not allocate a poller-id `String` per node for a map it is not going to write.
        if snapshots + deltas > 0 {
            let mut assign_map: HashMap<Uuid, String> = HashMap::new();
            for (m, target) in &targets {
                for node in target.keys() {
                    assign_map.insert(node.as_uuid(), (*m).to_owned());
                }
            }
            self.volatile.write_assignments(pool, &assign_map).await;
            self.stats.record_assignment_write();
        }
        for _ in 0..snapshots {
            self.stats.record_snapshot();
        }
        for _ in 0..deltas {
            self.stats.record_delta();
        }
    }

    /// Snapshot of every known poller for the Pollers API (Step 6). `online`/`seconds_since_seen`
    /// are computed against `now`; `results_total` is core's consumed-result count for the poller.
    #[must_use]
    pub fn poller_views(&self, now: Instant) -> Vec<PollerView> {
        // Read the result counters *first* and release their lock, so this never holds two locks at
        // once — the module's flat-locking discipline. The map is per-poller (tens of entries), so
        // materializing it is far cheaper than nesting it inside the registry guard.
        let counts: HashMap<String, u64> = self
            .results_seen
            .read()
            .expect("coordinator result counters poisoned")
            .iter()
            .map(|(id, seen)| (id.clone(), seen.load(Ordering::Relaxed)))
            .collect();
        let st = self.state.lock().expect("coordinator state poisoned");
        let mut views: Vec<PollerView> = st
            .pollers
            .iter()
            .map(|(id, e)| PollerView {
                id: id.clone(),
                pool: e.pool.clone(),
                online: now.saturating_duration_since(e.last_seen) < OFFLINE_AFTER,
                seconds_since_seen: now.saturating_duration_since(e.last_seen).as_secs(),
                version: e.version.clone(),
                incarnation: e.incarnation,
                working_set_nodes: e.working_set_nodes,
                working_set_specs: e.working_set_specs,
                inflight: e.inflight,
                results_total: counts.get(id).copied().unwrap_or(0),
                host: e.host.clone(),
                listeners: e.listeners.clone(),
                caps: e.caps.clone(),
                upgrade: e.upgrade.clone(),
            })
            .collect();
        views.sort_by(|a, b| a.id.cmp(&b.id));
        views
    }

    /// The live poller that currently holds `node` in its published working set, if any.
    ///
    /// Reads the **published baseline** — the literal record of what core last sent — rather than
    /// re-deriving the ring from [`live_members`]. A re-derivation would *lie* about the nodes the
    /// sweep drops before publishing (an empty spec set, and Meraki devices, which core's own org
    /// collector owns), naming an owner that was never told about them. Reading `published` also
    /// makes this and [`Self::published_nodes`] the same data, so the node fact and the poller
    /// drill-down can never disagree.
    ///
    /// Filtered to pollers still inside [`OFFLINE_AFTER`]: `published` is never reaped, so a dead
    /// poller keeps its last working set for the life of the core process and would otherwise
    /// answer as the current owner.
    #[must_use]
    pub fn owner_of(&self, node: NodeId, now: Instant) -> Option<String> {
        let st = self.state.lock().expect("coordinator state poisoned");
        st.published
            .iter()
            .find(|(id, set)| set.contains_key(&node) && is_live(&st, id, now))
            .map(|(id, _)| (*id).clone())
    }

    /// The node ids in `poller_id`'s published working set, sorted by uuid so paging is
    /// deterministic. `None` when the poller is unknown or offline — see [`Self::owner_of`] for why
    /// a stale `published` bucket must not be served as current. A live poller core has not yet
    /// published to (registered mid-sweep) correctly yields an empty `Vec`, not `None`.
    #[must_use]
    pub fn published_nodes(&self, poller_id: &str, now: Instant) -> Option<Vec<NodeId>> {
        let st = self.state.lock().expect("coordinator state poisoned");
        if !is_live(&st, poller_id, now) {
            return None;
        }
        let mut ids: Vec<NodeId> = st
            .published
            .get(poller_id)
            .map(|set| set.keys().copied().collect())
            .unwrap_or_default();
        ids.sort_unstable();
        Some(ids)
    }

    /// Move a poller into `pool` **now**: rewrite the registry entry, arm a full snapshot, and
    /// wake the sweep (ADR-107 Inc.2).
    ///
    /// The caller has already written `pollers.pool`; this is the in-process half that makes the
    /// change take effect before the next durable read. Between them: the ring rebuilds on the next
    /// sweep (woken here, so within milliseconds), the poller receives a snapshot of the new pool's
    /// nodes on `yagra.poller.assign.{id}` — **a subject that carries no pool token, which is why
    /// none of this needs the poller's cooperation to start working** — and that snapshot tells it
    /// which pool it is in so it can re-point the three subjects that *are* pool-derived.
    ///
    /// A no-op for a poller this core has not seen: it will pick the pool up from the inventory on
    /// its first beat.
    pub fn set_pool(&self, poller_id: &str, pool: &str) {
        let moved = {
            let mut st = self.state.lock().expect("coordinator state poisoned");
            match st.pollers.get_mut(poller_id) {
                Some(entry) if entry.pool != pool => {
                    entry.pool = pool.to_owned();
                    // A delta would describe a change to the *old* pool's set. The poller must
                    // replace its whole working set, so the next message has to be a snapshot —
                    // which is also what carries the new pool name to it.
                    entry.needs_snapshot = true;
                    true
                }
                _ => false,
            }
        };
        if moved {
            tracing::info!(poller = poller_id, %pool, "poller moved to a new pool");
            metrics::counter!("yagra_poller_pool_moves_total").increment(1);
            self.wake_sweep();
        }
    }

    /// Take the pool from the durable inventory, without the logging or the sweep nudge.
    ///
    /// Separate from [`Self::set_pool`] because this runs on the heartbeat path once a minute per
    /// poller and is almost always a no-op; treating every beat as a move would put a log line and
    /// a sweep behind each one.
    fn adopt_pool(&self, poller_id: &str, pool: &str) {
        let changed = {
            let mut st = self.state.lock().expect("coordinator state poisoned");
            match st.pollers.get_mut(poller_id) {
                Some(entry) if entry.pool != pool => {
                    entry.pool = pool.to_owned();
                    entry.needs_snapshot = true;
                    true
                }
                _ => false,
            }
        };
        if changed {
            tracing::info!(
                poller = poller_id,
                %pool,
                "adopted the pool from the inventory — another core moved this poller"
            );
            self.wake_sweep();
        }
    }

    /// The capabilities a live poller advertises. `None` when it is unknown or offline.
    ///
    /// Read by the move path to refuse a poller that cannot follow a pool change — see
    /// [`yagra_bus::CAP_POOL_FOLLOW`], where the failure that refusal exists for is spelled out.
    #[must_use]
    pub fn caps_of(&self, poller_id: &str, now: Instant) -> Option<Vec<String>> {
        let st = self.state.lock().expect("coordinator state poisoned");
        st.pollers
            .get(poller_id)
            .filter(|e| now.saturating_duration_since(e.last_seen) < OFFLINE_AFTER)
            .map(|e| e.caps.clone())
    }

    /// The pool a live poller serves, from the registry (not the durable inventory row, which
    /// outlives liveness). `None` when the poller is unknown or offline.
    #[must_use]
    pub fn pool_of(&self, poller_id: &str, now: Instant) -> Option<String> {
        let st = self.state.lock().expect("coordinator state poisoned");
        st.pollers
            .get(poller_id)
            .filter(|e| now.saturating_duration_since(e.last_seen) < OFFLINE_AFTER)
            .map(|e| e.pool.clone())
    }

    /// Ports that currently-online pollers have bound as passive-event listeners. Backs the
    /// forwarding loop guard (ADR-034): sending Yagra's own output back into one of its own
    /// listeners would amplify without bound.
    pub fn listener_ports(&self, now: Instant) -> HashSet<u16> {
        let st = self.state.lock().expect("coordinator state poisoned");
        st.pollers
            .values()
            .filter(|e| now.saturating_duration_since(e.last_seen) < OFFLINE_AFTER)
            .flat_map(|e| e.listeners.iter())
            // Labels look like `syslog:0.0.0.0:1514` / `trap:[::]:1162` — the port is the last token.
            .filter_map(|label| label.rsplit(':').next()?.parse::<u16>().ok())
            .collect()
    }

    /// Ask the sweep loop to run now rather than at its next scheduled round.
    ///
    /// Coalescing: several nudges before the sweep wakes produce one sweep, which is what is wanted
    /// — the sweep is idempotent and rebuilds the whole desired set anyway.
    pub fn wake_sweep(&self) {
        self.sweep_wake.notify_one();
    }

    /// Wait for a nudge from [`Self::wake_sweep`].
    ///
    /// The sweep selects on this against its own interval, so a poller leaving is reconciled in
    /// about the time it takes to publish a delta instead of waiting for the next round. Without
    /// it the `leaving` beat only shortens *detection*, and the actual hand-off still waits out the
    /// sweep period — which for a fleet of one-minute monitors is most of the benefit lost.
    pub async fn sweep_nudged(&self) {
        self.sweep_wake.notified().await;
    }

    /// Ids of online pollers whose build does **not** advertise `cap`. The Forwarding page uses this
    /// to say "byte-exact is configured but these pollers can't supply the original bytes yet"
    /// instead of silently shipping re-rendered output.
    pub fn pollers_missing_cap(&self, cap: &str, now: Instant) -> Vec<String> {
        let st = self.state.lock().expect("coordinator state poisoned");
        let mut ids: Vec<String> = st
            .pollers
            .iter()
            .filter(|(_, e)| now.saturating_duration_since(e.last_seen) < OFFLINE_AFTER)
            .filter(|(_, e)| !e.caps.iter().any(|c| c == cap))
            .map(|(id, _)| id.clone())
            .collect();
        ids.sort();
        ids
    }

    /// Count one poll result core consumed off the bus as produced by `poller_id`.
    ///
    /// Deliberately **not** on the registry mutex. This runs once per poll result (~1.7k/s at 50k
    /// nodes on a 30s interval) from `ingest_result`, and that mutex is also what
    /// [`Self::reconcile_pool`] takes to diff a pool's working set — so sharing it put per-result
    /// accounting behind a sweep once per pool per round. In the steady state this is a read guard
    /// plus a relaxed atomic add and allocates nothing; only the very first result from a given
    /// poller takes the write lock, to create its counter.
    pub fn record_result(&self, poller_id: &str) {
        {
            let seen = self
                .results_seen
                .read()
                .expect("coordinator result counters poisoned");
            if let Some(counter) = seen.get(poller_id) {
                counter.fetch_add(1, Ordering::Relaxed);
                return;
            }
        }
        self.results_seen
            .write()
            .expect("coordinator result counters poisoned")
            .entry(poller_id.to_owned())
            .or_insert_with(|| AtomicU64::new(0))
            .fetch_add(1, Ordering::Relaxed);
    }
}

/// Copy a node's borrowed spec set into the owned `Vec` a bus message has to carry.
///
/// The single place a [`JobSpec`] is cloned during a reconcile: everything upstream of a published
/// message — assignment, the capability filter, fingerprinting — works on references into the
/// scheduler's cached desired set.
fn owned(specs: &[&JobSpec]) -> Vec<JobSpec> {
    specs.iter().map(|s| (*s).clone()).collect()
}

/// Whether `poller_id` is a registered poller whose last heartbeat is within [`OFFLINE_AFTER`] of
/// `now`. The liveness gate for every read that keys off a poller id — in particular the
/// never-reaped [`CoordState::published`] buckets.
fn is_live(st: &CoordState, poller_id: &str, now: Instant) -> bool {
    st.pollers
        .get(poller_id)
        .is_some_and(|e| now.saturating_duration_since(e.last_seen) < OFFLINE_AFTER)
}

/// Whether `poller` advertised `cap` in its most recent heartbeat, per the capability snapshot the
/// reconcile took under the registry lock.
///
/// Absence is treated as "cannot", which is the safe direction: a poller too old to send caps at
/// all is exactly the one that would mishandle a new field.
fn has_cap(caps: &HashMap<String, Vec<String>>, poller: &str, cap: &str) -> bool {
    caps.get(poller).is_some_and(|c| c.iter().any(|x| x == cap))
}

/// The capability tokens `spec` requires of the poller that will run it — empty when any poller can.
///
/// The test for belonging here is not "is this field new". It is: **would a poller that silently
/// ignored the field produce an answer indistinguishable from a correct one?** Dropping `auth`
/// turns a healthy service into a 401 read as an outage; dropping `body_match` turns a page nobody
/// read into a passing content check. Both are confident and both are wrong, which is why they are
/// gated while every other `#[serde(default)]` field on the bus is not — those degrade visibly.
///
/// Allocation-free for the overwhelming majority of specs (`Vec::new()` does not allocate), which
/// matters because this runs for every spec of every node on every publish.
fn spec_required_caps(spec: &JobSpec) -> Vec<&'static str> {
    match &spec.check {
        CheckSpec::Http(http) => {
            let mut caps = Vec::new();
            if http.auth.is_some() {
                caps.push(CAP_HTTP_AUTH);
            }
            if http.body_match.is_some() {
                caps.push(CAP_HTTP_BODY);
            }
            caps
        }
        CheckSpec::Icmp(_)
        | CheckSpec::Snmp(_)
        | CheckSpec::SnmpTable(_)
        | CheckSpec::SnmpV3(_)
        | CheckSpec::SnmpV3Table(_)
        // Optical needs no capability gate: a poller that has never heard of the spec skips it
        // (per-element decode), and one that has produces either readings or nothing. There is no
        // way for it to answer confidently and wrongly, which is the test this list applies.
        | CheckSpec::SnmpOptical(_)
        | CheckSpec::SnmpV3Optical(_)
        // Media likewise: an older poller skips the spec, and a newer one either reads a
        // registration or reads nothing. No way to answer confidently and wrongly.
        | CheckSpec::SnmpMau(_)
        | CheckSpec::SnmpV3Mau(_)
        | CheckSpec::SnmpNeighbors(_)
        | CheckSpec::SnmpV3Neighbors(_)
        | CheckSpec::SnmpL3(_)
        | CheckSpec::SnmpV3L3(_)
        | CheckSpec::SnmpArp(_)
        | CheckSpec::SnmpV3Arp(_)
        | CheckSpec::SnmpRouting(_)
        | CheckSpec::SnmpV3Routing(_)
        | CheckSpec::Dns(_)
        | CheckSpec::MerakiCollect(_) => Vec::new(),
    }
}

/// The sorted ids of the pool's live members (heartbeat within [`OFFLINE_AFTER`] of `now`). Sorted
/// so the ring built from them is deterministic across sweeps.
fn live_members(st: &CoordState, pool: &str, now: Instant) -> Vec<String> {
    let mut members: Vec<String> = st
        .pollers
        .iter()
        .filter(|(_, e)| {
            e.pool == pool && now.saturating_duration_since(e.last_seen) < OFFLINE_AFTER
        })
        .map(|(id, _)| id.clone())
        .collect();
    members.sort();
    members
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};
    use yagra_bus::{CheckSpec, IcmpCheck, InMemoryBus, PollJob};

    /// Build a coordinator over an in-memory bus with Redis + PG mirrors disabled (pure registry).
    fn coordinator() -> (Arc<Coordinator>, Arc<InMemoryBus>, Arc<SchedulerStats>) {
        let bus = Arc::new(InMemoryBus::new(4096));
        let stats = Arc::new(SchedulerStats::default());
        let coord = Arc::new(Coordinator::new(
            bus.clone(),
            Arc::new(VolatileStore::disabled()),
            None,
            stats.clone(),
            None,
        ));
        (coord, bus, stats)
    }

    fn heartbeat(id: &str, pool: &str, incarnation: Uuid) -> HeartbeatMsg {
        HeartbeatMsg {
            poller_id: id.to_owned(),
            pool: pool.to_owned(),
            incarnation,
            version: "0.1.0".to_owned(),
            epoch: None,
            last_seq: 0,
            working_set_nodes: 0,
            working_set_specs: 0,
            inflight: 0,
            results_total: 0,
            listeners: Vec::new(),
            caps: Vec::new(),
            host: None,
            leaving: false,
            mgmt_addrs: Vec::new(),
            upgrade: None,
        }
    }

    #[tokio::test]
    async fn a_leaving_poller_is_dropped_at_once_not_after_three_missed_beats() {
        // The whole point of the flag: without it core waits OFFLINE_AFTER_SECS before the ring
        // changes, so a rolling upgrade leaves nodes assigned to a process that has already gone.
        let (coord, _bus, _stats) = coordinator();
        let now = Instant::now();
        coord
            .observe_heartbeat(heartbeat("edge-1", "tokyo", Uuid::nil()), now)
            .await;
        coord
            .observe_heartbeat(heartbeat("edge-2", "tokyo", Uuid::nil()), now)
            .await;
        assert_eq!(coord.poller_views(now).len(), 2);

        let mut bye = heartbeat("edge-1", "tokyo", Uuid::nil());
        bye.leaving = true;
        coord.observe_heartbeat(bye, now).await;

        // Gone immediately — not merely marked offline, and with no time having passed.
        let left: Vec<String> = coord.poller_views(now).into_iter().map(|v| v.id).collect();
        assert_eq!(left, vec!["edge-2".to_owned()]);
    }

    #[tokio::test]
    async fn a_leaving_poller_that_comes_back_is_registered_again() {
        // A restart is leave-then-rejoin. The rejoin must be a clean first sighting, so the poller
        // gets a full snapshot rather than a delta against a baseline core threw away.
        let (coord, _bus, _stats) = coordinator();
        let now = Instant::now();
        coord
            .observe_heartbeat(heartbeat("edge-1", "tokyo", Uuid::nil()), now)
            .await;
        let mut bye = heartbeat("edge-1", "tokyo", Uuid::nil());
        bye.leaving = true;
        coord.observe_heartbeat(bye, now).await;
        assert!(coord.poller_views(now).is_empty());

        let fresh = Uuid::new_v4();
        coord
            .observe_heartbeat(heartbeat("edge-1", "tokyo", fresh), now)
            .await;
        let views = coord.poller_views(now);
        assert_eq!(views.len(), 1);
        assert!(views[0].online);
    }

    /// `true` if a sweep nudge is pending. `Notify::notify_one` stores a permit, so this both
    /// observes and consumes it — call it once per assertion.
    async fn sweep_pending(coord: &Arc<Coordinator>) -> bool {
        tokio::time::timeout(Duration::from_millis(50), coord.sweep_nudged())
            .await
            .is_ok()
    }

    #[tokio::test]
    async fn a_rejoining_poller_nudges_the_sweep_just_like_a_leaving_one() {
        // The departure side has nudged since the `leaving` beat landed; the arrival side did not,
        // so a poller returning from a restart or an upgrade sat idle until the next sweep tick —
        // up to the fleet-minimum poll interval — while whoever covered for it kept its share.
        // Both directions of a rolling restart have to be prompt, or the second half eats the
        // saving the first half made (ADR-051).
        let (coord, _bus, _stats) = coordinator();
        let now = Instant::now();

        // First sighting is an arrival.
        coord
            .observe_heartbeat(heartbeat("edge-1", "tokyo", Uuid::nil()), now)
            .await;
        assert!(sweep_pending(&coord).await, "a new poller must nudge");

        // A steady beat from a poller core already knows changes nothing, so it must not.
        coord
            .observe_heartbeat(heartbeat("edge-1", "tokyo", Uuid::nil()), now)
            .await;
        assert!(
            !sweep_pending(&coord).await,
            "an ordinary beat must not wake the sweep — that would be a sweep every 10s per poller"
        );

        // A sync request is the earliest signal of a return: it arrives on boot, ahead of the beat.
        coord
            .observe_sync_request(
                SyncRequest {
                    poller_id: "edge-2".to_owned(),
                    pool: "tokyo".to_owned(),
                    incarnation: Uuid::new_v4(),
                },
                now,
            )
            .await;
        assert!(sweep_pending(&coord).await, "a sync request must nudge");
    }

    #[tokio::test]
    async fn reappearing_poller_after_offline_window_records_one_gap() {
        // Store-and-forward (Phase 3): a *known* poller whose heartbeats lapsed past the offline
        // window and then resumed closed a monitoring gap — recorded exactly once per transition.
        let (coord, _bus, _stats) = coordinator();
        let inc = Uuid::from_u128(1);
        let t0 = Instant::now();
        // First contact: unknown poller → not a gap.
        assert!(
            !coord
                .observe_heartbeat(heartbeat("edge-1", "default", inc), t0)
                .await
        );
        // A beat within the offline window → still no gap.
        let t1 = t0 + Duration::from_secs(5);
        assert!(
            !coord
                .observe_heartbeat(heartbeat("edge-1", "default", inc), t1)
                .await
        );
        // A beat AFTER the offline window elapsed → the poller reappeared: exactly one gap.
        let t2 = t1 + OFFLINE_AFTER + Duration::from_secs(1);
        assert!(
            coord
                .observe_heartbeat(heartbeat("edge-1", "default", inc), t2)
                .await
        );
        // The very next beat is fresh → no new gap.
        let t3 = t2 + Duration::from_secs(5);
        assert!(
            !coord
                .observe_heartbeat(heartbeat("edge-1", "default", inc), t3)
                .await
        );
    }

    /// A heartbeat that echoes a poller that has already applied snapshot `seq` on `epoch` (so it
    /// won't be seen as gapped/stale on the next observe).
    fn heartbeat_echo(
        id: &str,
        pool: &str,
        incarnation: Uuid,
        epoch: Uuid,
        seq: u64,
    ) -> HeartbeatMsg {
        HeartbeatMsg {
            epoch: Some(epoch),
            last_seq: seq,
            ..heartbeat(id, pool, incarnation)
        }
    }

    fn node(n: u128) -> NodeId {
        NodeId::from(Uuid::from_u128(n))
    }

    /// Deterministic, uniformly-distributed node ids. Structured `from_u128(i)` ids cluster on the
    /// hash ring (see ring.rs), so a fair 2-member split for the failover test needs uniform inputs;
    /// SplitMix64 gives that from a fixed seed (reproducible).
    fn uniform_nodes(n: u64) -> Vec<NodeId> {
        let mut state = 0x2545_f491_4f6c_dd1d_u64;
        let mut next = || {
            state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut z = state;
            z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            z ^ (z >> 31)
        };
        (0..n)
            .map(|_| {
                let hi = u128::from(next());
                let lo = u128::from(next());
                NodeId::from(Uuid::from_u128((hi << 64) | lo))
            })
            .collect()
    }

    fn icmp_spec(node: NodeId, interval: u32) -> JobSpec {
        let job = PollJob::icmp(
            Uuid::nil(),
            node,
            IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
            IcmpCheck::default(),
            interval,
        );
        JobSpec::from_job(&job)
    }

    fn desired(nodes: &[(NodeId, u32)]) -> HashMap<NodeId, Vec<JobSpec>> {
        nodes
            .iter()
            .map(|(n, i)| (*n, vec![icmp_spec(*n, *i)]))
            .collect()
    }

    /// Drain every `SyncMsg` currently queued for `id`, returning them (non-blocking). NB: the
    /// broadcast channel is shared, so this also consumes (and discards) messages for *other* ids —
    /// only safe in single-poller tests. Multi-poller tests use [`drain_buckets`].
    fn drain_syncs(
        rx: &mut tokio::sync::broadcast::Receiver<(String, SyncMsg)>,
        id: &str,
    ) -> Vec<SyncMsg> {
        let mut out = Vec::new();
        while let Ok((target, msg)) = rx.try_recv() {
            if target == id {
                out.push(msg);
            }
        }
        out
    }

    /// Drain all queued syncs (non-blocking) bucketed by target poller id, so a multi-poller test
    /// keeps every id's messages in one pass rather than losing them to a per-id filter.
    fn drain_buckets(
        rx: &mut tokio::sync::broadcast::Receiver<(String, SyncMsg)>,
    ) -> HashMap<String, Vec<SyncMsg>> {
        let mut out: HashMap<String, Vec<SyncMsg>> = HashMap::new();
        while let Ok((target, msg)) = rx.try_recv() {
            out.entry(target).or_default().push(msg);
        }
        out
    }

    fn t0() -> Instant {
        Instant::now()
    }

    /// The whole of ADR-107 Inc.2 on core's side, in one pass: a move changes which pool's nodes
    /// the poller is sent, forces a **snapshot** rather than a delta, and puts the new pool's name
    /// on that snapshot so the poller can re-point its three pool-derived subjects.
    ///
    /// 🚨 The snapshot half is not cosmetic. A delta describes a change to the set the poller
    /// already holds, and after a move that set belongs to a different pool — applying one would
    /// leave it polling a union of both pools' nodes, which no page would show as wrong.
    #[tokio::test]
    async fn moving_a_poller_sends_it_the_new_pools_nodes_as_a_snapshot_naming_the_pool() {
        let (coord, bus, _) = coordinator();
        let mut rx = bus.subscribe_sync();
        let now = t0();
        coord
            .observe_heartbeat(heartbeat("p1", "default", Uuid::new_v4()), now)
            .await;

        let a = NodeId::new();
        coord
            .reconcile_pool("default", &desired(&[(a, 30)]), now)
            .await;
        let first = drain_syncs(&mut rx, "p1");
        assert!(
            matches!(first.first(), Some(SyncMsg::SnapshotChunk(_))),
            "a new poller is snapshotted first: {first:?}"
        );

        // The move.
        coord.set_pool("p1", "tokyo");
        assert_eq!(
            coord.pool_of("p1", now).as_deref(),
            Some("tokyo"),
            "the registry must follow immediately — the ring is built from it"
        );

        let b = NodeId::new();
        coord
            .reconcile_pool("tokyo", &desired(&[(b, 30)]), now)
            .await;
        let after = drain_syncs(&mut rx, "p1");
        let Some(SyncMsg::SnapshotChunk(snap)) = after.first() else {
            panic!("a move must be followed by a snapshot, not a delta: {after:?}");
        };
        assert_eq!(
            snap.pool.as_deref(),
            Some("tokyo"),
            "the snapshot must name the pool, or the poller cannot re-point its subjects"
        );
        assert_eq!(
            snap.nodes.iter().map(|nj| nj.node_id).collect::<Vec<_>>(),
            vec![b],
            "the poller must be handed the destination pool's nodes, and only those"
        );

        // 🚨 The acceptance side of the rule that makes the move stick: the poller goes on
        // reporting its old pool until it has applied the snapshot, and core must ignore that.
        // Adopting it would revert the move within one beat, with no error anywhere.
        coord
            .observe_heartbeat(heartbeat("p1", "default", Uuid::new_v4()), now)
            .await;
        assert_eq!(
            coord.pool_of("p1", now).as_deref(),
            Some("tokyo"),
            "a heartbeat reporting the old pool must not undo the move"
        );
    }

    /// First contact is the one time the poller's own answer is the only one there is, and it must
    /// still be taken — otherwise a brand-new remote site lands in `default` whatever its kit said.
    #[tokio::test]
    async fn a_pollers_first_heartbeat_still_decides_its_pool() {
        let (coord, _bus, _) = coordinator();
        let now = t0();
        coord
            .observe_heartbeat(heartbeat("new-site", "osaka", Uuid::new_v4()), now)
            .await;
        assert_eq!(coord.pool_of("new-site", now).as_deref(), Some("osaka"));
    }

    /// The refusal core's API is built on: a build that cannot follow a pool change says nothing,
    /// and one that can says so on every beat.
    #[tokio::test]
    async fn a_pollers_caps_are_readable_for_the_move_refusal() {
        let (coord, _bus, _) = coordinator();
        let now = t0();
        let mut hb = heartbeat("old", "default", Uuid::new_v4());
        hb.caps = Vec::new();
        coord.observe_heartbeat(hb, now).await;
        assert_eq!(coord.caps_of("old", now), Some(Vec::new()));

        let mut hb = heartbeat("new", "default", Uuid::new_v4());
        hb.caps = vec![yagra_bus::CAP_POOL_FOLLOW.to_owned()];
        coord.observe_heartbeat(hb, now).await;
        assert!(coord
            .caps_of("new", now)
            .is_some_and(|c| c.iter().any(|c| c == yagra_bus::CAP_POOL_FOLLOW)));

        // Offline reads as "cannot", which is the answer the move path needs: there would be
        // nobody to receive the snapshot that tells the poller where it went.
        assert_eq!(coord.caps_of("new", now + OFFLINE_AFTER), None);
        assert_eq!(coord.caps_of("nobody", now), None);
    }

    #[tokio::test]
    async fn heartbeat_registers_then_reconcile_publishes_snapshot_chunks() {
        let (coord, bus, _stats) = coordinator();
        let mut rx = bus.subscribe_sync();
        let now = t0();
        coord
            .observe_heartbeat(heartbeat("p1", "default", Uuid::new_v4()), now)
            .await;
        assert!(coord.live_pools(now).contains("default"));

        // 101 nodes → two chunks (100 + 1), one shared seq, correct epoch, total_nodes=101.
        let nodes: Vec<(NodeId, u32)> = (0..101).map(|i| (node(i), 30)).collect();
        coord.reconcile_pool("default", &desired(&nodes), now).await;

        let msgs = drain_syncs(&mut rx, "p1");
        assert_eq!(msgs.len(), 2, "101 nodes chunk into 2 snapshot messages");
        let mut seen_nodes = 0usize;
        for (idx, msg) in msgs.iter().enumerate() {
            let SyncMsg::SnapshotChunk(snap) = msg else {
                panic!("expected snapshot chunk, got a delta");
            };
            assert_eq!(snap.epoch, coord.epoch, "chunk carries the core epoch");
            assert_eq!(snap.seq, 1, "all chunks share the one new seq");
            assert_eq!(snap.chunk_total, 2);
            assert_eq!(snap.chunk_index, u32::try_from(idx).unwrap());
            assert_eq!(snap.total_nodes, 101);
            seen_nodes += snap.nodes.len();
        }
        assert_eq!(seen_nodes, 101);
        // First chunk 100 nodes, second 1.
        let SyncMsg::SnapshotChunk(first) = &msgs[0] else {
            unreachable!()
        };
        let SyncMsg::SnapshotChunk(second) = &msgs[1] else {
            unreachable!()
        };
        assert_eq!(first.nodes.len(), 100);
        assert_eq!(second.nodes.len(), 1);
    }

    #[tokio::test]
    async fn identical_reconcile_publishes_nothing() {
        let (coord, bus, _stats) = coordinator();
        let mut rx = bus.subscribe_sync();
        let now = t0();
        let inc = Uuid::new_v4();
        coord
            .observe_heartbeat(heartbeat("p1", "default", inc), now)
            .await;
        let nodes = desired(&[(node(1), 30), (node(2), 30)]);
        coord.reconcile_pool("default", &nodes, now).await;
        let _ = drain_syncs(&mut rx, "p1"); // discard the first snapshot

        // Echo the applied snapshot so no gap/epoch resync is armed, then reconcile the same set.
        coord
            .observe_heartbeat(heartbeat_echo("p1", "default", inc, coord.epoch, 1), now)
            .await;
        coord.reconcile_pool("default", &nodes, now).await;
        assert!(
            drain_syncs(&mut rx, "p1").is_empty(),
            "an unchanged working set publishes nothing"
        );
    }

    #[tokio::test]
    async fn changing_one_spec_sends_a_single_upsert_delta() {
        let (coord, bus, _stats) = coordinator();
        let mut rx = bus.subscribe_sync();
        let now = t0();
        let inc = Uuid::new_v4();
        coord
            .observe_heartbeat(heartbeat("p1", "default", inc), now)
            .await;
        // 4-node baseline so a 1-node change is under the 50% snapshot threshold.
        let base = desired(&[(node(1), 30), (node(2), 30), (node(3), 30), (node(4), 30)]);
        coord.reconcile_pool("default", &base, now).await;
        let _ = drain_syncs(&mut rx, "p1");
        coord
            .observe_heartbeat(heartbeat_echo("p1", "default", inc, coord.epoch, 1), now)
            .await;

        // Change node(2)'s interval → its spec set differs → one upsert, no removes.
        let changed = desired(&[(node(1), 30), (node(2), 60), (node(3), 30), (node(4), 30)]);
        coord.reconcile_pool("default", &changed, now).await;

        let msgs = drain_syncs(&mut rx, "p1");
        assert_eq!(msgs.len(), 1);
        let SyncMsg::Delta(delta) = &msgs[0] else {
            panic!("expected a delta");
        };
        assert_eq!(delta.seq, 2, "delta advances the seq");
        assert_eq!(delta.upserts.len(), 1);
        assert_eq!(delta.upserts[0].node_id, node(2));
        assert!(delta.removes.is_empty());
        assert!(matches!(
            delta.upserts[0].specs[0].check,
            CheckSpec::Icmp(_)
        ));
    }

    #[tokio::test]
    async fn removing_a_node_sends_a_remove_delta() {
        let (coord, bus, _stats) = coordinator();
        let mut rx = bus.subscribe_sync();
        let now = t0();
        let inc = Uuid::new_v4();
        coord
            .observe_heartbeat(heartbeat("p1", "default", inc), now)
            .await;
        let base = desired(&[(node(1), 30), (node(2), 30), (node(3), 30), (node(4), 30)]);
        coord.reconcile_pool("default", &base, now).await;
        let _ = drain_syncs(&mut rx, "p1");
        coord
            .observe_heartbeat(heartbeat_echo("p1", "default", inc, coord.epoch, 1), now)
            .await;

        // Drop node(4).
        let fewer = desired(&[(node(1), 30), (node(2), 30), (node(3), 30)]);
        coord.reconcile_pool("default", &fewer, now).await;

        let msgs = drain_syncs(&mut rx, "p1");
        assert_eq!(msgs.len(), 1);
        let SyncMsg::Delta(delta) = &msgs[0] else {
            panic!("expected a delta");
        };
        assert!(delta.upserts.is_empty());
        assert_eq!(delta.removes, vec![node(4)]);
    }

    /// Every node id carried by a batch of syncs (snapshot chunk nodes + delta upserts).
    fn nodes_in(msgs: &[SyncMsg]) -> HashSet<NodeId> {
        let mut out = HashSet::new();
        for msg in msgs {
            match msg {
                SyncMsg::Delta(d) => out.extend(d.upserts.iter().map(|nj| nj.node_id)),
                SyncMsg::SnapshotChunk(s) => out.extend(s.nodes.iter().map(|nj| nj.node_id)),
            }
        }
        out
    }

    #[tokio::test]
    async fn killing_a_poller_reassigns_its_nodes_to_the_survivor() {
        let (coord, bus, _stats) = coordinator();
        let mut rx = bus.subscribe_sync();
        let now = t0();
        let (i1, i2) = (Uuid::new_v4(), Uuid::new_v4());
        coord
            .observe_heartbeat(heartbeat("p1", "default", i1), now)
            .await;
        coord
            .observe_heartbeat(heartbeat("p2", "default", i2), now)
            .await;

        // Enough uniform nodes that the 2-member ring gives each poller a real share. Snapshot both,
        // capturing which nodes each initially owns (bucketed so neither id's messages are lost).
        let all: Vec<(NodeId, u32)> = uniform_nodes(40).into_iter().map(|n| (n, 30)).collect();
        coord.reconcile_pool("default", &desired(&all), now).await;
        let initial = drain_buckets(&mut rx);
        let p2_initial = nodes_in(initial.get("p2").map_or(&[], Vec::as_slice));
        assert!(
            !p2_initial.is_empty(),
            "p2 must own some nodes to test failover"
        );
        // Echo both applied snapshots so neither is seen as gapped.
        coord
            .observe_heartbeat(heartbeat_echo("p1", "default", i1, coord.epoch, 1), now)
            .await;
        coord
            .observe_heartbeat(heartbeat_echo("p2", "default", i2, coord.epoch, 1), now)
            .await;

        // Advance past the offline window; p2 misses its beat but p1 keeps beating → p1 survives.
        let later = now + OFFLINE_AFTER + Duration::from_secs(1);
        coord
            .observe_heartbeat(heartbeat_echo("p1", "default", i1, coord.epoch, 1), later)
            .await;
        assert!(
            coord.live_pools(later).contains("default"),
            "the survivor keeps the pool live"
        );

        coord.reconcile_pool("default", &desired(&all), later).await;
        let after = drain_buckets(&mut rx);
        assert!(
            after.get("p2").is_none_or(Vec::is_empty),
            "nothing is published to the dead poller"
        );
        // Every one of the dead poller's former nodes reaches the survivor (as a delta upsert, or as
        // a full snapshot when the reassignment exceeds half the survivor's set — either is valid).
        let moved = nodes_in(after.get("p1").map_or(&[], Vec::as_slice));
        for n in &p2_initial {
            assert!(
                moved.contains(n),
                "p2's node {n} must reassign to the survivor"
            );
        }
    }

    // ── Assignment lookups (node detail "Polled by" + Pollers drill-down) ─────

    #[tokio::test]
    async fn owner_of_reports_the_poller_a_node_was_published_to() {
        let (coord, _bus, _stats) = coordinator();
        let now = t0();
        coord
            .observe_heartbeat(heartbeat("p1", "default", Uuid::new_v4()), now)
            .await;
        let published = node(1);
        coord
            .reconcile_pool("default", &desired(&[(published, 30)]), now)
            .await;

        assert_eq!(coord.owner_of(published, now).as_deref(), Some("p1"));
        // A node that never entered a working set has no owner. This is exactly why the lookup
        // reads `published` instead of re-deriving the ring: the ring would happily name p1 for a
        // node the sweep dropped (empty spec set, or a Meraki device core polls itself).
        assert_eq!(coord.owner_of(node(2), now), None);
        assert_eq!(coord.published_nodes("p1", now), Some(vec![published]));
        assert_eq!(coord.pool_of("p1", now).as_deref(), Some("default"));
    }

    #[tokio::test]
    async fn a_dead_pollers_stale_working_set_never_answers_as_current() {
        // Regression: `CoordState::published` is insert-only — nothing reaps a bucket when a poller
        // dies or is deleted, so it keeps its last working set for the life of the core process.
        // Without the liveness gate the node detail would keep naming a poller that stopped
        // polling long ago, which is worse than saying nothing.
        let (coord, _bus, _stats) = coordinator();
        let now = t0();
        coord
            .observe_heartbeat(heartbeat("p1", "default", Uuid::new_v4()), now)
            .await;
        let n = node(1);
        coord
            .reconcile_pool("default", &desired(&[(n, 30)]), now)
            .await;
        assert_eq!(coord.owner_of(n, now).as_deref(), Some("p1"));

        // p1 stops beating; its published bucket is untouched but it is no longer an answer.
        let later = now + OFFLINE_AFTER + Duration::from_secs(1);
        assert_eq!(coord.owner_of(n, later), None);
        assert_eq!(coord.published_nodes("p1", later), None);
        assert_eq!(coord.pool_of("p1", later), None);
    }

    #[tokio::test]
    async fn published_nodes_distinguishes_unknown_from_empty() {
        let (coord, _bus, _stats) = coordinator();
        let now = t0();
        // Never seen → unknown (the drill-down renders "offline", not "owns nothing").
        assert_eq!(coord.published_nodes("nobody", now), None);
        // Registered but not yet reconciled → live with an empty set, which is a real answer.
        coord
            .observe_heartbeat(heartbeat("p1", "default", Uuid::new_v4()), now)
            .await;
        assert_eq!(coord.published_nodes("p1", now), Some(Vec::new()));
    }

    #[tokio::test]
    async fn empty_pool_drops_out_of_live_pools() {
        let (coord, _bus, _stats) = coordinator();
        let now = t0();
        coord
            .observe_heartbeat(heartbeat("p1", "lab", Uuid::new_v4()), now)
            .await;
        assert!(coord.live_pools(now).contains("lab"));
        let later = now + OFFLINE_AFTER + Duration::from_secs(1);
        assert!(
            !coord.live_pools(later).contains("lab"),
            "a pool with no live poller drops out"
        );
    }

    #[tokio::test]
    async fn new_incarnation_forces_a_snapshot() {
        let (coord, bus, _stats) = coordinator();
        let mut rx = bus.subscribe_sync();
        let now = t0();
        let inc = Uuid::new_v4();
        coord
            .observe_heartbeat(heartbeat("p1", "default", inc), now)
            .await;
        let nodes = desired(&[(node(1), 30), (node(2), 30)]);
        coord.reconcile_pool("default", &nodes, now).await;
        let _ = drain_syncs(&mut rx, "p1");
        coord
            .observe_heartbeat(heartbeat_echo("p1", "default", inc, coord.epoch, 1), now)
            .await;

        // A restart: new incarnation → next reconcile snapshots even with the same set.
        coord
            .observe_heartbeat(heartbeat("p1", "default", Uuid::new_v4()), now)
            .await;
        coord.reconcile_pool("default", &nodes, now).await;
        let msgs = drain_syncs(&mut rx, "p1");
        assert!(
            msgs.iter().all(|m| matches!(m, SyncMsg::SnapshotChunk(_))),
            "a restarted poller is resnapshotted"
        );
        assert_eq!(msgs.len(), 1);
    }

    #[tokio::test]
    async fn explicit_sync_request_forces_a_snapshot() {
        let (coord, bus, _stats) = coordinator();
        let mut rx = bus.subscribe_sync();
        let now = t0();
        let inc = Uuid::new_v4();
        coord
            .observe_heartbeat(heartbeat("p1", "default", inc), now)
            .await;
        let nodes = desired(&[(node(1), 30), (node(2), 30)]);
        coord.reconcile_pool("default", &nodes, now).await;
        let _ = drain_syncs(&mut rx, "p1");
        coord
            .observe_heartbeat(heartbeat_echo("p1", "default", inc, coord.epoch, 1), now)
            .await;

        coord
            .observe_sync_request(
                SyncRequest {
                    poller_id: "p1".to_owned(),
                    pool: "default".to_owned(),
                    incarnation: inc,
                },
                now,
            )
            .await;
        coord.reconcile_pool("default", &nodes, now).await;
        let msgs = drain_syncs(&mut rx, "p1");
        assert!(msgs.iter().all(|m| matches!(m, SyncMsg::SnapshotChunk(_))));
    }

    #[tokio::test]
    async fn wrong_last_seq_echo_forces_a_snapshot() {
        let (coord, bus, _stats) = coordinator();
        let mut rx = bus.subscribe_sync();
        let now = t0();
        let inc = Uuid::new_v4();
        coord
            .observe_heartbeat(heartbeat("p1", "default", inc), now)
            .await;
        let nodes = desired(&[(node(1), 30), (node(2), 30)]);
        coord.reconcile_pool("default", &nodes, now).await;
        let _ = drain_syncs(&mut rx, "p1");

        // Poller echoes a seq that doesn't match what core sent (1) → gap → resnapshot.
        coord
            .observe_heartbeat(heartbeat_echo("p1", "default", inc, coord.epoch, 99), now)
            .await;
        coord.reconcile_pool("default", &nodes, now).await;
        let msgs = drain_syncs(&mut rx, "p1");
        assert!(msgs.iter().all(|m| matches!(m, SyncMsg::SnapshotChunk(_))));
    }

    #[tokio::test]
    async fn wrong_epoch_echo_forces_a_snapshot() {
        let (coord, bus, _stats) = coordinator();
        let mut rx = bus.subscribe_sync();
        let now = t0();
        let inc = Uuid::new_v4();
        coord
            .observe_heartbeat(heartbeat("p1", "default", inc), now)
            .await;
        let nodes = desired(&[(node(1), 30), (node(2), 30)]);
        coord.reconcile_pool("default", &nodes, now).await;
        let _ = drain_syncs(&mut rx, "p1");

        // Poller echoes a foreign epoch (e.g. it briefly talked to another core) → resnapshot.
        coord
            .observe_heartbeat(heartbeat_echo("p1", "default", inc, Uuid::new_v4(), 1), now)
            .await;
        coord.reconcile_pool("default", &nodes, now).await;
        let msgs = drain_syncs(&mut rx, "p1");
        assert!(msgs.iter().all(|m| matches!(m, SyncMsg::SnapshotChunk(_))));
    }

    #[tokio::test]
    async fn large_diff_snapshots_instead_of_delta() {
        let (coord, bus, _stats) = coordinator();
        let mut rx = bus.subscribe_sync();
        let now = t0();
        let inc = Uuid::new_v4();
        coord
            .observe_heartbeat(heartbeat("p1", "default", inc), now)
            .await;
        let base = desired(&[(node(1), 30), (node(2), 30), (node(3), 30), (node(4), 30)]);
        coord.reconcile_pool("default", &base, now).await;
        let _ = drain_syncs(&mut rx, "p1");
        coord
            .observe_heartbeat(heartbeat_echo("p1", "default", inc, coord.epoch, 1), now)
            .await;

        // Change 3 of 4 nodes → 3/4 > 50% → snapshot rather than a big delta.
        let changed = desired(&[(node(1), 60), (node(2), 60), (node(3), 60), (node(4), 30)]);
        coord.reconcile_pool("default", &changed, now).await;
        let msgs = drain_syncs(&mut rx, "p1");
        assert_eq!(msgs.len(), 1);
        assert!(matches!(msgs[0], SyncMsg::SnapshotChunk(_)));
    }

    #[tokio::test]
    async fn emptying_a_pollers_set_sends_an_empty_snapshot() {
        let (coord, bus, _stats) = coordinator();
        let mut rx = bus.subscribe_sync();
        let now = t0();
        let inc = Uuid::new_v4();
        coord
            .observe_heartbeat(heartbeat("p1", "default", inc), now)
            .await;
        let base = desired(&[(node(1), 30), (node(2), 30)]);
        coord.reconcile_pool("default", &base, now).await;
        let _ = drain_syncs(&mut rx, "p1");
        coord
            .observe_heartbeat(heartbeat_echo("p1", "default", inc, coord.epoch, 1), now)
            .await;

        // Everything moves away → one empty snapshot chunk (the poller learns it owns nothing).
        coord.reconcile_pool("default", &HashMap::new(), now).await;
        let msgs = drain_syncs(&mut rx, "p1");
        assert_eq!(msgs.len(), 1);
        let SyncMsg::SnapshotChunk(snap) = &msgs[0] else {
            panic!("expected an empty snapshot");
        };
        assert_eq!(snap.chunk_total, 1);
        assert_eq!(snap.total_nodes, 0);
        assert!(snap.nodes.is_empty());
    }

    #[tokio::test]
    async fn record_result_counts_per_poller_and_views_reflect_liveness() {
        let (coord, _bus, _stats) = coordinator();
        let now = t0();
        coord
            .observe_heartbeat(heartbeat("p1", "default", Uuid::new_v4()), now)
            .await;
        coord
            .observe_heartbeat(heartbeat("p2", "lab", Uuid::new_v4()), now)
            .await;
        coord.record_result("p1");
        coord.record_result("p1");
        coord.record_result("p2");

        let views = coord.poller_views(now);
        assert_eq!(views.len(), 2);
        assert_eq!(views[0].id, "p1"); // sorted by id
        assert_eq!(views[0].results_total, 2);
        assert!(views[0].online);
        assert_eq!(views[1].id, "p2");
        assert_eq!(views[1].results_total, 1);

        // Past the offline window with no fresh beat → offline in the view.
        let later = now + OFFLINE_AFTER + Duration::from_secs(1);
        let stale = coord.poller_views(later);
        assert!(stale.iter().all(|v| !v.online));
        assert!(stale[0].seconds_since_seen >= OFFLINE_AFTER_SECS);
    }

    #[tokio::test]
    async fn poller_view_carries_host_sample_from_the_beat() {
        let (coord, _bus, _stats) = coordinator();
        let now = t0();
        let mut hb = heartbeat("p1", "default", Uuid::new_v4());
        hb.host = Some(HostSample {
            cpu_pct: 33.0,
            mem_used_bytes: 4,
            mem_total_bytes: 8,
            ..Default::default()
        });
        coord.observe_heartbeat(hb, now).await;

        let views = coord.poller_views(now);
        let host = views[0].host.as_ref().expect("host sample surfaced");
        assert!((host.cpu_pct - 33.0).abs() < f64::EPSILON);
        assert_eq!(host.mem_used_pct(), Some(50.0));

        // A later beat that omits the host block keeps the last known sample.
        coord
            .observe_heartbeat(heartbeat("p1", "default", views[0].incarnation), now)
            .await;
        assert!(coord.poller_views(now)[0].host.is_some());
    }

    #[tokio::test]
    async fn snapshot_and_delta_counters_advance() {
        let (coord, bus, stats) = coordinator();
        let mut rx = bus.subscribe_sync();
        let now = t0();
        let inc = Uuid::new_v4();
        coord
            .observe_heartbeat(heartbeat("p1", "default", inc), now)
            .await;
        let base = desired(&[(node(1), 30), (node(2), 30), (node(3), 30), (node(4), 30)]);
        coord.reconcile_pool("default", &base, now).await; // snapshot
        let _ = drain_syncs(&mut rx, "p1");
        coord
            .observe_heartbeat(heartbeat_echo("p1", "default", inc, coord.epoch, 1), now)
            .await;
        let changed = desired(&[(node(1), 60), (node(2), 30), (node(3), 30), (node(4), 30)]);
        coord.reconcile_pool("default", &changed, now).await; // delta

        let snap = stats.snapshot();
        assert_eq!(snap.snapshots_published_total, 1);
        assert_eq!(snap.deltas_published_total, 1);
    }

    #[test]
    fn specs_fingerprint_is_stable_and_change_sensitive() {
        // S16: the reconcile baseline stores this fingerprint instead of the decrypted specs. It
        // must behave exactly like `Vec<JobSpec>` equality — equal sets match, any difference flips.
        let a = vec![icmp_spec(node(1), 30)];
        let b = vec![icmp_spec(node(1), 30)];
        assert_eq!(
            specs_fingerprint(&a),
            specs_fingerprint(&b),
            "identical spec sets fingerprint equal (⇒ no upsert)"
        );
        let interval_changed = vec![icmp_spec(node(1), 60)];
        assert_ne!(
            specs_fingerprint(&a),
            specs_fingerprint(&interval_changed),
            "a changed interval flips the fingerprint (⇒ upsert)"
        );
        let extra = vec![icmp_spec(node(1), 30), icmp_spec(node(2), 30)];
        assert_ne!(specs_fingerprint(&a), specs_fingerprint(&extra));
        assert_ne!(specs_fingerprint(&a), specs_fingerprint::<JobSpec>(&[]));
    }

    #[test]
    fn a_borrowed_spec_set_fingerprints_like_the_owned_one() {
        // `reconcile_pool` fingerprints `&[&JobSpec]` borrowed out of the scheduler's cached desired
        // set, so it never copies specs it is not going to send. That only works because serde
        // forwards `&T` to `T` and the two spellings hash identical bytes. If they ever diverged the
        // failure would be silent and expensive: every node would read as changed on every sweep, so
        // core would resend the whole fleet's decrypted credentials to every poller, every round.
        let held = vec![icmp_spec(node(1), 30), icmp_spec(node(2), 60)];
        let borrowed: Vec<&JobSpec> = held.iter().collect();
        assert_eq!(specs_fingerprint(&held), specs_fingerprint(&borrowed));
        assert_eq!(
            specs_fingerprint::<JobSpec>(&[]),
            specs_fingerprint::<&JobSpec>(&[])
        );
    }

    /// A URL job spec, optionally carrying credentials and/or a body rule.
    fn http_spec(n: NodeId, auth: bool, body: bool) -> JobSpec {
        let job = PollJob::http(
            Uuid::nil(),
            n,
            IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
            yagra_bus::HttpCheck {
                url: "https://example.test/health".to_owned(),
                method: yagra_common::HttpMethod::Get,
                expected_status: yagra_common::ExpectedStatus::TwoXx,
                verify_tls: true,
                follow_redirects: true,
                timeout_ms: 5000,
                auth: auth.then(|| yagra_common::HttpAuth::Bearer {
                    token: "tok".to_owned(),
                }),
                body_match: body.then(|| yagra_common::BodyMatch::contains("ok")),
                json_extract: Vec::new(),
                body_max_bytes: yagra_common::DEFAULT_BODY_MAX_BYTES,
            },
            60,
        );
        JobSpec::from_job(&job)
    }

    #[test]
    fn only_the_fields_an_old_poller_would_answer_wrongly_about_demand_a_capability() {
        // The gate is not "this field is new" — it is "an N-1 poller that dropped this field would
        // report something confidently wrong". A plain URL check must stay ungated, or a fleet
        // mid-upgrade would stop polling monitors it can poll perfectly well.
        let n = node(1);
        assert!(spec_required_caps(&http_spec(n, false, false)).is_empty());
        assert!(spec_required_caps(&icmp_spec(n, 30)).is_empty());

        assert_eq!(
            spec_required_caps(&http_spec(n, true, false)),
            [CAP_HTTP_AUTH]
        );
        assert_eq!(
            spec_required_caps(&http_spec(n, false, true)),
            [CAP_HTTP_BODY]
        );
        // Both, independently: a monitor may carry either and a poller may advertise either, so
        // one missing capability must not excuse the other.
        assert_eq!(
            spec_required_caps(&http_spec(n, true, true)),
            [CAP_HTTP_AUTH, CAP_HTTP_BODY]
        );

        // ⚠️ JSON extraction reads the body too, and is deliberately NOT gated. An old poller drops
        // the field and records no sample — an absent series, which is visible — where dropping
        // `body_match` produces a green result it never computed. Withholding here would stop the
        // whole monitor, taking `http_up` with it: a total loss of availability monitoring traded
        // for one missing extra metric. If someone "fixes" this asymmetry, that is what breaks.
        let mut extracting = http_spec(n, false, false);
        if let CheckSpec::Http(http) = &mut extracting.check {
            http.json_extract = vec![yagra_common::JsonExtract {
                metric: "queue_depth".to_owned(),
                path: "queue.depth".to_owned(),
            }];
        }
        assert!(
            spec_required_caps(&extracting).is_empty(),
            "extraction must not withhold the monitor from an older poller"
        );
    }

    #[tokio::test]
    async fn a_poller_without_http_body_is_not_sent_the_content_checked_monitor() {
        // The failure this prevents is the quiet one: an old poller never reads the body and
        // reports `http_up = 1`, which looks exactly like "the content rule passed". Better to
        // publish nothing for that spec and leave a gap someone can see.
        let (coord, bus, _stats) = coordinator();
        let mut rx = bus.subscribe_sync();
        let now = t0();
        let mut hb = heartbeat("p-old", "default", Uuid::new_v4());
        hb.caps = vec![CAP_HTTP_AUTH.to_owned()]; // understands auth, not bodies
        coord.observe_heartbeat(hb, now).await;

        let n = node(1);
        let mut desired = HashMap::new();
        desired.insert(n, vec![http_spec(n, false, true), icmp_spec(n, 30)]);
        coord.reconcile_pool("default", &desired, now).await;

        let published: Vec<JobSpec> = drain_syncs(&mut rx, "p-old")
            .into_iter()
            .flat_map(|m| match m {
                SyncMsg::SnapshotChunk(s) => s.nodes,
                SyncMsg::Delta(d) => d.upserts,
            })
            .flat_map(|nj| nj.specs)
            .collect();
        assert!(
            published
                .iter()
                .all(|s| !matches!(&s.check, CheckSpec::Http(h) if h.body_match.is_some())),
            "the content-checked spec must be withheld"
        );
        assert!(
            published
                .iter()
                .any(|s| matches!(&s.check, CheckSpec::Icmp(_))),
            "withholding one spec must not withhold the node's other work"
        );
    }

    #[tokio::test]
    async fn a_poller_that_gains_a_capability_is_sent_the_spec_it_was_denied() {
        // Why the capability filter still runs for every node every round even though it now only
        // borrows: the fingerprint is taken over the **kept** set, not the desired set. An operator
        // upgrading a poller in place changes nothing about core's desired set, so if the filter
        // were skipped for unchanged nodes the withheld monitor would stay withheld forever — a
        // permanent, silent monitoring gap that looks exactly like a healthy steady state.
        let (coord, bus, _stats) = coordinator();
        let mut rx = bus.subscribe_sync();
        let now = t0();
        let inc = Uuid::new_v4();
        coord
            .observe_heartbeat(heartbeat("p1", "default", inc), now) // no caps advertised
            .await;

        let n = node(1);
        let mut desired = HashMap::new();
        desired.insert(n, vec![http_spec(n, false, true), icmp_spec(n, 30)]);
        coord.reconcile_pool("default", &desired, now).await;
        let _ = drain_syncs(&mut rx, "p1");

        // The poller comes back on a build that reads bodies. Echo the applied snapshot with the
        // same incarnation so nothing rearms `needs_snapshot` — the resend can only come from the
        // diff noticing that the kept set grew.
        let mut upgraded = heartbeat_echo("p1", "default", inc, coord.epoch, 1);
        upgraded.caps = vec![CAP_HTTP_BODY.to_owned()];
        coord.observe_heartbeat(upgraded, now).await;
        coord.reconcile_pool("default", &desired, now).await;

        let published: Vec<JobSpec> = drain_syncs(&mut rx, "p1")
            .into_iter()
            .flat_map(|m| match m {
                SyncMsg::SnapshotChunk(s) => s.nodes,
                SyncMsg::Delta(d) => d.upserts,
            })
            .flat_map(|nj| nj.specs)
            .collect();
        assert!(
            published
                .iter()
                .any(|s| matches!(&s.check, CheckSpec::Http(h) if h.body_match.is_some())),
            "the withheld spec must reach the poller as soon as it advertises the capability"
        );
    }

    #[tokio::test]
    async fn assignment_mirror_write_is_skipped_when_nothing_changes() {
        // S18: the Redis assignment mirror is only rewritten when the sweep actually sent something.
        let (coord, bus, stats) = coordinator();
        let mut rx = bus.subscribe_sync();
        let now = t0();
        let inc = Uuid::new_v4();
        coord
            .observe_heartbeat(heartbeat("p1", "default", inc), now)
            .await;
        let nodes = desired(&[(node(1), 30), (node(2), 30)]);

        // First reconcile publishes a snapshot → mirror written once.
        coord.reconcile_pool("default", &nodes, now).await;
        let _ = drain_syncs(&mut rx, "p1");
        assert_eq!(stats.snapshot().assignment_mirror_writes_total, 1);

        // Identical reconcile publishes nothing → the O(fleet) mirror rewrite is skipped.
        coord
            .observe_heartbeat(heartbeat_echo("p1", "default", inc, coord.epoch, 1), now)
            .await;
        coord.reconcile_pool("default", &nodes, now).await;
        assert_eq!(
            stats.snapshot().assignment_mirror_writes_total,
            1,
            "an unchanged working set skips the assignment-mirror rewrite"
        );

        // A real change publishes a delta → mirror written again.
        let changed = desired(&[(node(1), 60), (node(2), 30)]);
        coord.reconcile_pool("default", &changed, now).await;
        assert_eq!(stats.snapshot().assignment_mirror_writes_total, 2);
    }
}
