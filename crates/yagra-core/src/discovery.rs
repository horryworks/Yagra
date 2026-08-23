// SPDX-License-Identifier: AGPL-3.0-only
//! Discovery orchestration (Phase C) — core side.
//!
//! Turns a scan request into a [`DiscoveryJob`] on the bus (the poller does the actual ICMP /
//! SNMP sweep), correlates the [`DiscoveryResult`]s back by `scan_id`, and classifies each
//! found device into a suggested device profile (via the core [`Classifier`](crate::classification)
//! — authoritative `sysObjectID` rules; vendor/model are pre-filled from `yagra_discovery::identify`).
//! The poller
//! publishes **cumulative** partial results as it sweeps, so a scan's status carries live
//! progress (probed/total + the address currently being probed). The operator reviews candidates
//! and imports the ones they want as real nodes (reusing the create-node path); nothing is added
//! automatically.
//!
//! ## Scan state is in memory, and that is a decision with a price (ADR-068)
//!
//! Scans are short-lived — the sweep is capped at 1024 targets, so the longest legitimate one runs
//! for minutes, not days — and their candidates are not an asset until they are imported. That is
//! why this is a `HashMap` and not a table: the threshold for persistence is *how long the work
//! lives*, not *whether a list of it is wanted*.
//!
//! Two consequences follow, and both are load-bearing rather than incidental:
//!
//! 1. **The map must be evicted** ([`evict`]). Until ADR-068 it never was, because nothing listed
//!    it — a scan registered here stayed for the life of the process.
//! 2. **A core restart orphans a running sweep.** The poller keeps sweeping, its results arrive for
//!    a `scan_id` this process has never heard of, and [`DiscoveryRunner::run_consumer`] drops them.
//!    The API answers 404, which the WebUI must render as *"this core does not know that scan"* —
//!    never as an empty page. Do not paper over this by inventing a scan record on an unknown
//!    result: that would report progress for a sweep whose targets and credentials are unknown.
//!
//! ⚠️ **`run_consumer` runs on the leader only** (`main.rs`'s `LeaderTasks`), while every core serves
//! the API. So a standby that accepted `POST /discovery/scan` would put real traffic on the wire and
//! never see a single result — which is why that route takes the `Leader` guard.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Utc};
use futures::stream::{Stream, StreamExt};
use serde::Serialize;
use uuid::Uuid;
use yagra_bus::{
    DiscoveryBus, DiscoveryCancel, DiscoveryCredential, DiscoveryJob, DiscoveryResult,
};

use crate::classification::Classifier;

/// Per-probe timeout pushed to the poller (ms).
const SCAN_TIMEOUT_MS: u32 = 2000;

/// What a sweep does with an address that does not answer ICMP (ADR-068 Increment 3).
///
/// A named pair rather than a `bool` parameter because [`DiscoveryRunner::start`] is called from
/// eleven places and `start(targets, communities, creds, pool, false)` says nothing at any of them
/// — `coding-conventions.md`'s "types over strings" applied to the case where the string is a
/// boolean.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SilentTargets {
    /// Stop at the ICMP timeout. The default, and what makes a /24 sweep quick: the addresses that
    /// answer nothing are the overwhelming majority, and each one used to cost several rate-limited
    /// SNMP attempts as well.
    Skip,
    /// Try every credential anyway — the escape hatch for a device that filters ICMP and answers
    /// SNMP. Slow in proportion to the size of the range, which is why it is a choice.
    ProbeSnmp,
}

/// How long a finished scan stays listable.
///
/// ⚠️ **This is also the Discovery-queue widget's window.** [`DiscoveryRunner::recent_candidates`]
/// reads the candidates of *every* retained scan, so whatever is evicted here leaves that widget
/// too. The value is therefore chosen for the widget, not for memory — 20 scans of at most 1024
/// candidates is not a memory problem, and picking a shorter window to "tidy up" would silently
/// empty a dashboard panel.
const FINISHED_TTL: Duration = Duration::from_secs(6 * 60 * 60);

/// Hard cap on retained scans. Only **finished** ones are dropped to honour it — evicting a running
/// scan would lose the operator's only handle on a sweep that is still putting traffic on the wire.
/// A deployment that somehow accumulates this many concurrently-running scans keeps them all until
/// [`RUNNING_MAX_AGE`] retires them.
const MAX_SCANS: usize = 20;

/// A scan still marked running after this is dropped: its poller died, or its final result was
/// lost. Comfortably longer than the slowest legitimate sweep (1024 targets, ~22s per target in the
/// worst credential-probe case, 16 at a time ⇒ well under an hour), so this can only catch a sweep
/// that is genuinely never going to report.
const RUNNING_MAX_AGE: Duration = Duration::from_secs(2 * 60 * 60);

/// One device a scan found, with a suggested profile for the operator to confirm on import.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct Candidate {
    pub address: String,
    pub reachable: bool,
    pub sysdescr: Option<String>,
    pub sysname: Option<String>,
    /// `sysObjectID` (dotted) if it answered SNMP — the authoritative device-type signal the
    /// classifier prefers. Shown to the operator and useful for authoring rules.
    pub sysobjectid: Option<String>,
    /// Suggested device profile, resolved **server-side** via the classification rules (by
    /// sysObjectID prefix, else sysDescr regex, else "Generic SNMP" when SNMP answered). An id,
    /// not a name, so the UI binds it robustly even if the profile was renamed.
    pub suggested_profile_id: Option<Uuid>,
    /// Maker / model best-effort parsed from sysDescr (editable on import) — pre-fills the
    /// node's descriptive metadata so the imported node displays "name (addr) (vendor) (model)".
    pub vendor: Option<String>,
    pub model: Option<String>,
    /// The stored credential that answered SNMP, by reference (never the value) — the UI
    /// preselects it on import so the working secret is bound automatically.
    pub matched_credential_id: Option<Uuid>,
}

/// Where a scan is in its life (ADR-068).
///
/// Deliberately has **no `Unknown` variant**, unlike the enums built by `stored_enum::token_enum!`:
/// those degrade a token a *newer writer* put in a database column, and this value is never read
/// back from storage — it only ever travels outward. The corresponding defensiveness lives on the
/// TypeScript side, which narrows the wire value and renders anything it does not recognise
/// neutrally. ⚠️ Rendering an unrecognised state as a failure is a real bug this codebase has
/// already shipped once (report runs, painted red by a `switch` with a `default:` arm).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DiscoveryScanState {
    /// Accepted and published to the bus, and no poller has said anything about it yet.
    ///
    /// **Core's own word — nothing on the bus ever reports it.** `yagra-poller`'s sweep loop is
    /// strictly sequential, so a job can sit on its subscription for the length of the sweep ahead
    /// of it; and a job published to a pool whose pollers are all gone sits there for good. Both
    /// used to render as `Running · 0/254`, which made "queued behind another sweep", "no poller
    /// ever took this" and "sweeping, and everything out there is silent" one sentence on screen.
    ///
    /// Non-terminal on purpose, so [`RUNNING_MAX_AGE`] retires a sweep nobody ever picks up rather
    /// than leaving it listed forever.
    ///
    /// ⚠️ Against an N-1 poller this reads early: that poller sends nothing until its first chunk
    /// completes, so a sweep it really is running shows as queued for the length of one chunk
    /// (~30s). That is a bounded wrong replacing an unbounded one.
    Queued,
    /// Sweeping; the poller is reporting progress.
    Running,
    /// A stop was requested and published; the poller has not confirmed yet.
    ///
    /// **Not the same thing as stopped, and the distinction is the honest one.** Core publishes the
    /// stop and cannot know it arrived: the sweep may be held by a poller too old to subscribe, or
    /// the message may simply be late. A scan sits here until its poller reports — which is either
    /// `cancelled: true` (it stopped) or a plain terminal result (it finished first). ⚠️ **Nothing
    /// times this out into [`Self::Cancelled`].** That would be inventing the confirmation the state
    /// exists to wait for; a stop that is never confirmed stays visible as unconfirmed until
    /// [`evict`] retires it.
    Cancelling,
    /// The poller confirmed it stopped early. `probed < total` is the evidence.
    Cancelled,
    /// The sweep ran to completion.
    Done,
}

impl DiscoveryScanState {
    /// Whether the scan will produce no further results.
    ///
    /// `Cancelling` is **not** terminal: the poller may still report, and that report is what
    /// distinguishes "stopped" from "finished before the stop arrived".
    #[must_use]
    pub fn is_terminal(self) -> bool {
        match self {
            Self::Cancelled | Self::Done => true,
            Self::Queued | Self::Running | Self::Cancelling => false,
        }
    }
}

/// A scan's current status returned by the API.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct ScanStatus {
    pub scan_id: Uuid,
    /// Terminal or not. Kept alongside `state` because it is part of the published contract (the
    /// MCP `get_config(kind="discovery_scan")` tool serves this type straight through), and derived
    /// from `state` rather than stored, so the two cannot disagree.
    pub done: bool,
    pub state: DiscoveryScanState,
    /// Targets probed so far, and the sweep's total. An address counts once a probe has been
    /// addressed to it, which is not the same as the sweep having finished identifying it.
    pub probed: u32,
    pub total: u32,
    /// The address the sweep is currently at (the next unprobed target), while running.
    pub scanning: Option<String>,
    /// When the sweep was accepted (RFC 3339) — RFC 3339 rather than epoch millis to match the
    /// discovered-endpoint rows this API already serves.
    pub started_at: String,
    /// When a result last moved this scan forward (RFC 3339).
    pub updated_at: String,
    /// The pool the job was **actually published to**; `null` for the global subject.
    pub pool: Option<String>,
    pub candidates: Vec<Candidate>,
}

/// One row of the scan list — everything [`ScanStatus`] has except the candidates themselves.
///
/// The omission is the point: 20 retained scans of up to 1024 candidates each would make listing
/// them far more expensive than the question deserves. A caller that wants a scan's candidates asks
/// for that scan.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct ScanSummary {
    pub scan_id: Uuid,
    pub state: DiscoveryScanState,
    pub probed: u32,
    pub total: u32,
    /// How many devices answered so far.
    pub candidate_count: u32,
    pub started_at: String,
    pub updated_at: String,
    /// The pool the job was **actually published to**; `null` for the global subject.
    pub pool: Option<String>,
}

struct ScanState {
    targets: Vec<IpAddr>,
    state: DiscoveryScanState,
    probed: u32,
    candidates: Vec<Candidate>,
    started_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    /// The route [`DiscoveryRunner::start`] actually published on — **not** the pool the caller
    /// asked for.
    ///
    /// ⚠️ The two differ: `api::discovery` falls back to the global subject when the requested pool
    /// has no live poller, so storing the request would make Increment 2 address a cancel at a pool
    /// that never received the job.
    pool: Option<String>,
}

impl ScanState {
    fn new(targets: Vec<IpAddr>, pool: Option<String>, now: DateTime<Utc>) -> Self {
        Self {
            targets,
            // Not `Running`: at this point the job has been published and nothing has confirmed
            // that any poller holds it. See [`DiscoveryScanState::Queued`].
            state: DiscoveryScanState::Queued,
            probed: 0,
            candidates: Vec::new(),
            started_at: now,
            updated_at: now,
            pool,
        }
    }

    /// Fold a (possibly partial) poller result in. Each message carries the cumulative
    /// found list, so candidates are replaced, not appended. Progress never regresses
    /// (guards against out-of-order delivery). The classifier resolves each device's
    /// suggested profile server-side from its sysObjectID / sysDescr.
    fn apply(&mut self, result: DiscoveryResult, classifier: &Classifier, now: DateTime<Utc>) {
        if result.probed < self.probed && !result.done {
            return;
        }
        // Any message at all means a poller has this sweep in hand, so this is where `Queued`
        // ends. Every message qualifies, deliberately: the zero-progress one a current poller sends
        // when it takes the job, and equally a first chunk from one too old to send that.
        if self.state == DiscoveryScanState::Queued {
            self.state = DiscoveryScanState::Running;
        }
        self.updated_at = now;
        self.candidates = result
            .found
            .into_iter()
            .map(|d| {
                // Authoritative profile suggestion from the classification rules (sysObjectID
                // first, then sysDescr, then the Generic-SNMP fallback when SNMP answered).
                let matched = classifier.classify(d.sysobjectid.as_deref(), d.sysdescr.as_deref());
                // Maker/model: a matching rule may pin them; otherwise fall back to the
                // best-effort sysDescr parse (editable by the operator on import).
                let parsed = d
                    .sysdescr
                    .as_deref()
                    .map(yagra_discovery::identify)
                    .unwrap_or_default();
                let vendor = matched
                    .as_ref()
                    .and_then(|m| m.vendor.clone())
                    .or(parsed.vendor);
                let model = matched
                    .as_ref()
                    .and_then(|m| m.model.clone())
                    .or(parsed.model);
                Candidate {
                    address: d.address.to_string(),
                    reachable: d.reachable,
                    sysdescr: d.sysdescr,
                    sysname: d.sysname,
                    sysobjectid: d.sysobjectid,
                    suggested_profile_id: matched.map(|m| m.profile_id),
                    vendor,
                    model,
                    matched_credential_id: d.matched_credential,
                }
            })
            .collect();
        self.probed = self.probed.max(result.probed);
        if result.done {
            // **The poller's report decides, not what core last recorded.** Reading `Cancelling` as
            // "therefore cancelled" would report a sweep that finished before the stop landed —
            // the N-1 case, where the poller never subscribed to the cancel subject at all — as
            // having been stopped. `DiscoveryResult::cancelled` exists precisely so this does not
            // have to be guessed.
            //
            // Exhaustive over the state rather than `_ =>`: a future variant must be decided here,
            // not defaulted into `Done` (extensibility.md §1).
            self.state = match (self.state, result.cancelled) {
                // `Queued` cannot actually reach here — the promotion above ran on this same
                // message — but it is listed rather than wildcarded, because the day a promotion
                // rule gains a condition is the day this needs to be decided again rather than
                // silently defaulting (extensibility.md §1).
                (
                    DiscoveryScanState::Queued
                    | DiscoveryScanState::Running
                    | DiscoveryScanState::Cancelling,
                    true,
                ) => DiscoveryScanState::Cancelled,
                (
                    DiscoveryScanState::Queued
                    | DiscoveryScanState::Running
                    | DiscoveryScanState::Cancelling,
                    false,
                ) => DiscoveryScanState::Done,
                // Already settled — a duplicate or late terminal result must not move it.
                (s @ (DiscoveryScanState::Cancelled | DiscoveryScanState::Done), _) => s,
            };
        }
    }

    fn total(&self) -> u32 {
        u32::try_from(self.targets.len()).unwrap_or(u32::MAX)
    }

    fn status(&self, scan_id: Uuid) -> ScanStatus {
        let total = self.total();
        // Only while a poller is actually working through the list — not merely "not finished".
        // A queued sweep would otherwise report `targets[0]`, i.e. "now probing 192.168.1.1" about
        // a sweep nobody has picked up, which is the exact false impression `Queued` exists to
        // remove.
        let scanning = matches!(
            self.state,
            DiscoveryScanState::Running | DiscoveryScanState::Cancelling
        )
        .then(|| {
            self.targets
                .get(self.probed as usize)
                .map(IpAddr::to_string)
        })
        .flatten();
        ScanStatus {
            scan_id,
            done: self.state.is_terminal(),
            state: self.state,
            probed: self.probed.min(total),
            total,
            scanning,
            started_at: self.started_at.to_rfc3339(),
            updated_at: self.updated_at.to_rfc3339(),
            pool: self.pool.clone(),
            candidates: self.candidates.clone(),
        }
    }

    fn summary(&self, scan_id: Uuid) -> ScanSummary {
        let total = self.total();
        ScanSummary {
            scan_id,
            state: self.state,
            probed: self.probed.min(total),
            total,
            candidate_count: u32::try_from(self.candidates.len()).unwrap_or(u32::MAX),
            started_at: self.started_at.to_rfc3339(),
            updated_at: self.updated_at.to_rfc3339(),
            pool: self.pool.clone(),
        }
    }
}

/// Drop scans nobody can act on any more, in place.
///
/// Three rules, in this order — the order matters, because the age rules are what stop a
/// never-reporting sweep from consuming the cap forever:
///
/// 1. a **running** scan older than [`RUNNING_MAX_AGE`] is gone (its poller is never going to
///    report),
/// 2. a **terminal** scan whose last update is older than [`FINISHED_TTL`] is gone,
/// 3. if more than [`MAX_SCANS`] remain, the **oldest terminal** ones go until the cap holds.
///
/// ⚠️ Rule 3 never touches a running scan. Evicting one would take away the operator's only handle
/// on a sweep that is still probing their network — the opposite of what a cap is for.
///
/// Takes `now` rather than reading the clock so the rules are testable without sleeping, the shape
/// `OidcFlight` established.
fn evict(scans: &mut HashMap<Uuid, ScanState>, now: DateTime<Utc>) {
    let older_than = |t: DateTime<Utc>, d: Duration| {
        now.signed_duration_since(t)
            .to_std()
            .is_ok_and(|age| age > d)
    };
    scans.retain(|_, s| {
        if s.state.is_terminal() {
            !older_than(s.updated_at, FINISHED_TTL)
        } else {
            !older_than(s.started_at, RUNNING_MAX_AGE)
        }
    });
    if scans.len() <= MAX_SCANS {
        return;
    }
    let mut finished: Vec<(Uuid, DateTime<Utc>)> = scans
        .iter()
        .filter(|(_, s)| s.state.is_terminal())
        .map(|(id, s)| (*id, s.updated_at))
        .collect();
    finished.sort_by_key(|(_, at)| *at);
    for (id, _) in finished.into_iter().take(scans.len() - MAX_SCANS) {
        scans.remove(&id);
    }
}

/// Orchestrates discovery scans: publishes jobs, accumulates results, exposes status.
pub struct DiscoveryRunner {
    /// The publish seam, not the NATS type: this held `Arc<NatsBus>` and so could only ever be
    /// driven by a running broker, which is why the routing decision in [`Self::start`] had no test.
    bus: Arc<dyn DiscoveryBus>,
    classifier: Arc<Classifier>,
    scans: Mutex<HashMap<Uuid, ScanState>>,
}

impl DiscoveryRunner {
    #[must_use]
    pub fn new(bus: Arc<dyn DiscoveryBus>, classifier: Arc<Classifier>) -> Self {
        Self {
            bus,
            classifier,
            scans: Mutex::new(HashMap::new()),
        }
    }

    /// Start a scan: register it and publish the sweep job. `credentials` are the stored
    /// secrets to try (already resolved/decrypted by the caller — ADR-018/020); they are
    /// passed through to the poller and never logged. Returns the scan id to poll.
    ///
    /// `pool` routes the sweep (ADR-009/020): `Some(p)` publishes to that pool's own discovery
    /// subject (the caller has already confirmed a live poller serves it), so a remote-site poller
    /// runs the scan on its network; `None` publishes to the legacy global discovery subject (the
    /// compatibility path, absorbed by an old wildcard poller).
    pub async fn start(
        &self,
        targets: Vec<IpAddr>,
        communities: Vec<String>,
        credentials: Vec<DiscoveryCredential>,
        pool: Option<&str>,
        silent: SilentTargets,
    ) -> anyhow::Result<Uuid> {
        let scan_id = Uuid::new_v4();
        {
            let now = Utc::now();
            let mut g = self.scans.lock().expect("scans mutex poisoned");
            // The route actually taken is stored, not the pool the caller asked for — see
            // `ScanState::pool`.
            g.insert(
                scan_id,
                ScanState::new(targets.clone(), pool.map(str::to_owned), now),
            );
            // Evicting *after* the insert is deliberate: the new scan is `Running`, which rule 3
            // never touches, so it cannot evict the very scan it was called for.
            evict(&mut g, now);
        }
        let job = DiscoveryJob {
            scan_id,
            targets,
            communities,
            credentials,
            timeout_ms: SCAN_TIMEOUT_MS,
            snmp_when_unreachable: silent == SilentTargets::ProbeSnmp,
        };
        match pool {
            Some(p) => self.bus.publish_discovery_job_for_pool(p, job).await?,
            None => self.bus.publish_discovery_job(job).await?,
        }
        Ok(scan_id)
    }

    /// Current status of a scan (progress + candidates so far + whether it completed).
    #[must_use]
    pub fn get(&self, scan_id: Uuid) -> Option<ScanStatus> {
        let g = self.scans.lock().expect("scans mutex poisoned");
        g.get(&scan_id).map(|s| s.status(scan_id))
    }

    /// Ask whoever is sweeping `scan_id` to stop (ADR-068 Inc.2).
    ///
    /// Returns the route the stop was published on, so the caller can report which pool was asked.
    ///
    /// **Publishes even for a scan this core has never heard of, and that is the point.** Scan
    /// state is in memory, so a restarted core forgets a sweep its pollers are still running — and
    /// if a stop required a local record, that sweep could never be stopped by anything short of
    /// restarting the poller. The id is an unguessable UUID and the caller already holds
    /// `ManageConfig`, so the only thing a made-up id achieves is a message nobody acts on.
    ///
    /// ⚠️ This differs deliberately from `analysis`'s cancel, which 404s an unknown run. There the
    /// 200/404 split is a read oracle for "is that job running"; here the equivalent split would
    /// only reveal whether *this core* remembers a sweep the caller must already have the id for.
    ///
    /// Fire-and-forget: `Ok` means the broker accepted the message, never that a sweep stopped.
    /// The poller's terminal result is what settles that — see [`ScanState::apply`].
    pub async fn cancel(&self, scan_id: Uuid) -> anyhow::Result<Option<String>> {
        let route = {
            let mut g = self.scans.lock().expect("scans mutex poisoned");
            match g.get_mut(&scan_id) {
                // Only a running sweep moves. A finished one stays finished — re-cancelling it
                // would rewrite history, and a `Cancelling` one is already asked.
                Some(s) => {
                    // `Queued` as well as `Running`. The poller's first cancel check runs when it
                    // takes a job off the bus, precisely so a sweep stopped while it queued never
                    // starts probing — and refusing to move a queued scan here would make that
                    // layer unreachable from the screen, which is the one case where stopping costs
                    // the network nothing at all.
                    if matches!(
                        s.state,
                        DiscoveryScanState::Running | DiscoveryScanState::Queued
                    ) {
                        s.state = DiscoveryScanState::Cancelling;
                        s.updated_at = Utc::now();
                    }
                    s.pool.clone()
                }
                // Unknown here means "this core restarted", not "no such sweep". The stop still
                // goes out — on the global subject, because the route it took is unknowable now.
                None => None,
            }
        };
        self.bus
            .publish_discovery_cancel(route.as_deref(), DiscoveryCancel { scan_id })
            .await?;
        Ok(route)
    }

    /// Every retained scan's summary, newest first, capped at `limit`.
    ///
    /// This is what makes a sweep survivable in the UI: the scan id used to live only in the
    /// browser tab that started it, so navigating away lost the sweep even though the poller kept
    /// probing. What bounds this list is [`evict`], not a query parameter — see [`FINISHED_TTL`].
    #[must_use]
    pub fn list(&self, limit: usize) -> Vec<ScanSummary> {
        let mut g = self.scans.lock().expect("scans mutex poisoned");
        // Retention is enforced on read as well as at [`Self::start`], because `start` is not
        // guaranteed to run again: a deployment where nobody sweeps a second time kept a
        // three-day-old scan listed here and feeding the dashboard's discovery-queue widget for the
        // life of the process. [`MAX_SCANS`] bounded the memory, so this was never a leak — it was
        // [`FINISHED_TTL`] not being a window, because nothing ever closed it.
        evict(&mut g, Utc::now());
        let mut ordered: Vec<(&Uuid, &ScanState)> = g.iter().collect();
        // Newest first — the scan an operator is coming back to is the one they just started.
        // Ordered on the `DateTime`, never on the rendered RFC 3339 string: they happen to sort
        // alike today only because every value is UTC with the same precision.
        ordered.sort_by(|a, b| {
            b.1.started_at
                .cmp(&a.1.started_at)
                // Two scans can share a timestamp; without this their order would vary per call
                // and the list would appear to shuffle on refresh.
                .then_with(|| b.0.cmp(a.0))
        });
        ordered
            .into_iter()
            .take(limit)
            .map(|(id, s)| s.summary(*id))
            .collect()
    }

    /// Recent discovered candidates across all in-memory scans, deduped by address (first seen
    /// wins), capped at `limit`. Backs the dashboard "discovery queue" widget — a standing view of
    /// unclassified finds without needing a scan id.
    ///
    /// ⚠️ **[`FINISHED_TTL`] is this widget's window too.** This reads whatever [`evict`] has left,
    /// so shortening that constant empties this panel — the two are the same number wearing two
    /// hats. Before ADR-068 nothing was ever evicted, so this accumulated for the life of the
    /// process and the widget only ever grew.
    #[must_use]
    pub fn recent_candidates(&self, limit: usize) -> Vec<Candidate> {
        let mut g = self.scans.lock().expect("scans mutex poisoned");
        // Pruned here too — see [`Self::list`]. Between the two, any deployment where somebody
        // looks at either the Discovery screen or the dashboard closes the window.
        //
        // ⚠️ Deliberately **not** in [`Self::get`]. That is the one read on a two-second timer,
        // and a poll that can delete the very scan it is reading is a shape worth not having.
        evict(&mut g, Utc::now());
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        for scan in g.values() {
            for c in &scan.candidates {
                if seen.insert(c.address.clone()) {
                    out.push(c.clone());
                    if out.len() >= limit {
                        return out;
                    }
                }
            }
        }
        out
    }

    /// Consume discovery results off the bus, folding each into its scan. Runs until the
    /// stream ends.
    pub async fn run_consumer<S>(self: Arc<Self>, mut results: S)
    where
        S: Stream<Item = DiscoveryResult> + Unpin,
    {
        while let Some(r) = results.next().await {
            tracing::debug!(
                scan = %r.scan_id,
                found = r.found.len(),
                probed = r.probed,
                done = r.done,
                "discovery result received"
            );
            let mut g = self.scans.lock().expect("scans mutex poisoned");
            if let Some(s) = g.get_mut(&r.scan_id) {
                s.apply(r, &self.classifier, Utc::now());
            }
            // An unknown scan_id is dropped on purpose: this core restarted (or was never the
            // leader) while a poller kept sweeping. Registering a scan here would report progress
            // for a sweep whose targets and credentials this process never knew.
        }
        tracing::warn!("discovery result stream ended");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;
    use yagra_bus::DiscoveredDevice;
    use yagra_common::ClassificationRule;

    /// Stable test profile id the Huawei rule maps to.
    const HUAWEI_PROFILE: u128 = 0x4A1;
    const GENERIC_PROFILE: u128 = 0x9E2E61C;

    /// A classifier with one Huawei sysObjectID-prefix rule + a Generic-SNMP fallback, so the
    /// fixture devices (Huawei sysObjectID) classify deterministically without a database.
    fn classifier() -> Classifier {
        let huawei = ClassificationRule {
            id: Uuid::from_u128(1),
            priority: 100,
            sysobjectid_prefix: Some("1.3.6.1.4.1.2011.".to_owned()),
            sysdescr_regex: None,
            profile_id: Uuid::from_u128(HUAWEI_PROFILE).into(),
            vendor: None,
            model: None,
            enabled: true,
        };
        Classifier::from_rules(vec![huawei], Some(Uuid::from_u128(GENERIC_PROFILE)))
    }

    fn targets(n: u8) -> Vec<IpAddr> {
        (1..=n)
            .map(|i| IpAddr::V4(Ipv4Addr::new(10, 0, 0, i)))
            .collect()
    }

    /// A scan of `n` targets starting now, on the global route.
    fn scan(n: u8) -> ScanState {
        ScanState::new(targets(n), None, Utc::now())
    }

    fn partial(probed: u32, done: bool, found: Vec<DiscoveredDevice>) -> DiscoveryResult {
        DiscoveryResult {
            scan_id: Uuid::nil(),
            found,
            probed,
            total: 4,
            done,
            cancelled: false,
        }
    }

    fn device(last_octet: u8, cred: Option<Uuid>) -> DiscoveredDevice {
        DiscoveredDevice {
            address: IpAddr::V4(Ipv4Addr::new(10, 0, 0, last_octet)),
            reachable: true,
            sysdescr: Some("Huawei Versatile Routing Platform USG6000".to_owned()),
            sysname: Some(format!("dev{last_octet}")),
            sysobjectid: Some("1.3.6.1.4.1.2011.2.1".to_owned()),
            matched_credential: cred,
        }
    }

    #[test]
    fn progress_advances_and_reports_current_address() {
        let c = classifier();
        let mut s = scan(4);
        let st = s.status(Uuid::nil());
        assert_eq!((st.probed, st.total), (0, 4));
        assert_eq!(
            st.scanning, None,
            "a queued sweep has no current address — saying '10.0.0.1' about one no poller has \
             picked up is the false impression `Queued` exists to remove"
        );

        s.apply(partial(2, false, vec![device(1, None)]), &c, Utc::now());
        let st = s.status(Uuid::nil());
        assert!(!st.done);
        assert_eq!(st.probed, 2);
        assert_eq!(st.scanning.as_deref(), Some("10.0.0.3"));
        assert_eq!(st.candidates.len(), 1);

        s.apply(
            partial(4, true, vec![device(1, None), device(3, None)]),
            &c,
            Utc::now(),
        );
        let st = s.status(Uuid::nil());
        assert!(st.done);
        assert_eq!(st.probed, 4);
        assert_eq!(st.scanning, None, "no current address once done");
        assert_eq!(st.candidates.len(), 2);
    }

    #[test]
    fn matched_credential_is_carried_into_the_candidate() {
        let cred = Uuid::from_u128(42);
        let mut s = scan(1);
        s.apply(
            partial(1, true, vec![device(1, Some(cred))]),
            &classifier(),
            Utc::now(),
        );
        let st = s.status(Uuid::nil());
        assert_eq!(st.candidates[0].matched_credential_id, Some(cred));
        // The device's sysObjectID resolves (server-side) to the Huawei profile by id, and the
        // maker/model are parsed from sysDescr to pre-fill the node's descriptive metadata.
        assert_eq!(
            st.candidates[0].suggested_profile_id,
            Some(Uuid::from_u128(HUAWEI_PROFILE))
        );
        assert_eq!(
            st.candidates[0].sysobjectid.as_deref(),
            Some("1.3.6.1.4.1.2011.2.1")
        );
        assert_eq!(st.candidates[0].vendor.as_deref(), Some("Huawei"));
        assert_eq!(st.candidates[0].model.as_deref(), Some("USG6000"));
    }

    #[test]
    fn snmp_device_with_no_rule_match_falls_back_to_generic_profile() {
        // No sysObjectID, sysDescr matches no rule → the Generic-SNMP fallback id (by id).
        let mut s = scan(1);
        let dev = DiscoveredDevice {
            address: IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
            reachable: true,
            sysdescr: Some("Linux server 5.10 net-snmp".to_owned()),
            sysname: Some("srv01".to_owned()),
            sysobjectid: None,
            matched_credential: None,
        };
        s.apply(partial(1, true, vec![dev]), &classifier(), Utc::now());
        let st = s.status(Uuid::nil());
        assert_eq!(
            st.candidates[0].suggested_profile_id,
            Some(Uuid::from_u128(GENERIC_PROFILE))
        );
    }

    #[test]
    fn stale_or_reordered_partials_never_regress_progress() {
        let c = classifier();
        let mut s = scan(4);
        s.apply(
            partial(4, false, vec![device(1, None), device(2, None)]),
            &c,
            Utc::now(),
        );
        // A late, out-of-order partial with lower progress must be ignored.
        s.apply(partial(2, false, vec![device(1, None)]), &c, Utc::now());
        let st = s.status(Uuid::nil());
        assert_eq!(st.probed, 4);
        assert_eq!(st.candidates.len(), 2);
    }

    #[test]
    fn old_poller_single_result_completes_the_scan() {
        // N-1 (ADR-017): an older poller sends one final message with default progress
        // fields (probed 0, done true) and no sysObjectID — the scan must still complete and
        // classify via the sysDescr fallback.
        let mut s = scan(2);
        s.apply(
            DiscoveryResult {
                scan_id: Uuid::nil(),
                found: vec![DiscoveredDevice {
                    address: IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
                    reachable: true,
                    sysdescr: Some("Huawei VRP USG6000".to_owned()),
                    sysname: Some("dev1".to_owned()),
                    sysobjectid: None,
                    matched_credential: None,
                }],
                probed: 0,
                total: 0,
                done: true,
                cancelled: false,
            },
            &classifier(),
            Utc::now(),
        );
        let st = s.status(Uuid::nil());
        assert!(st.done);
        assert_eq!(st.candidates.len(), 1);
        // Old poller sent no sysObjectID, so it falls back to the Generic-SNMP profile (the
        // Huawei rule here is sysObjectID-only). The classifier still resolves a suggestion.
        assert_eq!(
            st.candidates[0].suggested_profile_id,
            Some(Uuid::from_u128(GENERIC_PROFILE))
        );
    }

    /// `start`'s routing choice, which had no test while the runner held the NATS type. Getting it
    /// wrong is invisible locally and wrong remotely: a sweep meant for a remote site published on
    /// the global subject is absorbed by whichever poller answers first, so it scans the *wrong*
    /// network and reports nothing found.
    #[tokio::test]
    async fn a_pool_scoped_scan_is_published_only_to_that_pool() {
        let bus = Arc::new(yagra_bus::InMemoryBus::new(8));
        let mut pooled = bus.subscribe_pool_discovery_jobs();
        let mut global = bus.subscribe_discovery_jobs();
        let runner = DiscoveryRunner::new(bus.clone(), Arc::new(classifier()));

        let scan = runner
            .start(
                targets(2),
                vec!["public".to_owned()],
                Vec::new(),
                Some("tokyo"),
                SilentTargets::Skip,
            )
            .await
            .expect("publish succeeds");

        let (pool, job) = pooled.recv().await.expect("job published");
        assert_eq!(pool, "tokyo");
        assert_eq!(job.scan_id, scan);
        assert_eq!(job.targets.len(), 2);
        assert!(global.try_recv().is_err(), "must not also hit the wildcard");
        // The scan is registered before publish, so status is available the moment start returns.
        assert!(runner.get(scan).is_some());
    }

    #[tokio::test]
    async fn a_scan_with_no_pool_falls_back_to_the_global_subject() {
        let bus = Arc::new(yagra_bus::InMemoryBus::new(8));
        let mut pooled = bus.subscribe_pool_discovery_jobs();
        let mut global = bus.subscribe_discovery_jobs();
        let runner = DiscoveryRunner::new(bus.clone(), Arc::new(classifier()));

        let scan = runner
            .start(
                targets(1),
                Vec::new(),
                Vec::new(),
                None,
                SilentTargets::Skip,
            )
            .await
            .expect("publish succeeds");

        assert_eq!(global.recv().await.expect("job published").scan_id, scan);
        assert!(pooled.try_recv().is_err());
    }

    /// Credentials are passed through to the poller verbatim (it needs the plaintext to probe) but
    /// must never be logged — this pins the pass-through half so a future "sanitize the job" change
    /// cannot silently empty it and make every stored-credential sweep find nothing.
    #[tokio::test]
    async fn stored_credentials_reach_the_poller_in_the_job() {
        let bus = Arc::new(yagra_bus::InMemoryBus::new(8));
        let mut global = bus.subscribe_discovery_jobs();
        let runner = DiscoveryRunner::new(bus.clone(), Arc::new(classifier()));

        let cred_ref = Uuid::from_u128(9);
        runner
            .start(
                targets(1),
                Vec::new(),
                vec![DiscoveryCredential {
                    cred_ref,
                    community: Some("s3cret".to_owned()),
                    v3: None,
                }],
                None,
                SilentTargets::Skip,
            )
            .await
            .expect("publish succeeds");

        let job = global.recv().await.expect("job published");
        assert_eq!(job.credentials.len(), 1);
        assert_eq!(job.credentials[0].cred_ref, cred_ref);
    }

    /// The ICMP gate crosses two defaults pointing in opposite directions (ADR-068 Inc.3): the API
    /// reads an absent field as `Skip`, while the bus reads an absent field as "probe everything"
    /// so an N-1 core's sweeps do not change under an upgraded poller. Nothing else compares the
    /// two, so this pins the one place they meet — a polarity slip here would either make the
    /// checkbox do nothing or make it do the opposite, and both look fine at every other layer.
    #[tokio::test]
    async fn the_icmp_gate_choice_reaches_the_job_the_right_way_round() {
        let bus = Arc::new(yagra_bus::InMemoryBus::new(8));
        let mut global = bus.subscribe_discovery_jobs();
        let runner = DiscoveryRunner::new(bus.clone(), Arc::new(classifier()));

        runner
            .start(
                targets(1),
                Vec::new(),
                Vec::new(),
                None,
                SilentTargets::Skip,
            )
            .await
            .expect("publish succeeds");
        assert!(
            !global
                .recv()
                .await
                .expect("job published")
                .snmp_when_unreachable,
            "Skip must reach the poller as 'do not probe silent addresses'"
        );

        runner
            .start(
                targets(1),
                Vec::new(),
                Vec::new(),
                None,
                SilentTargets::ProbeSnmp,
            )
            .await
            .expect("publish succeeds");
        assert!(
            global
                .recv()
                .await
                .expect("job published")
                .snmp_when_unreachable,
            "ProbeSnmp must reach the poller as 'probe them anyway'"
        );
    }

    // ── Scan lifetime, listing and state (ADR-068 Inc.1) ─────────────────────

    /// Build a scan in a chosen state, started `age` ago and last updated `since` ago.
    fn aged(
        state: DiscoveryScanState,
        started_ago: chrono::Duration,
        updated_ago: chrono::Duration,
        now: DateTime<Utc>,
    ) -> ScanState {
        let mut s = ScanState::new(targets(1), None, now - started_ago);
        s.state = state;
        s.updated_at = now - updated_ago;
        s
    }

    #[test]
    fn a_finished_scan_outlives_its_window_and_then_goes() {
        let now = Utc::now();
        let mut scans = HashMap::new();
        let fresh = Uuid::from_u128(1);
        let stale = Uuid::from_u128(2);
        scans.insert(
            fresh,
            aged(
                DiscoveryScanState::Done,
                chrono::Duration::hours(7),
                chrono::Duration::hours(1),
                now,
            ),
        );
        scans.insert(
            stale,
            aged(
                DiscoveryScanState::Done,
                chrono::Duration::hours(9),
                chrono::Duration::hours(7),
                now,
            ),
        );
        evict(&mut scans, now);
        // The window is measured from the last update, not from the start: a long sweep that
        // finished recently is recent.
        assert!(scans.contains_key(&fresh));
        assert!(!scans.contains_key(&stale));
    }

    #[test]
    fn a_running_scan_is_kept_until_it_is_hopeless() {
        let now = Utc::now();
        let mut scans = HashMap::new();
        let live = Uuid::from_u128(1);
        let abandoned = Uuid::from_u128(2);
        // Well past FINISHED_TTL, but running — the finished window must not apply to it, or a
        // sweep still probing the network would vanish from the operator's list.
        scans.insert(
            live,
            aged(
                DiscoveryScanState::Running,
                chrono::Duration::hours(1),
                chrono::Duration::hours(1),
                now,
            ),
        );
        scans.insert(
            abandoned,
            aged(
                DiscoveryScanState::Running,
                chrono::Duration::hours(3),
                chrono::Duration::hours(3),
                now,
            ),
        );
        evict(&mut scans, now);
        assert!(scans.contains_key(&live));
        assert!(
            !scans.contains_key(&abandoned),
            "a scan whose poller will never report must not pin memory for the process's life"
        );
    }

    #[test]
    fn the_cap_drops_the_oldest_finished_and_never_a_running_one() {
        let now = Utc::now();
        let mut scans = HashMap::new();
        // MAX_SCANS finished scans, newest first by construction, plus one running scan that is
        // older than all of them. The running one must survive even though it is the oldest.
        for i in 0..MAX_SCANS {
            scans.insert(
                Uuid::from_u128(100 + i as u128),
                aged(
                    DiscoveryScanState::Done,
                    chrono::Duration::minutes(i as i64),
                    chrono::Duration::minutes(i as i64),
                    now,
                ),
            );
        }
        let running = Uuid::from_u128(1);
        scans.insert(
            running,
            aged(
                DiscoveryScanState::Running,
                chrono::Duration::minutes(90),
                chrono::Duration::minutes(90),
                now,
            ),
        );
        evict(&mut scans, now);
        assert_eq!(scans.len(), MAX_SCANS);
        assert!(
            scans.contains_key(&running),
            "a running scan is never capped away"
        );
        // The oldest *finished* one went instead.
        assert!(!scans.contains_key(&Uuid::from_u128(100 + (MAX_SCANS - 1) as u128)));
        assert!(scans.contains_key(&Uuid::from_u128(100)));
    }

    #[tokio::test]
    async fn starting_a_scan_never_evicts_the_scan_it_just_registered() {
        // Eviction runs on every start, and the cap is exactly the kind of rule that would bite the
        // newest row if it ran before the insert.
        let bus = Arc::new(yagra_bus::InMemoryBus::new(64));
        let runner = DiscoveryRunner::new(bus, Arc::new(classifier()));
        let mut ids = Vec::new();
        for _ in 0..(MAX_SCANS + 5) {
            ids.push(
                runner
                    .start(
                        targets(1),
                        Vec::new(),
                        Vec::new(),
                        None,
                        SilentTargets::Skip,
                    )
                    .await
                    .expect("publish succeeds"),
            );
        }
        // Every one of them was running when the next started, so none may have been dropped.
        for id in &ids {
            assert!(runner.get(*id).is_some(), "running scan {id} was evicted");
        }
    }

    #[tokio::test]
    async fn the_list_is_newest_first_and_carries_the_route_actually_used() {
        let bus = Arc::new(yagra_bus::InMemoryBus::new(16));
        let runner = DiscoveryRunner::new(bus, Arc::new(classifier()));
        let first = runner
            .start(
                targets(2),
                Vec::new(),
                Vec::new(),
                None,
                SilentTargets::Skip,
            )
            .await
            .expect("publish succeeds");
        let second = runner
            .start(
                targets(3),
                Vec::new(),
                Vec::new(),
                Some("tokyo"),
                SilentTargets::Skip,
            )
            .await
            .expect("publish succeeds");

        let rows = runner.list(10);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].scan_id, second, "newest first");
        assert_eq!(rows[1].scan_id, first);
        // The pool is what the job was published on, which is what Increment 2 must address a
        // cancel to.
        assert_eq!(rows[0].pool.as_deref(), Some("tokyo"));
        assert_eq!(rows[1].pool, None);
        assert_eq!(rows[0].total, 3);
        assert_eq!(
            rows[0].state,
            DiscoveryScanState::Queued,
            "published, and nothing has confirmed a poller has it"
        );
        assert_eq!(rows[0].candidate_count, 0);

        assert_eq!(runner.list(1).len(), 1, "limit applies");
    }

    #[test]
    fn a_scan_stays_queued_until_a_poller_says_something_about_it() {
        // 🚨 The point of the state, and the case that matters most is the second assertion's
        // absence of a poller: a sweep published to a pool whose pollers are all gone used to read
        // `Running · 0/254` forever. So did one queued behind another sweep — the poller's job loop
        // is strictly sequential, so with the gate off that wait is minutes.
        let c = classifier();
        let mut s = scan(4);
        assert_eq!(s.state, DiscoveryScanState::Queued);
        assert!(!s.state.is_terminal(), "so RUNNING_MAX_AGE can retire it");

        // The zero-progress message a current poller sends the moment it takes the job. It carries
        // nothing else at all — no progress, no candidates — and that is the whole signal.
        s.apply(partial(0, false, vec![]), &c, Utc::now());
        assert_eq!(s.state, DiscoveryScanState::Running);
    }

    #[test]
    fn an_old_pollers_first_chunk_also_ends_the_queue() {
        // N-1: a poller that predates the start message says nothing until its first chunk lands.
        // The promotion therefore keys off *any* message rather than that specific one — otherwise
        // a sweep an old poller is actively running would read as queued for its entire length.
        let c = classifier();
        let mut s = scan(4);
        s.apply(partial(2, false, vec![device(1, None)]), &c, Utc::now());
        assert_eq!(s.state, DiscoveryScanState::Running);
    }

    #[test]
    fn a_late_start_message_cannot_wipe_the_candidates_found_since() {
        // ⚠️ The start message carries an empty `found`, and `apply` replaces the candidate list
        // rather than appending to it — so a reordered or duplicated copy arriving after real
        // progress would, taken literally, empty the table under the operator. The regression guard
        // (`result.probed < self.probed`) already refuses it; this pins that it keeps doing so, since
        // the guard predates the message and nothing else connects the two.
        let c = classifier();
        let mut s = scan(4);
        s.apply(partial(0, false, vec![]), &c, Utc::now());
        s.apply(partial(2, false, vec![device(1, None)]), &c, Utc::now());
        assert_eq!(s.status(Uuid::nil()).candidates.len(), 1);

        s.apply(partial(0, false, vec![]), &c, Utc::now());
        let st = s.status(Uuid::nil());
        assert_eq!(
            st.candidates.len(),
            1,
            "a late start message is not 'found nothing'"
        );
        assert_eq!(st.probed, 2, "and it does not rewind the progress either");
    }

    #[test]
    fn a_terminal_result_settles_the_state_and_a_late_one_cannot_unsettle_it() {
        let c = classifier();
        let now = Utc::now();
        let mut s = scan(4);
        assert_eq!(s.state, DiscoveryScanState::Queued);
        s.apply(partial(4, true, vec![device(1, None)]), &c, now);
        assert_eq!(s.state, DiscoveryScanState::Done);
        assert!(s.status(Uuid::nil()).done, "`done` is derived from `state`");

        // A duplicate terminal result must not move a settled scan.
        s.apply(partial(4, true, vec![device(1, None)]), &c, now);
        assert_eq!(s.state, DiscoveryScanState::Done);
    }

    #[test]
    fn a_stop_becomes_a_fact_only_when_the_poller_reports() {
        // Increment 2's transition, pinned now because Increment 1 ships the states. `Cancelling`
        // is not terminal — the difference between "we asked" and "it stopped" is the whole point
        // of having two states rather than one.
        let c = classifier();
        let now = Utc::now();
        let mut s = scan(4);
        s.state = DiscoveryScanState::Cancelling;
        assert!(!s.state.is_terminal());
        assert!(!s.status(Uuid::nil()).done);
        assert!(
            s.status(Uuid::nil()).scanning.is_some(),
            "a scan being cancelled is still sweeping until the poller says otherwise"
        );

        // ⚠️ `cancelled: true` is what settles it, **not** the fact that core asked. Written as
        // `partial(…)` (which sends `cancelled: false`) this test passed while `apply` ignored the
        // flag entirely — and the behaviour it was asserting was wrong: a sweep that finished
        // before the stop arrived would have been recorded as stopped.
        s.apply(
            DiscoveryResult {
                scan_id: Uuid::nil(),
                found: vec![device(1, None)],
                probed: 2,
                total: 4,
                done: true,
                cancelled: true,
            },
            &c,
            now,
        );
        assert_eq!(s.state, DiscoveryScanState::Cancelled);
        let st = s.status(Uuid::nil());
        assert!(st.done);
        // `probed < total` is the evidence that it really stopped early.
        assert!(st.probed < st.total);
    }

    // ── Cancelling (ADR-068 Inc.2) ───────────────────────────────────────────

    #[tokio::test]
    async fn a_stop_follows_the_route_the_sweep_actually_took() {
        // The trap this guards: `api::discovery` falls back to the global subject when the
        // requested pool has no live poller, so a stop addressed at the *request* would go to a
        // pool that never received the job — the operator would watch "stopping…" forever while
        // the sweep ran on.
        let bus = Arc::new(yagra_bus::InMemoryBus::new(16));
        let mut rx = bus.subscribe_discovery_cancels();
        let runner = DiscoveryRunner::new(bus, Arc::new(classifier()));

        let pooled = runner
            .start(
                targets(2),
                Vec::new(),
                Vec::new(),
                Some("tokyo"),
                SilentTargets::Skip,
            )
            .await
            .expect("publish succeeds");
        assert_eq!(
            runner.cancel(pooled).await.unwrap().as_deref(),
            Some("tokyo")
        );
        assert_eq!(rx.recv().await.unwrap().0.as_deref(), Some("tokyo"));

        let global = runner
            .start(
                targets(2),
                Vec::new(),
                Vec::new(),
                None,
                SilentTargets::Skip,
            )
            .await
            .expect("publish succeeds");
        assert_eq!(runner.cancel(global).await.unwrap(), None);
        assert_eq!(rx.recv().await.unwrap().0, None);
    }

    #[tokio::test]
    async fn a_sweep_already_under_way_is_still_stoppable() {
        // The other side of the widened guard. Having taught `cancel` to move a `Queued` scan, a
        // guard that *only* accepted `Queued` would pass every test above while making the stop
        // button inert for the entire time it matters most — the sweep actually putting SNMP on the
        // network (rejection-only tests pass when everything rejects).
        let bus = Arc::new(yagra_bus::InMemoryBus::new(8));
        let runner = DiscoveryRunner::new(bus, Arc::new(classifier()));
        let scan = runner
            .start(
                targets(4),
                Vec::new(),
                Vec::new(),
                None,
                SilentTargets::Skip,
            )
            .await
            .expect("publish succeeds");

        // A poller reports, so the scan is genuinely running.
        {
            let mut g = runner.scans.lock().unwrap();
            g.get_mut(&scan).unwrap().apply(
                DiscoveryResult {
                    scan_id: scan,
                    found: Vec::new(),
                    probed: 0,
                    total: 4,
                    done: false,
                    cancelled: false,
                },
                &classifier(),
                Utc::now(),
            );
        }
        assert_eq!(
            runner.get(scan).expect("registered").state,
            DiscoveryScanState::Running
        );

        runner.cancel(scan).await.unwrap();
        assert_eq!(
            runner.get(scan).expect("still listed").state,
            DiscoveryScanState::Cancelling
        );
    }

    #[tokio::test]
    async fn a_stop_is_published_even_for_a_scan_this_core_has_forgotten() {
        // The core-restart case, and the reason this endpoint does not 404 like `analysis`'s does.
        // Scan state is in memory, so a restarted core has no record of sweeps its pollers are
        // still running — requiring one here would make exactly those sweeps unstoppable.
        let bus = Arc::new(yagra_bus::InMemoryBus::new(8));
        let mut rx = bus.subscribe_discovery_cancels();
        let runner = DiscoveryRunner::new(bus, Arc::new(classifier()));

        let forgotten = Uuid::from_u128(0xDEAD);
        assert_eq!(runner.cancel(forgotten).await.unwrap(), None);
        let (route, msg) = rx.recv().await.expect("the stop still goes out");
        assert_eq!(msg.scan_id, forgotten);
        assert_eq!(
            route, None,
            "with no record, the route is unknowable — global"
        );
    }

    #[tokio::test]
    async fn cancelling_moves_a_running_scan_and_leaves_a_settled_one_alone() {
        let bus = Arc::new(yagra_bus::InMemoryBus::new(8));
        let runner = DiscoveryRunner::new(bus, Arc::new(classifier()));
        let scan = runner
            .start(
                targets(4),
                Vec::new(),
                Vec::new(),
                None,
                SilentTargets::Skip,
            )
            .await
            .expect("publish succeeds");

        // ⚠️ Note which state this cancels *from*: `start` registers a scan as `Queued`, so this
        // is also the proof that a sweep no poller has picked up can be stopped. That is the
        // cheapest stop there is — the poller's first cancel check runs when it takes the job off
        // the bus, so the sweep never probes a single address — and gating `cancel` on `Running`
        // alone would have put that layer out of the screen's reach.
        assert_eq!(
            runner.get(scan).expect("registered").state,
            DiscoveryScanState::Queued
        );

        runner.cancel(scan).await.unwrap();
        let st = runner.get(scan).expect("still listed");
        assert_eq!(st.state, DiscoveryScanState::Cancelling);
        assert!(
            !st.done,
            "asking is not stopping — the scan is not terminal until its poller reports"
        );

        // The poller confirms.
        {
            let mut g = runner.scans.lock().unwrap();
            let s = g.get_mut(&scan).unwrap();
            s.apply(
                DiscoveryResult {
                    scan_id: scan,
                    found: Vec::new(),
                    probed: 2,
                    total: 4,
                    done: true,
                    cancelled: true,
                },
                &classifier(),
                Utc::now(),
            );
        }
        assert_eq!(
            runner.get(scan).unwrap().state,
            DiscoveryScanState::Cancelled
        );

        // Cancelling again must not rewrite that.
        runner.cancel(scan).await.unwrap();
        assert_eq!(
            runner.get(scan).unwrap().state,
            DiscoveryScanState::Cancelled,
            "a settled scan stays settled"
        );
    }

    #[tokio::test]
    async fn a_sweep_that_finished_before_the_stop_arrived_says_so() {
        // The N-1 outcome, and the one the UI has to be able to tell apart from success. An old
        // poller never subscribes to the cancel subject, runs to completion, and reports `done`
        // without `cancelled` — which is exactly true of it.
        let bus = Arc::new(yagra_bus::InMemoryBus::new(8));
        let runner = DiscoveryRunner::new(bus, Arc::new(classifier()));
        let scan = runner
            .start(
                targets(4),
                Vec::new(),
                Vec::new(),
                None,
                SilentTargets::Skip,
            )
            .await
            .expect("publish succeeds");
        runner.cancel(scan).await.unwrap();

        {
            let mut g = runner.scans.lock().unwrap();
            g.get_mut(&scan).unwrap().apply(
                DiscoveryResult {
                    scan_id: scan,
                    found: Vec::new(),
                    probed: 4,
                    total: 4,
                    done: true,
                    cancelled: false,
                },
                &classifier(),
                Utc::now(),
            );
        }
        let st = runner.get(scan).unwrap();
        assert_eq!(
            st.state,
            DiscoveryScanState::Done,
            "it finished; reporting it as cancelled would be inventing a stop that never landed"
        );
        assert_eq!(st.probed, st.total);
    }

    #[test]
    fn status_and_summary_agree_about_the_same_scan() {
        // Two shapes of one fact; they are built separately, so nothing but a test stops them
        // drifting.
        let c = classifier();
        let now = Utc::now();
        let mut s = scan(4);
        s.apply(partial(2, false, vec![device(1, None)]), &c, now);
        let id = Uuid::from_u128(7);
        let st = s.status(id);
        let sum = s.summary(id);
        assert_eq!(
            (st.scan_id, st.state, st.probed, st.total),
            (sum.scan_id, sum.state, sum.probed, sum.total)
        );
        assert_eq!(st.started_at, sum.started_at);
        assert_eq!(st.updated_at, sum.updated_at);
        assert_eq!(
            u32::try_from(st.candidates.len()).unwrap(),
            sum.candidate_count
        );
    }
}
