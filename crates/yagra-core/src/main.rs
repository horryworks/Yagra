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
mod bus_callout;
mod bus_cert;
mod cadence;
mod classification;
mod collection;
mod config;
mod config_bundle;
mod config_gen;
mod csv;
mod dashboard;
mod derived;
mod discovery;
mod dns_check;
mod events;
mod flow_ingest;
mod flowstore;
mod forward;
mod forward_store;
mod gcp;
mod groups;
mod history;
mod host_collector;
mod interface_util;
mod ipasn;
mod l3;
mod l3_routing;
mod ldap;
mod leader;
mod link_overrides;
mod logstore;
mod maintenance;
mod mcp;
// ⚠️ **This line used to live at the bottom of the file, and the position was load-bearing.**
// Twenty-one files in this crate computed "production code" as everything above the first test
// attribute, so a test-only `mod` declaration at the top of a file truncated their view to its
// imports — both of this file's structural tests failed the moment this sat here. ADR-091 moved
// all twenty-one onto `module_source`, which removes each test-only item instead of cutting at
// the first, so the constraint is gone and this line has come home to prove it.
mod meraki;
mod metric_meaning;
mod mib;
#[cfg(test)]
mod module_source;
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
/// Per-account WebUI preferences — one opaque JSON document per account (ADR-058).
mod preferences;
mod ratelimit;
mod rca;
mod repo;
mod reports;
mod result_ingest;
mod retention;
mod retention_sweep;
mod ring;
mod scheduler;
mod secrets;
mod seed_ids;
// The WebUI's own server certificate (ADR-044). Named apart from `tls`, which builds *client*
// configurations for outbound peers — see that module's doc.
mod server_cert;
mod sink;
// The table vocabulary the placement guards scan against, derived from `migrations/` (ADR-095).
// Apart from `module_source`, which answers what a module's own text is rather than what the
// schema declares.
#[cfg(test)]
mod sql_tables;
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
// What this deployment is running and how far back it can be taken (ADR-050). Named apart from
// `repo`, which *applies* migrations; this one reasons about what applying them cost.
mod poller_bundle;
mod poller_logs;
mod poller_upgrade;
mod upgrade;
mod url_check;
mod volatile;
mod webtls;

use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use yagra_telemetry::{shutdown_signal, spawn_cancellable, CancellationToken};

use ack::AckRepo;
use alerts::{AlertManager, Notifier};
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
use history::AlertHistoryStore;
use logstore::{LogStore, VlStore};
use maintenance::MaintenanceRepo;
use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};
use mib::MibRepo;
use notifications::NotificationRepo;
use pollers::PollerRepo;
use preferences::UserPrefsRepo;
use repo::{NodeListing, NodeRepo, StaticNodeList};
use secrets::CredentialStore;
use sink::InMemorySink;
use store::{MetricStore, VmStore};
use thresholds::ThresholdStore;
use uuid::Uuid;
use volatile::VolatileStore;
use yagra_bus::{NatsBus, PollResult, DEFAULT_POOL};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Before anything that could reach a TLS library — `healthcheck` included, since it makes an
    // HTTPS request. Two crypto providers are enabled in this dependency graph, so rustls installs
    // no default and `async_nats` panics building its own client config the moment the bus URL is
    // `tls://` (ADR-065 Inc.5 bug 3).
    yagra_bus::install_tls_crypto_provider();

    // Container HEALTHCHECK entry point: `yagra-core healthcheck` probes our own `/healthz` and
    // exits 0 (healthy) / 1 (not). Dependency-free (reqwest is already linked), so the slim runtime
    // image needs no curl/wget. Handled before any store/bus wiring so it's cheap and side-effect-free.
    if std::env::args().nth(1).as_deref() == Some("healthcheck") {
        std::process::exit(run_healthcheck().await);
    }

    // `yagra-core migrations` prints the migration set THIS binary embeds, as JSON, and exits.
    // No database, no telemetry, no config — deliberately, because the caller runs it inside the
    // *target* image (`docker run --rm ghcr.io/…/yagra-core:vX migrations`) to learn what an
    // upgrade would apply, before anything is touched (ADR-050 decision 6). Sits beside
    // `healthcheck` and before any wiring for the same reason: cheap and side-effect-free.
    if std::env::args().nth(1).as_deref() == Some("migrations") {
        print_embedded_migrations();
        return Ok(());
    }

    // `yagra-core bus-cert` establishes the TLS material the NATS bus serves and exits (ADR-065).
    // Run as a one-shot by the composition BEFORE the bus starts, which is the whole reason it is a
    // subcommand rather than something core does at startup: core needs the bus, so core cannot be
    // the thing that prepares it. Same pattern as `kek-init` / `tls-init`, and it reuses this
    // image so there is no second place the certificate rules live.
    //
    // Unlike the two probes above this one is NOT side-effect-free — it writes a database row and
    // two files. It is still handled here, before telemetry and any wiring, because everything
    // below it needs a bus.
    if std::env::args().nth(1).as_deref() == Some("bus-cert") {
        let _telemetry = yagra_telemetry::init("yagra-bus-cert");
        return run_bus_cert().await;
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

/// Print the embedded migration set as one JSON object on stdout (`yagra-core migrations`).
///
/// The checksum is included because it is what distinguishes "this version is already applied" from
/// "this version was applied from *different* SQL" — the second is the case an upgrade planner must
/// never wave through, and sqlx reports it as `VersionMismatch` rather than a missing version.
fn print_embedded_migrations() {
    let migrator = repo::embedded_migrations();
    let migrations: Vec<serde_json::Value> = migrator
        .iter()
        .map(|m| {
            let checksum: String = m.checksum.iter().map(|b| format!("{b:02x}")).collect();
            serde_json::json!({
                "version": m.version,
                "description": m.description.as_ref(),
                "checksum": checksum,
            })
        })
        .collect();
    let doc = serde_json::json!({
        "core_version": env!("CARGO_PKG_VERSION"),
        "migrations": migrations,
    });
    println!("{doc}");
}

/// `yagra-core bus-cert` — establish the bus's TLS material, then exit (ADR-065).
///
/// Reads `YAGRA_DATABASE_URL`, `YAGRA_KEK_FILE` and `YAGRA_BUS_TLS_DIR`; extra subject alternative
/// names come from `YAGRA_BUS_TLS_SANS`. Idempotent: with a valid certificate already stored it only
/// rewrites the files, which is what makes running it on every `up` correct rather than wasteful.
///
/// **It applies migrations, and that is deliberate rather than incidental.** The table it needs is
/// created by a migration, migrations are applied by core, core waits for the bus, and the bus waits
/// for this — so somebody in that cycle has to be able to bring the schema forward. It is the same
/// binary and the same embedded set, and sqlx takes an advisory lock, so core applying them a moment
/// later is a no-op rather than a race. The practical consequence worth knowing: on a fresh
/// deployment the migrations are applied by *this* container, so a failure here reads as
/// "bus-cert-init failed" rather than "core failed".
///
/// Every failure is fatal. This is a one-shot the composition waits on with
/// `service_completed_successfully`, so exiting non-zero stops the bus from starting against a
/// half-written volume — which is the outcome an operator can diagnose, unlike a bus that comes up
/// serving nothing.
async fn run_bus_cert() -> anyhow::Result<()> {
    let db = std::env::var("YAGRA_DATABASE_URL")
        .map_err(|_| anyhow::anyhow!("YAGRA_BUS_TLS needs YAGRA_DATABASE_URL"))?;
    let dir = std::env::var("YAGRA_BUS_TLS_DIR")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .map(std::path::PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("YAGRA_BUS_TLS_DIR is not set — nowhere to write to"))?;

    let kek = secrets::load_key_provider()?;
    let repo = repo::NodeRepo::connect(&db).await?;
    repo.migrate().await?;

    let bus_tls = bus_cert::BusTlsRepo::new(repo.pool(), kek.clone(), Some(dir.clone()));
    bus_tls.ensure_ready(&bus_cert::configured_names()).await?;
    // The configuration travels with the image exactly as the composition does (ADR-050 decision 5),
    // so this is a copy on every run rather than a create-if-absent: an upgrade that changes the
    // file must not leave the previous release's copy in place.
    bus_tls.install_server_conf(std::path::Path::new(bus_cert::CONF_IN_IMAGE))?;

    // The Auth Callout account key, and the block that names its public half (ADR-065 Inc.7).
    // Written HERE, in the same run from the same image as the file that includes it — that is the
    // whole design. Routing the issuer through `.env` would make it one value two files have to
    // agree about across an upgrade, and the composition defaults it to the *empty string* rather
    // than leaving it unset, so a disagreement is a bus that will not start.
    //
    // Unconditional, including on a deployment whose bus never leaves the host: it costs one row
    // and one file nats only reads with `-c`. The alternative — generating the key at the moment
    // the switch is pressed — puts a key generation inside the one request an operator is already
    // watching restart their monitoring.
    let callout = bus_callout::BusCalloutRepo::new(repo.pool(), kek, Some(dir));
    let identity = callout.ensure_ready(&config::callout_account()).await?;
    callout.install_conf(&identity)?;

    tracing::info!("bus TLS material is ready");
    Ok(())
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

    // Offline IP→ASN table for flow AS enrichment (ADR-031 Increment 3), behind a hot-swappable
    // handle shared by the flow writer (IP→AS) and the flow API (AS→name). Opt-in/default-OFF;
    // `ipasn::open` carries what a missing or empty dataset does. The reloader starts below, once
    // the shutdown token exists.
    let ipasn: crate::ipasn::IpAsnHandle = ipasn::open(cfg.ipasn_db_path.as_deref());

    // Alert engine + notifier (env default route + DB channels/rules, ADR-015) + history.
    let alerts = Arc::new(AlertManager::new());
    let notifier = Arc::new(Notifier::from_env());
    let notifications = Arc::new(NotificationRepo::new(repo.pool(), kek.clone()));
    let history = Arc::new(AlertHistoryStore::new(repo.pool()));
    // Give the engine back what it knew before this process started (ADR-097). Awaited here, and
    // here rather than in a task, because it has to finish before the first poll result arrives:
    // afterwards a still-broken check would already have re-fired, which is the duplicate incident
    // this exists to stop. It is idempotent and never overwrites, so it cannot undo an observation.
    alerts::restore::restore(&alerts, &history).await;
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
    // Auto-registration is turned OFF exactly when the bus already refuses an unregistered id — i.e.
    // when Auth Callout governs this bus (ADR-065 Inc.3). Keeping the two conditions the same one is
    // deliberate: with the callout off, refusing to create a row would make a poller invisible while
    // it happily polled, and with it on, creating one is what made "delete this poller" not stick.
    //
    // ⚠️ Inc.7 changed what "configured" means. It used to read `nats_callout_seed_file.is_some()`,
    // an env var the shipped composition never set — so this gate had never once closed in
    // production. It now asks `authcallout::is_governed`, the same question the responder asks, so
    // the two cannot answer differently. The visible consequence on an existing deployment that has
    // already accepted remote pollers: a heartbeat from an id with no inventory row stops creating
    // one. That is the intended behaviour and it is new behaviour.
    let poller_registration_gated = authcallout::is_governed(cfg.nats_poller_password.as_deref());
    let poller_repo =
        Arc::new(PollerRepo::new(repo.pool()).with_auto_register(!poller_registration_gated));
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

    // IP→ASN periodic reloader (ADR-031). Nothing starts unless a dataset and a non-zero interval
    // are both configured; on every core when they are. `ipasn::start_reload` carries why.
    ipasn::start_reload(
        ipasn.clone(),
        cfg.ipasn_db_path.as_deref(),
        cfg.ipasn_reload_secs,
        &shutdown,
    );

    // HA leader election (ADR-016): every leader-only background task (coordinator consumers, the
    // result-ingest → alert/notify/persist chain, the event pipeline, the schedulers, and the
    // config/retention/routing refresh loops) is deferred into `leader_work` below and spawned only
    // once this core holds the advisory lock. Read-only priming (event/alert/routing config) stays
    // inline so the API serves complete config from its first request, on the leader and standbys
    // alike. When HA is off, `leader_work` runs inline before `serve` — byte-identical to pre-HA.

    // Core self-observability (monitoring-conventions): the cache the System Health page reads, and
    // the sampler that fills it. On every core; `host_collector::start` carries why.
    let core_host: api::CoreHostSample = Arc::new(std::sync::Mutex::new(None));
    host_collector::start(
        store.clone(),
        core_host.clone(),
        repo.pool().clone(),
        &shutdown,
    );

    // Per-poller NATS credential scoping via Auth Callout (ADR-030, wired up by ADR-065 Inc.7). A
    // no-op on a bus the callout does not govern; on every core when it does. `authcallout::start`
    // carries why it is queue-subscribed.
    //
    // The key comes from the row the `bus-cert` one-shot established — the same run that wrote the
    // broker's `callout.conf`, so the issuer NATS trusts and the key core signs with cannot come
    // from different places. A mounted `YAGRA_NATS_CALLOUT_SEED_FILE` still wins, for a deployment
    // that set one up under ADR-030 before this existed.
    let callout_seed = match cfg.nats_callout_seed_file.as_deref() {
        Some(path) => std::fs::read_to_string(path)
            .map_err(|e| tracing::error!(error = %e, path, "auth-callout: cannot read the mounted seed file"))
            .ok(),
        None => bus_callout::BusCalloutRepo::new(repo.pool(), kek.clone(), None)
            .load_seed()
            .await
            .map_err(|e| tracing::error!(error = %e, "auth-callout: cannot read the stored account key"))
            .ok()
            .flatten(),
    };
    authcallout::start(
        callout_seed,
        &cfg.nats_callout_account,
        cfg.nats_poller_password.clone(),
        bus.client(),
        poller_repo.clone(),
        &shutdown,
    );

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
        Arc::new(alerts::sink::RecordingSink::new(
            history.clone(),
            notifier.clone(),
            "an event alert",
        )),
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
    let routing_repo = Arc::new(l3_routing::RoutingRepo::new(repo.pool()));
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
    // A failed priming load leaves the engine with its empty starting config rather than
    // installing a degraded one (ADR-080 決定 3). Nothing is active at boot, so an empty ruleset
    // cannot resolve anything; the leader's 30s refresh installs the real one. Refusing to start
    // would be worse — a transient query failure right after migrations would take monitoring down.
    match alerts::config::load_alert_config(
        &alerts::config::LiveConfigSources {
            repo: repo.clone(),
            thresholds: thresholds.clone(),
            groups: group_repo.clone(),
            topo: topo_sources.clone(),
        },
        &maintenance,
        &group_repo,
        &repo,
    )
    .await
    {
        Ok(config) => alerts.set_config(config),
        Err(e) => {
            metrics::counter!("yagra_alert_config_load_failures_total").increment(1);
            tracing::error!(error = %e, "priming the alert config failed; retrying on the refresh loop");
        }
    }

    // Notification templates (ADR-039) interpolate node names, groups and profiles, none of which
    // an `Alert` carries. Wired once, here, because it needs the write side; a skeleton-mode core
    // never reaches this and renders ids instead. It is only consulted when a channel actually has
    // a template, so a deployment with none issues no extra query.
    notifier.set_facts_source(Arc::new(notify_facts::CachedNodeFacts::new(repo.clone())));

    // Notification routing + mutes priming: load the DB channels/rules into the notifier now (env
    // channels stay always-on). The periodic refresh loop is leader-only (`leader_work`).
    alerts::config::load_routing(&notifier, &notifications).await;
    alerts::config::load_mutes(&notifier, &maintenance, &repo, &group_repo).await;

    // Write side (inventory + encrypted credentials + users + thresholds), sharing the pool.
    let users = Arc::new(UserStore::new(repo.pool()));
    // Bootstrap admin on a fresh database. `auth::ensure_bootstrap_admin` carries the rule that the
    // generated password is disclosed to the log exactly once, on purpose.
    auth::ensure_bootstrap_admin(&users).await?;
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
        analysis::AnalysisStores {
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
        flow_system_log_days: cfg.flow_system_log_days,
        creds: creds.clone(),
        dispatcher: dispatcher.clone(),
        discovery: discovery.clone(),
        event_engine: event_engine.clone(),
        events: events_repo.clone(),
        persist_rx,
        event_action_rx,
        logs: logs.clone(),
        flows: flows.clone(),
        ipasn: ipasn.clone(),
        thresholds: thresholds.clone(),
        maintenance: maintenance.clone(),
        groups: group_repo.clone(),
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
        maintenance: maintenance.clone(),
        classification,
        classifier,
        groups: group_repo,
        audit: audit_repo.clone(),
        dashboards: Arc::new(DashboardRepo::new(repo.pool())),
        shared_dashboard: Arc::new(SharedDashboardRepo::new(repo.pool())),
        prefs: Arc::new(UserPrefsRepo::new(repo.pool())),
        scheduler_stats: scheduler_stats.clone(),
        dispatcher,
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
    // Session store, plus the three revocation tasks when a signing key is mounted. On every core;
    // `auth::start_sessions` carries the fail-closed rule and why none of it is leader-gated.
    let sessions = auth::start_sessions(
        cfg.session_key_file.as_deref(),
        cfg.enable_ha,
        repo.pool(),
        bus.clone(),
        &shutdown,
    )
    .await?;
    // External-IdP login (OIDC, ADR-010 Phase 3): provider store (envelope-encrypted secret) + the
    // in-memory in-flight authorization map.
    let oidc = Some(Arc::new(oidc::OidcRepo::new(repo.pool(), kek.clone())));
    let oidc_flight = Arc::new(oidc::OidcFlight::new());
    // Directory login (LDAP/AD, ADR-041): the single configuration row, with the service account's
    // bind password sealed by the same KEK. No in-flight map — there is no redirect leg.
    let ldap = Some(Arc::new(ldap::LdapRepo::new(repo.pool(), kek.clone())));
    // The WebUI's own TLS certificate (ADR-044). Same KEK as every other secret.
    let webtls = webtls::open(repo.pool(), kek.clone());
    // The bus's certificate (ADR-065). Written by the `bus-cert` one-shot before this process
    // starts; core holds the repo so Settings ▸ Pollers can show what remote sites must pin and mint
    // a replacement covering a new site's address. Core deliberately runs no renewal timer for it —
    // see `bus_cert`'s module doc: a new bus certificate disconnects every site until each is given
    // the new one, so it is an operator action rather than a background chore.
    let bus_tls = bus_cert::open(repo.pool(), kek.clone());

    // ⚠️ Built before `repo` is erased to `Arc<dyn NodeListing>` below, which moves it.
    let upgrade = upgrade::open(repo.pool());
    // Settle the run that replaced the previous process, sweep abandoned archives, republish the
    // switch. On every core; `upgrade::start` carries why, and why it is not awaited.
    upgrade::start(
        &upgrade,
        audit_repo.clone(),
        maintenance.clone(),
        bus.clone(),
        coordinator.clone(),
        &shutdown,
    )
    .await;

    // Give the pollers inside this composition their inventory rows (ADR-065 Inc.8).
    //
    // Not a background task and deliberately awaited: it is one INSERT ... ON CONFLICT DO NOTHING
    // per co-located poller, and everything downstream reads that table. On every core, because
    // it is idempotent and a passive core that is promoted must not first discover its own poller
    // is missing.
    //
    // The switch does this too, at the moment auto-registration stops. This covers every LATER
    // moment: an upgrade reinstalls the composition from the target image without anyone pressing
    // the switch, so a poller id that moved would otherwise be neither refused by the bus (it is
    // on the bypassed static account) nor registered by anything. Measured cost of getting that
    // wrong: a poller polling 11 nodes that no page in the product lists.
    match pollers::register_local(&poller_repo, &upgrade).await {
        Ok(Some((created, known))) => {
            tracing::info!(created, known, "adopted the pollers this deployment owns")
        }
        Ok(None) => {}
        Err(e) => tracing::warn!(error = %e, "could not adopt the co-located pollers"),
    }

    // Support-log retrieval from remote-site pollers (ADR-045 Inc.4). Process-lifetime subscriber,
    // on every core; `poller_logs::start` carries why it is neither per-bundle nor leader-gated.
    let poller_log_collector = poller_logs::start(bus.clone(), &shutdown).await;

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
        event_engine: Some(event_engine),
        public_dashboard: cfg.public_dashboard,
        is_leader: is_leader.clone(),
        ldap,
        oidc,
        oidc_flight,
        enable_mcp: cfg.enable_mcp,
        rca,
        webtls: Some(webtls.clone()),
        bus_tls: Some(bus_tls),
        upgrade: Some(upgrade),
        metrics: Some(metrics.clone()),
        started: std::time::SystemTime::now(),
        poller_logs: Some(poller_log_collector),
    };

    // Materialize the WebUI certificate BEFORE the listener binds, and keep it renewed. Both halves
    // run on every core; `webtls::materialize_and_renew` carries the reason.
    webtls::materialize_and_renew(webtls.clone(), &shutdown).await;

    // Leader-only work starts now (HA off) or the moment this core wins the advisory lock (HA on).
    // The election itself lives in `leader.rs` — see `run_leader_work` for the standby behaviour.
    leader::run_leader_work(
        cfg.enable_ha,
        &cfg.database_url,
        cfg.core_id.as_deref(),
        is_leader.clone(),
        &shutdown,
        leader_work,
    )
    .await?;

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
    /// Retention for ClickHouse's own system logs (ADR-031 Inc.4); `0` leaves `system.*` alone.
    /// Leader-only like the rest of this struct, which is what keeps two cores from racing the
    /// same `ALTER`.
    flow_system_log_days: u32,
    creds: Arc<CredentialStore>,
    dispatcher: Arc<scheduler::PollDispatcher>,
    discovery: Arc<DiscoveryRunner>,
    event_engine: Arc<events::EventEngine>,
    events: Arc<events::EventRepo>,
    /// Passive-event persistence queue (moved — exactly one drainer).
    persist_rx: tokio::sync::mpsc::Receiver<events::PersistRecord>,
    /// Passive-event side-effect queue: history + notify (moved — exactly one drainer).
    event_action_rx: tokio::sync::mpsc::Receiver<events::QueuedAction>,
    logs: Option<Arc<dyn LogStore>>,
    flows: Option<Arc<dyn FlowStore>>,
    ipasn: ipasn::IpAsnHandle,
    thresholds: Arc<ThresholdStore>,
    maintenance: Arc<MaintenanceRepo>,
    groups: Arc<groups::GroupRepo>,
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
    routing: Arc<l3_routing::RoutingRepo>,
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
    /// A sink for one alert source, built from the two handles this struct already holds.
    ///
    /// The `subject` is what its failure log says — a shared message across the sources would tell
    /// an operator that *something* failed to record and not which loop. Nothing is stored: a sink
    /// is two `Arc` clones and a `&'static str`, and giving each source its own is what lets it be
    /// named (ADR-092).
    fn alert_sink(&self, subject: &'static str) -> Arc<dyn alerts::sink::AlertSink> {
        Arc::new(alerts::sink::RecordingSink::new(
            self.history.clone(),
            self.notifier.clone(),
            subject,
        ))
    }

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

        let (metrics_tx, metrics_rx) = tokio::sync::mpsc::channel::<Arc<PollResult>>(
            result_ingest::RESULT_PERSIST_CHANNEL_CAP,
        );
        let (meta_tx, meta_rx) = tokio::sync::mpsc::channel::<result_ingest::MetaRecord>(
            result_ingest::RESULT_PERSIST_CHANNEL_CAP,
        );
        let (history_tx, history_rx) = tokio::sync::mpsc::channel::<result_ingest::HistoryRecord>(
            result_ingest::RESULT_PERSIST_CHANNEL_CAP,
        );
        tokio::spawn(result_ingest::run_vm_writer(
            metrics_rx,
            self.store.clone(),
            self.shutdown.clone(),
        ));
        tokio::spawn(result_ingest::run_pg_writer(
            meta_rx,
            history_rx,
            result_ingest::MetaStores {
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
                result_ingest::consume_results(
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
                result_ingest::consume_results_backfill(backfill, metrics_tx, meta_tx),
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
            self.events.clone(),
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
            flow_ingest::ensure_flow_schema(&flow_store).await;
            flow_ingest::bound_clickhouse_system_logs(&flow_store, self.flow_system_log_days).await;
            let (flow_tx, flow_rx) =
                tokio::sync::mpsc::channel::<FlowRow>(flow_ingest::FLOW_PERSIST_CHANNEL_CAP);
            tokio::spawn(flow_ingest::run_flow_writer(
                flow_rx,
                flow_store,
                self.shutdown.clone(),
            ));
            let flow_stream = Box::pin(self.bus.subscribe_flows().await?);
            spawn_cancellable(
                &self.shutdown,
                flow_ingest::consume_flows(
                    flow_stream,
                    flow_tx,
                    self.repo.clone(),
                    self.ipasn.clone(),
                ),
            );
        }
        let raw_flows = Box::pin(self.bus.subscribe_raw_flows().await?);
        spawn_cancellable(
            &self.shutdown,
            flow_ingest::consume_raw_flows(raw_flows, self.forward_handle.clone()),
        );
        Ok(())
    }

    /// The two dispatch loops: the per-node scheduler (working-set syncs + jittered dispatch) and
    /// the Meraki collection scheduler.
    fn spawn_schedulers(&mut self) {
        spawn_cancellable(
            &self.shutdown,
            scheduler::run_scheduler(
                self.repo.clone(),
                self.groups.clone(),
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
            alerts::config::run_alert_config_refresh(
                self.alerts.clone(),
                self.repo.clone(),
                self.thresholds.clone(),
                self.maintenance.clone(),
                self.groups.clone(),
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
                events: self.events.clone(),
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
                self.groups.clone(),
            ),
        );
        // Per-interface bandwidth utilisation (ADR-076). Leader-only for the same reason pool
        // coverage is: the engine's check state is process-local and two evaluators would double
        // every notification.
        spawn_cancellable(
            &self.shutdown,
            interface_util::run_interface_utilization_watch(
                self.store.clone(),
                self.repo.clone(),
                self.alerts.clone(),
                self.alert_sink("an interface-utilisation transition"),
            ),
        );
        // Node-level derived metrics (ADR-105) — memory, disk, swap and load percentages that no
        // device reports directly. Leader-only for the same reason the two loops around it are.
        spawn_cancellable(
            &self.shutdown,
            derived::run_derived_metric_watch(
                self.store.clone(),
                self.alerts.clone(),
                self.alert_sink("a derived-metric transition"),
            ),
        );
        spawn_cancellable(
            &self.shutdown,
            pool_coverage::run_pool_coverage_watch(
                self.coordinator.clone(),
                self.repo.clone(),
                self.meraki_devices.clone(),
                self.groups.clone(),
                self.alerts.clone(),
                self.alert_sink("a pool-coverage transition"),
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
        event_engine: None,
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
        bus_tls: None,
        upgrade: None,
        metrics: Some(metrics.clone()),
        started: std::time::SystemTime::now(),
        poller_logs: None,
    };
    serve(state, "0.0.0.0:8080", metrics, CancellationToken::new()).await
}

/// Everything [`run_fleet_health_timeline`] snapshots or prunes.
///
/// A struct rather than a tenth parameter: `clippy::too_many_arguments` is a design signal, not a
/// lint to silence (coding-conventions), and this loop is where every PostgreSQL retention subject
/// bottoms out — so it grows every time ADR-040 gains a row.
struct TimelineSources {
    repo: Arc<NodeRepo>,
    alerts: Arc<AlertManager>,
    history: Arc<AlertHistoryStore>,
    events: Arc<events::EventRepo>,
    dns_checks: Arc<dns_check::DnsCheckRepo>,
    neighbors: Arc<neighbors::NeighborRepo>,
    l3: Arc<l3::L3Repo>,
    analyses: Arc<analysis::AnalysisRepo>,
    rca_reports: Arc<rca::store::RcaRepo>,
    pollers: Arc<PollerRepo>,
}

/// Leader-only loop: snapshot the node-state counts every few minutes into PostgreSQL so the
/// dashboard can chart "degrading vs recovering" over time, and prune old state snapshots + alert
/// history + events past the retention window (the only place these tables are trimmed). Runs until
/// the shutdown token drops it. Spawned in `leader_work` (see `run_live`).
async fn run_fleet_health_timeline(sources: TimelineSources) {
    let TimelineSources {
        repo,
        alerts,
        history,
        events,
        dns_checks,
        neighbors,
        l3,
        analyses,
        rca_reports,
        pollers,
    } = sources;
    // Built once: the sweep borrows these every tick rather than cloning nine Arcs per tick.
    let targets = retention_sweep::Targets {
        repo: repo.clone(),
        history,
        events,
        dns_checks,
        neighbors,
        l3,
        analyses,
        rca_reports,
        pollers,
    };
    const SNAPSHOT_SECS: u64 = 300;
    loop {
        tokio::time::sleep(Duration::from_secs(SNAPSHOT_SECS)).await;
        // Re-read the operator's retention policy every tick (ADR-040), the same way the scheduler
        // re-reads the poll interval, so an edit in Settings applies on the next sweep without a
        // restart. A read failure degrades to the compiled defaults rather than skipping the prune.
        let retention = repo.get_retention_settings().await;
        // The **raw engine view**, deliberately — not the reconciled view the live surfaces show.
        //
        // `node_states()` only knows nodes the in-memory matcher has observed since this process
        // started, so for the first poll interval after a core restart a node that has not yet been
        // re-observed is simply absent here and lands in the timeline's `unknown` bucket. The live
        // surfaces (fleet summary, inventory, node detail) fill that window in from persisted state
        // so an operator is not told the fleet went dark because core was upgraded. This series
        // therefore *can* disagree with what those surfaces showed at the same instant, and that is
        // the choice: a historical record must say what was actually being monitored at that moment,
        // and during the post-restart window the honest answer is "core did not yet know". Back-
        // filling it from persisted state would make the timeline claim coverage that did not exist
        // and would erase the visible cost of a restart. If you are here because the dip looks like
        // a bug: it is the record of a real gap, and the reconciliation belongs on the live side.
        let states = alerts.node_states();
        let mut counts: HashMap<String, i64> = HashMap::new();
        for s in states.values() {
            *counts.entry(s.as_str().to_owned()).or_insert(0) += 1;
        }
        let snapshot: Vec<(String, i64)> = counts.into_iter().collect();
        if let Err(e) = repo.insert_state_snapshot(&snapshot).await {
            tracing::warn!(error = %e, "node-state snapshot failed");
        }
        retention_sweep::sweep(&targets, &retention).await;
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
    routing: Arc<l3_routing::RoutingRepo>,
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
        alerts::config::load_routing(&notifier, &notifications).await;
        alerts::config::load_mutes(&notifier, &maintenance, &repo, &group_repo).await;
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

/// Connect to NATS with retry so startup ordering doesn't matter.
///
/// `YAGRA_BUS_CA_FILE` pins the bus's certificate, and core needs it for the same reason a remote
/// poller does: once the bus is TLS (ADR-065), the certificate is one Yagra minted and signed
/// itself, so it is in no container's trust store. Core reads the variable with the same
/// empty-means-unset rule the poller applies — the compose file always sets it and leaves it blank
/// on the plaintext single-node bus, so an empty string has to mean "no CA" rather than "a file
/// called nothing".
///
/// ⚠️ Core connected with **no** CA for the whole life of the remote-poller feature, which is why
/// the documented procedure told operators to add `YAGRA_BUS_CA_FILE` to core and then had core
/// ignore it. Turning TLS on would have left core unable to reach its own bus.
async fn connect_bus(url: &str) -> anyhow::Result<NatsBus> {
    const MAX_ATTEMPTS: u32 = 30;
    let ca = std::env::var("YAGRA_BUS_CA_FILE")
        .ok()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .map(std::path::PathBuf::from);
    let mut attempt = 0;
    loop {
        match NatsBus::connect_opts(url, ca.as_deref()).await {
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

    /// This file's own source, for the structural assertions below.
    ///
    /// Read through [`module_source`], which removes each test-only item rather than truncating
    /// at the first one — so this file can grow a test-only declaration anywhere without quietly
    /// shortening what these tests read (ADR-089/090/091).
    fn production_source() -> String {
        crate::module_source::code("src", "main")
    }

    /// `run_live`'s body, from its signature to the closing brace at column 0.
    ///
    /// 🚨 **The floor is not decoration.** Everything below asks whether something is *absent* from
    /// this text, and an absence claim over an empty string is satisfied by nothing at all — the
    /// fifth-time failure ADR-089 named. So a caller that gets a slice which does not look like the
    /// wiring must fail here, loudly, rather than report "nothing wrong".
    fn run_live_body() -> String {
        let src = production_source();
        let from = src
            .split_once("async fn run_live(")
            .expect("run_live is declared in main.rs")
            .1;
        let body = match from.find("\n}\n") {
            Some(i) => &from[..i],
            None => from,
        };
        let lines = body.lines().count();
        assert!(
            lines >= 250,
            "run_live came back as {lines} lines — the slice is wrong, and every assertion over it \
             would pass for want of anything to check"
        );
        assert!(
            body.contains("let state = ApiState {"),
            "the slice does not contain the state it exists to build; it is not run_live's body"
        );
        body.to_owned()
    }

    /// **`run_live` wires; it does not start anything itself.**
    ///
    /// ADR-090's property. Every background task now starts inside the module that owns its
    /// subject — `ipasn::start_reload`, `authcallout::start`, `auth::start_sessions`,
    /// `upgrade::start`, `webtls::materialize_and_renew`, `poller_logs::start`,
    /// `host_collector::start`, `leader::run_leader_work` — or in [`LeaderTasks`], which is the
    /// same rule for the leader-gated half. That is what makes "is this safe on every core?" a
    /// question with a written answer next to the loop, instead of a comment in the wiring.
    ///
    /// ⚠️ What this does **not** check is whether a given task is gated *correctly*. HA is off by
    /// default and the lab runs one core, so a mis-gated task is wrong only where nobody is
    /// looking. A grep over comments was considered and rejected: a noisy check gets ignored.
    ///
    /// The needles are assembled at runtime — literals would match this test's own source if the
    /// test tail ever stopped being cut.
    #[test]
    fn run_live_starts_no_task_of_its_own() {
        let body = run_live_body();
        for needle in [
            format!("tokio::{}", "spawn"),
            format!("spawn_{}(", "cancellable"),
        ] {
            assert_eq!(
                body.matches(needle.as_str()).count(),
                0,
                "run_live starts a task with `{needle}`; it belongs in the module that owns the \
                 subject, or in LeaderTasks if it is leader-gated (ADR-090)"
            );
        }
    }
}
