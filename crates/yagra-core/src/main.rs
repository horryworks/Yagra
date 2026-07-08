//! Yagra-core — Core/API.
//!
//! Orchestration, scheduling, and the northbound REST API (`/api/v1`, ADR-008/019) the
//! WebUI and external automation consume. Dispatches polling jobs to workers
//! (Yagra-poller) through the bus (Yagra-bus, ADR-003) and owns metadata-store
//! interaction (PostgreSQL) and the TSDB write/read path (VictoriaMetrics).
//!
//! Two run modes (see [`config`]): **live** when the store/bus URLs are present (real
//! PostgreSQL + NATS + VictoriaMetrics), else an in-memory **skeleton** so a bare
//! `cargo run` still serves the API.

mod ack;
mod alerts;
mod analysis;
mod api;
mod audit;
mod auth;
mod classification;
mod collection;
mod config;
mod dashboard;
mod discovery;
mod events;
mod groups;
mod history;
mod maintenance;
mod meraki;
mod mib;
mod notifications;
// Distributed poller pool (ADR-009/020): the coordinator owns the live registry + working-set
// distribution and consumes the ring / Redis mirror / durable inventory below.
mod coordinator;
mod pollers;
mod repo;
mod reports;
mod ring;
mod scheduler;
mod secrets;
mod sink;
mod store;
mod thresholds;
mod url_check;
mod volatile;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use ack::AckRepo;
use alerts::{ActiveMute, AlertConfig, AlertManager, NodeMeta, Notifier};
use api::{AdminState, ApiState};
use audit::AuditRepo;
use auth::{SessionStore, UserStore};
use axum::routing::get;
use collection::CollectionRepo;
use config::Config;
use coordinator::Coordinator;
use dashboard::{DashboardRepo, SharedDashboardRepo};
use discovery::DiscoveryRunner;
use futures::stream::{Stream, StreamExt};
use history::AlertHistoryStore;
use maintenance::MaintenanceRepo;
use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};
use mib::MibRepo;
use notifications::NotificationRepo;
use pollers::PollerRepo;
use repo::{NodeListing, NodeRepo, StaticNodeList};
use secrets::CredentialStore;
use sink::InMemorySink;
use store::{MetricStore, VmStore};
use thresholds::ThresholdStore;
use uuid::Uuid;
use volatile::VolatileStore;
use yagra_bus::{NatsBus, PollResult, DEFAULT_POOL};
use yagra_topology::Topology;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Structured logs + optional OpenTelemetry span export (self-observability). The guard flushes
    // spans at shutdown, so keep it alive for the whole process (`main` awaits the run loop below).
    let _telemetry = yagra_telemetry::init("yagra-core");

    // Self-observability (ADR / monitoring-conventions): install the Prometheus recorder
    // up front so `metrics::counter!` calls anywhere are captured; `/metrics` renders it.
    let metrics = PrometheusBuilder::new()
        .install_recorder()
        .map_err(|e| anyhow::anyhow!("install Prometheus recorder: {e}"))?;

    match Config::from_env() {
        Some(cfg) => run_live(cfg, metrics).await,
        None => run_skeleton(metrics).await,
    }
}

/// Live mode: PostgreSQL + NATS + VictoriaMetrics, real ICMP polling end to end.
async fn run_live(cfg: Config, metrics: PrometheusHandle) -> anyhow::Result<()> {
    tracing::info!(
        interval = cfg.poll_interval_secs,
        "Yagra-core starting (live mode)"
    );

    // Metadata store: connect, migrate, seed demo inventory if empty.
    let repo = Arc::new(NodeRepo::connect(&cfg.database_url).await?);
    repo.migrate().await?;
    repo.seed_demo_nodes_if_empty().await?;
    repo.seed_builtin_profiles().await?;
    // Seed the runtime-settings singleton, honoring YAGRA_POLL_INTERVAL_SECS as the *initial*
    // default on first boot only (ON CONFLICT DO NOTHING preserves later UI edits). After this the
    // DB value is authoritative and the scheduler re-reads it each round.
    repo.seed_default_poll_interval(cfg.poll_interval_secs)
        .await?;
    let mib = Arc::new(MibRepo::new(repo.pool()));
    mib.seed_builtin().await?;

    // Device classification: the operator-editable rules that map a discovered device's
    // sysObjectID / sysDescr to a suggested profile. Loaded into an in-memory classifier that
    // discovery consults per candidate; reloaded on a refresh loop + after a rule edit.
    let classification = Arc::new(classification::ClassificationRepo::new(repo.pool()));
    let classifier = Arc::new(classification::Classifier::empty());
    if let Err(e) = classifier.reload(&classification).await {
        tracing::warn!(error = %e, "failed to load classification rules; discovery will use the generic fallback");
    }

    // TSDB + bus.
    let store: Arc<dyn MetricStore> = Arc::new(VmStore::new(cfg.tsdb_url.clone()));
    let bus = Arc::new(connect_bus(&cfg.bus_url).await?);

    // Alert engine + notifier (env default route + DB channels/rules, ADR-015) + history.
    let alerts = Arc::new(AlertManager::new());
    let notifier = Arc::new(Notifier::from_env());
    let notifications = Arc::new(NotificationRepo::from_env(repo.pool()));
    let history = Arc::new(AlertHistoryStore::new(repo.pool()));
    // Inbound ack reflection from external tools (PagerDuty / JSM); read-only display (ADR-015).
    let acks = Arc::new(AckRepo::new(repo.pool()));

    // Result consumer: bus → TSDB + alert engine (+ history + notifications + interface
    // inventory upsert).
    // Self-monitoring counters for the poll loop, shared by the consumer + scheduler and read by
    // the poller-health endpoint.
    let scheduler_stats = Arc::new(scheduler::SchedulerStats::default());
    // Cisco Meraki: org/device stores (metadata in Postgres) + the per-org single-flight tracker
    // (shared by the Meraki scheduler and the result consumer, which clears an org's flight on the
    // collect's first result). Read-only integration.
    let meraki_orgs = Arc::new(meraki::MerakiOrgRepo::new(repo.pool()));
    let meraki_devices = Arc::new(meraki::MerakiDeviceRepo::new(repo.pool()));
    let meraki_inflight = Arc::new(meraki::MerakiInflight::new());

    // Distributed poller pool (ADR-009/020): the coordinator is core's live poller registry +
    // working-set publisher. Its Redis mirror (from config, independent of live/skeleton) and
    // durable PG inventory are best-effort — their loss only degrades observability (ADR-017); the
    // in-memory registry is the source of truth. Constructed before the result consumer so the
    // consumer can attribute results to their poller.
    let volatile = Arc::new(VolatileStore::from_optional_url(cfg.redis_url.as_deref()));
    let poller_repo = Arc::new(PollerRepo::new(repo.pool()));
    let coordinator = Arc::new(Coordinator::new(
        bus.clone(),
        volatile,
        Some(poller_repo.clone()),
        scheduler_stats.clone(),
        Some(store.clone()),
    ));
    // Feed the registry: heartbeats (liveness/telemetry) and snapshot requests. Thin consumer loops
    // that end when the bus stream closes (shutdown), mirroring the other bus consumers.
    {
        let coordinator = coordinator.clone();
        let stream = Box::pin(bus.subscribe_heartbeats().await?);
        tokio::spawn(coordinator.run_heartbeat_consumer(stream));
    }
    {
        let coordinator = coordinator.clone();
        let stream = Box::pin(bus.subscribe_sync_requests().await?);
        tokio::spawn(coordinator.run_sync_request_consumer(stream));
    }

    // Core self-observability (monitoring-conventions): sample core's own host every
    // HOST_SAMPLE_SECS, cache the latest for the System Health page, and persist the `yagra_host_*`
    // series to the TSDB (core is the single writer for its own host + every poller's).
    let core_host: api::CoreHostSample = Arc::new(std::sync::Mutex::new(None));
    {
        let store = store.clone();
        let cache = core_host.clone();
        let pool = repo.pool().clone();
        tokio::spawn(run_host_collector(store, cache, pool));
    }

    {
        let store = store.clone();
        let alerts = alerts.clone();
        let notifier = notifier.clone();
        let history = history.clone();
        let repo = repo.clone();
        let stats = scheduler_stats.clone();
        let meraki_inflight = meraki_inflight.clone();
        let coordinator = coordinator.clone();
        let results = Box::pin(bus.subscribe_results().await?);
        tokio::spawn(consume_results(
            results,
            store,
            alerts,
            notifier,
            history,
            repo,
            stats,
            meraki_inflight,
            coordinator,
        ));
    }

    // Discovery: a runner that publishes sweep jobs + a consumer that folds results back in,
    // classifying each found device into a suggested profile via the shared classifier.
    let discovery = Arc::new(DiscoveryRunner::new(bus.clone(), classifier.clone()));
    {
        let results = Box::pin(bus.subscribe_discovery_results().await?);
        tokio::spawn(discovery.clone().run_consumer(results));
    }

    // Passive events (Phase 2): syslog/traps arrive from pollers on `yagra.events`,
    // webhooks via the ingest endpoint. The engine matches rules and raises alerts
    // through the same alert/notify/history pipeline as poll results.
    let events_repo = Arc::new(events::EventRepo::new(repo.pool()));
    let event_engine = Arc::new(events::EventEngine::new(
        events_repo.clone(),
        alerts.clone(),
        notifier.clone(),
        history.clone(),
    ));
    event_engine.reload(&repo).await;
    {
        let stream = Box::pin(bus.subscribe_events().await?);
        tokio::spawn(events::consume_events(stream, event_engine.clone()));
    }
    tokio::spawn(events::run_ttl_sweeper(event_engine.clone()));

    // Credential store, shared by the API admin and the scheduler's SNMP resolution.
    let creds = Arc::new(CredentialStore::from_env(repo.pool()));

    // SNMP v2c (ADR-021): community is resolved per node from its bound credential; an env
    // community is a fallback for nodes without one. What to collect comes from the node's
    // resolved collection set (per-node/profile), falling back to the built-in catalog.
    let env_community = std::env::var("YAGRA_SNMP_COMMUNITY")
        .ok()
        .filter(|c| !c.is_empty());
    let collection = Arc::new(CollectionRepo::new(repo.pool()));
    // URL monitors: per-node HTTP/HTTPS check configs. Shared by the dispatcher (a node with one
    // becomes an HTTP-only poll) and the admin API (CRUD).
    let url_checks = Arc::new(url_check::UrlCheckRepo::new(repo.pool()));

    // Poll dispatcher: turns a node into bus jobs (ICMP + SNMP, or HTTP for URL monitors). Shared by
    // the periodic scheduler and the on-demand "poll now" API action so both build jobs the same way.
    let dispatcher = Arc::new(scheduler::PollDispatcher::new(
        bus.clone(),
        creds.clone(),
        collection.clone(),
        url_checks.clone(),
        meraki_devices.clone(),
        env_community.clone(),
        cfg.poll_interval_secs,
    ));

    // Scheduler: inventory → working-set syncs (pools with a live poller) or jittered per-job
    // dispatch (pools without one), decided per pool each sweep via the coordinator's `live_pools`.
    {
        let repo = repo.clone();
        let dispatcher = dispatcher.clone();
        let stats = scheduler_stats.clone();
        let meraki_devices = meraki_devices.clone();
        let coordinator = coordinator.clone();
        tokio::spawn(run_scheduler(
            repo,
            dispatcher,
            stats,
            meraki_devices,
            coordinator,
        ));
    }

    // Meraki scheduler: one org-scoped collect per due tier, single-flighted per org so the shared
    // API rate budget is never exceeded. Separate loop so the per-node scheduler is untouched.
    // Collects route to the configured Meraki pool (env `YAGRA_MERAKI_POOL`, default `default`).
    let meraki_pool = std::env::var("YAGRA_MERAKI_POOL")
        .ok()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_POOL.to_owned());
    {
        let orgs = meraki_orgs.clone();
        let devices = meraki_devices.clone();
        let creds = creds.clone();
        let bus = bus.clone();
        let inflight = meraki_inflight.clone();
        let sys = repo.clone();
        tokio::spawn(run_meraki_scheduler(
            orgs,
            devices,
            creds,
            bus,
            inflight,
            sys,
            meraki_pool,
        ));
    }

    // Thresholds + maintenance windows: snapshot into the alert engine now, then refresh
    // periodically so edits (and window start/end boundaries) take effect without a restart.
    let thresholds = Arc::new(ThresholdStore::new(repo.pool()));
    let maintenance = Arc::new(MaintenanceRepo::new(repo.pool()));
    // Shared group repo: maintenance/mute folder-group scopes and the analysis runner all expand a
    // group to its subtree, and AdminState serves group CRUD — one hierarchy, read in several places.
    let group_repo = Arc::new(groups::GroupRepo::new(repo.pool()));
    alerts.set_config(load_alert_config(&repo, &thresholds, &maintenance, &group_repo).await);
    {
        let alerts = alerts.clone();
        let repo = repo.clone();
        let thresholds = thresholds.clone();
        let maintenance = maintenance.clone();
        let group_repo = group_repo.clone();
        let classifier = classifier.clone();
        let classification = classification.clone();
        let event_engine = event_engine.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(30)).await;
                alerts.set_config(
                    load_alert_config(&repo, &thresholds, &maintenance, &group_repo).await,
                );
                // Pick up classification-rule edits without a restart (also reloaded inline by
                // the rule-edit handlers; this catches any drift / multi-instance future).
                if let Err(e) = classifier.reload(&classification).await {
                    tracing::warn!(error = %e, "failed to refresh classification rules");
                }
                // Event rules + node address map (also reloaded inline after rule edits).
                event_engine.reload(&repo).await;
            }
        });
    }

    // Fleet health timeline: snapshot the node-state counts every few minutes into PostgreSQL so
    // the dashboard can chart "degrading vs recovering" over time, and prune old snapshots +
    // alert history past the retention window (the only place these tables are trimmed).
    {
        let repo = repo.clone();
        let alerts = alerts.clone();
        let history = history.clone();
        let events_repo = events_repo.clone();
        tokio::spawn(async move {
            const SNAPSHOT_SECS: u64 = 300;
            const RETENTION_SECS: i64 = 90 * 86_400;
            loop {
                tokio::time::sleep(Duration::from_secs(SNAPSHOT_SECS)).await;
                let states = alerts.node_states();
                let mut counts: HashMap<String, i64> = HashMap::new();
                for s in states.values() {
                    *counts.entry(s.as_str().to_owned()).or_insert(0) += 1;
                }
                let snapshot: Vec<(String, i64)> = counts.into_iter().collect();
                if let Err(e) = repo.insert_state_snapshot(&snapshot).await {
                    tracing::warn!(error = %e, "node-state snapshot failed");
                }
                if let Err(e) = repo.prune_state_snapshots(RETENTION_SECS).await {
                    tracing::warn!(error = %e, "prune state snapshots failed");
                }
                if let Err(e) = history.prune_old(RETENTION_SECS).await {
                    tracing::warn!(error = %e, "prune alert history failed");
                }
                // Passive events: matched rows follow alert-history retention, unmatched
                // (rule-authoring material) are pruned at 24h — see events.rs constants.
                if let Err(e) = events_repo.prune_old().await {
                    tracing::warn!(error = %e, "prune events failed");
                }
            }
        });
    }

    // Notification routing + mutes: load the DB channels/rules into the notifier now, then
    // refresh periodically so edits take effect without a restart (env channels stay
    // always-on; expired mutes drop out on refresh).
    load_routing(&notifier, &notifications).await;
    load_mutes(&notifier, &maintenance, &repo, &group_repo).await;
    {
        let notifier = notifier.clone();
        let notifications = notifications.clone();
        let maintenance = maintenance.clone();
        let repo = repo.clone();
        let group_repo = group_repo.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(30)).await;
                load_routing(&notifier, &notifications).await;
                load_mutes(&notifier, &maintenance, &repo, &group_repo).await;
            }
        });
    }

    // Write side (inventory + encrypted credentials + users + thresholds), sharing the pool.
    let users = Arc::new(UserStore::new(repo.pool()));
    // Bootstrap admin: use YAGRA_ADMIN_PASSWORD when supplied, otherwise generate a random
    // one-time password — never a well-known default like "admin" (security.md).
    let provided_admin_password = std::env::var("YAGRA_ADMIN_PASSWORD")
        .ok()
        .filter(|p| !p.trim().is_empty());
    let admin_password = provided_admin_password
        .clone()
        .unwrap_or_else(auth::generate_bootstrap_password);
    if users.ensure_default_admin(&admin_password).await? {
        if provided_admin_password.is_some() {
            tracing::warn!(
                "SECURITY: seeded the initial 'admin' account from YAGRA_ADMIN_PASSWORD — \
                 change it after first login"
            );
        } else {
            // One-time disclosure of the generated bootstrap password so the operator can log in;
            // it is not stored in plaintext and will not be shown again.
            tracing::warn!(
                admin_bootstrap_password = %admin_password,
                "SECURITY: no YAGRA_ADMIN_PASSWORD set — generated a one-time bootstrap password \
                 for the 'admin' account (shown once above). Log in and change it immediately."
            );
        }
    }
    // Troubleshoot analysis runner (ADR-022): TSDB-read background diagnostics. Fail any job left
    // `running` by a previous core process (it can't resume), then build the runner over the same
    // TSDB seam and node inventory the rest of core uses.
    let analysis_repo = Arc::new(analysis::AnalysisRepo::new(repo.pool()));
    match analysis_repo.fail_orphans().await {
        Ok(n) if n > 0 => {
            tracing::warn!(
                orphans = n,
                "failed analysis jobs left running by a previous process"
            );
        }
        Err(e) => tracing::warn!(error = %e, "failed to reconcile orphaned analysis jobs"),
        _ => {}
    }
    // `group_repo` is created earlier (shared with maintenance/mute scope resolution).
    let analysis = Arc::new(analysis::AnalysisRunner::new(
        analysis_repo,
        store.clone(),
        repo.clone(),
        group_repo.clone(),
    ));

    // Reports (Dashboard → Reports): a TSDB+PostgreSQL-read background generator in core (mirrors the
    // analysis runner). Fail any run left `running` by a previous process, then build the runner over
    // the same store/inventory/alert/history seams. A 60s loop fires due schedules.
    let reports_repo = Arc::new(reports::ReportsRepo::new(repo.pool()));
    match reports_repo.fail_orphans().await {
        Ok(n) if n > 0 => {
            tracing::warn!(
                orphans = n,
                "failed report runs left running by a previous process"
            );
        }
        Err(e) => tracing::warn!(error = %e, "failed to reconcile orphaned report runs"),
        _ => {}
    }
    let reports = Arc::new(reports::ReportRunner::new(
        reports_repo,
        store.clone(),
        repo.clone(),
        alerts.clone(),
        history.clone(),
    ));
    {
        let reports = reports.clone();
        tokio::spawn(run_report_scheduler(reports));
    }

    let admin = Some(Arc::new(AdminState {
        repo: repo.clone(),
        creds,
        users,
        thresholds,
        collection,
        notifications,
        mib,
        discovery,
        maintenance,
        classification,
        classifier,
        groups: group_repo,
        audit: Arc::new(AuditRepo::new(repo.pool())),
        dashboards: Arc::new(DashboardRepo::new(repo.pool())),
        shared_dashboard: Arc::new(SharedDashboardRepo::new(repo.pool())),
        scheduler_stats: scheduler_stats.clone(),
        poll: dispatcher,
        analysis,
        reports,
        url_checks,
        meraki_orgs,
        meraki_devices,
        events: events_repo,
        coordinator: coordinator.clone(),
        pollers: poller_repo.clone(),
    }));
    let sessions = Arc::new(SessionStore::new());

    let nodes: Arc<dyn NodeListing> = repo;
    let state = ApiState {
        store,
        host_sample: core_host,
        nodes,
        alerts,
        admin,
        sessions,
        history: Some(history),
        ack: Some(acks),
        events: Some(event_engine),
        public_dashboard: cfg.public_dashboard,
    };
    serve(state, &cfg.api_addr, metrics).await
}

/// Skeleton mode: serve the API over an in-memory sink seeded with one demo reading.
async fn run_skeleton(metrics: PrometheusHandle) -> anyhow::Result<()> {
    tracing::warn!("store/bus URLs not set — running in in-memory skeleton mode (no real polling)");
    let sink = Arc::new(InMemorySink::default());
    // Demo seed so the walking-skeleton WebUI shows data before real polling is wired.
    sink.ingest(&PollResult {
        schema_version: 1,
        job_id: Uuid::nil(),
        node_id: yagra_common::NodeId::from(Uuid::nil()),
        at_unix_ms: 0,
        outcome: yagra_bus::CheckOutcome::Reachable,
        samples: vec![yagra_bus::Sample::gauge("icmp_rtt_ms", 8.0)],
        interfaces: Vec::new(),
        sys_descr: None,
        poller_id: None,
        trace_context: Default::default(),
    });
    let state = ApiState {
        store: sink,
        host_sample: Arc::new(std::sync::Mutex::new(None)),
        nodes: Arc::new(StaticNodeList::demo()),
        alerts: Arc::new(AlertManager::new()),
        admin: None,
        sessions: Arc::new(SessionStore::new()),
        history: None,
        ack: None,
        events: None,
        // Skeleton has no user store (login returns 503), so reads must stay open or the
        // dev dashboard would be unreachable. Auth gating applies in live mode.
        public_dashboard: true,
    };
    serve(state, "0.0.0.0:8080", metrics).await
}

/// How often core samples its own host resources (self-observability). Matches the WebUI refresh.
const HOST_SAMPLE_SECS: u64 = 15;

/// Sample core's own host every [`HOST_SAMPLE_SECS`]: refresh the shared latest-sample cache (read
/// by `GET /api/v1/system/hosts`) and persist the `yagra_host_*` series to the TSDB. Also records
/// PostgreSQL growth as a `mount="database"` used-only proxy — core can't `statvfs` the 0700 PG data
/// dir, so its size comes from `pg_database_size`. Runs for the process lifetime.
async fn run_host_collector(
    store: Arc<dyn MetricStore>,
    cache: api::CoreHostSample,
    pool: sqlx::PgPool,
) {
    let collector = yagra_hoststats::HostCollector::from_env();
    let mut tick = tokio::time::interval(Duration::from_secs(HOST_SAMPLE_SECS));
    loop {
        tick.tick().await;
        let mut sample = collector.sample();
        // Database growth trend: used-only proxy (capacity unknown ⇒ size_bytes = 0).
        match sqlx::query_scalar::<_, i64>("SELECT pg_database_size(current_database())")
            .fetch_one(&pool)
            .await
        {
            Ok(bytes) => sample.disks.push(yagra_common::DiskUsage {
                mount: "database".to_owned(),
                used_bytes: u64::try_from(bytes).unwrap_or(0),
                size_bytes: 0,
            }),
            Err(e) => tracing::debug!(error = %e, "pg_database_size query failed"),
        }
        let at_unix_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
            .unwrap_or(0);
        store
            .write_host_sample("core", "core", None, &sample, at_unix_ms)
            .await;
        if let Ok(mut g) = cache.lock() {
            *g = Some(sample);
        }
    }
}

/// Drain poll results off the bus into the metric store and the alert engine. Returns
/// when the stream ends.
#[allow(clippy::too_many_arguments)]
async fn consume_results<S>(
    mut results: S,
    store: Arc<dyn MetricStore>,
    alerts: Arc<AlertManager>,
    notifier: Arc<Notifier>,
    history: Arc<AlertHistoryStore>,
    repo: Arc<NodeRepo>,
    stats: Arc<scheduler::SchedulerStats>,
    meraki_inflight: Arc<meraki::MerakiInflight>,
    coordinator: Arc<Coordinator<NatsBus>>,
) where
    S: Stream<Item = PollResult> + Unpin,
{
    use tracing::Instrument as _;
    while let Some(result) = results.next().await {
        // Result-ingest span: child of the poller's poll span (via the result's carried trace
        // context), completing the poll's end-to-end distributed trace. Secret-free fields only.
        let ingest_span = tracing::info_span!(
            "poll.ingest",
            node_id = %result.node_id,
            job_id = %result.job_id,
        );
        yagra_telemetry::set_span_parent(&ingest_span, &result.trace_context);
        ingest_result(
            &result,
            &store,
            &alerts,
            &notifier,
            &history,
            &repo,
            &stats,
            &meraki_inflight,
            &coordinator,
        )
        .instrument(ingest_span)
        .await;
    }
    tracing::warn!("result stream ended");
}

/// Ingest one poll result: count it, attribute provenance, write metrics, upsert discovered
/// interfaces, classify identity from `sysDescr`, then evaluate alerts and notify. Split out of
/// [`consume_results`] so the per-result work is a single `.instrument`-able unit (the result-ingest
/// span) that reads at one indentation level. Every step is best-effort — a failure in one must not
/// drop the others.
#[allow(clippy::too_many_arguments)]
async fn ingest_result(
    result: &PollResult,
    store: &Arc<dyn MetricStore>,
    alerts: &Arc<AlertManager>,
    notifier: &Arc<Notifier>,
    history: &Arc<AlertHistoryStore>,
    repo: &Arc<NodeRepo>,
    stats: &Arc<scheduler::SchedulerStats>,
    meraki_inflight: &Arc<meraki::MerakiInflight>,
    coordinator: &Arc<Coordinator<NatsBus>>,
) {
    use crate::alerts::NotifyAction;
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
    store.write(result).await;
    // Batch-upsert all interfaces discovered on this poll (table walks) in ONE statement —
    // this is the hottest ingest path, so it must not fan out into a round-trip per
    // interface. Metadata only — names/aliases live in PostgreSQL, joined to metrics at
    // query time (ADR-011). Best-effort: a failure must not drop the metric write or alerting.
    if !result.interfaces.is_empty() {
        let rows: Vec<_> = result
            .interfaces
            .iter()
            .map(|iface| {
                (
                    i32::try_from(iface.ifindex.0).unwrap_or(i32::MAX),
                    iface.if_name.as_deref(),
                    iface.if_alias.as_deref(),
                    iface.if_speed,
                )
            })
            .collect();
        if let Err(e) = repo
            .upsert_interfaces(result.node_id.as_uuid(), &rows)
            .await
        {
            tracing::warn!(node = %result.node_id, error = %e, "failed to upsert interfaces");
        }
    }
    // Identity probe: if the poll fetched sysDescr, classify it and fill the node's blank
    // vendor/model. `fill_node_identity` uses COALESCE so a manually-set value is never
    // clobbered; we only classify when something useful was extracted. Best-effort.
    if let Some(descr) = result.sys_descr.as_deref() {
        let id = yagra_discovery::identify(descr);
        if id.vendor.is_some() || id.model.is_some() {
            match repo
                .fill_node_identity(
                    result.node_id.as_uuid(),
                    id.vendor.as_deref(),
                    id.model.as_deref(),
                )
                .await
            {
                Ok(true) => tracing::info!(
                    node = %result.node_id, vendor = ?id.vendor, model = ?id.model,
                    "classified node maker/model from sysDescr"
                ),
                Ok(false) => {}
                Err(e) => {
                    tracing::warn!(node = %result.node_id, error = %e, "failed to fill node identity");
                }
            }
        }
    }
    for action in alerts.observe(result) {
        // Persist the lifecycle transition (best-effort).
        let recorded = match &action {
            NotifyAction::Fire(alert) => history.record(alert, false).await,
            NotifyAction::Resolve(alert) => history.record(alert, true).await,
            // A roll-up (child rolled under a newly-down parent): the node is still down, so
            // this is not a lifecycle resolve — the notifier just closes its standalone
            // incident. Nothing to persist; the eventual real recovery records the resolve.
            NotifyAction::Suppress(_) => Ok(()),
        };
        if let Err(e) = recorded {
            tracing::warn!(error = %e, "failed to record alert history");
        }
        notifier.handle(action).await;
    }
}

/// Periodically turn the inventory into polling work, choosing per pool (ADR-009/020) between:
///
/// - **working-set mode** — a pool with at least one live poller (`coordinator.live_pools`): core
///   hands the coordinator the pool's *entire* desired spec set (built via
///   [`scheduler::PollDispatcher::build_node_specs`], **not** gated by `due()` — the working set
///   always holds every node, and an interval change flows as a spec change), and the coordinator
///   diffs + distributes it as snapshots/deltas. The poller schedules locally.
/// - **legacy mode** — a pool with no live poller: exactly the previous behavior (per-node
///   `due()` + anti-stampede jitter + per-job publish), but routed to the pool's own subject so an
///   old wildcard poller still absorbs it. This is the zero-poller fallback and N/N-1 safety net.
///
/// The mode is decided per pool every sweep, so a pool is served one way or the other but never
/// both (no double-polling). The effective interval per node is `profile override → global default`
/// (both re-read each round, so a UI edit applies next round). The loop wakes at the smallest
/// interval in play; legacy jitter spans that window.
async fn run_scheduler(
    repo: Arc<NodeRepo>,
    dispatcher: Arc<scheduler::PollDispatcher>,
    stats: Arc<scheduler::SchedulerStats>,
    meraki_devices: Arc<meraki::MerakiDeviceRepo>,
    coordinator: Arc<Coordinator<NatsBus>>,
) {
    use std::collections::HashSet;
    use std::time::Instant;
    let mut last_dispatched: HashMap<Uuid, Instant> = HashMap::new();
    loop {
        // Meraki device nodes are polled by the org collector, not per-node — preload their ids
        // once per round (like the interval overrides) and skip them, so no per-node lookup runs in
        // the hot loop. A load failure degrades to an empty set (they'd fall through to the
        // per-node dispatcher, which then short-circuits them anyway).
        let meraki_node_ids = meraki_devices.node_ids().await.unwrap_or_default();
        // Resolve the round's intervals: the global default (DB-backed) and any per-profile
        // overrides. On a read failure, degrade to the compiled default / no overrides rather than
        // stalling the poll loop.
        let default_secs = repo
            .get_default_poll_interval()
            .await
            .unwrap_or(crate::config::DEFAULT_POLL_INTERVAL_SECS);
        let overrides = repo.profile_interval_overrides().await.unwrap_or_default();
        let mut min_interval = default_secs;

        match repo.list_nodes().await {
            Ok(nodes) => {
                // Pair each node with its resolved interval, and find the round's smallest so the
                // jitter window matches the sleep period (a node is never double-scheduled per round).
                let resolved: Vec<_> = nodes
                    .into_iter()
                    .map(|node| {
                        let secs =
                            scheduler::resolve_interval(node.profile, &overrides, default_secs);
                        (node, secs)
                    })
                    .collect();
                for (_, secs) in &resolved {
                    min_interval = min_interval.min(*secs);
                }
                let window_ms = (u64::from(min_interval).saturating_mul(1000)).max(1);
                let node_count = resolved.len();

                let now = Instant::now();
                let live = coordinator.live_pools(now);

                // Group the non-Meraki nodes by pool (default `DEFAULT_POOL`) so each pool's mode is
                // decided once. Meraki device nodes are dropped here (the org collector owns them).
                let mut groups: HashMap<String, Vec<(yagra_common::Node, u32)>> = HashMap::new();
                for (node, secs) in resolved {
                    if meraki_node_ids.contains(&node.id.as_uuid()) {
                        continue;
                    }
                    let pool = node
                        .pool
                        .clone()
                        .unwrap_or_else(|| DEFAULT_POOL.to_string());
                    groups.entry(pool).or_default().push((node, secs));
                }

                tracing::debug!(
                    count = node_count,
                    pools = groups.len(),
                    default_secs,
                    min_interval,
                    "scheduling poll round"
                );

                // `present` tracks only legacy-dispatched nodes so the retain below can prune
                // last_dispatched without dropping their cadence; working-set nodes are removed
                // from it explicitly (so a later legacy fallback re-polls them at once).
                let mut present: HashSet<Uuid> = HashSet::new();
                let mut jobs_round: u64 = 0;
                let mut working_set_pools: u64 = 0;
                let mut legacy_pools: u64 = 0;

                for (pool, members) in groups {
                    if scheduler::pool_uses_working_set(&pool, &live) {
                        // Build the pool's whole desired working set and let the coordinator diff +
                        // distribute it (snapshots/deltas). Not gated by `due()`.
                        let mut desired = HashMap::new();
                        for (node, secs) in &members {
                            let specs = dispatcher.build_node_specs(node, *secs).await;
                            last_dispatched.remove(&node.id.as_uuid());
                            if !specs.is_empty() {
                                desired.insert(node.id, specs);
                            }
                        }
                        coordinator.reconcile_pool(&pool, desired, now).await;
                        working_set_pools += 1;
                    } else {
                        // Legacy: per-node due-check + jittered per-job publish to the pool subject.
                        for (node, secs) in &members {
                            let id = node.id.as_uuid();
                            present.insert(id);
                            let elapsed = last_dispatched.get(&id).map(|&t| now.duration_since(t));
                            if !scheduler::due(elapsed, Duration::from_secs(u64::from(*secs))) {
                                continue;
                            }
                            last_dispatched.insert(id, now);
                            for (job, kind) in dispatcher.build_node_jobs(node, *secs).await {
                                jobs_round += 1;
                                let dispatcher = dispatcher.clone();
                                let node_id = node.id;
                                let pool = pool.clone();
                                let delay =
                                    Duration::from_millis(rand::random::<u64>() % window_ms);
                                tokio::spawn(async move {
                                    tokio::time::sleep(delay).await;
                                    dispatcher.publish_job(job, kind, node_id, &pool).await;
                                });
                            }
                        }
                        legacy_pools += 1;
                    }
                }
                // Forget legacy nodes no longer present so the map can't grow unbounded (working-set
                // nodes were already removed above).
                last_dispatched.retain(|id, _| present.contains(id));
                stats.record_sweep(jobs_round);
                stats.set_pool_modes(working_set_pools, legacy_pools);
            }
            Err(e) => tracing::error!(error = %e, "scheduler: listing nodes failed"),
        }
        tokio::time::sleep(Duration::from_secs(u64::from(min_interval))).await;
    }
}

/// Dispatch Cisco Meraki org-scoped collects, one per due tier, **single-flighted per org** so an
/// org's shared Dashboard API rate budget is never exceeded (the #1 safeguard). Separate from the
/// per-node scheduler so that loop is untouched. Each short tick: honour the global kill switch,
/// then for every enabled org with no outstanding collect, pick its most-overdue due tier and
/// dispatch one collect (serial→node_id map + monitored networks inlined). Tiers have their own
/// cadences (free of the per-node 1h cap); the collect result clears the org's flight (a lease is
/// the backstop). An org with no imported devices is skipped to save budget.
async fn run_meraki_scheduler(
    orgs_repo: Arc<meraki::MerakiOrgRepo>,
    devices: Arc<meraki::MerakiDeviceRepo>,
    creds: Arc<CredentialStore>,
    bus: Arc<NatsBus>,
    inflight: Arc<meraki::MerakiInflight>,
    settings: Arc<NodeRepo>,
    meraki_pool: String,
) {
    use std::time::Instant;
    use yagra_bus::{PollJob, SyncBus};
    use yagra_common::MerakiTier;

    const TICK: Duration = Duration::from_secs(15);
    const LEASE: Duration = Duration::from_secs(300);
    let mut last: HashMap<(Uuid, MerakiTier), Instant> = HashMap::new();

    loop {
        tokio::time::sleep(TICK).await;
        // Global kill switch (safeguard): halt all Meraki polling instantly, without touching config.
        if !settings.get_meraki_polling_enabled().await {
            continue;
        }
        let orgs = match orgs_repo.list_enabled().await {
            Ok(o) => o,
            Err(e) => {
                tracing::error!(error = %e, "meraki scheduler: listing orgs failed");
                continue;
            }
        };
        let now = Instant::now();
        for org in orgs {
            if inflight.is_inflight(org.id, now) {
                continue; // a collect is still outstanding for this org
            }
            // Pick the most-overdue due tier (never-dispatched sorts first).
            let mut best: Option<(MerakiTier, Duration)> = None;
            for tier in org.active_tiers() {
                let cadence = Duration::from_secs(u64::from(org.tier_cadence(tier)));
                let (due, overdue) = match last.get(&(org.id, tier)) {
                    Some(&t) => {
                        let e = now.duration_since(t);
                        (e >= cadence, e)
                    }
                    None => (true, Duration::MAX),
                };
                if due && best.is_none_or(|(_, bo)| overdue > bo) {
                    best = Some((tier, overdue));
                }
            }
            let Some((tier, _)) = best else {
                continue; // nothing due
            };

            let device_refs = match devices.device_refs(org.id).await {
                Ok(d) if !d.is_empty() => d,
                Ok(_) => continue, // no imported devices → nothing to fan out to; save budget
                Err(e) => {
                    tracing::warn!(org = %org.org_id, error = %e, "meraki device refs load failed");
                    continue;
                }
            };
            let Some(api_key) = meraki::resolve_meraki_key(&creds, org.credential_id).await else {
                tracing::warn!(org = %org.org_id, "meraki key unresolved; skipping");
                continue;
            };
            let network_ids = orgs_repo
                .monitored_network_ids(org.id)
                .await
                .unwrap_or_default();

            let job_id = Uuid::new_v4();
            if !inflight.acquire(org.id, job_id, LEASE, now) {
                continue; // lost an acquire race
            }
            let check = meraki::build_collect_check(&org, tier, api_key, device_refs, network_ids);
            let interval = org.tier_cadence(tier);
            let job = PollJob::meraki_collect(job_id, check, interval);
            match bus.publish_job_for_pool(&meraki_pool, job).await {
                Ok(()) => {
                    last.insert((org.id, tier), now);
                    metrics::counter!("yagra_meraki_collects_dispatched_total").increment(1);
                    tracing::debug!(org = %org.org_id, tier = tier.as_str(), "dispatched meraki collect");
                }
                Err(e) => {
                    // Release the flight so the next tick retries rather than waiting out the lease.
                    inflight.complete(job_id);
                    tracing::error!(org = %org.org_id, error = %e, "meraki collect publish failed");
                }
            }
        }
    }
}

/// Fire due report schedules on a fixed tick, and prune old report runs hourly.
///
/// Each round: list schedules whose `next_run_at` has passed, enqueue a run for each (trigger =
/// scheduled), and advance `next_run_at` from the preset cadence. Generation is in-process in core
/// (no device I/O), so this loop only enqueues — the runner's background task does the work. Failures
/// degrade to a warn so one bad schedule never stalls the others.
async fn run_report_scheduler(reports: Arc<reports::ReportRunner>) {
    use chrono::Utc;
    const TICK_SECS: u64 = 60;
    const RETENTION_SECS: i64 = 90 * 86_400;
    // Prune ~hourly (every 60 ticks) rather than every minute.
    let mut tick: u64 = 0;
    let repo = reports.repo();
    loop {
        tokio::time::sleep(Duration::from_secs(TICK_SECS)).await;
        tick = tick.wrapping_add(1);
        match repo.due_schedules().await {
            Ok(due) => {
                for sched in due {
                    let next = reports::compute_next_run(
                        &sched.frequency,
                        sched.day_of_week,
                        sched.day_of_month,
                        sched.at_hour,
                        sched.at_minute,
                        Utc::now(),
                    );
                    let status = match reports
                        .run_now(sched.definition_id, "scheduled", None)
                        .await
                    {
                        Ok(Some(_)) => "queued",
                        Ok(None) => "missing-definition",
                        Err(e) => {
                            tracing::warn!(error = %e, schedule = %sched.id, "scheduled report failed to start");
                            "error"
                        }
                    };
                    if let Err(e) = repo.mark_fired(sched.id, status, next).await {
                        tracing::warn!(error = %e, schedule = %sched.id, "failed to advance schedule");
                    }
                }
            }
            Err(e) => tracing::warn!(error = %e, "report scheduler: due-query failed"),
        }
        if tick.is_multiple_of(60) {
            if let Err(e) = repo.prune_runs(RETENTION_SECS).await {
                tracing::warn!(error = %e, "prune report runs failed");
            }
        }
    }
}

/// Bind and serve the northbound API plus the Prometheus `/metrics` endpoint.
async fn serve(state: ApiState, addr: &str, metrics: PrometheusHandle) -> anyhow::Result<()> {
    let app = api::router(state)
        .route(
            "/metrics",
            get(move || {
                let handle = metrics.clone();
                async move { handle.render() }
            }),
        )
        // HTTP request tracing (self-observability): one span per request, exported over OTLP when
        // configured. INFO so it survives the default `info` filter; the northbound API is the
        // entry point of a request-driven trace (e.g. a "poll now" that fans out to the bus).
        .layer(
            tower_http::trace::TraceLayer::new_for_http().make_span_with(
                tower_http::trace::DefaultMakeSpan::new().level(tracing::Level::INFO),
            ),
        );
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(%addr, "Yagra-core API listening on /api/v1 (+ /metrics)");
    axum::serve(listener, app).await?;
    Ok(())
}

/// Build the alert engine's config snapshot: all thresholds, per-node metadata (profile +
/// group tag-values) for scope resolution, the dependency topology (parent edges) for
/// suppression / root-cause roll-up, and the nodes currently inside an active maintenance
/// window. Failures degrade to empty rather than crashing the refresh loop.
async fn load_alert_config(
    repo: &NodeRepo,
    thresholds: &ThresholdStore,
    maintenance: &MaintenanceRepo,
    groups: &groups::GroupRepo,
) -> AlertConfig {
    let rules = thresholds.list_all().await.unwrap_or_else(|e| {
        tracing::warn!(error = %e, "failed to load thresholds");
        Vec::new()
    });
    let scopes = maintenance.active_scopes().await.unwrap_or_else(|e| {
        tracing::warn!(error = %e, "failed to load maintenance windows");
        Vec::new()
    });
    let nodes = repo.list_nodes().await.unwrap_or_default();
    let mut in_maintenance = maintenance::nodes_in_maintenance(&scopes, &nodes);
    // Folder-group scopes resolve against the inventory tree (recursive incl. subgroups, ADR-022)
    // — the same chain the Troubleshoot scope uses. Only touch the DB when one is actually active.
    let folder_groups: Vec<Uuid> = scopes
        .iter()
        .filter(|(level, _)| *level == maintenance::WindowScope::FolderGroup)
        .filter_map(|(_, id)| Uuid::parse_str(id).ok())
        .collect();
    if !folder_groups.is_empty() {
        match groups.edges().await {
            Ok(edges) => {
                let mut group_ids: Vec<Uuid> = Vec::new();
                for root in folder_groups {
                    group_ids.extend(groups::group_subtree(&edges, root));
                }
                match repo.nodes_in_groups(&group_ids).await {
                    Ok(node_ids) => {
                        in_maintenance.extend(node_ids.into_iter().map(yagra_common::NodeId::from))
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "failed to resolve folder-group maintenance");
                    }
                }
            }
            Err(e) => tracing::warn!(error = %e, "failed to load group edges for maintenance"),
        }
    }
    let mut meta = HashMap::new();
    let mut topology = Topology::new();
    for node in nodes {
        // Dependency edge child → parent feeds parent-down suppression (ADR-015).
        if let Some(parent) = node.parent {
            topology.add_dependency(node.id, parent);
        }
        meta.insert(
            node.id,
            NodeMeta {
                profile: node.profile.map(|p| p.to_string()),
                groups: node.tags.into_values().collect(),
            },
        );
    }
    AlertConfig::new(rules, meta)
        .with_topology(topology)
        .with_maintenance(in_maintenance)
}

/// Load the unexpired mutes into the notifier (check ids recomputed from names here). A
/// group-scoped mute is expanded to one per-node entry across the folder-group subtree (recursive,
/// ADR-022), so the notifier's per-node matching is unchanged; the expansion re-runs each refresh
/// so membership changes are honored. Failures degrade to the existing snapshot (warn).
async fn load_mutes(
    notifier: &Notifier,
    maintenance: &MaintenanceRepo,
    repo: &NodeRepo,
    groups: &groups::GroupRepo,
) {
    let mutes = match maintenance.list_mutes().await {
        Ok(mutes) => mutes,
        Err(e) => {
            tracing::warn!(error = %e, "failed to load mutes");
            return;
        }
    };
    let mut active: Vec<ActiveMute> = Vec::new();
    let mut group_roots: Vec<Uuid> = Vec::new();
    for m in &mutes {
        match m.scope_kind {
            maintenance::MuteScope::Node => {
                if let Some(node_id) = m.node_id {
                    active.push(ActiveMute::new(node_id, m.check_name.as_deref()));
                }
            }
            maintenance::MuteScope::Group => {
                if let Some(group_id) = m.group_id {
                    group_roots.push(group_id);
                }
            }
        }
    }
    if !group_roots.is_empty() {
        match groups.edges().await {
            Ok(edges) => {
                let mut group_ids: Vec<Uuid> = Vec::new();
                for root in group_roots {
                    group_ids.extend(groups::group_subtree(&edges, root));
                }
                match repo.nodes_in_groups(&group_ids).await {
                    // A group mute silences the whole node (check=None).
                    Ok(node_ids) => {
                        active.extend(node_ids.into_iter().map(|n| ActiveMute::new(n, None)));
                    }
                    Err(e) => tracing::warn!(error = %e, "failed to resolve group mute nodes"),
                }
            }
            Err(e) => tracing::warn!(error = %e, "failed to load group edges for mutes"),
        }
    }
    notifier.set_mutes(active).await;
}

/// Load the DB notification channels + routing rules into the notifier. Failures degrade to
/// the existing snapshot (warn) rather than dropping routing.
async fn load_routing(notifier: &Notifier, notifications: &NotificationRepo) {
    let channels = notifications
        .list_open_channels()
        .await
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, "failed to load notification channels");
            Vec::new()
        });
    let rules = notifications.list_rules().await.unwrap_or_else(|e| {
        tracing::warn!(error = %e, "failed to load routing rules");
        Vec::new()
    });
    notifier.set_routing(channels, rules).await;
}

/// Connect to NATS with retry so startup ordering doesn't matter.
async fn connect_bus(url: &str) -> anyhow::Result<NatsBus> {
    const MAX_ATTEMPTS: u32 = 30;
    let mut attempt = 0;
    loop {
        match NatsBus::connect(url).await {
            Ok(bus) => return Ok(bus),
            Err(e) if attempt < MAX_ATTEMPTS => {
                attempt += 1;
                tracing::warn!(error = %e, attempt, "NATS not ready; retrying in 2s");
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
            Err(e) => anyhow::bail!("NATS connect failed after {MAX_ATTEMPTS} attempts: {e}"),
        }
    }
}
