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
