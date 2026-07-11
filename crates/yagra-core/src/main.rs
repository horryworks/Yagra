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
mod config_gen;
mod dashboard;
mod discovery;
mod events;
mod groups;
mod history;
mod logstore;
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

use yagra_telemetry::{shutdown_signal, spawn_cancellable, CancellationToken};

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
use logstore::{LogStore, VlStore};
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
use tokio::sync::mpsc::error::TrySendError;
use uuid::Uuid;
use volatile::VolatileStore;
use yagra_alert::Alert;
use yagra_bus::{JobSpec, NatsBus, PollResult, DEFAULT_POOL};
use yagra_common::NodeId;
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

    // Event log store (ADR-024, 4th data class). Optional: when a URL is set, passive events are
    // searched from and written to VictoriaLogs and PostgreSQL keeps only alert-linked rows; when
    // unset, events stay entirely in PostgreSQL (backward-compatible).
    let logs: Option<Arc<dyn LogStore>> = cfg
        .logs_url
        .as_deref()
        .map(|u| Arc::new(VlStore::new(u)) as Arc<dyn LogStore>);
    if logs.is_some() {
        tracing::info!("VictoriaLogs event log store enabled");
    }

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
    // Graceful shutdown: one token fans SIGTERM/Ctrl-C out to every background loop below and the
    // HTTP server, so a rolling upgrade drains in-flight work instead of being killed mid-write
    // (ADR-017). `serve` (end of `run`) installs the signal handler that cancels this token.
    let shutdown = CancellationToken::new();

    // Feed the registry: heartbeats (liveness/telemetry) and snapshot requests. Thin consumer loops
    // that end when the bus stream closes (shutdown), mirroring the other bus consumers.
    {
        let coordinator = coordinator.clone();
        let stream = Box::pin(bus.subscribe_heartbeats().await?);
        spawn_cancellable(&shutdown, coordinator.run_heartbeat_consumer(stream));
    }
    {
        let coordinator = coordinator.clone();
        let stream = Box::pin(bus.subscribe_sync_requests().await?);
        spawn_cancellable(&shutdown, coordinator.run_sync_request_consumer(stream));
    }

    // Core self-observability (monitoring-conventions): sample core's own host every
    // HOST_SAMPLE_SECS, cache the latest for the System Health page, and persist the `yagra_host_*`
    // series to the TSDB (core is the single writer for its own host + every poller's).
    let core_host: api::CoreHostSample = Arc::new(std::sync::Mutex::new(None));
    {
        let store = store.clone();
        let cache = core_host.clone();
        let pool = repo.pool().clone();
        spawn_cancellable(&shutdown, run_host_collector(store, cache, pool));
    }

    // Notification delivery runs on its own task, fed by the result consumer over a bounded queue,
    // so a slow/wedged vendor endpoint (retries, 10s timeouts, honored `Retry-After`) can never stall
    // the poll-result ingest pipeline. Single consumer ⇒ deliveries stay ordered; a bounded channel
    // ⇒ sustained overload applies backpressure rather than growing memory unbounded.
    let (notify_tx, mut notify_rx) =
        tokio::sync::mpsc::channel::<crate::alerts::NotifyAction>(1024);
    {
        let notifier = notifier.clone();
        spawn_cancellable(&shutdown, async move {
            while let Some(action) = notify_rx.recv().await {
                notifier.handle(action).await;
            }
        });
    }

    // Poll-result ingestion (ADR-025): a single in-memory matcher hands persistence to async batch
    // writers over bounded channels. Metrics (VictoriaMetrics) and interface metadata are best-effort
    // — they shed on sustained overload (a shed metric never loses an alert; metadata self-heals,
    // re-emitted every poll). Alert history is preserved (the matcher falls back to an inline write
    // when its channel is full). The writers take the shutdown token directly (not `spawn_cancellable`)
    // so they do a best-effort final flush on cancel rather than being dropped mid-batch.
    let (metrics_tx, metrics_rx) =
        tokio::sync::mpsc::channel::<Arc<PollResult>>(RESULT_PERSIST_CHANNEL_CAP);
    let (meta_tx, meta_rx) = tokio::sync::mpsc::channel::<MetaRecord>(RESULT_PERSIST_CHANNEL_CAP);
    let (history_tx, history_rx) =
        tokio::sync::mpsc::channel::<HistoryRecord>(RESULT_PERSIST_CHANNEL_CAP);
    tokio::spawn(run_vm_writer(metrics_rx, store.clone(), shutdown.clone()));
    tokio::spawn(run_pg_writer(
        meta_rx,
        history_rx,
        repo.clone(),
        history.clone(),
        shutdown.clone(),
    ));
    {
        let alerts = alerts.clone();
        let history = history.clone();
        let stats = scheduler_stats.clone();
        let meraki_inflight = meraki_inflight.clone();
        let coordinator = coordinator.clone();
        let results = Box::pin(bus.subscribe_results().await?);
        spawn_cancellable(
            &shutdown,
            consume_results(
                results,
                alerts,
                notify_tx,
                metrics_tx,
                meta_tx,
                history_tx,
                history,
                stats,
                meraki_inflight,
                coordinator,
            ),
        );
    }

    // Discovery: a runner that publishes sweep jobs + a consumer that folds results back in,
    // classifying each found device into a suggested profile via the shared classifier.
    let discovery = Arc::new(DiscoveryRunner::new(bus.clone(), classifier.clone()));
    {
        let results = Box::pin(bus.subscribe_discovery_results().await?);
        spawn_cancellable(&shutdown, discovery.clone().run_consumer(results));
    }

    // Passive events (Phase 2): syslog/traps arrive from pollers on `yagra.events`,
    // webhooks via the ingest endpoint. The engine matches rules and raises alerts
    // through the same alert/notify/history pipeline as poll results.
    // Persistence is split off the matcher's hot path (ADR-024): the engine matches (in-memory,
    // synchronous) and hands each event to the batch writer over a bounded channel, which fans it
    // out to PostgreSQL and/or the log store. The writer takes the shutdown token directly (not
    // `spawn_cancellable`) so it can do a best-effort final flush on cancel rather than being
    // dropped mid-batch.
    let events_repo = Arc::new(events::EventRepo::new(repo.pool()));
    let (persist_tx, persist_rx) =
        tokio::sync::mpsc::channel::<events::PersistRecord>(events::PERSIST_CHANNEL_CAP);
    // Alert side effects (history + notification) for matched events also run off the matcher's hot
    // path (S10) so an event storm doesn't serialize DB round-trips / vendor delivery on the single
    // matcher. Unlike the persist queue this never sheds (audit trail + FIFO fire→resolve order).
    let (event_action_tx, event_action_rx) =
        tokio::sync::mpsc::channel::<events::EventAction>(events::ACTION_CHANNEL_CAP);
    let event_engine = Arc::new(events::EventEngine::new(
        events_repo.clone(),
        alerts.clone(),
        notifier.clone(),
        history.clone(),
        Some(persist_tx),
        Some(event_action_tx),
    ));
    event_engine.reload(&repo).await;
    {
        let stream = Box::pin(bus.subscribe_events().await?);
        spawn_cancellable(
            &shutdown,
            events::consume_events(stream, event_engine.clone()),
        );
    }
    spawn_cancellable(&shutdown, events::run_ttl_sweeper(event_engine.clone()));
    tokio::spawn(events::run_persist_writer(
        persist_rx,
        events_repo.clone(),
        logs.clone(),
        shutdown.clone(),
    ));
    tokio::spawn(events::run_event_action_writer(
        event_action_rx,
        history.clone(),
        notifier.clone(),
        shutdown.clone(),
    ));

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
        spawn_cancellable(
            &shutdown,
            run_scheduler(repo, dispatcher, stats, meraki_devices, coordinator),
        );
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
        spawn_cancellable(
            &shutdown,
            run_meraki_scheduler(orgs, devices, creds, bus, inflight, sys, meraki_pool),
        );
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
        spawn_cancellable(&shutdown, async move {
            // Cache the config-derived alert base keyed by the config generation, so the full node
            // scan + meta/topology rebuild runs only after an actual config change (S6). Maintenance
            // windows are time-dependent, so re-resolve them each cycle over the cached node list,
            // and only swap the live config when the base or the in-maintenance set actually changed.
            let mut cached_base: Option<(u64, AlertConfigBase)> = None;
            let mut last_maintenance: Option<std::collections::BTreeSet<NodeId>> = None;
            loop {
                tokio::time::sleep(Duration::from_secs(30)).await;
                let generation = config_gen::current();
                let base_changed = cached_base.as_ref().map(|(g, _)| *g) != Some(generation);
                if base_changed {
                    cached_base =
                        Some((generation, load_alert_config_base(&repo, &thresholds).await));
                }
                let base = &cached_base.as_ref().expect("alert base set above").1;
                let in_maintenance =
                    resolve_maintenance(&maintenance, &group_repo, &repo, &base.nodes).await;
                if base_changed || last_maintenance.as_ref() != Some(&in_maintenance) {
                    let config = AlertConfig::new(base.rules.clone(), base.meta.clone())
                        .with_topology(base.topology.clone())
                        .with_maintenance(in_maintenance.clone());
                    alerts.set_config(config);
                    last_maintenance = Some(in_maintenance);
                }
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
        spawn_cancellable(&shutdown, async move {
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
                // Passive events in PostgreSQL: matched rows follow alert-history retention,
                // unmatched (rule-authoring material) are pruned at 24h — see events.rs constants.
                // When the log store is enabled (ADR-024) unmatched rows never land in PostgreSQL,
                // so this pruning naturally trims PostgreSQL to the alert-linked subset; the log
                // store keeps the full firehose under its own retention (`-retentionPeriod`).
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
        spawn_cancellable(&shutdown, async move {
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
        spawn_cancellable(&shutdown, run_report_scheduler(reports));
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
        logs,
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
    serve(state, &cfg.api_addr, metrics, shutdown).await
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
        logs: None,
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
    serve(state, "0.0.0.0:8080", metrics, CancellationToken::new()).await
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

/// Bounded queue between the single result matcher and each async batch persist writer (ADR-025,
/// mirroring the event pipeline's ADR-024 split). Like events, sustained overload sheds the newest
/// record rather than blocking the matcher or growing memory unbounded.
const RESULT_PERSIST_CHANNEL_CAP: usize = 8192;
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

/// One interface row for the batched metadata upsert (matcher extracts it from the result so the
/// writer re-derives nothing): `(ifindex, if_name, if_alias, if_speed)`.
type OwnedIface = (i32, Option<String>, Option<String>, Option<i64>);

/// One result's metadata for the async PG writer: discovered interfaces to upsert plus an optional
/// `(vendor, model)` identity classified from `sysDescr`. Shed-able and self-healing — re-emitted on
/// every poll, so a dropped record is re-upserted next cycle.
struct MetaRecord {
    node_id: Uuid,
    interfaces: Vec<OwnedIface>,
    identity: Option<(Option<String>, Option<String>)>,
}

/// One alert-lifecycle transition for the async PG writer's history batch. Never shed — the matcher
/// falls back to an inline write when this channel is full (the audit trail must not be lost).
struct HistoryRecord {
    alert: Alert,
    resolved: bool,
}

/// Drain poll results off the bus, match them in-memory (single logical consumer), and hand all
/// persistence to the async batch writers over bounded channels (ADR-025). Returns when the stream
/// ends. The matcher does no blocking I/O — `alerts.observe` is synchronous and in-memory, and every
/// persist step is a non-blocking `try_send` (history has an inline fallback).
#[allow(clippy::too_many_arguments)]
async fn consume_results<S>(
    mut results: S,
    alerts: Arc<AlertManager>,
    notify_tx: tokio::sync::mpsc::Sender<crate::alerts::NotifyAction>,
    metrics_tx: tokio::sync::mpsc::Sender<Arc<PollResult>>,
    meta_tx: tokio::sync::mpsc::Sender<MetaRecord>,
    history_tx: tokio::sync::mpsc::Sender<HistoryRecord>,
    history: Arc<AlertHistoryStore>,
    stats: Arc<scheduler::SchedulerStats>,
    meraki_inflight: Arc<meraki::MerakiInflight>,
    coordinator: Arc<Coordinator<NatsBus>>,
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
            &metrics_tx,
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
    metrics_tx: &tokio::sync::mpsc::Sender<Arc<PollResult>>,
    meta_tx: &tokio::sync::mpsc::Sender<MetaRecord>,
    history_tx: &tokio::sync::mpsc::Sender<HistoryRecord>,
    history: &Arc<AlertHistoryStore>,
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

    // End-to-end ingest lag (poll timestamp → matcher entry) — the primary scale health signal.
    if result.at_unix_ms > 0 {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
        metrics::gauge!("yagra_result_ingest_lag_ms")
            .set((now_ms - result.at_unix_ms).max(0) as f64);
    }

    // Metrics → VM writer. Shed-able: alerts are computed in-memory below and never read VM back,
    // so a dropped sample never loses an alert (best-effort observational tier, ADR-025).
    if !result.samples.is_empty() {
        match metrics_tx.try_send(Arc::clone(&result)) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                metrics::counter!("yagra_result_metrics_persist_dropped_total", "reason" => "channel_full")
                    .increment(1);
            }
            Err(TrySendError::Closed(_)) => {}
        }
    }

    // Interface metadata + `sysDescr` identity → PG meta writer. Shed-able and self-healing (both are
    // re-emitted every poll). `identify()` is cheap in-memory work, kept on the matcher; only the PG
    // UPDATE is offloaded. Metadata only — names/aliases live in PostgreSQL, joined at query time (ADR-011).
    let interfaces: Vec<OwnedIface> = result
        .interfaces
        .iter()
        .map(|iface| {
            (
                i32::try_from(iface.ifindex.0).unwrap_or(i32::MAX),
                iface.if_name.clone(),
                iface.if_alias.clone(),
                iface.if_speed,
            )
        })
        .collect();
    let identity = result.sys_descr.as_deref().and_then(|descr| {
        let id = yagra_discovery::identify(descr);
        (id.vendor.is_some() || id.model.is_some()).then_some((id.vendor, id.model))
    });
    if !interfaces.is_empty() || identity.is_some() {
        let rec = MetaRecord {
            node_id: result.node_id.as_uuid(),
            interfaces,
            identity,
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

    // Alerts: evaluate synchronously in-memory (never shed — the loss-free matcher core), record each
    // lifecycle transition (batched via `history_tx`, inline fallback), and hand delivery to the
    // notification task (bounded queue) so a slow vendor endpoint can't stall ingest.
    for action in alerts.observe(&result) {
        match &action {
            NotifyAction::Fire(alert) => enqueue_history(history_tx, history, alert, false).await,
            NotifyAction::Resolve(alert) => enqueue_history(history_tx, history, alert, true).await,
            // A roll-up (child rolled under a newly-down parent): the node is still down, so this is
            // not a lifecycle resolve — nothing to persist; the eventual real recovery records it.
            NotifyAction::Suppress(_) => {}
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
async fn run_vm_writer(
    mut rx: tokio::sync::mpsc::Receiver<Arc<PollResult>>,
    store: Arc<dyn MetricStore>,
    shutdown: CancellationToken,
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
                        flush_vm(&store, &mut buf, &mut spill).await;
                    }
                }
                flush_vm(&store, &mut buf, &mut spill).await;
                break;
            }
            first = rx.recv() => {
                match first {
                    None => { flush_vm(&store, &mut buf, &mut spill).await; break; }
                    Some(r) => {
                        buf.push(r);
                        while buf.len() < VM_BATCH_MAX_RESULTS {
                            match rx.try_recv() {
                                Ok(r) => buf.push(r),
                                Err(_) => break,
                            }
                        }
                        flush_vm(&store, &mut buf, &mut spill).await;
                        metrics::gauge!("yagra_persist_queue_depth", "stream" => "metrics")
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
            if spill.len() >= VM_SPILL_MAX_BATCHES {
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
async fn run_pg_writer(
    mut meta_rx: tokio::sync::mpsc::Receiver<MetaRecord>,
    mut history_rx: tokio::sync::mpsc::Receiver<HistoryRecord>,
    repo: Arc<NodeRepo>,
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
                        flush_meta(&repo, &mut meta_buf).await;
                    }
                }
                while let Ok(h) = history_rx.try_recv() {
                    hist_buf.push(h);
                    if hist_buf.len() >= RESULT_PERSIST_BATCH_MAX {
                        flush_history(&history, &mut hist_buf).await;
                    }
                }
                flush_meta(&repo, &mut meta_buf).await;
                flush_history(&history, &mut hist_buf).await;
                break;
            }
            m = meta_rx.recv() => {
                let Some(m) = m else {
                    flush_meta(&repo, &mut meta_buf).await;
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
                flush_meta(&repo, &mut meta_buf).await;
                metrics::gauge!("yagra_persist_queue_depth", "stream" => "meta")
                    .set(meta_rx.len() as f64);
            }
            h = history_rx.recv() => {
                let Some(h) = h else {
                    flush_meta(&repo, &mut meta_buf).await;
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
async fn flush_meta(repo: &Arc<NodeRepo>, buf: &mut Vec<MetaRecord>) {
    if buf.is_empty() {
        return;
    }
    let count = buf.len() as u64;
    let mut iface_rows: Vec<repo::InterfaceBatchRow> = Vec::new();
    let mut ident_rows: Vec<(Uuid, Option<String>, Option<String>)> = Vec::new();
    for rec in buf.drain(..) {
        for (ifindex, name, alias, speed) in rec.interfaces {
            iface_rows.push((rec.node_id, ifindex, name, alias, speed));
        }
        if let Some((vendor, model)) = rec.identity {
            ident_rows.push((rec.node_id, vendor, model));
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
/// Cached result of a full sweep's spec resolution, reused while config is unchanged (S2). Holds the
/// per-pool desired working sets so a steady-state round costs no `list_nodes` scan, no per-node spec
/// build, and no credential decrypt — the coordinator is fed the cached sets and handles poller
/// membership + its own diff. Populated **only** when the whole fleet was working-set at build time
/// (a legacy pool needs node rows each round, so a mixed fleet keeps rebuilding, as before).
struct SweepCache {
    /// Config generation this was built at; a mismatch forces a rebuild.
    generation: u64,
    /// Sleep period for the round (fleet-minimum interval), cached so the fast path needn't rescan.
    min_interval: u32,
    /// Per-pool desired working set (`build_node_specs` output).
    desired_by_pool: HashMap<String, HashMap<NodeId, Vec<JobSpec>>>,
}

impl SweepCache {
    /// Whether this cache can serve the current round: config unchanged since it was built AND every
    /// cached pool still has a live poller (working-set). A pool that fell back to legacy needs node
    /// rows this round, so the cache can't serve it — force a rebuild. (Config-derived pool membership
    /// is stable while the generation is unchanged, so the cached pool set equals the current one.)
    fn reusable(&self, generation: u64, live: &std::collections::HashSet<String>) -> bool {
        self.generation == generation
            && self
                .desired_by_pool
                .keys()
                .all(|p| scheduler::pool_uses_working_set(p, live))
    }
}

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
    let mut cache: Option<SweepCache> = None;
    loop {
        // Read the config generation before any work so a change racing the rebuild is caught next
        // round (the cache is tagged with the pre-work value).
        let generation = config_gen::current();
        let now = Instant::now();
        let live = coordinator.live_pools(now);

        // Fast path: config unchanged since the cache was built AND every cached pool still has a
        // live poller (working-set). Reuse the cached desired sets — no DB scan, no per-node spec
        // build, no credential decrypt — and let the coordinator handle poller membership + its diff.
        if let Some(c) = &cache {
            if c.reusable(generation, &live) {
                for (pool, desired) in &c.desired_by_pool {
                    coordinator.reconcile_pool(pool, desired.clone(), now).await;
                }
                stats.record_sweep(0);
                stats.set_pool_modes(c.desired_by_pool.len() as u64, 0);
                let sleep_secs = c.min_interval;
                metrics::counter!("yagra_sweep_cache_hits_total").increment(1);
                tokio::time::sleep(Duration::from_secs(u64::from(sleep_secs))).await;
                continue;
            }
        }
        metrics::counter!("yagra_sweep_cache_misses_total").increment(1);

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
                // Collect this rebuild's working-set desired sets to seed the cache (see below).
                let mut new_desired_by_pool: HashMap<String, HashMap<NodeId, Vec<JobSpec>>> =
                    HashMap::new();

                // Per-node working-set builds fan out with bounded concurrency: each resolves a
                // node's URL/SNMP/collection config with a few DB round-trips, so at tens of
                // thousands of nodes doing them strictly one-at-a-time would let the build alone
                // exceed the poll interval. Bounded so the DB connection pool isn't overwhelmed.
                const SWEEP_BUILD_CONCURRENCY: usize = 16;

                for (pool, members) in groups {
                    if scheduler::pool_uses_working_set(&pool, &live) {
                        // Build the pool's whole desired working set and let the coordinator diff +
                        // distribute it (snapshots/deltas). Not gated by `due()`. These nodes leave
                        // the legacy `last_dispatched` map (a later legacy fallback re-polls at once).
                        for (node, _secs) in &members {
                            last_dispatched.remove(&node.id.as_uuid());
                        }
                        // Own each item into the stream and clone the `Arc` per future so no borrow
                        // crosses an `.await` (keeps the concurrent builds free of lifetime coupling).
                        let desired: HashMap<_, _> =
                            futures::stream::iter(members)
                                .map(|(node, secs)| {
                                    let dispatcher = dispatcher.clone();
                                    async move {
                                        (node.id, dispatcher.build_node_specs(&node, secs).await)
                                    }
                                })
                                .buffer_unordered(SWEEP_BUILD_CONCURRENCY)
                                .filter_map(|(id, specs)| async move {
                                    (!specs.is_empty()).then_some((id, specs))
                                })
                                .collect()
                                .await;
                        new_desired_by_pool.insert(pool.clone(), desired.clone());
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
                            for (job, kind) in dispatcher.build_scheduled_jobs(node, *secs).await {
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
                // Seed the fast-path cache only when the whole fleet was working-set — a legacy pool
                // needs node rows every round, so a mixed fleet keeps rebuilding (unchanged behavior).
                // Tagged with the generation read before the rebuild so a racing config change is
                // detected next round.
                cache = if legacy_pools == 0 {
                    Some(SweepCache {
                        generation,
                        min_interval,
                        desired_by_pool: new_desired_by_pool,
                    })
                } else {
                    None
                };
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
async fn serve(
    state: ApiState,
    addr: &str,
    metrics: PrometheusHandle,
    shutdown: CancellationToken,
) -> anyhow::Result<()> {
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
    // Turn SIGTERM/Ctrl-C into cancellation of the shared token: the background loops stop and the
    // server below leaves its accept loop, draining in-flight requests before returning (ADR-017).
    {
        let shutdown = shutdown.clone();
        tokio::spawn(async move {
            shutdown_signal().await;
            tracing::info!("shutdown signal received — draining and stopping");
            shutdown.cancel();
        });
    }
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(%addr, "Yagra-core API listening on /api/v1 (+ /metrics)");
    axum::serve(listener, app)
        .with_graceful_shutdown(async move { shutdown.cancelled().await })
        .await?;
    Ok(())
}

/// Build the alert engine's config snapshot: all thresholds, per-node metadata (profile +
/// group tag-values) for scope resolution, the dependency topology (parent edges) for
/// suppression / root-cause roll-up, and the nodes currently inside an active maintenance
/// window. Failures degrade to empty rather than crashing the refresh loop.
/// The config-derived half of the alert config: all thresholds + a full node scan folded into the
/// node-meta map and dependency topology. This is the expensive full-fleet work (a `list_nodes`
/// scan + a 50k-entry map/topology build), gated behind the config generation so it runs only after
/// an actual config change rather than every 30s refresh (S6). The raw node list is retained so the
/// time-dependent maintenance resolution can run each cycle without re-scanning the DB.
struct AlertConfigBase {
    rules: Vec<thresholds::StoredThreshold>,
    nodes: Vec<yagra_common::Node>,
    meta: HashMap<NodeId, NodeMeta>,
    topology: Topology,
}

/// Load the config-derived alert base (thresholds + node-meta + dependency topology).
async fn load_alert_config_base(repo: &NodeRepo, thresholds: &ThresholdStore) -> AlertConfigBase {
    let rules = thresholds.list_all().await.unwrap_or_else(|e| {
        tracing::warn!(error = %e, "failed to load thresholds");
        Vec::new()
    });
    let nodes = repo.list_nodes().await.unwrap_or_default();
    let mut meta = HashMap::new();
    let mut topology = Topology::new();
    for node in &nodes {
        // Dependency edge child → parent feeds parent-down suppression (ADR-015).
        if let Some(parent) = node.parent {
            topology.add_dependency(node.id, parent);
        }
        meta.insert(
            node.id,
            NodeMeta {
                profile: node.profile.as_ref().map(ToString::to_string),
                groups: node.tags.values().cloned().collect(),
            },
        );
    }
    AlertConfigBase {
        rules,
        nodes,
        meta,
        topology,
    }
}

/// Resolve the set of nodes currently inside an active maintenance window. Time-dependent (window
/// boundaries move with wall-clock), so it runs every refresh cycle — but over the *cached* node
/// list, not a fresh DB scan. Folder-group scopes expand against the inventory tree (recursive incl.
/// subgroups, ADR-022) — the same chain the Troubleshoot scope uses; only touches the DB when one is
/// actually active.
async fn resolve_maintenance(
    maintenance: &MaintenanceRepo,
    groups: &groups::GroupRepo,
    repo: &NodeRepo,
    nodes: &[yagra_common::Node],
) -> std::collections::BTreeSet<NodeId> {
    let scopes = maintenance.active_scopes().await.unwrap_or_else(|e| {
        tracing::warn!(error = %e, "failed to load maintenance windows");
        Vec::new()
    });
    let mut in_maintenance = maintenance::nodes_in_maintenance(&scopes, nodes);
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
    in_maintenance
}

/// Assemble the full alert config (config base + current maintenance). Used for the initial
/// synchronous load at startup; the refresh loop uses the two halves directly with generation
/// caching so the base isn't rebuilt when config is unchanged (S6).
async fn load_alert_config(
    repo: &NodeRepo,
    thresholds: &ThresholdStore,
    maintenance: &MaintenanceRepo,
    groups: &groups::GroupRepo,
) -> AlertConfig {
    let base = load_alert_config_base(repo, thresholds).await;
    let in_maintenance = resolve_maintenance(maintenance, groups, repo, &base.nodes).await;
    AlertConfig::new(base.rules, base.meta)
        .with_topology(base.topology)
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicBool, Ordering};
    use store::MetricPoint;
    use yagra_bus::{CheckOutcome, Sample};
    use yagra_common::{NodeId, SeriesKey};

    /// A [`MetricStore`] whose `write_batch` succeeds or fails on demand, for exercising the VM
    /// writer's retry/spill bookkeeping without a network. The read methods are never hit here.
    struct FakeStore {
        fail: AtomicBool,
    }
    impl FakeStore {
        fn new(fail: bool) -> Self {
            Self {
                fail: AtomicBool::new(fail),
            }
        }
    }
    #[async_trait::async_trait]
    impl MetricStore for FakeStore {
        async fn write(&self, _result: &PollResult) {}
        async fn write_batch(&self, _results: &[Arc<PollResult>]) -> bool {
            !self.fail.load(Ordering::SeqCst)
        }
        async fn latest(&self, _k: &SeriesKey) -> Option<f64> {
            None
        }
        async fn range(&self, _k: &SeriesKey, _f: i64, _t: i64, _s: u64) -> Vec<MetricPoint> {
            Vec::new()
        }
        async fn rate(&self, _k: &SeriesKey, _l: u64) -> Option<f64> {
            None
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
        async fn fresh_node_ids(&self, _m: &str, _w: u64) -> Vec<Uuid> {
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
        async fn node_metric_names(&self, _n: Uuid, _w: u64) -> Vec<String> {
            Vec::new()
        }
    }

    fn sample_result() -> Arc<PollResult> {
        Arc::new(PollResult {
            schema_version: 1,
            job_id: Uuid::nil(),
            node_id: NodeId::new(),
            at_unix_ms: 1,
            outcome: CheckOutcome::Reachable,
            samples: vec![Sample::gauge("icmp_rtt_ms", 9.0)],
            interfaces: Vec::new(),
            sys_descr: None,
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
        flush_vm(&store, &mut buf, &mut spill).await;
        assert!(buf.is_empty(), "fresh buffer is taken by the flush");
        assert_eq!(spill.len(), 1, "a batch that fails every retry is spilled");

        // Still down: a later flush retries the spilled batch (fails) and keeps it.
        flush_vm(&store, &mut buf, &mut spill).await;
        assert_eq!(spill.len(), 1, "spill retained while VM is down");

        // Recover: the next flush drains the spill.
        fake.fail.store(false, Ordering::SeqCst);
        flush_vm(&store, &mut buf, &mut spill).await;
        assert!(
            spill.is_empty(),
            "spill drains once VM accepts writes again"
        );
    }

    fn sweep_cache(generation: u64, pools: &[&str]) -> SweepCache {
        SweepCache {
            generation,
            min_interval: 30,
            desired_by_pool: pools
                .iter()
                .map(|p| ((*p).to_owned(), HashMap::new()))
                .collect(),
        }
    }

    fn live(pools: &[&str]) -> std::collections::HashSet<String> {
        pools.iter().map(|p| (*p).to_owned()).collect()
    }

    #[test]
    fn sweep_cache_reused_only_when_gen_matches_and_all_pools_working_set() {
        let c = sweep_cache(7, &["default", "site-a"]);
        // Config unchanged and both pools have live pollers → reuse.
        assert!(c.reusable(7, &live(&["default", "site-a"])));
        // A newer generation (config edited) → rebuild.
        assert!(!c.reusable(8, &live(&["default", "site-a"])));
        // A cached pool lost its poller (fell back to legacy) → rebuild.
        assert!(!c.reusable(7, &live(&["default"])));
        // An empty-fleet cache is vacuously reusable while the generation holds.
        assert!(sweep_cache(7, &[]).reusable(7, &live(&[])));
    }

    // The spill is bounded: past the cap the oldest batch is dropped rather than growing unbounded.
    #[tokio::test(start_paused = true)]
    async fn vm_flush_bounds_the_spill() {
        let fake = Arc::new(FakeStore::new(true)); // permanently "down"
        let store: Arc<dyn MetricStore> = fake.clone();
        let mut spill: VecDeque<Vec<Arc<PollResult>>> = VecDeque::new();

        for _ in 0..(VM_SPILL_MAX_BATCHES + 5) {
            let mut buf = vec![sample_result()];
            flush_vm(&store, &mut buf, &mut spill).await;
        }
        assert_eq!(
            spill.len(),
            VM_SPILL_MAX_BATCHES,
            "spill never exceeds its bound; the oldest is dropped"
        );
    }
}
