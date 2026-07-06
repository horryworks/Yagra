//! Job scheduling: turn inventory into [`PollJob`]s for the bus.
//!
//! The scheduler is the core-side producer of work. For the walking skeleton it builds
//! one ICMP job per node; the full scheduler adds per-metric intervals, jitter, and
//! pool-aware dispatch (ADR-009). Jobs carry everything the poller needs (ADR-003), so
//! this is pure given a node.

use crate::collection::CollectionRepo;
use crate::secrets::{self, CredentialStore, SnmpV3Secret};
use crate::url_check::UrlCheckRepo;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use uuid::Uuid;
use yagra_bus::{
    HttpCheck, IcmpCheck, JobSpec, NatsBus, PollJob, SnmpCheck, SnmpColumn, SnmpMetaColumn,
    SnmpTableCheck, SnmpV3Check, SyncBus, DEFAULT_POOL,
};
use yagra_common::{
    builtin_interface_meta_columns, CollectionItem, CollectionKind, Node, NodeId, ProfileId,
    UrlCheckConfig,
};

/// The effective polling interval (seconds) for a node: its profile's override if one is set, else
/// the global default. Pure (no I/O) so the scheduler's resolution is unit-testable.
#[must_use]
pub fn resolve_interval(
    profile: Option<ProfileId>,
    overrides: &HashMap<Uuid, u32>,
    default_secs: u32,
) -> u32 {
    profile
        .and_then(|p| overrides.get(&p.0).copied())
        .unwrap_or(default_secs)
}

/// Whether a node is due to poll, given the time elapsed since its last dispatch. `None` ⇒ never
/// dispatched (due immediately). Pure so the due-check is unit-testable without a clock.
#[must_use]
pub fn due(elapsed_since_last: Option<Duration>, interval: Duration) -> bool {
    match elapsed_since_last {
        Some(elapsed) => elapsed >= interval,
        None => true,
    }
}

/// Whether a pool is served in **working-set** mode on a sweep: exactly when it has ≥1 live poller
/// (`live_pools`, from the coordinator). Legacy per-job mode is the strict complement, so the
/// scheduler runs each pool in one mode or the other — never both — and a node is never
/// double-polled (ADR-009). Pure so the per-pool mode decision is unit-testable.
#[must_use]
pub fn pool_uses_working_set(pool: &str, live_pools: &std::collections::HashSet<String>) -> bool {
    live_pools.contains(pool)
}

/// Per-poll SNMP timeout pushed to the poller (ms). Matches the periodic and on-demand paths.
const SNMP_TIMEOUT_MS: u32 = 2000;

/// Build an ICMP poll job targeting a node's management address.
#[must_use]
pub fn build_icmp_job(node: &Node, check: IcmpCheck, interval_secs: u32, job_id: Uuid) -> PollJob {
    PollJob::icmp(job_id, node.id, node.address, check, interval_secs)
}

/// Build an SNMP v2c scalar poll job targeting a node's management address.
#[must_use]
pub fn build_snmp_job(node: &Node, check: SnmpCheck, interval_secs: u32, job_id: Uuid) -> PollJob {
    PollJob::snmp(job_id, node.id, node.address, check, interval_secs)
}

/// Build an SNMP v2c table-walk poll job targeting a node's management address.
#[must_use]
pub fn build_snmp_table_job(
    node: &Node,
    check: SnmpTableCheck,
    interval_secs: u32,
    job_id: Uuid,
) -> PollJob {
    PollJob::snmp_table(job_id, node.id, node.address, check, interval_secs)
}

/// Build an SNMP v3 (USM) scalar poll job targeting a node's management address.
#[must_use]
pub fn build_snmp_v3_job(
    node: &Node,
    check: SnmpV3Check,
    interval_secs: u32,
    job_id: Uuid,
) -> PollJob {
    PollJob::snmp_v3(job_id, node.id, node.address, check, interval_secs)
}

/// Map a stored [`UrlCheckConfig`] into the bus [`HttpCheck`]. Auth is not inlined yet (MVP probe
/// is unauthenticated); when it lands, core resolves/decrypts `cfg.credential` here (ADR-018/020).
#[must_use]
pub fn build_http_check(cfg: &UrlCheckConfig) -> HttpCheck {
    HttpCheck {
        url: cfg.url.clone(),
        method: cfg.method,
        expected_status: cfg.expected_status.clone(),
        verify_tls: cfg.verify_tls,
        follow_redirects: cfg.follow_redirects,
        timeout_ms: cfg.timeout_ms,
    }
}

/// Build an HTTP/HTTPS URL-monitor poll job. `target` is the node's management address (display /
/// optional ICMP); the real request target is `check.url`.
#[must_use]
pub fn build_http_job(node: &Node, check: HttpCheck, interval_secs: u32, job_id: Uuid) -> PollJob {
    PollJob::http(job_id, node.id, node.address, check, interval_secs)
}

/// Build the SNMP v3 scalar check for a node from its resolved collection set and v3
/// credential. `None` when the set has no scalar items. Table items are **not** polled
/// over v3 yet (the v3 GETBULK walk is a follow-up) — the caller logs what was skipped.
#[must_use]
pub fn build_snmp_v3_check(
    secret: &SnmpV3Secret,
    items: &[CollectionItem],
    timeout_ms: u32,
) -> Option<SnmpV3Check> {
    let scalar: Vec<SnmpColumn> = items
        .iter()
        .filter(|i| i.kind == CollectionKind::Scalar)
        .map(|i| SnmpColumn {
            metric_name: i.metric_name.clone(),
            oid: i.oid.clone(),
            kind: i.metric_kind,
        })
        .collect();
    (!scalar.is_empty()).then(|| SnmpV3Check {
        user: secret.user.clone(),
        security_level: secret.security_level.clone(),
        auth_protocol: secret.auth_protocol.clone(),
        auth_key: secret.auth_key.clone(),
        priv_protocol: secret.priv_protocol.clone(),
        priv_key: secret.priv_key.clone(),
        oids: Vec::new(),
        columns: scalar,
        timeout_ms,
    })
}

/// Split a resolved collection set into the scalar GET check and the table-walk check for a
/// node, given the resolved `community`. Each is `None` when that shape has no items.
///
/// Scalar items become named [`SnmpColumn`]s so their configured metric names are honoured
/// (not just the poller's built-in OID map). Table items become walk columns, plus the
/// standard interface-metadata columns (ifName/ifAlias/ifSpeed) so discovered interfaces get
/// names — those are PostgreSQL metadata, never TSDB labels (ADR-011).
#[must_use]
pub fn build_snmp_checks(
    community: &str,
    items: &[CollectionItem],
    timeout_ms: u32,
) -> (Option<SnmpCheck>, Option<SnmpTableCheck>) {
    let to_column = |i: &CollectionItem| SnmpColumn {
        metric_name: i.metric_name.clone(),
        oid: i.oid.clone(),
        kind: i.metric_kind,
    };
    let scalar: Vec<SnmpColumn> = items
        .iter()
        .filter(|i| i.kind == CollectionKind::Scalar)
        .map(to_column)
        .collect();
    let table: Vec<SnmpColumn> = items
        .iter()
        .filter(|i| i.kind == CollectionKind::Table)
        .map(to_column)
        .collect();

    let scalar_check = (!scalar.is_empty()).then(|| SnmpCheck {
        community: community.to_owned(),
        oids: Vec::new(),
        columns: scalar,
        timeout_ms,
    });
    let table_check = (!table.is_empty()).then(|| SnmpTableCheck {
        community: community.to_owned(),
        columns: table,
        meta_columns: builtin_interface_meta_columns()
            .into_iter()
            .map(|(field, oid)| SnmpMetaColumn {
                field,
                oid: oid.to_owned(),
            })
            .collect(),
        timeout_ms,
    });
    (scalar_check, table_check)
}

/// A node's resolved SNMP authentication: a v2c community string or a v3 USM document.
pub enum SnmpAuth {
    V2c(String),
    V3(secrets::SnmpV3Secret),
}

/// Assemble every poll job for one node from its already-resolved SNMP auth + collection set:
/// an ICMP liveness job always, plus the SNMP scalar/table (v2c) or scalar (v3) jobs the set
/// calls for. Each job is tagged with a short kind label for logging. Pure (no I/O) — the async
/// credential/collection resolution lives in [`PollDispatcher::build_node_jobs`] — so the
/// job-shape logic is unit-testable without a database or bus.
///
/// `probe_identity` is set on the SNMP job only while the node's maker is unknown, so once a node
/// is classified we stop re-fetching sysDescr every poll (same rule as the periodic scheduler).
#[must_use]
pub fn assemble_node_jobs(
    node: &Node,
    auth: Option<&SnmpAuth>,
    items: &[CollectionItem],
    url_check: Option<&UrlCheckConfig>,
    interval_secs: u32,
) -> Vec<(PollJob, &'static str)> {
    // A URL monitor is its own node kind: dispatch the HTTP job and nothing else. ICMP is *not*
    // added (a URL target may be non-pingable, e.g. behind a CDN); SNMP doesn't apply here.
    if let Some(cfg) = url_check {
        let job = build_http_job(node, build_http_check(cfg), interval_secs, Uuid::new_v4());
        return vec![(job, "http")];
    }
    let mut jobs = Vec::new();
    let probe_identity = node.vendor.is_none();
    match auth {
        Some(SnmpAuth::V2c(community)) => {
            let (scalar, table) = build_snmp_checks(community, items, SNMP_TIMEOUT_MS);
            if let Some(check) = scalar {
                let mut job = build_snmp_job(node, check, interval_secs, Uuid::new_v4());
                job.probe_identity = probe_identity;
                jobs.push((job, "snmp"));
            }
            if let Some(check) = table {
                let job = build_snmp_table_job(node, check, interval_secs, Uuid::new_v4());
                jobs.push((job, "snmp_table"));
            }
        }
        Some(SnmpAuth::V3(secret)) => {
            // v3 polls the scalar set; table walks over v3 are a follow-up (the GETBULK walk).
            if let Some(check) = build_snmp_v3_check(secret, items, SNMP_TIMEOUT_MS) {
                let mut job = build_snmp_v3_job(node, check, interval_secs, Uuid::new_v4());
                job.probe_identity = probe_identity;
                jobs.push((job, "snmp_v3"));
            }
        }
        None => {}
    }
    // ICMP liveness is always polled, regardless of SNMP configuration.
    let icmp = build_icmp_job(node, IcmpCheck::default(), interval_secs, Uuid::new_v4());
    jobs.push((icmp, "icmp"));
    jobs
}

/// Turns a single node into the poll jobs the bus needs and publishes them. Shared by the
/// periodic [`run_scheduler`](crate) loop (which jitters each publish to avoid a stampede) and
/// the on-demand "poll now" API action (which publishes immediately). Holds the bus plus the
/// seams needed to resolve a node's SNMP auth and collection set — never a direct poller call
/// (core⇄poller flows only through the bus, ADR-003).
pub struct PollDispatcher {
    bus: Arc<NatsBus>,
    creds: Arc<CredentialStore>,
    collection: Arc<CollectionRepo>,
    /// Per-node URL-monitor configs (a node with one is a URL monitor → HTTP job, no ICMP/SNMP).
    url_checks: Arc<UrlCheckRepo>,
    /// Per-node Meraki bindings (a node with one is polled by the org collector → no ICMP/SNMP).
    meraki_devices: Arc<crate::meraki::MerakiDeviceRepo>,
    /// v2c community fallback for nodes without a bound credential.
    env_community: Option<String>,
    /// Fallback poll interval (seconds) stamped on on-demand "poll now" jobs. The periodic
    /// scheduler resolves the effective interval per node (profile override → DB default) and
    /// passes it explicitly; this is only the manual-poll default (the poller ignores the value).
    interval_secs: u32,
}

impl PollDispatcher {
    #[must_use]
    pub fn new(
        bus: Arc<NatsBus>,
        creds: Arc<CredentialStore>,
        collection: Arc<CollectionRepo>,
        url_checks: Arc<UrlCheckRepo>,
        meraki_devices: Arc<crate::meraki::MerakiDeviceRepo>,
        env_community: Option<String>,
        interval_secs: u32,
    ) -> Self {
        Self {
            bus,
            creds,
            collection,
            url_checks,
            meraki_devices,
            env_community,
            interval_secs,
        }
    }

    /// Resolve a node's poll jobs. A URL monitor (it has a `url_checks` row) gets a single HTTP
    /// job; otherwise SNMP auth (decrypted in core, never the poller — ADR-018/020) + collection
    /// set + the always-on ICMP. The caller resolves the effective interval and decides whether to
    /// jitter (periodic) or publish at once (poll now).
    pub async fn build_node_jobs(
        &self,
        node: &Node,
        interval_secs: u32,
    ) -> Vec<(PollJob, &'static str)> {
        // Meraki short-circuit: a node with a Meraki binding is polled by the org collector, so it
        // emits no per-node ICMP/SNMP/HTTP job (guards the on-demand "poll now" path; the periodic
        // scheduler already skips these nodes by preloading their ids).
        match self.meraki_devices.get(node.id.as_uuid()).await {
            Ok(Some(_)) => return Vec::new(),
            Ok(None) => {}
            Err(e) => {
                tracing::warn!(node = %node.id, error = %e, "meraki-device load failed; treating as non-Meraki node");
            }
        }
        // URL monitor short-circuit: a node with a URL check is HTTP-only.
        match self.url_checks.get(node.id.as_uuid()).await {
            Ok(Some(cfg)) => return assemble_node_jobs(node, None, &[], Some(&cfg), interval_secs),
            Ok(None) => {}
            Err(e) => {
                tracing::warn!(node = %node.id, error = %e, "url-check load failed; treating as non-URL node");
            }
        }
        let auth = resolve_snmp_auth(&self.creds, node, self.env_community.as_deref()).await;
        // Only resolve the collection set when SNMP is configured (ICMP needs none).
        let items = if auth.is_some() {
            resolve_node_collection(&self.collection, node).await
        } else {
            Vec::new()
        };
        if matches!(auth, Some(SnmpAuth::V3(_)))
            && items.iter().any(|i| i.kind == CollectionKind::Table)
        {
            tracing::debug!(node = %node.id, "v3 table items skipped (v3 walk not yet supported)");
        }
        assemble_node_jobs(node, auth.as_ref(), &items, None, interval_secs)
    }

    /// The node's poll jobs as reusable working-set [`JobSpec`]s (ADR-020) — the distributed-pool
    /// analogue of [`Self::build_node_jobs`], reusing its exact auth/collection resolution (zero
    /// duplication). The per-dispatch `job_id` is dropped here; the poller stamps a fresh one each
    /// time it schedules the spec locally.
    pub async fn build_node_specs(&self, node: &Node, interval_secs: u32) -> Vec<JobSpec> {
        self.build_node_jobs(node, interval_secs)
            .await
            .iter()
            .map(|(job, _kind)| JobSpec::from_job(job))
            .collect()
    }

    /// Publish one already-built job to `pool`'s subject, bumping the published-jobs counter (used
    /// by the periodic scheduler after its per-job jitter delay). Routing per pool keeps a job local
    /// to the pollers serving that pool (ADR-009). Returns whether the publish succeeded.
    pub async fn publish_job(&self, job: PollJob, kind: &str, node: NodeId, pool: &str) -> bool {
        publish(&self.bus, pool, job, kind, node).await
    }

    /// Build and immediately publish every poll job for a node (no jitter) — the operator's
    /// "poll now" action. Routes to the node's pool subject (default [`DEFAULT_POOL`]). Returns how
    /// many jobs were published. Stamps the dispatcher's default interval (the poller ignores the
    /// value; cadence is owned by the periodic scheduler).
    pub async fn poll_now(&self, node: &Node) -> usize {
        let pool = node.pool.as_deref().unwrap_or(DEFAULT_POOL);
        let jobs = self.build_node_jobs(node, self.interval_secs).await;
        let mut published = 0u64;
        for (job, kind) in jobs {
            if publish(&self.bus, pool, job, kind, node.id).await {
                published += 1;
            }
        }
        metrics::counter!("yagra_manual_poll_jobs_total").increment(published);
        usize::try_from(published).unwrap_or(usize::MAX)
    }
}

/// Resolve a node's effective collection set, defaulting to the built-in catalog when nothing is
/// configured (or the lookup fails) so polling always has a sensible default.
async fn resolve_node_collection(collection: &CollectionRepo, node: &Node) -> Vec<CollectionItem> {
    match collection
        .list_items_for_node(node.id.as_uuid(), node.profile.map(|p| p.0))
        .await
    {
        Ok(scoped) => {
            let resolved = yagra_common::resolve_collection_set(&scoped);
            if resolved.is_empty() {
                yagra_common::builtin_catalog()
            } else {
                resolved
            }
        }
        Err(e) => {
            tracing::warn!(node = %node.id, error = %e, "collection load failed; using built-in catalog");
            yagra_common::builtin_catalog()
        }
    }
}

/// Resolve a node's SNMP auth from its bound credential (decrypted in core, never the poller —
/// ADR-018/020). The credential `kind` picks the protocol: `snmp_v3` secrets are USM JSON docs;
/// anything else is treated as a v2c community (back-compat with credentials created before kinds
/// were meaningful). The env community is a v2c fallback for nodes without a bound credential.
async fn resolve_snmp_auth(
    creds: &CredentialStore,
    node: &Node,
    env_community: Option<&str>,
) -> Option<SnmpAuth> {
    if let Some(cred) = node.credential {
        match creds.open(cred.as_uuid()).await {
            Ok(Some((kind, bytes))) => {
                if kind == secrets::KIND_SNMP_V3 {
                    match secrets::SnmpV3Secret::parse(&bytes) {
                        Ok(secret) => return Some(SnmpAuth::V3(secret)),
                        // Static reason only — never echo any part of the secret.
                        Err(reason) => {
                            tracing::warn!(node = %node.id, %reason, "invalid snmp_v3 credential");
                        }
                    }
                } else if let Ok(community) = String::from_utf8(bytes) {
                    return Some(SnmpAuth::V2c(community));
                }
            }
            Ok(None) => tracing::warn!(node = %node.id, "bound credential not found"),
            Err(e) => tracing::warn!(node = %node.id, error = %e, "credential decrypt failed"),
        }
    }
    env_community.map(|c| SnmpAuth::V2c(c.to_owned()))
}

/// Publish a job to `pool`'s subject and bump the published-jobs counter, logging failures.
/// Returns success. Uses [`SyncBus::publish_job_for_pool`] so legacy per-job dispatch, "poll now",
/// and Meraki all stay local to the target pool (ADR-009) while an old wildcard poller still
/// absorbs them (N/N-1 compatible).
async fn publish(bus: &NatsBus, pool: &str, mut job: PollJob, kind: &str, node: NodeId) -> bool {
    // Seed the job with this dispatch's trace context so the poller's poll span (and core's later
    // result-ingest span) join one distributed trace (yagra-telemetry). A short-lived dispatch span
    // is the trace root for legacy/poll-now jobs; injection is a no-op when tracing export is off,
    // so `trace_context` stays empty and off the wire (N/N-1 safe, ADR-017). Enter/inject/drop the
    // guard before the await — never hold a span guard across `.await`.
    {
        let dispatch = tracing::info_span!("dispatch.poll_job", %kind, node = %node, pool);
        let _enter = dispatch.enter();
        job.trace_context = yagra_telemetry::current_trace_context();
    }
    match bus.publish_job_for_pool(pool, job).await {
        Ok(()) => {
            metrics::counter!("yagra_jobs_published_total").increment(1);
            true
        }
        Err(e) => {
            tracing::warn!(error = %e, %kind, node = %node, pool, "failed to publish job");
            false
        }
    }
}

/// Live self-monitoring counters for the poll loop, shared between the scheduler (producer) and
/// the result consumer. Lock-free atomics — updated on the hot path, read by the poller-health
/// endpoint. Per-poller breakdown needs poller identity on the bus and is a later addition.
#[derive(Default)]
pub struct SchedulerStats {
    last_sweep_ms: std::sync::atomic::AtomicI64,
    jobs_last_round: std::sync::atomic::AtomicU64,
    results_total: std::sync::atomic::AtomicU64,
    // Distributed poller pool (ADR-009/020). Counters bumped by the coordinator as it distributes
    // working sets; the two `pools_*` are gauge-style (overwritten each sweep by the scheduler).
    snapshots_published_total: std::sync::atomic::AtomicU64,
    deltas_published_total: std::sync::atomic::AtomicU64,
    pools_working_set: std::sync::atomic::AtomicU64,
    pools_legacy: std::sync::atomic::AtomicU64,
}

/// A point-in-time view of [`SchedulerStats`] for the API.
#[derive(serde::Serialize)]
pub struct SchedulerStatsSnapshot {
    /// When the last poll round was dispatched (Unix ms), or `None` if none yet.
    pub last_sweep_unix_ms: Option<i64>,
    /// Jobs published in the most recent round (legacy per-job dispatch only).
    pub jobs_last_round: u64,
    /// Total poll results consumed since start.
    pub results_total: u64,
    /// Working-set snapshots core has published to pollers since start (ADR-020).
    pub snapshots_published_total: u64,
    /// Working-set deltas core has published to pollers since start (ADR-020).
    pub deltas_published_total: u64,
    /// Pools served in working-set mode in the most recent sweep (a live poller owns them).
    pub pools_working_set: u64,
    /// Pools served in legacy per-job mode in the most recent sweep (no live poller).
    pub pools_legacy: u64,
}

impl SchedulerStats {
    /// Record a completed dispatch round (`jobs` published, stamped now).
    pub fn record_sweep(&self, jobs: u64) {
        use std::sync::atomic::Ordering;
        let ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
            .unwrap_or(0);
        self.last_sweep_ms.store(ms, Ordering::Relaxed);
        self.jobs_last_round.store(jobs, Ordering::Relaxed);
    }

    /// Count one consumed poll result.
    pub fn record_result(&self) {
        self.results_total
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    /// Count one working-set snapshot published (a snapshot, not each of its chunks).
    pub fn record_snapshot(&self) {
        self.snapshots_published_total
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    /// Count one working-set delta published.
    pub fn record_delta(&self) {
        self.deltas_published_total
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    /// Record how many pools ran in each mode this sweep (gauge-style: overwrites).
    pub fn set_pool_modes(&self, working_set: u64, legacy: u64) {
        use std::sync::atomic::Ordering;
        self.pools_working_set.store(working_set, Ordering::Relaxed);
        self.pools_legacy.store(legacy, Ordering::Relaxed);
    }

    /// Snapshot for the API.
    #[must_use]
    pub fn snapshot(&self) -> SchedulerStatsSnapshot {
        use std::sync::atomic::Ordering;
        let ms = self.last_sweep_ms.load(Ordering::Relaxed);
        SchedulerStatsSnapshot {
            last_sweep_unix_ms: (ms > 0).then_some(ms),
            jobs_last_round: self.jobs_last_round.load(Ordering::Relaxed),
            results_total: self.results_total.load(Ordering::Relaxed),
            snapshots_published_total: self.snapshots_published_total.load(Ordering::Relaxed),
            deltas_published_total: self.deltas_published_total.load(Ordering::Relaxed),
            pools_working_set: self.pools_working_set.load(Ordering::Relaxed),
            pools_legacy: self.pools_legacy.load(Ordering::Relaxed),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};
    use yagra_bus::CheckSpec;
    use yagra_common::NodeId;

    #[test]
    fn interval_resolves_profile_override_then_default() {
        let p1 = ProfileId::new();
        let p2 = ProfileId::new();
        let mut overrides = HashMap::new();
        overrides.insert(p1.0, 15u32);
        // Profile with an override → its value.
        assert_eq!(resolve_interval(Some(p1), &overrides, 30), 15);
        // Profile without an override → the global default.
        assert_eq!(resolve_interval(Some(p2), &overrides, 30), 30);
        // No profile at all → the global default.
        assert_eq!(resolve_interval(None, &overrides, 30), 30);
    }

    #[test]
    fn pool_mode_is_working_set_xor_legacy() {
        use std::collections::HashSet;
        let live: HashSet<String> = ["tokyo".to_string()].into_iter().collect();
        // A pool with a live poller runs working-set; one without runs legacy.
        assert!(pool_uses_working_set("tokyo", &live));
        assert!(!pool_uses_working_set("osaka", &live));
        // For every pool the two modes are exclusive (working-set == !legacy) — no double-polling.
        for pool in ["tokyo", "osaka", "default"] {
            let working_set = pool_uses_working_set(pool, &live);
            let legacy = !pool_uses_working_set(pool, &live);
            assert_ne!(working_set, legacy, "a pool is working-set XOR legacy");
        }
    }

    #[test]
    fn due_when_never_dispatched_or_interval_elapsed() {
        let interval = Duration::from_secs(30);
        // Never dispatched → due immediately.
        assert!(due(None, interval));
        // Less than the interval has passed → not due.
        assert!(!due(Some(Duration::from_secs(29)), interval));
        // Exactly the interval → due (>=).
        assert!(due(Some(Duration::from_secs(30)), interval));
        // Well past the interval → due.
        assert!(due(Some(Duration::from_secs(120)), interval));
    }

    #[test]
    fn icmp_job_targets_node_address_and_id() {
        let addr = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 5));
        let node = Node::new(NodeId::new(), "edge-1", addr);

        let job = build_icmp_job(&node, IcmpCheck::default(), 30, Uuid::nil());

        assert_eq!(job.node_id, node.id);
        assert_eq!(job.target, addr);
        assert_eq!(job.interval_secs, 30);
        assert!(matches!(job.check, CheckSpec::Icmp(_)));
    }

    #[test]
    fn snmp_job_carries_check_and_target() {
        let addr = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 6));
        let node = Node::new(NodeId::new(), "sw-1", addr);
        let check = SnmpCheck {
            community: "public".to_owned(),
            oids: vec!["1.3.6.1.2.1.1.3.0".to_owned()],
            columns: Vec::new(),
            timeout_ms: 2000,
        };
        let job = build_snmp_job(&node, check, 60, Uuid::nil());
        assert_eq!(job.target, addr);
        assert!(matches!(job.check, CheckSpec::Snmp(_)));
    }

    #[test]
    fn snmp_table_job_carries_table_check() {
        let addr = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 7));
        let node = Node::new(NodeId::new(), "sw-2", addr);
        let check = SnmpTableCheck {
            community: "public".to_owned(),
            columns: vec![SnmpColumn {
                metric_name: "if_hc_in_octets".to_owned(),
                oid: "1.3.6.1.2.1.31.1.1.1.6".to_owned(),
                kind: yagra_common::MetricKind::Counter,
            }],
            meta_columns: Vec::new(),
            timeout_ms: 2000,
        };
        let job = build_snmp_table_job(&node, check, 60, Uuid::nil());
        assert_eq!(job.target, addr);
        assert!(matches!(job.check, CheckSpec::SnmpTable(_)));
    }

    fn item(metric: &str, oid: &str, kind: CollectionKind) -> CollectionItem {
        CollectionItem {
            metric_name: metric.to_owned(),
            oid: oid.to_owned(),
            kind,
            metric_kind: yagra_common::MetricKind::Gauge,
        }
    }

    #[test]
    fn build_snmp_checks_splits_scalar_and_table_and_adds_meta() {
        let items = [
            item(
                "snmp_sys_uptime_ticks",
                "1.3.6.1.2.1.1.3.0",
                CollectionKind::Scalar,
            ),
            item(
                "if_oper_status",
                "1.3.6.1.2.1.2.2.1.8",
                CollectionKind::Table,
            ),
        ];
        let (scalar, table) = build_snmp_checks("public", &items, 2000);
        let scalar = scalar.expect("scalar check present");
        assert_eq!(scalar.columns.len(), 1);
        assert_eq!(scalar.columns[0].metric_name, "snmp_sys_uptime_ticks");
        assert!(scalar.oids.is_empty()); // configured scalars travel as named columns
        let table = table.expect("table check present");
        assert_eq!(table.columns.len(), 1);
        // Standard interface-metadata columns are attached so interfaces get names.
        assert!(!table.meta_columns.is_empty());
    }

    #[test]
    fn build_snmp_checks_empty_set_yields_no_checks() {
        let (scalar, table) = build_snmp_checks("public", &[], 2000);
        assert!(scalar.is_none());
        assert!(table.is_none());
    }

    fn v3_secret() -> SnmpV3Secret {
        SnmpV3Secret::parse(
            br#"{"user":"monitor","security_level":"authpriv","auth_protocol":"sha256",
                 "auth_key":"a-pass","priv_protocol":"aes128","priv_key":"p-pass"}"#,
        )
        .expect("valid v3 secret")
    }

    #[test]
    fn build_snmp_v3_check_carries_usm_params_and_scalar_columns_only() {
        let items = [
            item(
                "snmp_sys_uptime_ticks",
                "1.3.6.1.2.1.1.3.0",
                CollectionKind::Scalar,
            ),
            item(
                "if_oper_status",
                "1.3.6.1.2.1.2.2.1.8",
                CollectionKind::Table,
            ),
        ];
        let check = build_snmp_v3_check(&v3_secret(), &items, 2000).expect("scalar check");
        assert_eq!(check.user, "monitor");
        assert_eq!(check.security_level, "authpriv");
        assert_eq!(check.auth_protocol.as_deref(), Some("sha256"));
        // Only the scalar item travels; tables are a v3 follow-up.
        assert_eq!(check.columns.len(), 1);
        assert_eq!(check.columns[0].metric_name, "snmp_sys_uptime_ticks");
        assert!(check.oids.is_empty());
    }

    #[test]
    fn build_snmp_v3_check_without_scalars_yields_none() {
        let items = [item(
            "if_oper_status",
            "1.3.6.1.2.1.2.2.1.8",
            CollectionKind::Table,
        )];
        assert!(build_snmp_v3_check(&v3_secret(), &items, 2000).is_none());
    }

    #[test]
    fn snmp_v3_job_targets_node_and_tags_check() {
        let addr = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 8));
        let node = Node::new(NodeId::new(), "fw-1", addr);
        let items = [item(
            "snmp_sys_uptime_ticks",
            "1.3.6.1.2.1.1.3.0",
            CollectionKind::Scalar,
        )];
        let check = build_snmp_v3_check(&v3_secret(), &items, 2000).unwrap();
        let job = build_snmp_v3_job(&node, check, 60, Uuid::nil());
        assert_eq!(job.target, addr);
        assert!(matches!(job.check, CheckSpec::SnmpV3(_)));
    }

    fn node(name: &str) -> Node {
        Node::new(NodeId::new(), name, IpAddr::V4(Ipv4Addr::new(10, 0, 0, 20)))
    }

    /// The kinds present in an assembled job list (order-independent assertions).
    fn kinds(jobs: &[(PollJob, &'static str)]) -> Vec<&'static str> {
        jobs.iter().map(|(_, k)| *k).collect()
    }

    #[test]
    fn assemble_without_auth_yields_icmp_only() {
        // No SNMP credential resolved ⇒ liveness only, regardless of the collection set.
        let jobs = assemble_node_jobs(&node("ping-only"), None, &[], None, 30);
        assert_eq!(kinds(&jobs), vec!["icmp"]);
        assert!(matches!(jobs[0].0.check, CheckSpec::Icmp(_)));
    }

    #[test]
    fn assemble_url_monitor_yields_http_only_no_icmp() {
        // A URL monitor is HTTP-only: no ICMP (target may be non-pingable) and no SNMP.
        let cfg = UrlCheckConfig::new("https://api.example.com/health");
        let jobs = assemble_node_jobs(&node("url-mon"), None, &[], Some(&cfg), 30);
        assert_eq!(kinds(&jobs), vec!["http"]);
        assert!(matches!(jobs[0].0.check, CheckSpec::Http(_)));
    }

    #[test]
    fn assemble_v2c_builds_scalar_table_and_icmp_and_probes_identity() {
        let items = [
            item(
                "snmp_sys_uptime_ticks",
                "1.3.6.1.2.1.1.3.0",
                CollectionKind::Scalar,
            ),
            item(
                "if_oper_status",
                "1.3.6.1.2.1.2.2.1.8",
                CollectionKind::Table,
            ),
        ];
        let auth = SnmpAuth::V2c("public".to_owned());
        let jobs = assemble_node_jobs(&node("sw"), Some(&auth), &items, None, 30);
        assert_eq!(kinds(&jobs), vec!["snmp", "snmp_table", "icmp"]);
        // The maker is unknown (vendor None), so the scalar SNMP job carries the identity probe.
        let snmp = jobs.iter().find(|(_, k)| *k == "snmp").unwrap();
        assert!(snmp.0.probe_identity, "unknown-maker node probes sysDescr");
    }

    #[test]
    fn assemble_skips_identity_probe_once_maker_is_known() {
        let mut n = node("classified");
        n.vendor = Some("Cisco".to_owned());
        let items = [item(
            "snmp_sys_uptime_ticks",
            "1.3.6.1.2.1.1.3.0",
            CollectionKind::Scalar,
        )];
        let auth = SnmpAuth::V2c("public".to_owned());
        let jobs = assemble_node_jobs(&n, Some(&auth), &items, None, 30);
        let snmp = jobs.iter().find(|(_, k)| *k == "snmp").unwrap();
        assert!(
            !snmp.0.probe_identity,
            "known-maker node stops probing sysDescr"
        );
    }

    #[test]
    fn assemble_v3_builds_scalar_and_icmp_only() {
        // v3 polls the scalar set; the table item is not walked over v3 yet.
        let items = [
            item(
                "snmp_sys_uptime_ticks",
                "1.3.6.1.2.1.1.3.0",
                CollectionKind::Scalar,
            ),
            item(
                "if_oper_status",
                "1.3.6.1.2.1.2.2.1.8",
                CollectionKind::Table,
            ),
        ];
        let auth = SnmpAuth::V3(v3_secret());
        let jobs = assemble_node_jobs(&node("fw"), Some(&auth), &items, None, 30);
        assert_eq!(kinds(&jobs), vec!["snmp_v3", "icmp"]);
        assert!(matches!(
            jobs.iter().find(|(_, k)| *k == "snmp_v3").unwrap().0.check,
            CheckSpec::SnmpV3(_)
        ));
    }
}
