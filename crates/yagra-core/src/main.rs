// SPDX-License-Identifier: AGPL-3.0-only
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

// Global allocator (default-on `mimalloc` feature; `--no-default-features` gives the system one
// back). Core allocates short-lived buffers across dozens of Tokio workers for weeks, which is the
// shape glibc's per-thread arenas handle worst: measured on 50k nodes, its resident set climbed
// +162 MiB over 20 minutes and kept going, where this one moved +6 MiB at identical throughput.
// The full comparison is in the workspace Cargo.toml.
#[cfg(feature = "mimalloc")]
#[global_allocator]
static GLOBAL_ALLOC: mimalloc::MiMalloc = mimalloc::MiMalloc;

mod ack;
mod alerts;
mod analysis;
mod api;
mod apitokens;
mod arp;
mod audit;
mod auth;
mod authcallout;
mod bigquery;
mod cadence;
mod classification;
mod collection;
mod config;
mod config_bundle;
mod config_gen;
mod dashboard;
mod discovery;
mod dns_check;
mod events;
mod flowstore;
mod forward;
mod forward_store;
mod gcp;
mod groups;
mod history;
mod ipasn;
mod l3;
mod ldap;
mod leader;
mod link_overrides;
mod logstore;
mod maintenance;
mod mcp;
mod meraki;
mod mib;
mod neighbors;
mod notifications;
mod notify_facts;
mod notify_render;
mod oidc;
// Distributed poller pool (ADR-009/020): the coordinator owns the live registry + working-set
// distribution and consumes the ring / Redis mirror / durable inventory below.
mod coordinator;
mod pollers;
mod pool_coverage;
/// Effective poll-pool resolution (node > ancestor folder > default).
mod poolres;
mod ratelimit;
mod rca;
mod repo;
mod reports;
mod retention;
mod ring;
mod routing;
mod scheduler;
mod secrets;
mod seed_ids;
// The WebUI's own server certificate (ADR-044). Named apart from `tls`, which builds *client*
// configurations for outbound peers — see that module's doc.
mod server_cert;
mod sink;
mod store;
mod stored_enum;
// Diagnostic snapshot for a deployment nobody can open a shell on (ADR-045). Named apart from
// `config_bundle`, which moves configuration *between* deployments; this one describes one.
mod support_bundle;
mod thresholds;
mod tls;
mod token;
// Storage + volume materialization for the WebUI's certificate (ADR-044). `server_cert` decides
// what is acceptable; this decides where it lives.
mod topology_links;
mod topology_mode;
mod topology_projection;
mod url_check;
mod volatile;
mod webtls;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use yagra_telemetry::{shutdown_signal, spawn_cancellable, CancellationToken};

use ack::AckRepo;
use alerts::{ActiveMute, AlertConfig, AlertManager, NodeMeta, Notifier};
use api::{AdminState, ApiState};
use audit::AuditRepo;
use auth::{LoginThrottle, SessionStore, UserStore};
use axum::routing::get;
use collection::CollectionRepo;
use config::Config;
use coordinator::Coordinator;
use dashboard::{DashboardRepo, SharedDashboardRepo};
use discovery::DiscoveryRunner;
use flowstore::{ChStore, FlowRow, FlowStore};
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
use yagra_bus::{FlowBatch, JobSpec, NatsBus, PollResult, DEFAULT_POOL};
use yagra_common::NodeId;
use yagra_topology::Topology;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Container HEALTHCHECK entry point: `yagra-core healthcheck` probes our own `/healthz` and
    // exits 0 (healthy) / 1 (not). Dependency-free (reqwest is already linked), so the slim runtime
    // image needs no curl/wget. Handled before any store/bus wiring so it's cheap and side-effect-free.
    if std::env::args().nth(1).as_deref() == Some("healthcheck") {
        std::process::exit(run_healthcheck().await);
    }

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

/// Probe our own `/healthz` for the container HEALTHCHECK. Returns a process exit code: 0 when the
/// endpoint answers 2xx, 1 otherwise. Derives the port from the configured API address (default
/// 8080) so a custom `YAGRA_API_ADDR` still works.
async fn run_healthcheck() -> i32 {
    let addr = std::env::var("YAGRA_API_ADDR").unwrap_or_else(|_| "0.0.0.0:8080".to_owned());
    let port = addr.rsplit(':').next().unwrap_or("8080");
    let url = format!("http://127.0.0.1:{port}/healthz");
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
    {
        Ok(c) => c,
        Err(_) => return 1,
    };
    match client.get(&url).send().await {
        Ok(resp) if resp.status().is_success() => 0,
        _ => 1,
    }
}

/// Live mode: PostgreSQL + NATS + VictoriaMetrics, real ICMP polling end to end.
async fn run_live(cfg: Config, metrics: PrometheusHandle) -> anyhow::Result<()> {
    tracing::info!(
        interval = cfg.poll_interval_secs,
        "Yagra-core starting (live mode)"
    );

    // The KEK, before anything that seals or opens a secret. Loaded once and shared by all five
    // envelope stores (credentials, notification channels, forwarding destinations, OIDC, LLM
    // config). A configured-but-unreadable key file fails startup here rather than later and
    // silently: booting on a substituted key leaves every stored credential undecryptable while
    // the deployment looks healthy (ADR-018/ADR-040).
    let kek = secrets::load_key_provider()?;

    // Metadata store: connect, migrate, seed demo inventory if empty.
    let repo = Arc::new(NodeRepo::connect(&cfg.database_url).await?);
    repo.migrate().await?;
    repo.seed_demo_nodes_if_empty().await?;
    repo.seed_builtin_profiles().await?;
    // Seed the runtime-settings singleton, honoring YAGRA_POLL_INTERVAL_SECS and
    // YAGRA_FLOW_RETENTION_DAYS as the *initial* defaults on first boot only (ON CONFLICT DO
    // NOTHING preserves later UI edits). After this the DB value is authoritative and the
    // scheduler / prune loops re-read it each round.
    repo.seed_app_settings(cfg.poll_interval_secs, cfg.flow_retention_days)
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

    // Flow store (ADR-031, the 5th store — traffic-flow tier). Optional/default-OFF: when a
    // ClickHouse URL is set, core subscribes to `yagra.flows`, writes flow records, and serves the
    // flow-query API; when unset the flow receiver is disabled and the API returns 503. On the read
    // side this client is on `ApiState` for all cores; the writer runs leader-only (in `leader_work`).
    // Retention comes from the database, not from `cfg`: YAGRA_FLOW_RETENTION_DAYS only seeds the
    // settings row on first boot (above), and after that the operator's value is authoritative.
    // Constructing from `cfg` here would make every restart quietly revert an edit made in the UI.
    let flow_retention_days = repo.get_retention_settings().await.flow_days;
    let flows: Option<Arc<dyn FlowStore>> = cfg
        .flow_url
        .as_deref()
        .map(|u| Arc::new(ChStore::with_retention(u, flow_retention_days)) as Arc<dyn FlowStore>);
    if flows.is_some() {
        tracing::info!(
            retention_days = flow_retention_days,
            "ClickHouse flow store enabled (ADR-031)"
        );
    }

    // Offline IP→ASN table for flow AS enrichment (ADR-031 Increment 3). Opt-in/default-OFF: loaded
    // once from a mounted iptoasn.com TSV. A missing/unreadable file logs and disables enrichment
    // (non-fatal) so a stale path never takes core down. Shared by the writer (IP→AS) and the flow
    // API (AS→name).
    let ipasn_initial: Option<Arc<crate::ipasn::IpAsnDb>> =
        cfg.ipasn_db_path.as_deref().and_then(|p| {
            match crate::ipasn::IpAsnDb::load(std::path::Path::new(p)) {
                Ok(db) if db.is_empty() => {
                    tracing::warn!(
                        path = p,
                        "IP→ASN dataset loaded 0 ranges — AS enrichment disabled"
                    );
                    None
                }
                Ok(db) => {
                    tracing::info!(
                        ranges = db.len(),
                        path = p,
                        "IP→ASN enrichment enabled (ADR-031)"
                    );
                    Some(db)
                }
                Err(e) => {
                    tracing::warn!(error = %e, path = p, "IP→ASN dataset load failed — AS enrichment disabled");
                    None
                }
            }
        });
    // Hot-swappable handle shared by the writer (IP→AS) and flow API (AS→name). A background reloader
    // (below, when YAGRA_IPASN_RELOAD_SECS > 0) can replace it without a restart, so an external
    // updater refreshing the file keeps the table current (ADR-031).
    let ipasn: crate::ipasn::IpAsnHandle =
        std::sync::Arc::new(std::sync::RwLock::new(ipasn_initial));

    // Alert engine + notifier (env default route + DB channels/rules, ADR-015) + history.
    let alerts = Arc::new(AlertManager::new());
    let notifier = Arc::new(Notifier::from_env());
    let notifications = Arc::new(NotificationRepo::new(repo.pool(), kek.clone()));
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

    // IP→ASN periodic reloader (ADR-031): when enabled, re-read the dataset from disk every
    // `ipasn_reload_secs` and hot-swap it in, so an external updater (the compose `ipasn-updater`
    // sidecar writing to a shared volume) keeps the table fresh without restarting core. Runs on every
    // core (leader and standbys) since the flow API resolves names everywhere. A failed/empty reload
    // keeps the previous table. Also recovers if the file appeared only after startup.
    if let Some(path) = cfg.ipasn_db_path.clone() {
        if cfg.ipasn_reload_secs > 0 {
            let handle = ipasn.clone();
            let sd = shutdown.clone();
            let secs = cfg.ipasn_reload_secs;
            tokio::spawn(async move {
                let mut ticker = tokio::time::interval(Duration::from_secs(secs));
                ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                ticker.tick().await; // consume the immediate first tick
                loop {
                    tokio::select! {
                        () = sd.cancelled() => break,
                        _ = ticker.tick() => {
                            // Offload the blocking file read + parse/sort of the ~500k-row iptoasn TSV
                            // onto the blocking pool so a reload tick never stalls a Tokio worker
                            // (same discipline as the store-and-forward disk read).
                            let p = path.clone();
                            let loaded = tokio::task::spawn_blocking(move || {
                                crate::ipasn::IpAsnDb::load(std::path::Path::new(&p))
                            })
                            .await;
                            match loaded {
                                Ok(Ok(db)) if !db.is_empty() => {
                                    let ranges = db.len();
                                    *handle.write().unwrap() = Some(db);
                                    tracing::info!(ranges, "IP→ASN dataset reloaded (ADR-031)");
                                }
                                Ok(Ok(_)) => tracing::warn!("IP→ASN reload read 0 ranges — keeping previous table"),
                                Ok(Err(e)) => tracing::warn!(error = %e, "IP→ASN reload failed — keeping previous table"),
                                Err(e) => tracing::warn!(error = %e, "IP→ASN reload task panicked — keeping previous table"),
                            }
                        }
                    }
                }
            });
        }
    }

    // HA leader election (ADR-016): every leader-only background task (coordinator consumers, the
    // result-ingest → alert/notify/persist chain, the event pipeline, the schedulers, and the
    // config/retention/routing refresh loops) is deferred into `leader_work` below and spawned only
    // once this core holds the advisory lock. Read-only priming (event/alert/routing config) stays
    // inline so the API serves complete config from its first request, on the leader and standbys
    // alike. When HA is off, `leader_work` runs inline before `serve` — byte-identical to pre-HA.

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

    // Per-poller NATS credential scoping via Auth Callout (ADR-030). When a callout account seed is
    // mounted AND the poller bootstrap secret is set, run core as the NATS auth service on EVERY core
    // (queue-subscribed, not leader-gated) so authentication survives a failover. Unset ⇒ NATS uses
    // its static account config and this is a no-op — byte-identical to today's deployments.
    if let (Some(seed_path), Some(secret)) = (
        cfg.nats_callout_seed_file.as_deref(),
        cfg.nats_poller_password.clone(),
    ) {
        match std::fs::read_to_string(seed_path) {
            Ok(seed) => {
                match yagra_authz::AccountSigner::from_seed(
                    seed.trim(),
                    cfg.nats_callout_account.clone(),
                ) {
                    Ok(signer) => {
                        // The issuer public key is NOT a secret — the operator pastes it into the
                        // NATS `auth_callout { issuer }` config, so surface it on startup.
                        tracing::info!(
                            issuer = %signer.issuer_public_key(),
                            account = %cfg.nats_callout_account,
                            "auth-callout enabled (ADR-030) — set this issuer in nats-server.conf auth_callout"
                        );
                        spawn_cancellable(
                            &shutdown,
                            authcallout::run_auth_callout(bus.client(), Arc::new(signer), secret),
                        );
                    }
                    Err(e) => tracing::error!(
                        error = %e,
                        "auth-callout: invalid account seed; per-poller credential scoping disabled"
                    ),
                }
            }
            Err(e) => tracing::error!(
                error = %e,
                path = %seed_path,
                "auth-callout: cannot read seed file; per-poller credential scoping disabled"
            ),
        }
    }

    // Discovery: a runner that publishes sweep jobs (shared with the API) + a consumer that folds
    // results back in — the consumer is leader-only (spawned in `leader_work`).
    let discovery = Arc::new(DiscoveryRunner::new(bus.clone(), classifier.clone()));

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
        tokio::sync::mpsc::channel::<events::QueuedAction>(events::ACTION_CHANNEL_CAP);
    let event_engine = Arc::new(events::EventEngine::new(
        events_repo.clone(),
        alerts.clone(),
        notifier.clone(),
        history.clone(),
        Some(persist_tx),
        Some(event_action_tx),
    ));
    // Priming: load the rule/address snapshot now so the API's webhook/rule-test endpoints read a
    // populated engine from the first request (also refreshed by the leader's 30s loop). The event
    // consumer, TTL sweeper, and the persist/action writers are leader-only (spawned in
    // `leader_work`); `persist_rx`/`event_action_rx` are held for it.
    event_engine.reload(&repo).await;

    // Credential store, shared by the API admin and the scheduler's SNMP resolution.
    let creds = Arc::new(CredentialStore::new(repo.pool(), kek.clone()));

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
    let dns_checks = Arc::new(dns_check::DnsCheckRepo::new(repo.pool()));
    let neighbor_repo = Arc::new(neighbors::NeighborRepo::new(repo.pool()));
    let l3_repo = Arc::new(l3::L3Repo::new(repo.pool()));
    let arp_repo = Arc::new(arp::ArpRepo::new(repo.pool()));
    let routing_repo = Arc::new(routing::RoutingRepo::new(repo.pool()));
    let discovered_repo = Arc::new(arp::DiscoveredRepo::new(repo.pool()));
    let topo_link_repo = Arc::new(topology_links::TopoLinkRepo::new(repo.pool()));
    let link_override_repo = Arc::new(link_overrides::LinkOverrideRepo::new(repo.pool()));

    // Poll dispatcher: turns a node into bus jobs (ICMP + SNMP, or HTTP for URL monitors, or DNS for
    // DNS monitors). Shared by the periodic scheduler and the on-demand "poll now" API action so
    // both build jobs the same way.
    let dispatcher = Arc::new(scheduler::PollDispatcher::new(
        scheduler::PollDispatcherSeams {
            bus: bus.clone(),
            creds: creds.clone(),
            collection: collection.clone(),
            url_checks: url_checks.clone(),
            dns_checks: dns_checks.clone(),
            meraki_devices: meraki_devices.clone(),
            settings: repo.clone(),
            l3: l3_repo.clone(),
            env_community: env_community.clone(),
            interval_secs: cfg.poll_interval_secs,
        },
    ));

    // Scheduler + Meraki scheduler are leader-only (spawned in `leader_work`). Collects route to the
    // configured Meraki pool (env `YAGRA_MERAKI_POOL`, default `default`); resolve it here so the
    // value is available to `leader_work`.
    let meraki_pool = std::env::var("YAGRA_MERAKI_POOL")
        .ok()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_POOL.to_owned());

    // Thresholds + maintenance windows: snapshot into the alert engine now, then refresh
    // periodically so edits (and window start/end boundaries) take effect without a restart.
    let thresholds = Arc::new(ThresholdStore::new(repo.pool()));
    let maintenance = Arc::new(MaintenanceRepo::new(repo.pool()));
    // Shared group repo: maintenance/mute folder-group scopes and the analysis runner all expand a
    // group to its subtree, and AdminState serves group CRUD — one hierarchy, read in several places.
    let group_repo = Arc::new(groups::GroupRepo::new(repo.pool()));
    // Priming: snapshot the alert config now so `GET /alerts` reads populated thresholds/topology
    // from the first request. The 30s refresh loop that follows edits is leader-only (`leader_work`).
    let topo_sources = topology_projection::TopologySources {
        links: topo_link_repo.clone(),
        pollers: poller_repo.clone(),
        l3: l3_repo.clone(),
        nodes: repo.clone(),
    };
    alerts.set_config(
        load_alert_config(&repo, &thresholds, &maintenance, &group_repo, &topo_sources).await,
    );

    // Notification templates (ADR-039) interpolate node names, groups and profiles, none of which
    // an `Alert` carries. Wired once, here, because it needs the write side; a skeleton-mode core
    // never reaches this and renders ids instead. It is only consulted when a channel actually has
    // a template, so a deployment with none issues no extra query.
    notifier.set_facts_source(Arc::new(notify_facts::CachedNodeFacts::new(repo.clone())));

    // Notification routing + mutes priming: load the DB channels/rules into the notifier now (env
    // channels stay always-on). The periodic refresh loop is leader-only (`leader_work`).
    load_routing(&notifier, &notifications).await;
    load_mutes(&notifier, &maintenance, &repo, &group_repo).await;

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
    // Troubleshoot analysis runner (ADR-022): read-only background diagnostics. Reads VictoriaMetrics
    // plus, for the event/flow analyses (ADR-024/031 increment), the passive-event store and the flow
    // store (both read-only + admission-bounded — still a "read"). The orphan-reconcile (fail jobs
    // left `running` by a previous process) is a leader-only DB mutation — deferred to `leader_work`
    // so a standby never fails the live leader's in-flight jobs; `analysis_repo` is held for it. The
    // runner itself serves the API on all cores.
    let analysis_repo = Arc::new(analysis::AnalysisRepo::new(repo.pool()));
    // `group_repo` is created earlier (shared with maintenance/mute scope resolution); `events_repo`,
    // `flows` (None when the flow tier is off), and `ipasn` are shared with the API/ingest paths.
    let analysis = Arc::new(analysis::AnalysisRunner::new(
        analysis_repo.clone(),
        analysis::AnalysisSeams {
            store: store.clone(),
            nodes: repo.clone(),
            groups: group_repo.clone(),
            events: events_repo.clone(),
            logs: logs.clone(),
            flows: flows.clone(),
            ipasn: ipasn.clone(),
            topo: topology_projection::TopologySources {
                links: topo_link_repo.clone(),
                pollers: poller_repo.clone(),
                l3: l3_repo.clone(),
                nodes: repo.clone(),
            },
        },
    ));

    // Reports (Dashboard → Reports): a TSDB+PostgreSQL-read background generator in core (mirrors the
    // analysis runner). Fail any run left `running` by a previous process, then build the runner over
    // the same store/inventory/alert/history seams. A 60s loop fires due schedules.
    let reports_repo = Arc::new(reports::ReportsRepo::new(repo.pool()));
    // Orphan reconcile + the 60s schedule-firing loop are leader-only (deferred to `leader_work`);
    // `reports_repo` / `reports` are held for it. The runner serves the API on all cores.
    let reports = Arc::new(reports::ReportRunner::new(
        reports_repo.clone(),
        store.clone(),
        repo.clone(),
        alerts.clone(),
        history.clone(),
    ));

    // ── HA leader election (ADR-016) ────────────────────────────────────────────────────────────
    // Forwarding ("tee", ADR-034): received syslog/traps are also relayed to external collectors.
    // The store backs Settings ▸ Forwarding on every core; the dispatcher is leader-only (below) so
    // a passive core never double-sends. With no destinations configured this is inert.
    let forward_store = Arc::new(forward_store::ForwardStore::new(repo.pool(), kek.clone()));
    let (forward_handle, forward_runner) = forward::prepare(forward_store.clone());

    // AI-assisted RCA store (ADR-029). Built here rather than beside the orchestrator below because
    // two things need it: `AdminState` (Settings ▸ AI, on every core) and the leader's retention
    // loop (`retention::Subject::RcaReports`, leader-only like every other prune).
    let llm_repo = Arc::new(rca::store::RcaRepo::new(repo.pool(), kek.clone()));

    // `is_leader` drives `/readyz` and the `yagra_core_is_leader` gauge. With HA off this core is
    // always the leader (ready); with HA on it flips true only once the advisory lock is won.
    let is_leader = Arc::new(AtomicBool::new(!cfg.enable_ha));
    metrics::gauge!("yagra_core_is_leader").set(if cfg.enable_ha { 0.0 } else { 1.0 });

    // Every leader-only background task, deferred so it starts exactly once — the moment this core
    // holds the advisory lock (immediately when HA is off). `LeaderTasks` holds *clones* so
    // `AdminState`/`ApiState` below keep their own handles; the single-owner resources (the two
    // channel receivers, the forwarding runner, the Meraki pool name) move in, because only the
    // leader drains/runs them. Bus subscriptions happen inside `run`, so a standby never subscribes.
    // On a standby none of this runs, which is why the event-webhook/close API handlers 503 rather
    // than enqueue to an undrained channel.
    let leader_tasks = LeaderTasks {
        llm: llm_repo.clone(),
        shutdown: shutdown.clone(),
        bus: bus.clone(),
        coordinator: coordinator.clone(),
        notifier: notifier.clone(),
        store: store.clone(),
        repo: repo.clone(),
        history: history.clone(),
        alerts: alerts.clone(),
        scheduler_stats: scheduler_stats.clone(),
        meraki_inflight: meraki_inflight.clone(),
        meraki_devices: meraki_devices.clone(),
        meraki_orgs: meraki_orgs.clone(),
        meraki_pool,
        creds: creds.clone(),
        dispatcher: dispatcher.clone(),
        discovery: discovery.clone(),
        event_engine: event_engine.clone(),
        events_repo: events_repo.clone(),
        persist_rx,
        event_action_rx,
        logs: logs.clone(),
        flows: flows.clone(),
        ipasn: ipasn.clone(),
        thresholds: thresholds.clone(),
        maintenance: maintenance.clone(),
        group_repo: group_repo.clone(),
        classifier: classifier.clone(),
        classification: classification.clone(),
        notifications: notifications.clone(),
        analysis_repo: analysis_repo.clone(),
        analysis: analysis.clone(),
        reports_repo: reports_repo.clone(),
        reports: reports.clone(),
        dns_checks: dns_checks.clone(),
        neighbors: neighbor_repo.clone(),
        l3: l3_repo.clone(),
        arp: arp_repo.clone(),
        routing: routing_repo.clone(),
        discovered: discovered_repo.clone(),
        topology_links: topo_link_repo.clone(),
        link_overrides: link_override_repo.clone(),
        pollers: poller_repo.clone(),
        forward_handle: forward_handle.clone(),
        forward_runner: Some(forward_runner),
    };
    let leader_work = leader_tasks.run();

    // AI-assisted RCA (ADR-029). The store backs Settings ▸ AI; the orchestrator serves the
    // on-demand endpoint. Present on every core, leader or not: generating an explanation is a read
    // plus one outbound call, so there is nothing for a standby to double-do. With no config row
    // (the default) the orchestrator answers 503 and nothing leaves the building.
    let audit_repo = Arc::new(AuditRepo::new(repo.pool()));
    let rca = Some(Arc::new(rca::orchestrator::RcaOrchestrator::new(
        llm_repo.clone(),
        repo.clone(),
        alerts.clone(),
        analysis.clone(),
        audit_repo.clone(),
    )));

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
        audit: audit_repo,
        dashboards: Arc::new(DashboardRepo::new(repo.pool())),
        shared_dashboard: Arc::new(SharedDashboardRepo::new(repo.pool())),
        scheduler_stats: scheduler_stats.clone(),
        poll: dispatcher,
        analysis,
        reports,
        url_checks,
        dns_checks,
        neighbors: neighbor_repo.clone(),
        l3: l3_repo.clone(),
        arp: arp_repo.clone(),
        // No `routing` here on purpose: Increment 4 adds no read endpoint. Its edges reach the API
        // through the links `run_topology_derivation` writes, which `/topology/links` already
        // serves — so the route ledger gains no line and the MCP gap does not move (ADR-042).
        discovered: discovered_repo.clone(),
        topology_links: topo_link_repo.clone(),
        link_overrides: link_override_repo.clone(),
        meraki_orgs,
        meraki_devices,
        events: events_repo,
        coordinator: coordinator.clone(),
        pollers: poller_repo.clone(),
        api_tokens: Arc::new(apitokens::ApiTokenStore::new(
            repo.pool().clone(),
            cfg.pat_oidc_idle_days,
        )),
        forward: forward_store,
        forward_handle,
        llm: llm_repo,
        config_bundle: Arc::new(config_bundle::ConfigBundleRepo::new(repo.pool().clone())),
        support: Arc::new(support_bundle::SupportRepo::new(repo.pool().clone())),
    }));
    // Session store. Default: opaque per-process tokens (byte-identical to pre-HA). When a session
    // signing key is mounted (`YAGRA_SESSION_KEY_FILE`), mint stateless HMAC-signed tokens that any
    // core sharing the key verifies synchronously — the Core HA active/active session substrate
    // (ADR-016 Increment 2a). Revocation rides a per-core denylist fed by the durable
    // `auth_revocations` table (cold-loaded here) and the `yagra.auth.revoke` bus fan-out.
    let sessions = if let Some(key_path) = cfg.session_key_file.as_deref() {
        // Fail-closed: a configured-but-unreadable/invalid key aborts startup (don't silently
        // downgrade to per-core sessions under a multi-core expectation). The key is never logged.
        let key = token::load_session_key(key_path)?;
        tracing::info!(
            path = %key_path,
            "signed session tokens enabled (ADR-016 Increment 2a) — sessions verify on any core sharing the key"
        );
        let (revoke_tx, revoke_rx) =
            tokio::sync::mpsc::unbounded_channel::<yagra_bus::AuthRevoke>();
        let store = Arc::new(SessionStore::with_signer(
            token::TokenSigner::new(key),
            revoke_tx,
        ));
        // Cold-load durable revocations so a restart / promotion honors prior logouts & disables.
        match auth::load_active_revocations(&repo.pool()).await {
            Ok(list) => {
                let n = list.len();
                for r in &list {
                    store.apply_remote_revoke(r);
                }
                if n > 0 {
                    tracing::info!(
                        count = n,
                        "loaded active session revocations from auth_revocations"
                    );
                }
            }
            Err(e) => tracing::warn!(error = %e, "failed to load session revocations (continuing)"),
        }
        // These run on EVERY core (not leader-gated) so a revocation is durable + reaches all cores,
        // and every core honors revocations made elsewhere — required once reads go active/active.
        {
            let bus = bus.clone();
            let pool = repo.pool().clone();
            spawn_cancellable(&shutdown, run_auth_revoke_writer(revoke_rx, bus, pool));
        }
        {
            let bus = bus.clone();
            let store = store.clone();
            spawn_cancellable(&shutdown, run_auth_revoke_subscriber(bus, store));
        }
        {
            let store = store.clone();
            let pool = repo.pool().clone();
            spawn_cancellable(&shutdown, run_revocation_pruner(store, pool));
        }
        store
    } else {
        if cfg.enable_ha {
            tracing::warn!(
                "HA enabled without YAGRA_SESSION_KEY_FILE — sessions remain per-core in-memory \
                 (fine for active/passive; set a key file for the coming active/active read scale-out)"
            );
        }
        Arc::new(SessionStore::new())
    };
    // External-IdP login (OIDC, ADR-010 Phase 3): provider store (envelope-encrypted secret) + the
    // in-memory in-flight authorization map.
    let oidc = Some(Arc::new(oidc::OidcRepo::new(repo.pool(), kek.clone())));
    let oidc_flight = Arc::new(oidc::OidcFlight::new());
    // Directory login (LDAP/AD, ADR-041): the single configuration row, with the service account's
    // bind password sealed by the same KEK. No in-flight map — there is no redirect leg.
    let ldap = Some(Arc::new(ldap::LdapRepo::new(repo.pool(), kek.clone())));
    // The WebUI's own TLS certificate (ADR-044). Same KEK as every other secret; the directory is
    // where the certificate is materialized for nginx to read, and is absent on a deployment that
    // terminates TLS somewhere else.
    let webtls = Arc::new(webtls::WebTlsRepo::new(
        repo.pool(),
        kek.clone(),
        std::env::var("YAGRA_TLS_DIR")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .map(std::path::PathBuf::from),
    ));

    let nodes: Arc<dyn NodeListing> = repo;
    let state = ApiState {
        store,
        logs,
        flows,
        ipasn: ipasn.clone(),
        host_sample: core_host,
        nodes,
        alerts,
        admin,
        sessions,
        login_throttle: Arc::new(LoginThrottle::new()),
        history: Some(history),
        ack: Some(acks),
        events: Some(event_engine),
        public_dashboard: cfg.public_dashboard,
        is_leader: is_leader.clone(),
        ldap,
        oidc,
        oidc_flight,
        enable_mcp: cfg.enable_mcp,
        rca,
        webtls: Some(webtls.clone()),
        metrics: Some(metrics.clone()),
        started: std::time::SystemTime::now(),
    };

    // Establish the certificate BEFORE the listener binds, so "core is healthy" implies the file
    // nginx is about to open already exists — which is what turns the compose files'
    // `depends_on: core: {condition: service_healthy}` into a guarantee instead of a race that
    // usually goes the right way. Deliberately not leader-gated: on a fresh database a standby that
    // starts first must still be able to bootstrap one, or `web` waits forever for a file the leader
    // has not been elected to write.
    let tls_names = webtls::configured_names();
    webtls.ensure_ready(&tls_names).await;
    {
        // Renewal. Also not leader-gated, and safe not to be: the write is content-addressed and
        // atomic, so two cores producing the same bytes is a no-op, and only a self-signed
        // certificate is ever replaced.
        let webtls = webtls.clone();
        let shutdown_renew = shutdown.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(webtls::RENEWAL_CHECK_INTERVAL);
            tick.tick().await; // the first tick is immediate, and ensure_ready just ran
            loop {
                tokio::select! {
                    () = shutdown_renew.cancelled() => break,
                    _ = tick.tick() => webtls.ensure_ready(&tls_names).await,
                }
            }
        });
    }

    if cfg.enable_ha {
        // Standby until the advisory lock is won. The API (incl. `/healthz`) already serves below,
        // so the container stays healthy while waiting; promotion is live (no restart). A lost lock
        // connection = graceful shutdown for orchestrator restart (spawn-once, ADR-016 model B).
        let shutdown_lease = shutdown.clone();
        let is_leader = is_leader.clone();
        let db_url = cfg.database_url.clone();
        let core_label = cfg.core_id.clone().unwrap_or_else(|| "core".to_owned());
        tokio::spawn(async move {
            tracing::info!(core = %core_label, "HA enabled — standby, waiting for leadership");
            let Some(conn) = leader::acquire(&db_url, &shutdown_lease).await else {
                return; // shutdown fired before leadership was won
            };
            is_leader.store(true, Ordering::Release);
            metrics::gauge!("yagra_core_is_leader").set(1.0);
            tracing::info!(core = %core_label, "acquired leadership — starting coordinator and background workers");
            if let Err(e) = leader_work.await {
                tracing::error!(error = %e, "leader startup failed — shutting down for restart");
                shutdown_lease.cancel();
                return;
            }
            leader::hold_until_lost(conn, &shutdown_lease).await;
            if !shutdown_lease.is_cancelled() {
                is_leader.store(false, Ordering::Release);
                metrics::gauge!("yagra_core_is_leader").set(0.0);
                tracing::error!(
                    "lost leadership (PostgreSQL connection lost) — shutting down for restart"
                );
                shutdown_lease.cancel();
            }
        });
    } else {
        // Single active core: run all leader work inline before serving — byte-identical to pre-HA.
        leader_work.await?;
    }

    serve(state, &cfg.api_addr, metrics, shutdown).await
}

/// Everything the leader-only background work needs, and the work itself.
///
/// This used to be an 828-line `run_live` whose leader half was a single async block preceded by
/// 31 anonymous `let x = x.clone();` lines. Adding one background task meant four coordinated
/// edits — a clone in that prologue, a spawn buried in a ~200-line block, a free `run_*` fn, and
/// often a new `AdminState` field — with nothing naming what the block actually started.
///
/// Now the dependencies are named fields (documented once, here) and the work is grouped by
/// pipeline into the methods below. Adding a task is: add a field if it needs a new handle, and a
/// spawn in the method whose pipeline it belongs to.
///
/// **Ownership is the interesting part.** Most fields are `Arc` clones, so `ApiState`/`AdminState`
/// keep their own handles and both sides stay live. Four are *moved* in because they are
/// single-owner and only the leader may have them: the two channel receivers (a second drainer
/// would silently split the stream), the forwarding runner, and the Meraki pool name.
struct LeaderTasks {
    /// Cancelled on shutdown; every spawned task is cancellable on it.
    shutdown: CancellationToken,
    bus: Arc<NatsBus>,
    coordinator: Arc<Coordinator>,
    notifier: Arc<Notifier>,
    store: Arc<dyn MetricStore>,
    repo: Arc<NodeRepo>,
    history: Arc<AlertHistoryStore>,
    alerts: Arc<AlertManager>,
    scheduler_stats: Arc<scheduler::SchedulerStats>,
    meraki_inflight: Arc<meraki::MerakiInflight>,
    meraki_devices: Arc<meraki::MerakiDeviceRepo>,
    meraki_orgs: Arc<meraki::MerakiOrgRepo>,
    /// Which poller pool Meraki collection jobs are published to (moved: read once at startup).
    meraki_pool: String,
    creds: Arc<CredentialStore>,
    dispatcher: Arc<scheduler::PollDispatcher>,
    discovery: Arc<DiscoveryRunner>,
    event_engine: Arc<events::EventEngine>,
    events_repo: Arc<events::EventRepo>,
    /// Passive-event persistence queue (moved — exactly one drainer).
    persist_rx: tokio::sync::mpsc::Receiver<events::PersistRecord>,
    /// Passive-event side-effect queue: history + notify (moved — exactly one drainer).
    event_action_rx: tokio::sync::mpsc::Receiver<events::QueuedAction>,
    logs: Option<Arc<dyn LogStore>>,
    flows: Option<Arc<dyn FlowStore>>,
    ipasn: ipasn::IpAsnHandle,
    thresholds: Arc<ThresholdStore>,
    maintenance: Arc<MaintenanceRepo>,
    group_repo: Arc<groups::GroupRepo>,
    classifier: Arc<classification::Classifier>,
    classification: Arc<classification::ClassificationRepo>,
    notifications: Arc<NotificationRepo>,
    analysis_repo: Arc<analysis::AnalysisRepo>,
    analysis: Arc<analysis::AnalysisRunner>,
    reports_repo: Arc<reports::ReportsRepo>,
    reports: Arc<reports::ReportRunner>,
    dns_checks: Arc<dns_check::DnsCheckRepo>,
    neighbors: Arc<neighbors::NeighborRepo>,
    l3: Arc<l3::L3Repo>,
    arp: Arc<arp::ArpRepo>,
    routing: Arc<routing::RoutingRepo>,
    discovered: Arc<arp::DiscoveredRepo>,
    topology_links: Arc<topology_links::TopoLinkRepo>,
    link_overrides: Arc<link_overrides::LinkOverrideRepo>,
    /// Durable poller inventory — where the pollers are, which is what roots the derived dependency
    /// graph (ADR-043 I2). Also the home of `monitoring_gaps`, which the retention loop prunes.
    pollers: Arc<PollerRepo>,
    /// Generated LLM root-cause reports — held only so the retention loop can prune them
    /// (`retention::Subject::RcaReports`); generating one is not a leader task.
    llm: Arc<rca::store::RcaRepo>,
    forward_handle: forward::ForwardHandle,
    /// The forwarding dispatcher itself (moved — leader-only so a standby never double-sends).
    forward_runner: Option<forward::ForwardRunner>,
}

impl LeaderTasks {
    /// Start every leader-only pipeline. Returns once they are all spawned (they run until the
    /// shutdown token fires); an `Err` means a bus subscription failed, which the caller treats as
    /// a failed promotion and restarts for.
    ///
    /// Order matters in one place only: the ingest pipeline creates the writer channels that the
    /// backfill consumer clones, so those two live in the same method.
    async fn run(mut self) -> anyhow::Result<()> {
        self.spawn_coordinator().await?;
        self.spawn_result_ingest().await?;
        self.spawn_discovery_and_forwarding().await?;
        self.spawn_event_pipeline().await?;
        self.spawn_flow_pipeline().await?;
        self.spawn_schedulers();
        self.spawn_refresh_loops();
        self.reconcile_orphaned_jobs().await;
        Ok(())
    }

    /// Coordinator registry: poller heartbeats + working-set snapshot requests (ADR-009/020).
    async fn spawn_coordinator(&self) -> anyhow::Result<()> {
        let hb = Box::pin(self.bus.subscribe_heartbeats().await?);
        spawn_cancellable(
            &self.shutdown,
            self.coordinator.clone().run_heartbeat_consumer(hb),
        );
        let sr = Box::pin(self.bus.subscribe_sync_requests().await?);
        spawn_cancellable(
            &self.shutdown,
            self.coordinator.clone().run_sync_request_consumer(sr),
        );
        Ok(())
    }

    /// Poll-result ingestion (ADR-025): the notification worker, the async batch writers, the
    /// single in-memory matcher, and the store-and-forward backfill consumer.
    ///
    /// These are one method because they share channels: the matcher hands off to the VM/PG
    /// writers, and the backfill consumer reuses those same writers via cloned senders (it imports
    /// metrics at their original timestamp but must NEVER run alert evaluation — replayed samples
    /// would re-fire dwell-based alerts as a flood).
    async fn spawn_result_ingest(&self) -> anyhow::Result<()> {
        // Notification delivery worker (bounded queue, single ordered consumer) — fed by the
        // matcher so a slow vendor endpoint can never stall ingest.
        let (notify_tx, mut notify_rx) =
            tokio::sync::mpsc::channel::<crate::alerts::NotifyAction>(1024);
        {
            let notifier = self.notifier.clone();
            spawn_cancellable(&self.shutdown, async move {
                while let Some(action) = notify_rx.recv().await {
                    notifier.handle(action).await;
                }
            });
        }

        let (metrics_tx, metrics_rx) =
            tokio::sync::mpsc::channel::<Arc<PollResult>>(RESULT_PERSIST_CHANNEL_CAP);
        let (meta_tx, meta_rx) =
            tokio::sync::mpsc::channel::<MetaRecord>(RESULT_PERSIST_CHANNEL_CAP);
        let (history_tx, history_rx) =
            tokio::sync::mpsc::channel::<HistoryRecord>(RESULT_PERSIST_CHANNEL_CAP);
        tokio::spawn(run_vm_writer(
            metrics_rx,
            self.store.clone(),
            self.shutdown.clone(),
        ));
        tokio::spawn(run_pg_writer(
            meta_rx,
            history_rx,
            MetaStores {
                repo: self.repo.clone(),
                dns: self.dns_checks.clone(),
                neighbors: self.neighbors.clone(),
                l3: self.l3.clone(),
                arp: self.arp.clone(),
                routing: self.routing.clone(),
            },
            self.history.clone(),
            self.shutdown.clone(),
        ));
        {
            let results = Box::pin(self.bus.subscribe_results().await?);
            spawn_cancellable(
                &self.shutdown,
                consume_results(
                    results,
                    self.alerts.clone(),
                    notify_tx,
                    metrics_tx.clone(),
                    meta_tx.clone(),
                    history_tx,
                    self.history.clone(),
                    self.scheduler_stats.clone(),
                    self.meraki_inflight.clone(),
                    self.coordinator.clone(),
                ),
            );
        }
        {
            let backfill = Box::pin(self.bus.subscribe_results_backfill().await?);
            spawn_cancellable(
                &self.shutdown,
                consume_results_backfill(backfill, metrics_tx, meta_tx),
            );
        }
        Ok(())
    }

    /// Discovery-result folding (swept devices classified back into the inventory) and the
    /// forwarding dispatcher (ADR-034), which owns the per-destination sender tasks and reloads
    /// them on every config change.
    async fn spawn_discovery_and_forwarding(&mut self) -> anyhow::Result<()> {
        let results = Box::pin(self.bus.subscribe_discovery_results().await?);
        spawn_cancellable(&self.shutdown, self.discovery.clone().run_consumer(results));

        // Taken, not cloned: the dispatcher owns the per-destination sender tasks, and a second
        // one would double-send every relayed datagram.
        if let Some(runner) = self.forward_runner.take() {
            tokio::spawn(runner.run(self.shutdown.clone()));
        }
        Ok(())
    }

    /// Passive-event pipeline (ADR-024): the bus consumer (which tees to the forwarder before rule
    /// matching), the TTL sweeper, and the two writers that drain the match stage's queues.
    async fn spawn_event_pipeline(&mut self) -> anyhow::Result<()> {
        let stream = Box::pin(self.bus.subscribe_events().await?);
        spawn_cancellable(
            &self.shutdown,
            events::consume_events(
                stream,
                self.event_engine.clone(),
                Some(self.forward_handle.clone()),
            ),
        );
        spawn_cancellable(
            &self.shutdown,
            events::run_ttl_sweeper(self.event_engine.clone()),
        );

        let (persist_rx, event_action_rx) = self.take_event_queues();
        tokio::spawn(events::run_persist_writer(
            persist_rx,
            self.events_repo.clone(),
            self.logs.clone(),
            self.shutdown.clone(),
        ));
        tokio::spawn(events::run_event_action_writer(
            event_action_rx,
            self.history.clone(),
            self.notifier.clone(),
            self.shutdown.clone(),
        ));
        Ok(())
    }

    /// Take the two single-owner event queues out of `self`. They are replaced with closed stubs:
    /// draining a queue twice would silently split the event stream, so the receiver must not be
    /// reachable again after this.
    fn take_event_queues(
        &mut self,
    ) -> (
        tokio::sync::mpsc::Receiver<events::PersistRecord>,
        tokio::sync::mpsc::Receiver<events::QueuedAction>,
    ) {
        let (_, closed_persist) = tokio::sync::mpsc::channel(1);
        let (_, closed_action) = tokio::sync::mpsc::channel(1);
        (
            std::mem::replace(&mut self.persist_rx, closed_persist),
            std::mem::replace(&mut self.event_action_rx, closed_action),
        )
    }

    /// Traffic-flow pipeline (ADR-031/034). Two independent halves:
    ///
    ///  - **Storage** — only when ClickHouse is configured (default-OFF). Match/persist are split
    ///    (S27) so a slow ClickHouse cannot stall the `yagra.flows` subscription into a silent
    ///    NATS slow-consumer drop; leader-only, so exactly one writer persists.
    ///  - **Verbatim relay** — subscribed unconditionally, because forwarding raw flow datagrams
    ///    is useful without a flow store, and cheap when unused (`offer_flow` returns after one
    ///    relaxed load when no flow destination exists).
    async fn spawn_flow_pipeline(&self) -> anyhow::Result<()> {
        if let Some(flow_store) = self.flows.clone() {
            ensure_flow_schema(&flow_store).await;
            let (flow_tx, flow_rx) =
                tokio::sync::mpsc::channel::<FlowRow>(FLOW_PERSIST_CHANNEL_CAP);
            tokio::spawn(run_flow_writer(flow_rx, flow_store, self.shutdown.clone()));
            let flow_stream = Box::pin(self.bus.subscribe_flows().await?);
            spawn_cancellable(
                &self.shutdown,
                consume_flows(flow_stream, flow_tx, self.repo.clone(), self.ipasn.clone()),
            );
        }
        let raw_flows = Box::pin(self.bus.subscribe_raw_flows().await?);
        spawn_cancellable(
            &self.shutdown,
            consume_raw_flows(raw_flows, self.forward_handle.clone()),
        );
        Ok(())
    }

    /// The two dispatch loops: the per-node scheduler (working-set syncs + jittered dispatch) and
    /// the Meraki collection scheduler.
    fn spawn_schedulers(&mut self) {
        spawn_cancellable(
            &self.shutdown,
            run_scheduler(
                self.repo.clone(),
                self.group_repo.clone(),
                self.dispatcher.clone(),
                self.scheduler_stats.clone(),
                self.meraki_devices.clone(),
                self.coordinator.clone(),
            ),
        );
        spawn_cancellable(
            &self.shutdown,
            run_meraki_scheduler(
                self.meraki_orgs.clone(),
                self.meraki_devices.clone(),
                self.creds.clone(),
                self.bus.clone(),
                self.meraki_inflight.clone(),
                self.repo.clone(),
                std::mem::take(&mut self.meraki_pool),
            ),
        );
    }

    /// The periodic reload loops that pick up operator config edits, plus the report schedule
    /// firing loop.
    fn spawn_refresh_loops(&self) {
        spawn_cancellable(
            &self.shutdown,
            run_alert_config_refresh(
                self.alerts.clone(),
                self.repo.clone(),
                self.thresholds.clone(),
                self.maintenance.clone(),
                self.group_repo.clone(),
                self.classifier.clone(),
                self.classification.clone(),
                self.event_engine.clone(),
                topology_projection::TopologySources {
                    links: self.topology_links.clone(),
                    pollers: self.pollers.clone(),
                    l3: self.l3.clone(),
                    nodes: self.repo.clone(),
                },
            ),
        );
        spawn_cancellable(
            &self.shutdown,
            run_fleet_health_timeline(TimelineSources {
                repo: self.repo.clone(),
                alerts: self.alerts.clone(),
                history: self.history.clone(),
                events_repo: self.events_repo.clone(),
                dns_checks: self.dns_checks.clone(),
                neighbors: self.neighbors.clone(),
                l3: self.l3.clone(),
                analyses: self.analysis_repo.clone(),
                rca_reports: self.llm.clone(),
                pollers: self.pollers.clone(),
            }),
        );
        spawn_cancellable(
            &self.shutdown,
            run_topology_derivation(
                self.repo.clone(),
                self.l3.clone(),
                self.neighbors.clone(),
                self.routing.clone(),
                self.topology_links.clone(),
                self.link_overrides.clone(),
            ),
        );
        spawn_cancellable(
            &self.shutdown,
            run_endpoint_discovery(self.arp.clone(), self.discovered.clone()),
        );
        spawn_cancellable(
            &self.shutdown,
            run_routing_refresh(
                self.notifier.clone(),
                self.notifications.clone(),
                self.maintenance.clone(),
                self.repo.clone(),
                self.group_repo.clone(),
            ),
        );
        spawn_cancellable(
            &self.shutdown,
            run_pool_coverage_watch(
                self.coordinator.clone(),
                self.repo.clone(),
                self.meraki_devices.clone(),
                self.group_repo.clone(),
                self.alerts.clone(),
                self.notifier.clone(),
                self.history.clone(),
            ),
        );
        // Report schedule-firing loop (60s tick, advances `next_run_at`, prunes runs).
        spawn_cancellable(
            &self.shutdown,
            run_report_scheduler(self.reports.clone(), self.repo.clone()),
        );
        // Analysis schedule-firing loop (60s tick) — same cadence maths, different admission rules.
        spawn_cancellable(
            &self.shutdown,
            run_analysis_scheduler(self.analysis.clone()),
        );
    }

    /// Fail analysis/report jobs a previous leader left `running`. Leader-only — a standby must
    /// never touch the live leader's in-flight jobs. Best-effort: a failure here is logged, not
    /// fatal, because it only affects the display state of already-dead jobs.
    async fn reconcile_orphaned_jobs(&self) {
        match self.analysis_repo.fail_orphans().await {
            Ok(n) if n > 0 => tracing::warn!(
                orphans = n,
                "failed analysis jobs left running by a previous process"
            ),
            Err(e) => tracing::warn!(error = %e, "failed to reconcile orphaned analysis jobs"),
            _ => {}
        }
        match self.reports_repo.fail_orphans().await {
            Ok(n) if n > 0 => tracing::warn!(
                orphans = n,
                "failed report runs left running by a previous process"
            ),
            Err(e) => tracing::warn!(error = %e, "failed to reconcile orphaned report runs"),
            _ => {}
        }
    }
}

/// Skeleton mode: serve the API over an in-memory sink seeded with one demo reading.
async fn run_skeleton(metrics: PrometheusHandle) -> anyhow::Result<()> {
    tracing::warn!("store/bus URLs not set — running in in-memory skeleton mode (no real polling)");
    let sink = Arc::new(InMemorySink::default());
    // Demo seed so the walking-skeleton WebUI shows data before real polling is wired.
    sink.ingest(&PollResult {
        job_id: Uuid::nil(),
        node_id: yagra_common::NodeId::from(Uuid::nil()),
        at_unix_ms: 0,
        outcome: yagra_bus::CheckOutcome::Reachable,
        samples: vec![yagra_bus::Sample::gauge("icmp_rtt_ms", 8.0)],
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
    });
    let state = ApiState {
        store: sink,
        logs: None,
        flows: None,
        ipasn: crate::ipasn::empty_handle(),
        host_sample: Arc::new(std::sync::Mutex::new(None)),
        nodes: Arc::new(StaticNodeList::demo()),
        alerts: Arc::new(AlertManager::new()),
        admin: None,
        sessions: Arc::new(SessionStore::new()),
        login_throttle: Arc::new(LoginThrottle::new()),
        history: None,
        ack: None,
        events: None,
        // Skeleton has no user store (login returns 503), so reads must stay open or the
        // dev dashboard would be unreachable. Auth gating applies in live mode.
        public_dashboard: true,
        // Skeleton has no directory store either; `login` treats that as "no directory configured"
        // rather than an error, so the local path is unaffected.
        ldap: None,
        // Skeleton has no leader election — always "ready".
        is_leader: Arc::new(AtomicBool::new(true)),
        // Skeleton has no metadata store, so no OIDC provider config.
        oidc: None,
        oidc_flight: Arc::new(oidc::OidcFlight::new()),
        // Skeleton has no API-token store (admin is None), so MCP has no PAT auth backend — leave it
        // off regardless of the flag. MCP is a live-mode feature (its tools read the live seams).
        enable_mcp: false,
        // No metadata store ⇒ nowhere to keep a provider config or a report; the RCA endpoints 503.
        rca: None,
        webtls: None,
        metrics: Some(metrics.clone()),
        started: std::time::SystemTime::now(),
    };
    serve(state, "0.0.0.0:8080", metrics, CancellationToken::new()).await
}

/// How often core samples its own host resources (self-observability). Matches the WebUI refresh.
const HOST_SAMPLE_SECS: u64 = 15;

/// Drain locally-produced session revocations (logout / user disable-demote-reset-delete): persist
/// each to the durable `auth_revocations` table so it survives restart/failover, then fan it out on
/// `yagra.auth.revoke` so every other core denies the token too (Core HA active/active, ADR-016
/// Increment 2a). Runs on every core. Loops until the channel closes on shutdown.
async fn run_auth_revoke_writer(
    mut rx: tokio::sync::mpsc::UnboundedReceiver<yagra_bus::AuthRevoke>,
    bus: Arc<dyn yagra_bus::PeerBus>,
    pool: sqlx::PgPool,
) {
    while let Some(revoke) = rx.recv().await {
        // Persist first (durable source of truth), then fan out (best-effort live propagation).
        if let Err(e) = auth::persist_revocation(&pool, &revoke).await {
            tracing::warn!(error = %e, "failed to persist session revocation");
        }
        if let Err(e) = bus.publish_auth_revoke(revoke).await {
            tracing::warn!(error = %e, "failed to fan out session revocation to other cores");
        }
    }
}

/// Apply session revocations fanned out by other cores to this core's in-memory denylist so a token
/// revoked anywhere is denied here (Core HA active/active, ADR-016 Increment 2a). Runs on every core.
async fn run_auth_revoke_subscriber(bus: Arc<NatsBus>, sessions: Arc<SessionStore>) {
    let stream = match bus.subscribe_auth_revoke().await {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(error = %e, "auth-revoke subscribe failed; cross-core session revocation is DOWN");
            return;
        }
    };
    tokio::pin!(stream);
    while let Some(revoke) = stream.next().await {
        sessions.apply_remote_revoke(&revoke);
    }
}

/// Periodically drop expired denylist entries (in-memory) and expired rows (durable table) so both
/// stay bounded. Hourly is ample — entries live at most the token absolute TTL (24h).
async fn run_revocation_pruner(sessions: Arc<SessionStore>, pool: sqlx::PgPool) {
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(3600));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tick.tick().await;
        sessions.prune_denylist();
        if let Err(e) = auth::prune_revocations(&pool).await {
            tracing::debug!(error = %e, "session-revocation table prune failed");
        }
    }
}

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

/// Drain **backfilled** poll results (store-and-forward replay, Phase 3) and persist only their
/// metrics + interface metadata — **never** alert evaluation. A poller replays a partition's buffered
/// results here on reconnect; core imports them to the TSDB at their original `at_unix_ms` so history
/// fills at true time. Re-running the sample-count dwell machine over a stale burst would re-fire
/// resolved alerts, so the alert/notify/history path is deliberately skipped (the separate subject is
/// what routes them here — see [`yagra_bus::subjects::results_backfill`]). Leader-only, like the live
/// consumer (fan-out subscribe; only the leader ingests). Returns when the stream ends.
async fn consume_results_backfill<S>(
    mut results: S,
    metrics_tx: tokio::sync::mpsc::Sender<Arc<PollResult>>,
    meta_tx: tokio::sync::mpsc::Sender<MetaRecord>,
) where
    S: Stream<Item = PollResult> + Unpin,
{
    while let Some(result) = results.next().await {
        metrics::counter!("yagra_core_backfill_results_total").increment(1);
        persist_metrics_and_meta(&Arc::new(result), &metrics_tx, &meta_tx);
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
    metrics_tx: &tokio::sync::mpsc::Sender<Arc<PollResult>>,
    meta_tx: &tokio::sync::mpsc::Sender<MetaRecord>,
) {
    // Metrics → VM writer. Shed-able: alerts are computed in-memory and never read VM back, so a
    // dropped sample never loses an alert (best-effort observational tier, ADR-025).
    if !result.samples.is_empty() {
        match metrics_tx.try_send(Arc::clone(result)) {
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

/// Leader-only refresh loop: keep the alert engine's config fresh. Rebuild the config-derived base
/// only when the config generation changes (S6, [`config_gen`]); re-resolve time-dependent
/// maintenance windows each cycle; reload classifier + event rules so edits apply without a restart.
/// Runs until the shutdown token drops it. Spawned in `leader_work` (see `run_live`).
#[allow(clippy::too_many_arguments)]
async fn run_alert_config_refresh(
    alerts: Arc<AlertManager>,
    repo: Arc<NodeRepo>,
    thresholds: Arc<ThresholdStore>,
    maintenance: Arc<MaintenanceRepo>,
    group_repo: Arc<groups::GroupRepo>,
    classifier: Arc<classification::Classifier>,
    classification: Arc<classification::ClassificationRepo>,
    event_engine: Arc<events::EventEngine>,
    topo_sources: topology_projection::TopologySources,
) {
    // Cache the config-derived alert base keyed by the config generation, so the full node scan +
    // meta/topology rebuild runs only after an actual config change (S6). Maintenance windows are
    // time-dependent, so re-resolve them each cycle over the cached node list, and only swap the
    // live config when the base or the in-maintenance set actually changed.
    let mut cached_base: Option<(u64, AlertConfigBase)> = None;
    let mut last_maintenance: Option<std::collections::BTreeSet<NodeId>> = None;
    loop {
        tokio::time::sleep(Duration::from_secs(30)).await;
        let generation = config_gen::current();
        let base_changed = cached_base.as_ref().map(|(g, _)| *g) != Some(generation);
        if base_changed {
            cached_base = Some((
                generation,
                load_alert_config_base(&repo, &thresholds, &group_repo, &topo_sources).await,
            ));
        }
        let base = &cached_base.as_ref().expect("alert base set above").1;
        let in_maintenance =
            resolve_maintenance(&maintenance, &group_repo, &repo, &base.nodes).await;
        if base_changed || last_maintenance.as_ref() != Some(&in_maintenance) {
            let config = AlertConfig::new(base.rules.clone(), base.meta.clone())
                .with_topology(base.topology.clone())
                .with_maintenance(in_maintenance.clone())
                .with_pool_groups(base.pool_groups.clone());
            alerts.set_config(config);
            last_maintenance = Some(in_maintenance);
        }
        // Pick up classification-rule edits without a restart (also reloaded inline by the
        // rule-edit handlers; this catches any drift / multi-instance future).
        if let Err(e) = classifier.reload(&classification).await {
            tracing::warn!(error = %e, "failed to refresh classification rules");
        }
        // Event rules + node address map (also reloaded inline after rule edits).
        event_engine.reload(&repo).await;
    }
}

/// Leader-only loop: snapshot the node-state counts every few minutes into PostgreSQL so the
/// dashboard can chart "degrading vs recovering" over time, and prune old state snapshots + alert
/// history + events past the retention window (the only place these tables are trimmed). Runs until
/// the shutdown token drops it. Spawned in `leader_work` (see `run_live`).
/// Everything [`run_fleet_health_timeline`] snapshots or prunes.
///
/// A struct rather than a tenth parameter: `clippy::too_many_arguments` is a design signal, not a
/// lint to silence (coding-conventions), and this loop is where every PostgreSQL retention subject
/// bottoms out — so it grows every time ADR-040 gains a row.
struct TimelineSources {
    repo: Arc<NodeRepo>,
    alerts: Arc<AlertManager>,
    history: Arc<AlertHistoryStore>,
    events_repo: Arc<events::EventRepo>,
    dns_checks: Arc<dns_check::DnsCheckRepo>,
    neighbors: Arc<neighbors::NeighborRepo>,
    l3: Arc<l3::L3Repo>,
    analyses: Arc<analysis::AnalysisRepo>,
    rca_reports: Arc<rca::store::RcaRepo>,
    pollers: Arc<PollerRepo>,
}

async fn run_fleet_health_timeline(sources: TimelineSources) {
    let TimelineSources {
        repo,
        alerts,
        history,
        events_repo,
        dns_checks,
        neighbors,
        l3,
        analyses,
        rca_reports,
        pollers,
    } = sources;
    const SNAPSHOT_SECS: u64 = 300;
    loop {
        tokio::time::sleep(Duration::from_secs(SNAPSHOT_SECS)).await;
        // Re-read the operator's retention policy every tick (ADR-040), the same way the scheduler
        // re-reads the poll interval, so an edit in Settings applies on the next sweep without a
        // restart. A read failure degrades to the compiled defaults rather than skipping the prune.
        let retention = repo.get_retention_settings().await;
        let alert_linked_secs = retention.alert_linked_secs();
        let states = alerts.node_states();
        let mut counts: HashMap<String, i64> = HashMap::new();
        for s in states.values() {
            *counts.entry(s.as_str().to_owned()).or_insert(0) += 1;
        }
        let snapshot: Vec<(String, i64)> = counts.into_iter().collect();
        if let Err(e) = repo.insert_state_snapshot(&snapshot).await {
            tracing::warn!(error = %e, "node-state snapshot failed");
        }
        if let Err(e) = repo.prune_state_snapshots(alert_linked_secs).await {
            tracing::warn!(error = %e, "prune state snapshots failed");
        }
        if let Err(e) = history.prune_old(alert_linked_secs).await {
            tracing::warn!(error = %e, "prune alert history failed");
        }
        // Passive events in PostgreSQL: matched rows follow alert-history retention, unmatched
        // (rule-authoring material) get their own shorter window. When the log store is enabled
        // (ADR-024) unmatched rows never land in PostgreSQL, so this pruning naturally trims
        // PostgreSQL to the alert-linked subset; the log store keeps the full firehose.
        if let Err(e) = events_repo
            .prune_old(alert_linked_secs, retention.unmatched_event_secs())
            .await
        {
            tracing::warn!(error = %e, "prune events failed");
        }
        // DNS chain history is append-on-change, so a healthy fleet writes almost nothing here —
        // the canonicalization in `DnsChain::content_key` is exactly what keeps it that way. Prune
        // on the same window as alert history so the retention story stays consistent.
        if let Err(e) = dns_checks.prune_chain_changes(alert_linked_secs).await {
            tracing::warn!(error = %e, "prune dns chain history failed");
        }
        // Adjacency history is append-on-change for the same reason and on the same window
        // (`retention::Subject::NeighborChanges`): a rack nobody is repatching writes nothing.
        if let Err(e) = neighbors.prune_changes(alert_linked_secs).await {
            tracing::warn!(error = %e, "prune neighbour history failed");
        }
        // Interface-address history, same shape and same window (`retention::Subject::L3Changes`):
        // a network nobody is re-subnetting writes nothing.
        if let Err(e) = l3.prune_changes(alert_linked_secs).await {
            tracing::warn!(error = %e, "prune interface-address history failed");
        }
        // Monitoring gaps go on the alert-linked window too (`retention::Subject::MonitoringGaps`)
        // — a gap explains an absence of alerts, so outliving the alert history would leave a
        // window nothing can be read against.
        if let Err(e) = pollers.prune_monitoring_gaps(alert_linked_secs).await {
            tracing::warn!(error = %e, "prune monitoring gaps failed");
        }
        // Diagnostic artefacts get their own window (`retention::Subject::AnalysisRuns` /
        // `RcaReports`): both are reproducible by asking again, unlike everything above. Analysis
        // findings need no prune of their own — they cascade from the job.
        let diagnostic_secs = retention.diagnostic_secs();
        if let Err(e) = analyses.prune_jobs(diagnostic_secs).await {
            tracing::warn!(error = %e, "prune analysis runs failed");
        }
        if let Err(e) = rca_reports.prune_reports(diagnostic_secs).await {
            tracing::warn!(error = %e, "prune RCA reports failed");
        }
    }
}

/// How often the derived connectivity graph is recomputed (ADR-043).
///
/// Slow on purpose: its inputs are walked hourly, so a tighter cycle would re-derive an unchanged
/// graph. The watermark check below usually makes even this a no-op.
const TOPO_DERIVE_INTERVAL_SECS: u64 = 300;

/// Leader-only loop: recompute the derived connectivity graph from the L2 and L3 observations
/// (ADR-043).
///
/// **Leader-only** because it is a whole-fleet read followed by a whole-table write; two cores doing
/// it concurrently would be pure waste (the upsert is idempotent, so it would not corrupt anything —
/// it would just double the load).
///
/// **Skipped when nothing moved.** The trigger is a watermark over the *observations* plus
/// `config_gen` (ADR-026), and it needs both: `config_gen` moves when an operator edits
/// configuration, which is exactly what a poll does not do — gating on it alone would leave the map
/// frozen while the network changed underneath it. Gating on the watermark alone would miss a node
/// being deleted or re-addressed by hand.
async fn run_topology_derivation(
    repo: Arc<NodeRepo>,
    l3: Arc<l3::L3Repo>,
    neighbors: Arc<neighbors::NeighborRepo>,
    routing: Arc<routing::RoutingRepo>,
    links: Arc<topology_links::TopoLinkRepo>,
    overrides: Arc<link_overrides::LinkOverrideRepo>,
) {
    type Watermark = Option<chrono::DateTime<chrono::Utc>>;
    let mut last_signal: Option<(u64, Watermark, Watermark, Watermark)> = None;
    loop {
        tokio::time::sleep(Duration::from_secs(TOPO_DERIVE_INTERVAL_SECS)).await;

        let l3_mark = l3.observation_watermark().await.unwrap_or(None);
        let nb_mark = neighbors.observation_watermark().await.unwrap_or(None);
        // The third watermark, added with Increment 4: a point-to-point link appearing changes no
        // address and no CDP/LLDP row, so without this the map would not redraw for it.
        let rt_mark = routing.observation_watermark().await.unwrap_or(None);
        let signal = (config_gen::current(), l3_mark, nb_mark, rt_mark);
        if last_signal.as_ref() == Some(&signal) {
            metrics::counter!("yagra_topology_derive_skipped_total").increment(1);
            continue;
        }

        let started = std::time::Instant::now();
        let nodes = repo.list_nodes().await.unwrap_or_default();
        let inventory: Vec<(yagra_common::NodeId, std::net::IpAddr)> =
            nodes.iter().map(|n| (n.id, n.address)).collect();
        let l3_rows = match l3.all_current().await {
            Ok(rows) => rows,
            Err(e) => {
                tracing::warn!(error = %e, "topology derivation: reading interface addresses failed");
                continue;
            }
        };
        let nb_rows = match neighbors.all_current().await {
            Ok(rows) => rows,
            Err(e) => {
                tracing::warn!(error = %e, "topology derivation: reading adjacency failed");
                continue;
            }
        };
        let rt_rows = match routing.all_current().await {
            Ok(rows) => rows,
            Err(e) => {
                tracing::warn!(error = %e, "topology derivation: reading routing adjacency failed");
                continue;
            }
        };

        // An override read that fails degrades to *no* overrides rather than skipping the cycle:
        // the derivation's own output is still correct, it just lacks the operator's corrections
        // for one cycle. Skipping instead would freeze the whole map on a transient.
        let ovr = overrides.all().await.unwrap_or_else(|e| {
            tracing::warn!(error = %e, "topology derivation: reading link overrides failed");
            Vec::new()
        });

        let out = yagra_topology::derive::derive_links(yagra_topology::derive::DeriveInput {
            nodes: &inventory,
            l3: &l3_rows,
            neighbors: &nb_rows,
            routing: &rt_rows,
            overrides: &ovr,
        });

        if let Err(e) = links.upsert_batch(&out.links).await {
            tracing::warn!(error = %e, "topology derivation: writing links failed");
            continue;
        }
        // Only prune once the write succeeded, so a failed cycle never deletes a live graph.
        match links
            .prune_stale(i64::try_from(TOPO_DERIVE_INTERVAL_SECS).unwrap_or(i64::MAX))
            .await
        {
            Ok(n) if n > 0 => tracing::info!(removed = n, "pruned stale topology links"),
            Ok(_) => {}
            Err(e) => tracing::warn!(error = %e, "topology derivation: pruning stale links failed"),
        }
        if let Err(e) = links.record_run(&out.summary, out.links.len()).await {
            tracing::warn!(error = %e, "topology derivation: recording the run failed");
        }

        last_signal = Some(signal);
        metrics::gauge!("yagra_topology_links_total").set(out.links.len() as f64);
        metrics::histogram!("yagra_topology_derive_seconds")
            .record(started.elapsed().as_secs_f64());
        tracing::debug!(
            links = out.links.len(),
            unmatched_lldp = out.summary.unmatched_lldp_rows,
            oversized_segments = out.summary.oversized_segments,
            "derived the connectivity graph"
        );
    }
}

/// Leader-only loop: turn the fleet's ARP observations into the discovered-endpoint table
/// (ADR-043 Increment 3).
///
/// **Leader-only** for the same reason the derivation is: a whole-fleet read followed by a
/// whole-table write, idempotent but pure waste if two cores do it.
///
/// **Free when nobody opted in.** ARP discovery ships off, so the usual state of this loop is "no
/// observation watermark ⇒ return before reading anything". That ordering is the point: the
/// inventory read and the address projection below are the expensive part, and a deployment that
/// never enabled the walk must not pay for them every five minutes.
///
/// **The trigger is the watermark alone**, unlike the derivation, which also watches `config_gen`.
/// Both were considered; `config_gen` would fire this on every unrelated configuration edit, and the
/// thing it would catch — a node created by hand, so that an endpoint is no longer unmonitored — is
/// already handled by [`arp::DiscoveredRepo::reconcile_promotions`], which the sweep runs every pass
/// and the import handler runs immediately.
async fn run_endpoint_discovery(arp: Arc<arp::ArpRepo>, discovered: Arc<arp::DiscoveredRepo>) {
    let mut last_mark: Option<chrono::DateTime<chrono::Utc>> = None;
    loop {
        tokio::time::sleep(Duration::from_secs(arp::ENDPOINT_SWEEP_INTERVAL_SECS)).await;

        let Ok(Some(mark)) = arp.observation_watermark().await else {
            continue;
        };
        // `reconcile_promotions` still runs on an unchanged watermark: a node added by hand does
        // not move it, and an endpoint that quietly became monitored must stop being listed as
        // unmonitored without waiting for the next ARP walk.
        if let Err(e) = discovered.reconcile_promotions().await {
            tracing::warn!(error = %e, "endpoint discovery: reconciling promotions failed");
        }
        if last_mark == Some(mark) {
            metrics::counter!("yagra_endpoint_sweep_skipped_total").increment(1);
            continue;
        }

        let started = std::time::Instant::now();
        let summaries = match arp.all_current().await {
            Ok(rows) => rows,
            Err(e) => {
                tracing::warn!(error = %e, "endpoint discovery: reading ARP observations failed");
                continue;
            }
        };
        // A failed address read must **skip the cycle**, never fall back to an empty set: an empty
        // "known" set means every monitored device's own addresses are reported as unmonitored
        // endpoints, which is a wrong answer written to a table an operator then reviews.
        let known = match discovered.known_addresses().await {
            Ok(set) => set,
            Err(e) => {
                tracing::warn!(error = %e, "endpoint discovery: reading known addresses failed");
                continue;
            }
        };

        let found = arp::unmonitored(&summaries, &known);
        if let Err(e) = discovered.upsert_batch(&found).await {
            tracing::warn!(error = %e, "endpoint discovery: writing endpoints failed");
            continue;
        }
        // Only prune once the write succeeded, so a failed cycle never ages out a live table.
        match discovered
            .prune(
                arp::DISCOVERED_RETENTION_SECS,
                arp::MAX_DISCOVERED_ENDPOINTS,
            )
            .await
        {
            Ok(n) if n > 0 => tracing::info!(removed = n, "pruned discovered endpoints"),
            Ok(_) => {}
            Err(e) => tracing::warn!(error = %e, "endpoint discovery: pruning failed"),
        }

        last_mark = Some(mark);
        metrics::gauge!("yagra_discovered_endpoints_total").set(found.len() as f64);
        metrics::histogram!("yagra_endpoint_sweep_seconds").record(started.elapsed().as_secs_f64());
        tracing::debug!(
            endpoints = found.len(),
            nodes = summaries.len(),
            "swept the fleet's ARP observations for unmonitored endpoints"
        );
    }
}

/// Leader-only loop: reload notification routing (DB channels/rules) + mutes into the notifier every
/// 30s so edits take effect without a restart (env channels stay always-on; expired mutes drop out).
/// Runs until the shutdown token drops it. Spawned in `leader_work` (see `run_live`).
async fn run_routing_refresh(
    notifier: Arc<Notifier>,
    notifications: Arc<NotificationRepo>,
    maintenance: Arc<MaintenanceRepo>,
    repo: Arc<NodeRepo>,
    group_repo: Arc<groups::GroupRepo>,
) {
    loop {
        tokio::time::sleep(Duration::from_secs(30)).await;
        load_routing(&notifier, &notifications).await;
        load_mutes(&notifier, &maintenance, &repo, &group_repo).await;
    }
}

/// Leader-only loop: alert when a poller pool has nodes but no live poller (ADR-009's blind spot).
///
/// **Leader-only is a correctness requirement, not an optimization.** A standby core runs no
/// coordinator — `run_heartbeat_consumer` is spawned here in `leader_work` — so its registry is
/// permanently empty and it would read *every* pool as dark and page for the whole fleet. This is
/// the same hazard `api/pollers.rs::resolve_polled_by` guards with an `is_leader` check.
///
/// **The node scan is generation-gated** (ADR-026, the idiom `SweepCache` already uses): pool
/// membership is config-derived and `audit_mw` bumps `config_gen` on every config mutation, so an
/// unchanged generation means the counts are identical and a steady-state tick costs one in-memory
/// registry read. Without that this would scan the node table every 30 seconds, which at 50,000
/// nodes is not affordable.
async fn run_pool_coverage_watch(
    coordinator: Arc<Coordinator>,
    repo: Arc<NodeRepo>,
    meraki: Arc<meraki::MerakiDeviceRepo>,
    groups: Arc<groups::GroupRepo>,
    alerts: Arc<AlertManager>,
    notifier: Arc<Notifier>,
    history: Arc<AlertHistoryStore>,
) {
    let mut watch = pool_coverage::CoverageWatch::from_env();
    if watch.disabled() {
        tracing::info!(
            env = pool_coverage::RAISE_AFTER_ENV,
            "poller-pool coverage notifications are disabled; gauges still published"
        );
    }
    let mut cached: Option<(u64, HashMap<String, usize>)> = None;
    loop {
        tokio::time::sleep(pool_coverage::WATCH_TICK).await;

        let generation = config_gen::current();
        if cached.as_ref().is_none_or(|(gen, _)| *gen != generation) {
            let counts = pool_coverage::node_counts_by_pool(&repo, &meraki, &groups).await;
            // An empty map means "no pool has any nodes", which silently disables the whole check.
            // `node_counts_by_pool` already degrades on a read error, so only a genuinely empty
            // inventory produces one legitimately — keep the previous answer when we had one
            // rather than letting a bad read look like a healthy fleet.
            if counts.is_empty() && cached.is_some() {
                tracing::warn!("pool coverage: node counts came back empty; keeping the last set");
            } else {
                cached = Some((generation, counts));
            }
        }
        let Some((_, node_pools)) = cached.as_ref() else {
            continue;
        };

        let coverage =
            pool_coverage::coverage(&coordinator.poller_views(Instant::now()), node_pools);
        pool_coverage::publish_gauges(&coverage);

        for event in watch.observe(&coverage, Instant::now()) {
            let action = match event {
                pool_coverage::CoverageEvent::Raise { pool, nodes } => {
                    tracing::warn!(
                        pool = %pool,
                        nodes,
                        "poller pool has nodes but no live poller — they are not being monitored"
                    );
                    pool_coverage::count_notification("raise");
                    alerts.raise_pool_coverage_alert(&pool, pool_coverage::now_unix_ms())
                }
                pool_coverage::CoverageEvent::Clear { pool } => {
                    tracing::info!(pool = %pool, "poller pool has a live poller again");
                    pool_coverage::count_notification("clear");
                    alerts.resolve_pool_coverage_alert(&pool)
                }
            };
            let Some(action) = action else { continue };
            // Persist before notifying, and write **inline** rather than through the batch
            // channel: that channel exists to keep the poll-result hot path off the database, and
            // this loop is a 30-second tick that emits at most one row per pool per debounce. A
            // failure here is logged and the notification still goes out — an operator being paged
            // matters more than the row, and the row is what History reads afterwards.
            if let Some(alert) = coverage_alert_of(&action) {
                let resolved = matches!(action, crate::alerts::NotifyAction::Resolve(_));
                if let Err(e) = history.record(alert, resolved).await {
                    tracing::warn!(error = %e, "recording a pool-coverage transition failed");
                }
            }
            notifier.handle(action).await;
        }
    }
}

/// The alert inside a coverage action, for the history write.
///
/// `Suppress` is unreachable here — dependency suppression is a property of the node graph and a
/// pool is not in it — but it is spelled out rather than caught by a wildcard so a new action
/// variant has to decide what History should do with it.
fn coverage_alert_of(action: &crate::alerts::NotifyAction) -> Option<&Alert> {
    match action {
        crate::alerts::NotifyAction::Fire(a) | crate::alerts::NotifyAction::Resolve(a) => Some(a),
        crate::alerts::NotifyAction::Suppress(_) => None,
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
    metrics_tx: &tokio::sync::mpsc::Sender<Arc<PollResult>>,
    meta_tx: &tokio::sync::mpsc::Sender<MetaRecord>,
    history_tx: &tokio::sync::mpsc::Sender<HistoryRecord>,
    history: &Arc<AlertHistoryStore>,
    stats: &Arc<scheduler::SchedulerStats>,
    meraki_inflight: &Arc<meraki::MerakiInflight>,
    coordinator: &Arc<Coordinator>,
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

    // Metrics → VM writer + interface/identity metadata → PG writer. Shed-able/self-healing and
    // alert-independent, so it's shared with the backfill path (`consume_results_backfill`).
    persist_metrics_and_meta(&result, metrics_tx, meta_tx);

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

/// Max flow rows buffered before a forced ClickHouse insert (bounds memory between flush ticks).
const FLOW_INSERT_MAX_ROWS: usize = 10_000;
/// How often the flow writer flushes accumulated rows to ClickHouse.
const FLOW_INSERT_FLUSH_SECS: u64 = 5;
/// How often the flow consumer refreshes its exporter-IP → node-id snapshot.
const FLOW_ADDR_REFRESH_SECS: u64 = 60;
/// Cap on the flow consumer's "already-missed" exporter set (throttles miss-triggered addr-map
/// reloads to once per distinct exporter). Bounded well above any realistic exporter count; a
/// pathological flood of distinct source IPs clears it, re-arming the periodic refresh as the
/// backstop rather than growing memory unbounded.
const FLOW_MISS_CACHE_MAX: usize = 65_536;
/// Bounded hand-off queue between the flow consumer (bus → rows) and the ClickHouse writer. A full
/// queue means the writer is behind a slow/hung ClickHouse; the consumer then drops + counts rows
/// (`channel_full`) instead of stalling the `yagra.flows` subscription into a silent NATS drop (S27).
const FLOW_PERSIST_CHANNEL_CAP: usize = 16_384;

/// Ensure the ClickHouse flow schema exists, with a short retry to tolerate ClickHouse coming up
/// after core (compose gates on `service_started`, not health). Best-effort: after the retries it
/// logs and moves on — inserts will keep failing (dropped, loss-tolerant tier) until ClickHouse is
/// reachable, at which point they succeed against the now-present tables.
async fn ensure_flow_schema(store: &Arc<dyn FlowStore>) {
    for attempt in 1..=5u32 {
        match store.ensure_schema().await {
            Ok(()) => return,
            Err(e) => {
                tracing::warn!(attempt, error = %e, "ClickHouse flow schema ensure failed; retrying");
                tokio::time::sleep(Duration::from_secs(3)).await;
            }
        }
    }
    tracing::error!(
        "could not ensure ClickHouse flow schema after retries — flow inserts will fail until reachable"
    );
}

/// Build the ClickHouse rows for one edge-aggregated flow batch: resolve the exporter to a node and
/// fill in only the AS numbers the exporter didn't provide from the offline IP→ASN table (the
/// exporter's own BGP view is authoritative — ADR-031). Returns `None` when the exporter isn't
/// mapped to a node (the batch is dropped by the caller). Pure — unit-tested.
fn flow_rows_from_batch(
    batch: &FlowBatch,
    addr_map: &HashMap<std::net::IpAddr, Uuid>,
    ipasn: Option<&Arc<crate::ipasn::IpAsnDb>>,
) -> Option<Vec<FlowRow>> {
    let node_id = addr_map.get(&batch.exporter_ip).copied()?;
    let mut rows = Vec::with_capacity(batch.records.len());
    for rec in &batch.records {
        let mut src_as = rec.src_as;
        let mut dst_as = rec.dst_as;
        if let Some(db) = ipasn {
            if src_as == 0 {
                src_as = db.lookup(rec.src_ip).unwrap_or(0);
            }
            if dst_as == 0 {
                dst_as = db.lookup(rec.dst_ip).unwrap_or(0);
            }
        }
        rows.push(FlowRow {
            node_id,
            ts_unix_ms: batch.bucket_start_ms,
            exporter_ip: batch.exporter_ip,
            if_index: rec.if_index,
            src_ip: rec.src_ip,
            dst_ip: rec.dst_ip,
            src_port: rec.src_port,
            dst_port: rec.dst_port,
            proto: rec.proto,
            tos: rec.tos,
            src_as,
            dst_as,
            bytes: rec.bytes,
            packets: rec.packets,
            flows: rec.flows,
        });
    }
    Some(rows)
}

/// Hand rows to the flow writer without ever awaiting ClickHouse (ADR-024/025 match/persist split).
/// A full queue means the writer is behind a slow/hung ClickHouse: the row is dropped and counted
/// (`channel_full`) so the `yagra.flows` subscription keeps draining — turning what used to be an
/// invisible NATS slow-consumer drop into a measured one (S27). Returns the number of rows dropped.
fn send_flow_rows(tx: &tokio::sync::mpsc::Sender<FlowRow>, rows: Vec<FlowRow>) -> u64 {
    let mut dropped = 0u64;
    for row in rows {
        match tx.try_send(row) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => dropped += 1,
            // Writer gone (shutdown): stop — teardown does the final flush of what's already queued.
            Err(TrySendError::Closed(_)) => break,
        }
    }
    if dropped > 0 {
        metrics::counter!("yagra_flow_rows_dropped_total", "reason" => "channel_full")
            .increment(dropped);
    }
    dropped
}

/// Decide whether a flow batch from `exporter_ip` should trigger an out-of-band address-map reload.
/// Returns `true` only for the *first* miss of each distinct exporter (recording it in `missed`), so
/// a steady stream of batches from an unregistered/never-mapped exporter — the normal case, since
/// routers often export from a loopback that differs from their configured management address —
/// cannot spin a full-table `SELECT` + map rebuild on the flow-ingest hot path once per batch. A
/// genuinely just-added node is still picked up: its first miss reloads immediately, and thereafter
/// it is present in the map; anything still unmapped is caught by the periodic refresh (S27 follow-up).
fn should_reload_on_miss(
    addr_map: &HashMap<std::net::IpAddr, Uuid>,
    missed: &mut std::collections::HashSet<std::net::IpAddr>,
    exporter_ip: std::net::IpAddr,
) -> bool {
    !addr_map.contains_key(&exporter_ip) && missed.insert(exporter_ip)
}

/// Consume verbatim flow datagrams from `yagra.flows.raw` and tee them to the forwarder (ADR-034
/// Increment 2). Deliberately does nothing else: these datagrams exist only so a forwarding
/// destination can be given what the exporter actually sent. ClickHouse is fed by the aggregate
/// stream above, and duplicating that here would double-count every flow.
async fn consume_raw_flows<S>(mut stream: S, forward: crate::forward::ForwardHandle)
where
    S: Stream<Item = yagra_bus::RawFlowDatagram> + Unpin,
{
    while let Some(datagram) = stream.next().await {
        // Never blocks: with no flow destination this is one relaxed atomic load, and a full inlet
        // drops and counts rather than back-pressuring the subscription into a NATS slow-consumer.
        forward.offer_flow(&datagram);
    }
}

/// Consume edge-aggregated flow batches from the bus, resolve each exporter to a node (via the same
/// address map the event pipeline uses), enrich AS numbers, and hand the rows to the ClickHouse
/// writer over a bounded queue. Never awaits ClickHouse, so a slow/hung ClickHouse can't stall the
/// `yagra.flows` subscription into a silent NATS slow-consumer drop (ADR-024/025 match/persist split,
/// S27). Spawned via `spawn_cancellable` — the writer owns the final flush on shutdown.
async fn consume_flows<S>(
    mut flows: S,
    tx: tokio::sync::mpsc::Sender<FlowRow>,
    repo: Arc<NodeRepo>,
    ipasn: crate::ipasn::IpAsnHandle,
) where
    S: Stream<Item = FlowBatch> + Unpin,
{
    let mut addr_map = repo.address_map().await.unwrap_or_default();
    let mut last_refresh = Instant::now();
    // Exporters we've already tried (and failed) to resolve since startup — throttles the
    // miss-triggered reload below to once per distinct exporter (see `should_reload_on_miss`).
    let mut missed: std::collections::HashSet<std::net::IpAddr> = std::collections::HashSet::new();
    while let Some(batch) = flows.next().await {
        // Refresh the exporter→node snapshot periodically (nodes are added/removed at runtime).
        if last_refresh.elapsed() >= Duration::from_secs(FLOW_ADDR_REFRESH_SECS) {
            if let Ok(m) = repo.address_map().await {
                addr_map = m;
            }
            last_refresh = Instant::now();
        }
        // On a mapping miss, reload once more in case the exporter's node was just added — but only
        // the first time we see each exporter, so a never-registered exporter can't reload per batch.
        if missed.len() >= FLOW_MISS_CACHE_MAX {
            missed.clear();
        }
        if should_reload_on_miss(&addr_map, &mut missed, batch.exporter_ip) {
            if let Ok(m) = repo.address_map().await {
                addr_map = m;
                last_refresh = Instant::now();
            }
        }
        // Snapshot the hot-swappable IP→ASN table once per batch (not per record).
        let ipasn_now = ipasn.read().unwrap().clone();
        let Some(rows) = flow_rows_from_batch(&batch, &addr_map, ipasn_now.as_ref()) else {
            metrics::counter!("yagra_flow_batches_unmapped_total").increment(1);
            tracing::debug!(exporter = %batch.exporter_ip, "flow batch from unmapped exporter — dropped");
            continue;
        };
        send_flow_rows(&tx, rows);
    }
}

/// Async ClickHouse flow writer (ADR-031): drains the bounded flow queue, batches rows across
/// consumed items, and bulk-inserts on a size (`FLOW_INSERT_MAX_ROWS`) or time
/// (`FLOW_INSERT_FLUSH_SECS`) trigger. Best-effort/loss-tolerant (ADR-017): an insert failure drops
/// the batch and counts it (unlike the metrics path, which spills). Takes the shutdown token
/// directly (not `spawn_cancellable`) so it can do a best-effort final flush on cancel.
async fn run_flow_writer(
    mut rx: tokio::sync::mpsc::Receiver<FlowRow>,
    store: Arc<dyn FlowStore>,
    shutdown: CancellationToken,
) {
    let mut buf: Vec<FlowRow> = Vec::new();
    let mut ticker = tokio::time::interval(Duration::from_secs(FLOW_INSERT_FLUSH_SECS));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            biased;
            () = shutdown.cancelled() => {
                while let Ok(r) = rx.try_recv() {
                    buf.push(r);
                    if buf.len() >= FLOW_INSERT_MAX_ROWS {
                        flush_flow(&store, &mut buf).await;
                    }
                }
                flush_flow(&store, &mut buf).await;
                break;
            }
            _ = ticker.tick() => {
                flush_flow(&store, &mut buf).await;
            }
            first = rx.recv() => {
                match first {
                    None => {
                        flush_flow(&store, &mut buf).await;
                        break;
                    }
                    Some(r) => {
                        buf.push(r);
                        while buf.len() < FLOW_INSERT_MAX_ROWS {
                            match rx.try_recv() {
                                Ok(r) => buf.push(r),
                                Err(_) => break,
                            }
                        }
                        if buf.len() >= FLOW_INSERT_MAX_ROWS {
                            flush_flow(&store, &mut buf).await;
                        }
                        metrics::gauge!("yagra_persist_queue_depth", "stream" => "flow")
                            .set(rx.len() as f64);
                    }
                }
            }
        }
    }
}

/// Insert one buffered batch of flow rows; on failure the batch is dropped and counted (flow is a
/// loss-tolerant tier, unlike the metrics path which spills — ADR-017).
async fn flush_flow(store: &Arc<dyn FlowStore>, buf: &mut Vec<FlowRow>) {
    if buf.is_empty() {
        return;
    }
    let rows = std::mem::take(buf);
    let n = rows.len() as u64;
    match store.insert_batch(&rows).await {
        Ok(()) => metrics::counter!("yagra_flow_rows_written_total").increment(n),
        Err(e) => {
            metrics::counter!("yagra_flow_rows_dropped_total", "reason" => "insert_error")
                .increment(n);
            tracing::warn!(error = %e, rows = n, "ClickHouse flow insert failed — batch dropped (loss-tolerant tier)");
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
/// Where a batch of result metadata lands. One struct rather than four parameters: these are not
/// independent knobs, they are "the stores the metadata tier writes to", and every one of them is
/// needed by both `run_pg_writer` and `flush_meta`.
#[derive(Clone)]
struct MetaStores {
    repo: Arc<NodeRepo>,
    dns: Arc<dns_check::DnsCheckRepo>,
    neighbors: Arc<neighbors::NeighborRepo>,
    l3: Arc<l3::L3Repo>,
    arp: Arc<arp::ArpRepo>,
    routing: Arc<routing::RoutingRepo>,
}

async fn run_pg_writer(
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
        for (ifindex, name, alias, speed) in rec.interfaces {
            iface_rows.push((rec.node_id, ifindex, name, alias, speed));
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

/// Group the round's nodes by the pool that should poll them.
///
/// The pool is the node's **effective** one (own > ancestor folder > default, [`poolres`]), so a
/// folder-level assignment routes its whole subtree. Kept a separate pure function so that
/// resolution is unit-testable without a scheduler loop.
///
/// The map is also **seeded with every live pool** before the node loop. Without that, a pool whose
/// last node moved away simply vanishes from the map and is never reconciled again — its poller
/// keeps polling the stale working set for the life of the core process, double-polling nodes that
/// have since moved elsewhere. `reconcile_pool` with an empty desired set publishes one empty
/// snapshot and is idempotent afterwards, so seeding costs nothing in steady state.
///
/// Meraki device nodes are dropped: core's org collector owns them, not a pool poller.
fn group_by_pool(
    resolved: Vec<(yagra_common::Node, u32)>,
    meraki_node_ids: &std::collections::HashSet<Uuid>,
    live: &std::collections::HashSet<String>,
    resolver: &poolres::PoolResolver,
) -> HashMap<String, Vec<(yagra_common::Node, u32)>> {
    let mut groups: HashMap<String, Vec<(yagra_common::Node, u32)>> = HashMap::new();
    for pool in live {
        groups.entry(pool.clone()).or_default();
    }
    for (node, secs) in resolved {
        if meraki_node_ids.contains(&node.id.as_uuid()) {
            continue;
        }
        let pool = resolver.resolve_pool(&node).to_owned();
        groups.entry(pool).or_default().push((node, secs));
    }
    groups
}

async fn run_scheduler(
    repo: Arc<NodeRepo>,
    groups_repo: Arc<groups::GroupRepo>,
    dispatcher: Arc<scheduler::PollDispatcher>,
    stats: Arc<scheduler::SchedulerStats>,
    meraki_devices: Arc<meraki::MerakiDeviceRepo>,
    coordinator: Arc<Coordinator>,
) {
    use std::collections::HashSet;
    use std::time::Instant;
    let mut last_dispatched: HashMap<Uuid, Instant> = HashMap::new();
    // Legacy-mode cadence for jobs that run slower than their node's interval, keyed by
    // (node, job kind). Only the neighbour walk is in here today; it is keyed by kind rather than
    // special-cased so a second slow job needs no new bookkeeping.
    let mut last_slow: HashMap<(Uuid, &'static str), Instant> = HashMap::new();
    let mut cache: Option<SweepCache> = None;
    // Last successfully-built folder-pool resolver. A transient DB error must NOT degrade to "no
    // inheritance": that would silently move every folder-assigned node to the default pool for one
    // round, churning both pools' working sets. Reusing the last-known map is the safe failure.
    let mut resolver: Option<poolres::PoolResolver> = None;
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
                // Wake early if a poller announced it is leaving: the ring changed, so the desired
                // set must be re-pushed now rather than after a full poll interval.
                tokio::select! {
                    () = tokio::time::sleep(Duration::from_secs(u64::from(sleep_secs))) => {}
                    () = coordinator.sweep_nudged() => {}
                }
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
        // Adjacency policy (ADR-038): read once per rebuild, exactly like the intervals above, so
        // no per-node settings query enters the sweep. Degrades to the compiled default.
        //
        // Resolved through the dispatcher rather than straight off `repo`, because it also carries
        // the route-probe plan (ADR-043 Increment 4), which the dispatcher caches on its own TTL —
        // rebuilding that per sweep would mean a JSONB scan of `node_l3` every round.
        let neighbors = dispatcher.adjacency_policy().await;
        // Folder-pool inheritance (ADR-009/020). One small query per rebuild — never on the cached
        // fast path above, which is already generation-keyed.
        match groups_repo.pool_rows().await {
            Ok(rows) => resolver = Some(poolres::PoolResolver::build(rows)),
            Err(e) => {
                let Some(_) = resolver.as_ref() else {
                    tracing::error!(
                        error = %e,
                        "scheduler: loading folder pools failed and none is cached — skipping the round \
                         rather than routing the fleet to the wrong pool"
                    );
                    // Wake early if a poller announced it is leaving: the ring changed, so the desired
                    // set must be re-pushed now rather than after a full poll interval.
                    tokio::select! {
                        () = tokio::time::sleep(Duration::from_secs(u64::from(default_secs))) => {}
                        () = coordinator.sweep_nudged() => {}
                    }
                    continue;
                };
                tracing::warn!(error = %e, "scheduler: loading folder pools failed; reusing the last-known map");
            }
        }
        let pool_resolver = resolver
            .clone()
            .unwrap_or_else(poolres::PoolResolver::empty);
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

                // Group the non-Meraki nodes by their effective pool so each pool's mode is decided
                // once — and seed every live pool so one that has lost all its nodes still gets
                // reconciled (see `group_by_pool`).
                let groups = group_by_pool(resolved, &meraki_node_ids, &live, &pool_resolver);

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

                // URL and DNS monitors each live in their own 1:1 side table, so resolving a node's
                // kind means a query per table. Preload both id sets once per sweep and let the
                // dispatcher skip the query for every node that isn't one — the same reason Meraki
                // ids are preloaded above. Without this the sweep pays one extra round trip per
                // node per table per round at fleet scale.
                let monitor_ids = Arc::new((
                    dispatcher.url_node_ids().await,
                    dispatcher.dns_node_ids().await,
                ));

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
                        let desired: HashMap<_, _> = futures::stream::iter(members)
                            .map(|(node, secs)| {
                                let dispatcher = dispatcher.clone();
                                let monitor_ids = monitor_ids.clone();
                                // Cheap: scalars plus one `Arc` to the shared route-probe plan.
                                let neighbors = neighbors.clone();
                                async move {
                                    let (url_ids, dns_ids) = monitor_ids.as_ref();
                                    let specs = dispatcher
                                        .build_node_specs(
                                            &node,
                                            secs,
                                            scheduler::MonitorHints {
                                                url: Some(url_ids),
                                                dns: Some(dns_ids),
                                            },
                                            &neighbors,
                                        )
                                        .await;
                                    (node.id, specs)
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
                            for (job, kind) in dispatcher
                                .build_scheduled_jobs_hinted(
                                    node,
                                    *secs,
                                    scheduler::MonitorHints {
                                        url: Some(&monitor_ids.0),
                                        dns: Some(&monitor_ids.1),
                                    },
                                    &neighbors,
                                )
                                .await
                            {
                                // A job whose own cadence is slower than the node's (today: the
                                // neighbour walk) gets its own due-check. Working-set mode needs
                                // nothing here — the poller schedules each spec by its own
                                // `interval_secs` — but this path publishes on the *node's* tick,
                                // so without the gate an hourly walk would go out every minute.
                                // Existing jobs all carry `*secs`, so they never enter this branch.
                                if job.interval_secs > *secs {
                                    let key = (id, kind);
                                    let elapsed =
                                        last_slow.get(&key).map(|&t| now.duration_since(t));
                                    let cadence = Duration::from_secs(u64::from(job.interval_secs));
                                    if !scheduler::due(elapsed, cadence) {
                                        continue;
                                    }
                                    last_slow.insert(key, now);
                                }
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
                last_slow.retain(|(id, _), _| present.contains(id));
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
        // Wake early if a poller announced it is leaving: the ring changed, so the desired
        // set must be re-pushed now rather than after a full poll interval.
        tokio::select! {
            () = tokio::time::sleep(Duration::from_secs(u64::from(min_interval))) => {}
            () = coordinator.sweep_nudged() => {}
        }
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
async fn run_report_scheduler(reports: Arc<reports::ReportRunner>, settings: Arc<NodeRepo>) {
    use chrono::Utc;
    const TICK_SECS: u64 = 60;
    // Prune ~hourly (every 60 ticks) rather than every minute.
    let mut tick: u64 = 0;
    let repo = reports.repo();
    loop {
        tokio::time::sleep(Duration::from_secs(TICK_SECS)).await;
        tick = tick.wrapping_add(1);
        match repo.due_schedules().await {
            Ok(due) => {
                for sched in due {
                    let next = cadence::compute_next_run(
                        cadence::Schedule {
                            frequency: sched.frequency,
                            day_of_week: sched.day_of_week,
                            day_of_month: sched.day_of_month,
                            at_hour: sched.at_hour,
                            at_minute: sched.at_minute,
                        },
                        Utc::now(),
                    );
                    let status = match reports
                        .run_now(
                            sched.definition_id,
                            reports::ReportRunTrigger::Scheduled,
                            None,
                        )
                        .await
                    {
                        Ok(Some(_)) => reports::ReportScheduleStatus::Queued,
                        Ok(None) => reports::ReportScheduleStatus::MissingDefinition,
                        Err(e) => {
                            tracing::warn!(error = %e, schedule = %sched.id, "scheduled report failed to start");
                            reports::ReportScheduleStatus::Error
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
            // Report runs have their own window (ADR-040): the artefacts are regenerable from their
            // definition, so lowering this must not be able to touch alert history. `settings` is
            // the NodeRepo purely as the app_settings reader — `repo` above is the report store.
            let secs = settings.get_retention_settings().await.report_run_secs();
            if let Err(e) = repo.prune_runs(secs).await {
                tracing::warn!(error = %e, "prune report runs failed");
            }
        }
    }
}

/// Fire due analysis schedules on a fixed tick.
///
/// The same shape as [`run_report_scheduler`] and the same cadence maths, with one difference that
/// is the whole reason it is a separate loop: **the analysis runner has admission control.**
/// `create` can refuse with `TooManyConcurrent` / `RateLimited`, which are transient. Treating a
/// refusal like a fire — stamping a status and advancing `next_run_at` — would skip the whole
/// period, so a daily 03:00 analysis that happened to collide with a busy minute would simply not
/// run that day, with a `last_status` nobody reads and an empty runs list that looks normal.
/// A refused schedule is therefore left due, and the next tick retries it a minute later.
///
/// Failures degrade to a warn so one bad schedule never stalls the others.
async fn run_analysis_scheduler(analysis: Arc<analysis::AnalysisRunner>) {
    const TICK_SECS: u64 = 60;
    let repo = analysis.repo();
    loop {
        tokio::time::sleep(Duration::from_secs(TICK_SECS)).await;
        let due = match repo.due_schedules().await {
            Ok(due) => due,
            Err(e) => {
                tracing::warn!(error = %e, "analysis scheduler: due-query failed");
                continue;
            }
        };
        for sched in due {
            // Re-validated at fire time rather than trusted as stored — the clamps are edge
            // validation, and a schedule saved before a bound moved must not keep firing outside
            // the new one.
            let params = match api::analysis::scheduled_params(&sched) {
                Ok(p) => p,
                Err(e) => {
                    tracing::warn!(
                        schedule = %sched.id, code = e.code(),
                        "scheduled analysis has params this build will not accept; skipping"
                    );
                    // Advance anyway: retrying an unacceptable row every minute would be a busy
                    // loop, and the status says why.
                    advance(&repo, &sched, analysis::AnalysisScheduleStatus::Error).await;
                    continue;
                }
            };
            match analysis.create(params, Some("scheduler".to_owned())).await {
                Ok(_) => advance(&repo, &sched, analysis::AnalysisScheduleStatus::Queued).await,
                Err(
                    analysis::CreateError::TooManyConcurrent(_)
                    | analysis::CreateError::RateLimited(_),
                ) => {
                    tracing::info!(
                        schedule = %sched.id,
                        "scheduled analysis deferred — the runner is at its admission limit; \
                         staying due for the next tick"
                    );
                    if let Err(e) = repo.mark_deferred(sched.id).await {
                        tracing::warn!(error = %e, schedule = %sched.id, "failed to defer schedule");
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, schedule = %sched.id, "scheduled analysis failed to start");
                    advance(&repo, &sched, analysis::AnalysisScheduleStatus::Error).await;
                }
            }
        }
    }
}

/// Stamp the outcome and move a schedule on to its next firing instant.
async fn advance(
    repo: &analysis::AnalysisRepo,
    sched: &analysis::AnalysisSchedule,
    status: analysis::AnalysisScheduleStatus,
) {
    let next = cadence::compute_next_run(
        cadence::Schedule {
            frequency: sched.frequency,
            day_of_week: sched.day_of_week,
            day_of_month: sched.day_of_month,
            at_hour: sched.at_hour,
            at_minute: sched.at_minute,
        },
        chrono::Utc::now(),
    );
    if let Err(e) = repo.mark_fired(sched.id, status, next).await {
        tracing::warn!(error = %e, schedule = %sched.id, "failed to advance analysis schedule");
    }
}

/// Bind and serve the northbound API plus the Prometheus `/metrics` endpoint.
async fn serve(
    state: ApiState,
    addr: &str,
    metrics: PrometheusHandle,
    shutdown: CancellationToken,
) -> anyhow::Result<()> {
    // MCP tool surface (ADR-028): mounted only when enabled, so a disabled deployment 404s `/mcp`
    // (byte-identical to pre-MCP). Built before the router consumes `state`; the service shares the
    // server shutdown token (child) so in-flight MCP sessions drain on stop (ADR-017).
    let mcp_router = state
        .enable_mcp
        .then(|| mcp::build_router(state.clone(), shutdown.child_token()));
    if mcp_router.is_some() {
        tracing::info!("MCP server enabled — read-only tool surface at /mcp (ADR-028)");
    }
    let mut app = api::router(state);
    if let Some(mcp_router) = mcp_router {
        app = app.nest("/mcp", mcp_router);
    }
    let app = app
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
    /// Folder groups holding at least one node in each **effective** poll pool — what makes a
    /// pool-coverage alert (ADR-009) visible to the group-scoped operator whose site went dark.
    /// Built here because this is the one place that already scans the whole fleet, and rebuilt on
    /// the same config-generation gate so it costs a steady-state refresh nothing.
    pool_groups: HashMap<String, std::collections::BTreeSet<Uuid>>,
    /// The graph the alert engine suppresses with.
    ///
    /// **This is the only topology in the struct, deliberately.** ADR-043 決定 5's shadow mode does
    /// not put a second graph here for the engine to maybe-use: in `shadow` this field holds the
    /// *manual* graph, exactly as in `manual`, and the derived alternative is computed on demand by
    /// the read-side endpoint that displays the difference. There is therefore no runtime state in
    /// which a shadow graph can suppress anything — not because a flag says so, but because the
    /// engine is never given one.
    topology: Topology,
}

/// Load the config-derived alert base (thresholds + node-meta + dependency topology).
async fn load_alert_config_base(
    repo: &NodeRepo,
    thresholds: &ThresholdStore,
    groups: &groups::GroupRepo,
    topo: &topology_projection::TopologySources,
) -> AlertConfigBase {
    let rules = thresholds.list_all().await.unwrap_or_else(|e| {
        tracing::warn!(error = %e, "failed to load thresholds");
        Vec::new()
    });
    let nodes = repo.list_nodes().await.unwrap_or_default();
    // Folder-pool inheritance (0054). Degrading to `empty()` resolves every node to its own pool
    // or `default`, which narrows what a scoped operator can see rather than widening it.
    let pools = match groups.pool_rows().await {
        Ok(rows) => poolres::PoolResolver::build(rows),
        Err(e) => {
            tracing::warn!(error = %e, "loading folder pools failed; scoping pool alerts without inheritance");
            poolres::PoolResolver::empty()
        }
    };
    let mut meta = HashMap::new();
    let mut pool_groups: HashMap<String, std::collections::BTreeSet<Uuid>> = HashMap::new();
    for node in &nodes {
        // Ungrouped nodes contribute nothing: a scoped caller cannot see them either way, and
        // adding a `None` bucket would be the fail-open reading.
        if let Some(group) = node.group {
            pool_groups
                .entry(pools.resolve_pool(node).to_owned())
                .or_default()
                .insert(group.as_uuid());
        }
        meta.insert(
            node.id,
            NodeMeta {
                profile: node.profile.as_ref().map(ToString::to_string),
                // Tag values (threshold scope) and the folder group (RBAC visibility) are two
                // different things — see the `NodeMeta` docs before touching either.
                tag_groups: node.tags.values().cloned().collect(),
                folder_group: node.group.map(|g| g.as_uuid()),
            },
        );
    }

    // ADR-043 決定 5. The engine gets the derived graph only in `derived`; `shadow` is byte-for-byte
    // `manual` here, and the comparison an operator reviews is computed by the read-side endpoint.
    let topology = if repo.get_topology_mode().await.uses_derived() {
        topology_projection::derived_topology(topo, &nodes).await.0
    } else {
        // Dependency edge child → parent feeds parent-down suppression (ADR-015).
        topology_projection::manual_topology(&nodes)
    };

    AlertConfigBase {
        rules,
        nodes,
        meta,
        pool_groups,
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
    topo: &topology_projection::TopologySources,
) -> AlertConfig {
    let base = load_alert_config_base(repo, thresholds, groups, topo).await;
    let in_maintenance = resolve_maintenance(maintenance, groups, repo, &base.nodes).await;
    AlertConfig::new(base.rules, base.meta)
        .with_topology(base.topology)
        .with_maintenance(in_maintenance)
        .with_pool_groups(base.pool_groups)
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

    /// This file's own source, for the structural assertion below.
    const SRC: &str = include_str!("main.rs");

    /// **A coverage transition is written to History, not only notified.**
    ///
    /// ⚠️ Found on real hardware, not by a test: Increment 1 deliberately kept pool alerts out of
    /// `alert_history`, so the watch loop was wired to the notifier alone. Increment 2 opened the
    /// store to every subject — and nothing in the type system connected the two, so the alert
    /// raised, paged, and left no row. The gauges and the log line all looked correct.
    ///
    /// Structural because the loop's body is a 30-second tick around a real database; the
    /// behaviour a unit test *can* reach is `coverage_alert_of`, tested below.
    #[test]
    fn a_pool_coverage_transition_reaches_the_history_store() {
        let production = SRC
            .split("#[cfg(test)]")
            .next()
            .expect("split always yields a first element");
        let watch = production
            .split("async fn run_pool_coverage_watch")
            .nth(1)
            .expect("the watch loop exists");
        let body = &watch[..watch.find("\nfn ").unwrap_or(watch.len())];
        assert!(
            body.contains("history.record("),
            "the coverage watch notifies without recording — the alert pages and History stays \
             empty, which is exactly what shipped in Increment 1"
        );
        assert!(
            body.contains("notifier.handle("),
            "…and it must still notify"
        );
    }

    #[test]
    fn every_notify_action_decides_what_history_does_with_it() {
        use yagra_alert::{Alert, Breach, Subject};
        let alert = Alert {
            subject: Subject::Pool("tokyo".to_owned()),
            check: yagra_common::CheckId::from(Uuid::nil()),
            severity: yagra_common::Severity::Critical,
            state: yagra_common::NodeState::Unreachable,
            at_unix_ms: 0,
            root_cause: None,
            flapping: false,
            metric: "live_pollers".to_owned(),
            breach: None::<Breach>,
        };
        // A fire and a resolve are both rows; the two are told apart by `resolved`, which the
        // caller derives from the same action.
        assert!(coverage_alert_of(&crate::alerts::NotifyAction::Fire(alert.clone())).is_some());
        assert!(coverage_alert_of(&crate::alerts::NotifyAction::Resolve(alert.clone())).is_some());
        // Suppression is a property of the node dependency graph, which a pool is not in.
        assert!(coverage_alert_of(&crate::alerts::NotifyAction::Suppress(alert)).is_none());
    }

    /// **In shadow mode the alert engine receives the manual graph, and nothing else.**
    ///
    /// ADR-043 決定 5's safety property, and the reason `AlertConfigBase` carries one topology
    /// rather than two: the derived graph is chosen *only* when the mode says to use it, so there is
    /// no runtime state in which a preview graph could suppress a real alert. That is a property of
    /// one expression, and this is what stops the expression growing a second branch.
    ///
    /// The needles are assembled at runtime — a literal one would match this test's own source and
    /// pass forever.
    #[test]
    fn only_the_derived_mode_hands_the_engine_a_derived_graph() {
        let production = SRC
            .split("#[cfg(test)]")
            .next()
            .expect("split always yields a first element");
        let guard = format!("get_topology_mode().await.{}()", "uses_derived");
        assert!(
            production.contains(&guard),
            "the topology choice is no longer gated on `uses_derived`"
        );
        let call = format!("{}::derived_topology", "topology_projection");
        assert_eq!(
            production.matches(call.as_str()).count(),
            1,
            "the derived graph reaches the engine from exactly one place; a second call site is \
             how a preview graph starts suppressing alerts"
        );
        assert!(
            !production.contains("shadow_topology"),
            "a shadow graph must not be a field the engine could be handed"
        );
    }

    // ── Sweep pool grouping (effective pool + live-pool seeding) ──────────────

    fn test_node(pool: Option<&str>, group: Option<Uuid>) -> yagra_common::Node {
        use std::net::{IpAddr, Ipv4Addr};
        let mut n =
            yagra_common::Node::new(NodeId::new(), "n", IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)));
        n.pool = pool.map(str::to_owned);
        n.group = group.map(yagra_common::GroupId::from);
        n
    }

    #[test]
    fn group_by_pool_uses_the_effective_pool() {
        let folder = Uuid::from_u128(1);
        let resolver = poolres::PoolResolver::build(vec![(folder, None, Some("tokyo".to_owned()))]);
        let nodes = vec![
            (test_node(None, Some(folder)), 30),          // inherits tokyo
            (test_node(Some("osaka"), Some(folder)), 30), // own pool wins
            (test_node(None, None), 30),                  // default
        ];
        let groups = group_by_pool(
            nodes,
            &std::collections::HashSet::new(),
            &std::collections::HashSet::new(),
            &resolver,
        );
        assert_eq!(groups.get("tokyo").map(Vec::len), Some(1));
        assert_eq!(groups.get("osaka").map(Vec::len), Some(1));
        assert_eq!(groups.get(yagra_bus::DEFAULT_POOL).map(Vec::len), Some(1));
    }

    fn live_set(pools: &[&str]) -> std::collections::HashSet<String> {
        pools.iter().map(|p| (*p).to_owned()).collect()
    }

    #[test]
    fn group_by_pool_seeds_live_pools_that_have_no_nodes() {
        // Regression: without seeding, a pool whose last node moved away vanishes from the map and
        // is never reconciled again — its poller keeps polling a stale working set forever, so the
        // moved node ends up polled by two pollers. Editing pools from the UI makes this routine.
        let groups = group_by_pool(
            vec![(test_node(Some("osaka"), None), 30)],
            &std::collections::HashSet::new(),
            &live_set(&["tokyo", "osaka"]),
            &poolres::PoolResolver::empty(),
        );
        assert_eq!(
            groups.get("tokyo").map(Vec::len),
            Some(0),
            "an emptied pool must still be reconciled (with an empty desired set)"
        );
        assert_eq!(groups.get("osaka").map(Vec::len), Some(1));
    }

    #[test]
    fn group_by_pool_drops_meraki_nodes() {
        // Core's org collector polls Meraki devices; no pool poller ever should.
        let meraki = test_node(Some("tokyo"), None);
        let meraki_ids: std::collections::HashSet<Uuid> =
            [meraki.id.as_uuid()].into_iter().collect();
        let groups = group_by_pool(
            vec![(meraki, 30), (test_node(Some("tokyo"), None), 30)],
            &meraki_ids,
            &std::collections::HashSet::new(),
            &poolres::PoolResolver::empty(),
        );
        assert_eq!(groups.get("tokyo").map(Vec::len), Some(1));
    }

    #[tokio::test]
    async fn backfill_consumer_persists_metrics_and_meta_only() {
        // Store-and-forward (Phase 3): a backfilled result reaches the VM + meta writers (so history
        // fills at its original timestamp) but the backfill consumer has no access to alert state at
        // all — replaying a stale burst can never re-fire resolved alerts.
        use yagra_bus::DiscoveredInterface;
        use yagra_common::IfIndex;
        let (metrics_tx, mut metrics_rx) = tokio::sync::mpsc::channel::<Arc<PollResult>>(8);
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
        consume_results_backfill(futures::stream::iter(vec![result]), metrics_tx, meta_tx).await;
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
        let (notify_tx, mut notify_rx) = tokio::sync::mpsc::channel(64);
        let (metrics_tx, _metrics_rx) = tokio::sync::mpsc::channel::<Arc<PollResult>>(64);
        let (meta_tx, _meta_rx) = tokio::sync::mpsc::channel::<MetaRecord>(64);
        let (history_tx, _history_rx) = tokio::sync::mpsc::channel::<HistoryRecord>(64);
        // The history store is never touched: the channel is wide enough that `enqueue_history`
        // never takes its inline-write fallback. `connect_lazy` gives a handle that connects to
        // nothing (the same trick `events.rs`'s planner tests use).
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
                &metrics_tx,
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

    // ---- Flow ingest match/persist split (S27, ADR-031) ----

    fn flow_rec(
        src: &str,
        dst: &str,
        src_as: u32,
        dst_as: u32,
        bytes: u64,
    ) -> yagra_bus::FlowRecord {
        yagra_bus::FlowRecord {
            src_ip: src.parse().unwrap(),
            dst_ip: dst.parse().unwrap(),
            src_port: 1234,
            dst_port: 443,
            proto: 6,
            tos: 0,
            if_index: 2,
            src_as,
            dst_as,
            bytes,
            packets: 10,
            flows: 1,
        }
    }

    fn flow_batch(exporter: &str, records: Vec<yagra_bus::FlowRecord>) -> FlowBatch {
        FlowBatch {
            poller_id: "test-poller".into(),
            pool: DEFAULT_POOL.into(),
            exporter_ip: exporter.parse().unwrap(),
            bucket_start_ms: 1_700_000_000_000,
            bucket_secs: 60,
            records,
            dropped: 0,
        }
    }

    #[test]
    fn flow_rows_dropped_when_exporter_unmapped() {
        let addr_map: HashMap<std::net::IpAddr, Uuid> = HashMap::new();
        let batch = flow_batch(
            "198.51.100.7",
            vec![flow_rec("10.0.0.1", "8.8.8.8", 0, 0, 100)],
        );
        assert!(
            flow_rows_from_batch(&batch, &addr_map, None).is_none(),
            "an unmapped exporter yields no rows (the batch is dropped by the caller)"
        );
    }

    #[test]
    fn miss_reload_throttled_to_once_per_exporter() {
        use std::collections::HashSet;
        use std::net::IpAddr;
        let mapped: IpAddr = "10.0.0.1".parse().unwrap();
        let addr_map: HashMap<IpAddr, Uuid> = HashMap::from([(mapped, Uuid::from_u128(1))]);
        let mut missed: HashSet<IpAddr> = HashSet::new();

        // A mapped exporter never triggers a reload and is never recorded as missed.
        assert!(!should_reload_on_miss(&addr_map, &mut missed, mapped));
        assert!(missed.is_empty());

        // First batch from an unmapped exporter reloads once…
        let unknown: IpAddr = "198.51.100.7".parse().unwrap();
        assert!(should_reload_on_miss(&addr_map, &mut missed, unknown));
        // …and every subsequent batch from the same still-unmapped exporter is throttled (no reload).
        assert!(!should_reload_on_miss(&addr_map, &mut missed, unknown));
        assert!(!should_reload_on_miss(&addr_map, &mut missed, unknown));

        // A different unmapped exporter still gets its own one-shot reload.
        let unknown2: IpAddr = "203.0.113.9".parse().unwrap();
        assert!(should_reload_on_miss(&addr_map, &mut missed, unknown2));
        assert!(!should_reload_on_miss(&addr_map, &mut missed, unknown2));
    }

    #[test]
    fn flow_rows_resolve_node_and_preserve_fields() {
        let exporter: std::net::IpAddr = "198.51.100.7".parse().unwrap();
        let node = Uuid::from_u128(42);
        let addr_map: HashMap<std::net::IpAddr, Uuid> = HashMap::from([(exporter, node)]);
        let batch = flow_batch(
            "198.51.100.7",
            vec![
                flow_rec("10.0.0.1", "8.8.8.8", 0, 0, 100),
                flow_rec("10.0.0.2", "1.1.1.1", 0, 0, 200),
            ],
        );
        let rows = flow_rows_from_batch(&batch, &addr_map, None).expect("mapped exporter");
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|r| r.node_id == node));
        assert_eq!(rows[0].ts_unix_ms, 1_700_000_000_000);
        assert_eq!((rows[0].bytes, rows[1].bytes), (100, 200));
        // No IP→ASN table and the exporter sent 0 → AS stays unknown.
        assert_eq!((rows[0].src_as, rows[0].dst_as), (0, 0));
    }

    #[test]
    fn flow_as_enrichment_fills_only_zeros() {
        // Offline IP→ASN table maps 8.8.8.0/24 → AS15169; an exporter's own non-zero AS still wins.
        let db = crate::ipasn::IpAsnDb::from_tsv("8.8.8.0\t8.8.8.255\t15169\tUS\tGOOGLE\n");
        let exporter: std::net::IpAddr = "198.51.100.7".parse().unwrap();
        let node = Uuid::from_u128(7);
        let addr_map: HashMap<std::net::IpAddr, Uuid> = HashMap::from([(exporter, node)]);
        let batch = flow_batch(
            "198.51.100.7",
            vec![
                // dst 8.8.8.8, dst_as=0 → enriched to 15169; src_as=64500 provided → preserved.
                flow_rec("10.0.0.1", "8.8.8.8", 64500, 0, 100),
                // dst 9.9.9.9 not in the table → stays unknown.
                flow_rec("10.0.0.2", "9.9.9.9", 0, 0, 50),
            ],
        );
        let rows = flow_rows_from_batch(&batch, &addr_map, Some(&db)).expect("mapped exporter");
        assert_eq!(
            rows[0].src_as, 64500,
            "exporter-provided AS is authoritative"
        );
        assert_eq!(
            rows[0].dst_as, 15169,
            "a zero AS is filled from the offline table"
        );
        assert_eq!(
            rows[1].dst_as, 0,
            "an address not in the table stays unknown"
        );
    }

    #[tokio::test]
    async fn flow_send_drops_and_counts_when_queue_full() {
        // A full hand-off queue (writer behind a slow ClickHouse) drops rows and reports the count
        // instead of blocking the bus consumer — the S27 backpressure contract.
        let node = Uuid::from_u128(1);
        let mk = |n: usize| -> Vec<FlowRow> {
            (0..n)
                .map(|i| FlowRow {
                    node_id: node,
                    ts_unix_ms: 0,
                    exporter_ip: "198.51.100.7".parse().unwrap(),
                    if_index: 0,
                    src_ip: "10.0.0.1".parse().unwrap(),
                    dst_ip: "8.8.8.8".parse().unwrap(),
                    src_port: 0,
                    dst_port: 0,
                    proto: 6,
                    tos: 0,
                    src_as: 0,
                    dst_as: 0,
                    bytes: i as u64,
                    packets: 0,
                    flows: 1,
                })
                .collect()
        };
        let (tx, mut rx) = tokio::sync::mpsc::channel::<FlowRow>(2);
        // Queue cap 2: the first two rows are accepted, the remaining three are dropped.
        let dropped = send_flow_rows(&tx, mk(5));
        assert_eq!(dropped, 3, "rows beyond the queue capacity are dropped");
        assert_eq!(rx.try_recv().unwrap().bytes, 0);
        assert_eq!(rx.try_recv().unwrap().bytes, 1);
        assert!(rx.try_recv().is_err(), "only the accepted rows are queued");
    }
}
