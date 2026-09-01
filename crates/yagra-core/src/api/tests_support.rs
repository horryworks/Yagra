// SPDX-License-Identifier: AGPL-3.0-only
//! Shared test fixtures for the API modules.
//!
//! Skeleton-mode [`ApiState`] builders, so each domain module's tests (and the cross-cutting
//! [`super::extract`] / [`super::route_table`] ones) construct state the same way instead of each
//! repeating the ~20-field literal. Compiled only under `cfg(test)`.

use super::ApiState;
use crate::alerts::AlertManager;
use crate::auth::{LoginThrottle, SessionStore};
use crate::repo::StaticNodeList;
use crate::sink::InMemorySink;
use crate::store::MetricStore;
use std::sync::Arc;

/// Skeleton-mode state with `public_dashboard` set as given and no write side.
fn base(store: Arc<dyn MetricStore>, public_dashboard: bool) -> ApiState {
    ApiState {
        store,
        logs: None,
        flows: None,
        ipasn: crate::ipasn::empty_handle(),
        nodes: Arc::new(StaticNodeList::demo()),
        alerts: Arc::new(AlertManager::new()),
        host_sample: Arc::new(std::sync::Mutex::new(None)),
        admin: None,
        sessions: Arc::new(SessionStore::new()),
        login_throttle: Arc::new(LoginThrottle::new()),
        history: None,
        ack: None,
        event_engine: None,
        public_dashboard,
        is_leader: Arc::new(std::sync::atomic::AtomicBool::new(true)),
        ldap: None,
        oidc: None,
        oidc_flight: Arc::new(crate::oidc::OidcFlight::new()),
        enable_mcp: false,
        rca: None,
        webtls: None,
        bus_tls: None,
        upgrade: None,
        upgrade_bus: None,
        // No Prometheus recorder is installed in the test binary, so the support bundle records
        // the scrape as an omission rather than carrying one.
        metrics: None,
        started: std::time::SystemTime::now(),
        poller_logs: None,
    }
}

/// Public-dashboard state: read endpoints are open, so a test can exercise a route without
/// minting a token first.
pub(crate) fn public_state() -> ApiState {
    base(Arc::new(InMemorySink::default()), true)
}

/// Auth-required state: every endpoint, including reads, needs a valid session. Mint a token off
/// `state.sessions` for the role under test.
pub(crate) fn private_state() -> ApiState {
    base(Arc::new(InMemorySink::default()), false)
}

// ── Live mode: the write side, over a real PostgreSQL (ADR-115) ─────────────────────────────────

/// Live-mode state over the database the test harness handed us — the half [`base`] leaves out.
///
/// [`public_state`] and [`private_state`] are *skeleton* mode: `admin` is `None`, so the 193
/// handlers that take [`super::extract::Admin`] answer `503` and their bodies never run. Measured
/// on 2026-09-01, that left the whole `api/` suite asserting refusals — 401 ninety-six times, 503
/// sixty-nine, 403 forty-nine — against **no** `201` and **no** `202` anywhere, while the handlers
/// return twenty-nine and nine of them. This builds the other half, so a write can be *accepted*.
///
/// ## What is faked — and this is the whole list
///
/// Five handles cannot come from a pool. They are named here rather than left to be discovered,
/// because the compiler cannot tell a field wired differently from production from one wired the
/// same: it only insists every field is named, since [`super::AdminState`] has no `Default`.
///
/// | handle | stand-in | what a test therefore may not conclude |
/// |---|---|---|
/// | the bus | `InMemoryBus` | a published job reaches no poller; "poll now" means "the job was built" |
/// | the TSDB | [`InMemorySink`] | a metric read answers *empty*, not *wrong* — never read one as a fact |
/// | Redis | `VolatileStore::disabled` | the liveness mirror is a no-op; PostgreSQL still holds the durable copy |
/// | the KEK | `StaticKeyProvider::single` | a sealed value round-trips inside this test and nowhere else |
/// | the notifier | `Notifier::with_default(None)` | nothing is delivered — see below |
///
/// 🚨 **The notifier is never `Notifier::from_env()`.** That reader takes `YAGRA_WEBHOOK_URL` and
/// `YAGRA_SMTP_*` from the process environment, so a developer who happens to have one exported
/// would make these tests deliver a real webhook or a real mail. `with_default(None)` has no route
/// at all and therefore cannot. Everything else here is the production type over `pool`.
///
/// ## Seeded like boot, minus the demo nodes
///
/// `run_live` seeds four things before it serves. This does three of them, in the same order:
/// built-in profiles, `app_settings`, and the MIB catalogue. It deliberately skips
/// `seed_demo_nodes_if_empty` — that one inserts three nodes, and a fixture starting with three
/// nodes cannot express "the inventory is empty", which is where most of these tests begin.
///
/// ## What it does not start
///
/// No background task. Every constructor below spawns nothing (`forward::prepare` hands back a
/// runner that is dropped here, the coordinator sweeps only when told), so a test is
/// deterministic. The handlers that *do* spawn — report generation, analysis jobs, discovery
/// sweeps — are still asynchronous: assert the accepted `202` and the row that records it, not the
/// outcome.
pub(crate) async fn live_state(pool: sqlx::PgPool) -> ApiState {
    use crate::alerts::Notifier;
    use crate::secrets::CredentialStore;
    use crate::volatile::VolatileStore;

    let kek: crate::secrets::Kek = Arc::new(yagra_secrets::StaticKeyProvider::single([7u8; 32]));
    // Erased once, here, so no call site below has to rely on a coercion firing in an inferred
    // position. The three trait objects are the same bus.
    let bus = Arc::new(yagra_bus::InMemoryBus::new(64));
    let sync_bus: Arc<dyn yagra_bus::SyncBus> = bus.clone();
    let discovery_bus: Arc<dyn yagra_bus::DiscoveryBus> = bus.clone();
    let upgrade_bus: Arc<dyn yagra_bus::UpgradeBus> = bus;
    let store: Arc<dyn MetricStore> = Arc::new(InMemorySink::default());
    let repo = Arc::new(crate::repo::NodeRepo::from_pool(pool.clone()));

    // Boot's seeding, in boot's order. Demo nodes excluded — see the doc above.
    repo.seed_builtin_profiles().await.expect("seed profiles");
    repo.seed_app_settings(
        crate::config::DEFAULT_POLL_INTERVAL_SECS,
        crate::flowstore::DEFAULT_FLOW_RETENTION_DAYS,
    )
    .await
    .expect("seed settings");
    let mib = Arc::new(crate::mib::MibRepo::new(pool.clone()));
    mib.seed_builtin().await.expect("seed mib");

    let alerts = Arc::new(AlertManager::new());
    let history = Arc::new(crate::history::AlertHistoryStore::new(pool.clone()));
    let group_repo = Arc::new(crate::groups::GroupRepo::new(pool.clone()));
    let events_repo = Arc::new(crate::events::EventRepo::new(pool.clone()));
    let poller_repo = Arc::new(crate::pollers::PollerRepo::new(pool.clone()));
    let l3_repo = Arc::new(crate::l3::L3Repo::new(pool.clone()));
    let topo_link_repo = Arc::new(crate::topology_links::TopoLinkRepo::new(pool.clone()));
    let scheduler_stats = Arc::new(crate::scheduler::SchedulerStats::default());
    let classifier = Arc::new(crate::classification::Classifier::empty());
    let creds = Arc::new(CredentialStore::new(pool.clone(), kek.clone()));
    let collection = Arc::new(crate::collection::CollectionRepo::new(pool.clone()));
    let url_checks = Arc::new(crate::url_check::UrlCheckRepo::new(pool.clone()));
    let dns_checks = Arc::new(crate::dns_check::DnsCheckRepo::new(pool.clone()));
    let meraki_devices = Arc::new(crate::meraki::MerakiDeviceRepo::new(pool.clone()));
    let reports_repo = Arc::new(crate::reports::ReportsRepo::new(pool.clone()));
    let audit_repo = Arc::new(crate::audit::AuditRepo::new(pool.clone()));
    let forward_store = Arc::new(crate::forward_store::ForwardStore::new(
        pool.clone(),
        kek.clone(),
    ));
    let llm_repo = Arc::new(crate::rca::store::RcaRepo::new(pool.clone(), kek.clone()));

    // The engine's two writer channels are `None`. Their readers are leader-only background tasks
    // in `run_live`, and starting them here would make a test concurrent with itself — so an event
    // that matches a rule is matched but not persisted. The events tests target rule and source
    // CRUD, which reach `EventRepo` directly.
    let event_engine = Arc::new(crate::events::EventEngine::new(
        events_repo.clone(),
        alerts.clone(),
        Arc::new(crate::alerts::sink::RecordingSink::new(
            history.clone(),
            Arc::new(Notifier::with_default(None)),
            "a test event alert",
        )),
        None,
        None,
    ));

    let analysis = Arc::new(crate::analysis::AnalysisRunner::new(
        Arc::new(crate::analysis::AnalysisRepo::new(pool.clone())),
        crate::analysis::AnalysisStores {
            store: store.clone(),
            nodes: repo.clone(),
            groups: group_repo.clone(),
            events: events_repo.clone(),
            logs: None,
            flows: None,
            ipasn: crate::ipasn::empty_handle(),
            topo: crate::topology_projection::TopologySources {
                links: topo_link_repo.clone(),
                pollers: poller_repo.clone(),
                l3: l3_repo.clone(),
                nodes: repo.clone(),
            },
        },
    ));

    let admin = Arc::new(super::AdminState {
        repo: repo.clone(),
        creds: creds.clone(),
        users: Arc::new(crate::auth::UserStore::new(pool.clone())),
        thresholds: Arc::new(crate::thresholds::ThresholdStore::new(pool.clone())),
        collection: collection.clone(),
        notifications: Arc::new(crate::notifications::NotificationRepo::new(
            pool.clone(),
            kek.clone(),
        )),
        mib,
        discovery: Arc::new(crate::discovery::DiscoveryRunner::new(
            discovery_bus,
            classifier.clone(),
        )),
        maintenance: Arc::new(crate::maintenance::MaintenanceRepo::new(pool.clone())),
        classification: Arc::new(crate::classification::ClassificationRepo::new(pool.clone())),
        classifier,
        groups: group_repo,
        audit: audit_repo.clone(),
        dashboards: Arc::new(crate::dashboard::DashboardRepo::new(pool.clone())),
        shared_dashboard: Arc::new(crate::dashboard::SharedDashboardRepo::new(pool.clone())),
        prefs: Arc::new(crate::preferences::UserPrefsRepo::new(pool.clone())),
        scheduler_stats: scheduler_stats.clone(),
        dispatcher: Arc::new(crate::scheduler::PollDispatcher::new(
            crate::scheduler::PollDispatcherStores {
                bus: sync_bus.clone(),
                creds,
                collection,
                url_checks: url_checks.clone(),
                dns_checks: dns_checks.clone(),
                meraki_devices: meraki_devices.clone(),
                settings: repo.clone(),
                l3: l3_repo.clone(),
                env_community: None,
                interval_secs: crate::config::DEFAULT_POLL_INTERVAL_SECS,
            },
        )),
        analysis: analysis.clone(),
        reports: Arc::new(crate::reports::ReportRunner::new(
            reports_repo.clone(),
            store.clone(),
            repo.clone(),
            alerts.clone(),
            history.clone(),
        )),
        reports_repo,
        url_checks,
        dns_checks,
        neighbors: Arc::new(crate::neighbors::NeighborRepo::new(pool.clone())),
        l3: l3_repo,
        arp: Arc::new(crate::arp::ArpRepo::new(pool.clone())),
        discovered: Arc::new(crate::arp::DiscoveredRepo::new(pool.clone())),
        topology_links: topo_link_repo,
        link_overrides: Arc::new(crate::link_overrides::LinkOverrideRepo::new(pool.clone())),
        meraki_orgs: Arc::new(crate::meraki::MerakiOrgRepo::new(pool.clone())),
        meraki_devices,
        events: events_repo,
        coordinator: Arc::new(crate::coordinator::Coordinator::new(
            sync_bus,
            Arc::new(VolatileStore::disabled()),
            Some(poller_repo.clone()),
            scheduler_stats,
            Some(store.clone()),
        )),
        pollers: poller_repo,
        api_tokens: Arc::new(crate::apitokens::ApiTokenStore::new(
            pool.clone(),
            crate::config::DEFAULT_PAT_OIDC_IDLE_DAYS,
        )),
        forward: forward_store.clone(),
        // The runner half is dropped on purpose: nothing here delivers, which is the point.
        forward_handle: crate::forward::prepare(forward_store).0,
        llm: llm_repo.clone(),
        config_bundle: Arc::new(crate::config_bundle::ConfigBundleRepo::new(pool.clone())),
        support: Arc::new(crate::support_bundle::SupportRepo::new(pool.clone())),
    });

    ApiState {
        store,
        logs: None,
        flows: None,
        ipasn: crate::ipasn::empty_handle(),
        nodes: repo.clone(),
        alerts: alerts.clone(),
        host_sample: Arc::new(std::sync::Mutex::new(None)),
        admin: Some(admin),
        sessions: Arc::new(SessionStore::new()),
        login_throttle: Arc::new(LoginThrottle::new()),
        history: Some(history),
        ack: Some(Arc::new(crate::ack::AckRepo::new(pool.clone()))),
        event_engine: Some(event_engine),
        public_dashboard: false,
        is_leader: Arc::new(std::sync::atomic::AtomicBool::new(true)),
        ldap: Some(Arc::new(crate::ldap::LdapRepo::new(
            pool.clone(),
            kek.clone(),
        ))),
        oidc: Some(Arc::new(crate::oidc::OidcRepo::new(
            pool.clone(),
            kek.clone(),
        ))),
        oidc_flight: Arc::new(crate::oidc::OidcFlight::new()),
        enable_mcp: false,
        rca: Some(Arc::new(crate::rca::orchestrator::RcaOrchestrator::new(
            llm_repo, repo, alerts, analysis, audit_repo,
        ))),
        webtls: Some(crate::webtls::open(pool.clone(), kek.clone())),
        bus_tls: Some(crate::bus_cert::open(pool.clone(), kek)),
        upgrade: Some(crate::upgrade::open(pool)),
        upgrade_bus: Some(upgrade_bus),
        // No Prometheus recorder in the test binary, and no bus collector: both are recorded as
        // omissions by the support bundle rather than faked into looking present.
        metrics: None,
        started: std::time::SystemTime::now(),
        poller_logs: None,
    }
}

/// A bearer token for `role`, with fleet-wide scope.
///
/// One spelling of `sessions.issue(...)`, which is currently written out at 39 call sites across
/// 19 files. New tests use this; the existing 39 are deliberately left alone, because rewriting
/// them changes no behaviour and would bury the tests that do.
pub(crate) fn token(st: &ApiState, role: yagra_common::Role) -> String {
    st.sessions.issue(
        uuid::Uuid::new_v4(),
        yagra_common::Principal::new(role, yagra_common::Scope::All),
        "fixture",
    )
}

/// An Admin token that can see only `groups` — what a caller under ADR-014 group scoping presents.
///
/// Admin on purpose: the interesting question is whether *scope* narrows the answer, and a role
/// that cannot reach the endpoint would answer it with a 403 instead.
pub(crate) fn scoped_token(st: &ApiState, groups: &[uuid::Uuid]) -> String {
    st.sessions.issue(
        uuid::Uuid::new_v4(),
        yagra_common::Principal::new(
            yagra_common::Role::Admin,
            yagra_common::Scope::groups(groups.iter().map(ToString::to_string)),
        ),
        "scoped-fixture",
    )
}

/// One request against the whole router, as a browser would make it.
///
/// The full [`super::router`] rather than a domain's own `routes()`, so the request also passes
/// `audit_mw` — which is what writes the audit row a mutating call is supposed to leave, and what
/// bumps the process-wide config generation. A test that bypassed it would prove the handler and
/// not the endpoint.
///
/// Returns the status and the parsed body, or `Value::Null` for an empty one (a `204`).
pub(crate) async fn send(
    st: &ApiState,
    method: &str,
    uri: &str,
    bearer: &str,
    body: Option<serde_json::Value>,
) -> (axum::http::StatusCode, serde_json::Value) {
    use tower::ServiceExt as _;
    let req = axum::http::Request::builder()
        .method(method)
        .uri(uri)
        .header(
            axum::http::header::AUTHORIZATION,
            format!("Bearer {bearer}"),
        )
        .header(axum::http::header::CONTENT_TYPE, "application/json");
    let req = match body {
        Some(v) => req.body(axum::body::Body::from(v.to_string())),
        None => req.body(axum::body::Body::empty()),
    }
    .expect("request");
    let res = super::router(st.clone())
        .oneshot(req)
        .await
        .expect("response");
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 22)
        .await
        .expect("body");
    let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, json)
}

/// A token for an account that actually exists in `users`, and its id.
///
/// [`token`] mints a session for a random uuid, which is enough for every permission check but not
/// for the handlers that look the account up — `PUT /dashboard`, `PUT /preferences` and
/// `POST /api-tokens` answer `404`/`400` for a caller whose row is missing. Those want this one.
pub(crate) async fn account_token(
    st: &ApiState,
    username: &str,
    role: yagra_common::Role,
) -> (String, uuid::Uuid) {
    let admin = st.admin.as_ref().expect("live state");
    let id = match admin
        .users
        .create(username, "correct horse battery staple", role.key())
        .await
        .expect("create user")
    {
        crate::auth::UserCreateOutcome::Created(id) => id,
        crate::auth::UserCreateOutcome::UsernameTaken => panic!("fixture reused a username"),
    };
    let tok = st.sessions.issue(
        id,
        yagra_common::Principal::new(role, yagra_common::Scope::All),
        username,
    );
    (tok, id)
}
