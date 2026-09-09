// SPDX-License-Identifier: AGPL-3.0-only
//! What an **anonymous** request may reach (ADR-123 決定 5).
//!
//! Before this module, "public dashboard" meant every `RequireView` handler answered without a
//! credential — 76 read endpoints, so the node list, the event log and the shared board composed
//! for colleagues were all served to strangers. The name said one board; the implementation said
//! the whole read surface.
//!
//! It now means one board. The allow-list is **derived from what that board displays**: each
//! widget type declares the routes it reads (`web/src/dashboard/registry.tsx`, generated into
//! `widgetRoutes.json`), and the set open to an anonymous caller is the union over the widgets the
//! public board actually carries. Take a widget off the board and its routes close; the board is
//! the access-control list, which is why composing it takes `manage_system` rather than the
//! `manage_config` its shared-board sibling takes.
//!
//! **Three properties worth stating, because each is a way this could have been built wrong:**
//!
//! 1. **Closed is the default at every step.** No switch ⇒ closed. Switch on but no board saved ⇒
//!    closed (an empty widget list is an empty route set, not an open one). A layout core cannot
//!    parse ⇒ closed. A widget type the running binary does not know ⇒ that widget contributes
//!    nothing, rather than being skipped in a way that widens anything.
//! 2. **Route granularity, and that is a real limit.** `GET /api/v1/nodes` open means every node
//!    is readable; there is no way to say "this group only". The widgets on the public board are
//!    fleet-wide summaries, so this matches what they display — but a future "publish one group"
//!    would need a different mechanism, not another entry here.
//! 3. **This grants reads. It cannot grant a write.** `Require<P>` still refuses every permission
//!    but `View` on a public deployment (`api::extract`'s `OPEN_ON_PUBLIC_DASHBOARD`), deliberately
//!    kept as a second wall: if the derivation above were ever wrong, the worst it can do is
//!    expose a read.

use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock, RwLock};

/// The widget-type → routes table, generated from the WebUI registry.
///
/// ⚠️ Committed build output, like `web/src/api/schema.d.ts`. Regenerate with
/// `cd web && npm run generate:widget-routes`; CI fails on a diff. Editing it by hand edits an
/// access-control table without the declaration that produced it.
const WIDGET_ROUTES_JSON: &str = include_str!("../../../web/src/dashboard/widgetRoutes.json");

/// Routes an anonymous visitor may reach whenever the switch is on, whatever the board carries.
///
/// Exactly one entry, and it is the bootstrap: without the layout there is no page to draw, so a
/// public deployment that served nothing here would show a blank screen and no way to diagnose it.
/// It leaks the *shape* of the public board (which widgets, where) and no monitoring data.
///
/// ⚠️ `GET /api/v1/config` and `GET /api/v1/version` are **not** listed: they take no permission
/// guard at all (`security(())`), so they never reach this check.
const ALWAYS_OPEN: &[(&str, &str)] = &[("GET", "/api/v1/public-dashboard")];

/// The anonymous surface of this deployment: whether it is open at all, and to exactly what.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PublicAccess {
    enabled: bool,
    /// Every read is open, without consulting a board. **Never derivable from a layout** — the two
    /// constructors that set it say why, and both are outside a real deployment.
    unrestricted: bool,
    routes: HashSet<(String, String)>,
}

impl PublicAccess {
    /// Nothing is open. The value every failure path produces.
    #[must_use]
    pub fn closed() -> Self {
        Self::default()
    }

    /// Skeleton mode: every read open, because there is no database to hold a board.
    ///
    /// `run_skeleton` is the developer stack — no user store, so `POST /auth/login` answers 503 and
    /// there is no way to sign in. Closing reads there would leave the dev dashboard unreachable
    /// with no way to open it, which is why the flag it replaces was unconditionally `true`
    /// (`main.rs`'s own comment says so).
    ///
    /// ⚠️ Safe **only** because skeleton mode has no write side at all: `ApiState::admin` is `None`,
    /// so every mutating handler answers 503 before authorization is even reached. This must never
    /// be constructed on a path that could run against a real deployment — a live core reaches
    /// [`Self::derive`], and a database it cannot read reaches [`Self::closed`].
    #[must_use]
    pub fn skeleton_open() -> Self {
        Self {
            enabled: true,
            unrestricted: true,
            routes: HashSet::new(),
        }
    }

    /// Derive the surface from the switch and the saved public board.
    ///
    /// `layout` is the opaque document the WebUI owns; only widget **types** are read out of it.
    /// An unparseable or absent layout yields an empty route set — see property 1 in the module
    /// doc. A widget type this binary does not know (an older core reading a board composed by a
    /// newer one) contributes nothing, which is the N-1-safe direction: the widget renders empty
    /// rather than the core opening a route it cannot reason about.
    #[must_use]
    pub fn derive(enabled: bool, layout: Option<&Value>) -> Self {
        if !enabled {
            return Self::closed();
        }
        let table = widget_route_table();
        let mut routes = HashSet::new();
        for ty in widget_types(layout) {
            if let Some(rs) = table.get(ty.as_str()) {
                routes.extend(rs.iter().cloned());
            }
        }
        Self {
            enabled,
            // 🚨 Never `true` here. A board can only ever *name* routes; the unrestricted form is
            // reachable from `skeleton_open` alone, and a test below pins that.
            unrestricted: false,
            routes,
        }
    }

    /// Is anonymous viewing switched on at all? Drives `GET /api/v1/config`'s `public_dashboard`
    /// flag, which is what the WebUI branches on to decide whether to show a login screen.
    #[must_use]
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// May an anonymous caller reach `method path`?
    ///
    /// `path` is the **matched route pattern** (`/api/v1/nodes/{node_id}/interfaces`), never the
    /// concrete request path — axum's `MatchedPath` is where the caller gets it.
    #[must_use]
    pub fn allows(&self, method: &str, path: &str) -> bool {
        if !self.enabled {
            return false;
        }
        if self.unrestricted {
            return true;
        }
        if ALWAYS_OPEN.iter().any(|(m, p)| *m == method && *p == path) {
            return true;
        }
        self.routes
            .contains(&(method.to_string(), path.to_string()))
    }

    /// How many routes this board opens. For the confirmation dialog, which names the cost before
    /// the click (ADR-123 決定 1), and for the switch's audit trail.
    #[must_use]
    pub fn route_count(&self) -> usize {
        self.routes.len()
    }
}

/// Shared, swappable current value. Read on every guarded request, replaced by the refresh loop
/// and by the two writes (the switch, the board).
pub type PublicAccessHandle = Arc<RwLock<Arc<PublicAccess>>>;

/// A handle over a starting value.
#[must_use]
pub fn handle(initial: PublicAccess) -> PublicAccessHandle {
    Arc::new(RwLock::new(Arc::new(initial)))
}

/// Read the current value. A poisoned lock reports [`PublicAccess::closed`] rather than panicking:
/// a request path must not 500 because a writer panicked, and the safe answer is "closed".
#[must_use]
pub fn current(h: &PublicAccessHandle) -> Arc<PublicAccess> {
    h.read()
        .map(|g| Arc::clone(&g))
        .unwrap_or_else(|_| Arc::new(PublicAccess::closed()))
}

/// Replace the current value. A poisoned lock is recovered from — dropping the update would leave
/// a switched-off deployment serving, which is the direction that matters.
pub fn store(h: &PublicAccessHandle, next: PublicAccess) {
    match h.write() {
        Ok(mut g) => *g = Arc::new(next),
        Err(poisoned) => *poisoned.into_inner() = Arc::new(next),
    }
}

/// How often a core re-derives its anonymous surface from the database.
///
/// The switch and the board are both rows, and both are read on **every** guarded request — so
/// unlike `meraki_polling_enabled` (read once per polling cycle) this cannot query PostgreSQL at
/// the point of use. The cost of caching is a window: after an admin flips the switch or edits the
/// board, a **standby** core in an HA pair keeps the old answer for up to this long. The writing
/// core updates itself synchronously, so the operator's own session sees the change immediately.
///
/// 30s matches `alerts::config`'s refresh, which is the existing answer to the same question.
pub const REFRESH_SECS: u64 = 30;

/// Keep this core's anonymous surface in step with the database, for the process lifetime.
///
/// **Every-core, not leader-gated, and the reason is that this is not work — it is a cache.** A
/// standby answers `GET /api/v1/config` and serves the public board to anyone a load balancer sends
/// it, so a standby holding a stale answer is the failure: it would keep serving after the leader
/// closed the deployment. Both cores reading the same rows is idempotent and costs one query per
/// 30 seconds.
///
/// ⚠️ Started by the module that owns the subject, called from `run_live` — never spawned inside
/// `run_live` itself (`run_live_starts_no_task_of_its_own` pins that).
///
/// The first read happens inside the task, so a core serves **closed** for the moment before it
/// lands. That direction is deliberate: a deployment briefly refusing anonymous reads recovers by
/// itself, where the other order would serve for a moment on a deployment whose switch is off.
pub fn start(
    handle: PublicAccessHandle,
    settings: Arc<crate::repo::NodeRepo>,
    board: Arc<crate::dashboard::PublicDashboardRepo>,
    shutdown: &yagra_telemetry::CancellationToken,
) {
    yagra_telemetry::spawn_cancellable(shutdown, async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(REFRESH_SECS));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut last: Option<PublicAccess> = None;
        loop {
            tick.tick().await;
            let next = refresh(&settings, &board).await;
            if last.as_ref() != Some(&next) {
                tracing::info!(
                    enabled = next.enabled(),
                    routes = next.route_count(),
                    "public dashboard access surface changed"
                );
                last = Some(next.clone());
            }
            store(&handle, next);
        }
    });
}

/// Read the switch and the board, and derive the surface.
///
/// 🚨 A board that cannot be read is **not** an empty board: an unreachable database would
/// otherwise silently close a deployment that is meant to be public, and the operator would see an
/// outage with no error anywhere. So a read error keeps the switch's answer and an empty route set
/// — which is closed in effect, and logged — rather than being confused with "nothing is on the
/// board". The distinction matters for the log line, not for what is served.
async fn refresh(
    settings: &crate::repo::NodeRepo,
    board: &crate::dashboard::PublicDashboardRepo,
) -> PublicAccess {
    let enabled = settings.get_public_dashboard_enabled().await;
    if !enabled {
        return PublicAccess::closed();
    }
    match board.get_public().await {
        Ok(layout) => PublicAccess::derive(true, layout.as_ref()),
        Err(e) => {
            tracing::warn!(
                error = %e,
                "could not read the public dashboard layout — serving no anonymous routes this cycle"
            );
            PublicAccess::derive(true, None)
        }
    }
}

// ⚠️ There was a `refresh_now` here, for the two writers that must not wait out `REFRESH_SECS`.
// It had no callers: `api/public_dashboard.rs::apply` does the same job with what it already has —
// the switch value it just wrote and the board it re-reads through `AdminState` — so routing it
// through here would mean handing that module a `NodeRepo` it otherwise has no reason to hold.
// A helper with no user is dead code, so it went rather than being kept "for symmetry".

/// The generated table, parsed once.
fn widget_route_table() -> &'static HashMap<String, Vec<(String, String)>> {
    static TABLE: OnceLock<HashMap<String, Vec<(String, String)>>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let raw: HashMap<String, Vec<String>> = serde_json::from_str(WIDGET_ROUTES_JSON)
            .expect("widgetRoutes.json is a committed build output and must parse");
        raw.into_iter()
            .map(|(ty, routes)| {
                let parsed = routes
                    .iter()
                    .filter_map(|r| r.split_once(' '))
                    .map(|(m, p)| (m.to_string(), p.to_string()))
                    .collect();
                (ty, parsed)
            })
            .collect()
    })
}

/// Every widget type placed on the board, across all of its boards.
///
/// Handles both layout shapes the WebUI has shipped: v2+ (`{ boards: [{ widgets: [...] }] }`) and
/// v1 (`{ widgets: [...] }`). Anything else yields nothing.
fn widget_types(layout: Option<&Value>) -> Vec<String> {
    let Some(doc) = layout else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut take = |widgets: &Value| {
        if let Some(arr) = widgets.as_array() {
            for w in arr {
                if let Some(t) = w.get("type").and_then(Value::as_str) {
                    out.push(t.to_string());
                }
            }
        }
    };
    if let Some(boards) = doc.get("boards").and_then(Value::as_array) {
        for b in boards {
            if let Some(w) = b.get("widgets") {
                take(w);
            }
        }
    } else if let Some(w) = doc.get("widgets") {
        take(w);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn board(types: &[&str]) -> Value {
        json!({
            "version": 2,
            "boards": [{
                "id": "b1",
                "name": "Public",
                "widgets": types.iter().enumerate()
                    .map(|(i, t)| json!({ "instanceId": format!("w{i}"), "type": t }))
                    .collect::<Vec<_>>(),
            }],
        })
    }

    #[test]
    fn the_generated_table_covers_the_whole_catalog() {
        // A floor on what was PARSED. The healthy answer to every other test here is "refused",
        // and an empty table refuses everything — so without this, a build that failed to embed
        // the JSON would look like a working, very secure deployment.
        let table = widget_route_table();
        assert!(
            table.len() >= 40,
            "only {} widget types in the generated table — did the generator run?",
            table.len()
        );
        assert!(table.contains_key("status-summary"));
        assert!(table.contains_key("audit"));
    }

    #[test]
    fn the_switch_being_off_closes_everything_including_the_layout() {
        let a = PublicAccess::derive(false, Some(&board(&["status-summary"])));
        assert!(!a.enabled());
        assert!(!a.allows("GET", "/api/v1/fleet/summary"));
        // Even the bootstrap route: with the switch off there is no public page to draw.
        assert!(!a.allows("GET", "/api/v1/public-dashboard"));
    }

    #[test]
    fn the_switch_being_on_with_no_board_opens_only_the_layout_route() {
        // 🚨 The direction that matters. "Switched on but never composed" must mean nothing is
        // readable — not everything, which is what the previous implementation did.
        let a = PublicAccess::derive(true, None);
        assert!(a.enabled());
        assert_eq!(a.route_count(), 0);
        assert!(a.allows("GET", "/api/v1/public-dashboard"));
        assert!(!a.allows("GET", "/api/v1/fleet/summary"));
        assert!(!a.allows("GET", "/api/v1/nodes"));
    }

    #[test]
    fn a_widget_opens_exactly_the_routes_it_declares() {
        let a = PublicAccess::derive(true, Some(&board(&["status-summary"])));
        assert!(a.allows("GET", "/api/v1/fleet/summary"));
        // Not a route some *other* widget reads…
        assert!(!a.allows("GET", "/api/v1/events"));
        // …and not another method on the same path.
        assert!(!a.allows("POST", "/api/v1/fleet/summary"));
    }

    #[test]
    fn removing_a_widget_closes_its_routes() {
        // The property the whole design rests on: the board *is* the allow-list, so editing it
        // narrows the anonymous surface without anyone editing a list of routes.
        let both = PublicAccess::derive(true, Some(&board(&["status-summary", "event-feed"])));
        assert!(both.allows("GET", "/api/v1/events"));
        let one = PublicAccess::derive(true, Some(&board(&["status-summary"])));
        assert!(!one.allows("GET", "/api/v1/events"));
        assert!(one.allows("GET", "/api/v1/fleet/summary"));
    }

    #[test]
    fn a_widget_type_this_binary_does_not_know_opens_nothing() {
        // N-1: a board composed by a newer core carries a widget this one has never heard of. The
        // safe reading is "it contributes no routes", never "skip the check for it".
        let a = PublicAccess::derive(true, Some(&board(&["status-summary", "not-a-real-widget"])));
        assert_eq!(a.route_count(), 1);
        assert!(a.allows("GET", "/api/v1/fleet/summary"));
    }

    #[test]
    fn an_unparseable_layout_opens_nothing() {
        for junk in [
            json!(null),
            json!(42),
            json!("boards"),
            json!({"boards": "no"}),
        ] {
            let a = PublicAccess::derive(true, Some(&junk));
            assert_eq!(a.route_count(), 0, "junk layout opened routes: {junk}");
        }
    }

    #[test]
    fn the_v1_layout_shape_still_parses() {
        // v1 was a flat `{ widgets: [...] }`. A deployment upgrading with an old public board must
        // not silently get an empty allow-list — that reads as "the board broke", not "we closed".
        let v1 = json!({ "widgets": [{ "instanceId": "a", "type": "status-summary" }] });
        assert!(PublicAccess::derive(true, Some(&v1)).allows("GET", "/api/v1/fleet/summary"));
    }

    #[test]
    fn widgets_are_collected_across_every_board() {
        let two = json!({
            "version": 2,
            "boards": [
                { "id": "a", "name": "One", "widgets": [{ "instanceId": "x", "type": "status-summary" }] },
                { "id": "b", "name": "Two", "widgets": [{ "instanceId": "y", "type": "event-feed" }] },
            ],
        });
        let a = PublicAccess::derive(true, Some(&two));
        assert!(a.allows("GET", "/api/v1/fleet/summary"));
        assert!(a.allows("GET", "/api/v1/events"));
    }

    #[test]
    fn the_handle_round_trips_and_reads_closed_by_default() {
        let h = handle(PublicAccess::closed());
        assert!(!current(&h).enabled());
        store(
            &h,
            PublicAccess::derive(true, Some(&board(&["status-summary"]))),
        );
        assert!(current(&h).allows("GET", "/api/v1/fleet/summary"));
        store(&h, PublicAccess::closed());
        assert!(!current(&h).allows("GET", "/api/v1/fleet/summary"));
    }

    #[test]
    fn no_board_can_produce_the_unrestricted_form() {
        // 🚨 The failure this exists for: `skeleton_open` opens every read, and it is only safe
        // because skeleton mode has no write side. If a layout could reach that state — a widget
        // type spelled `*`, an empty board read as "no restriction" — a real deployment would open
        // its whole read surface, which is exactly what ADR-123 closed.
        let boards = [
            board(&[]),
            board(&["status-summary"]),
            board(&["*"]),
            json!({ "boards": [] }),
            json!({ "widgets": [] }),
        ];
        for b in boards {
            let a = PublicAccess::derive(true, Some(&b));
            assert!(
                !a.allows("GET", "/api/v1/nodes"),
                "a board opened a route no widget on it declares: {b}"
            );
        }
        assert!(PublicAccess::skeleton_open().allows("GET", "/api/v1/nodes"));
    }

    #[test]
    fn the_audit_widget_would_open_the_audit_log_which_is_why_the_catalog_refuses_it() {
        // Not a rule this module enforces — the WebUI catalog is what keeps the widget off the
        // public board (it takes `view_audit`, which `Require<P>` refuses anonymously whatever
        // this list says). Pinned here so the two halves cannot drift apart unnoticed: if this
        // ever stops being true, the catalog filter is filtering on the wrong thing.
        let a = PublicAccess::derive(true, Some(&board(&["audit"])));
        assert!(a.allows("GET", "/api/v1/audit"));
    }
}
