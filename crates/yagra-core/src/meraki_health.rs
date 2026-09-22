// SPDX-License-Identifier: AGPL-3.0-only
//! Whether the Dashboard API is answering each Meraki organization's collects — and the **one alert
//! per organization** that says so when it is not (ADR-164 決定 18).
//!
//! # Why this exists
//!
//! A Meraki node is never pinged and is outside the freshness sweep (`alerts/stale.rs`): what the
//! availability collect says is all the alert engine ever hears about it. So when that collect
//! stops — a revoked key, a Dashboard outage, rate limiting, a poller that cannot get out — every
//! node of the organization simply keeps the last state it had. Usually `ok`. Nothing alerted,
//! because a collect that failed published nothing, and core cannot tell silence from "nothing is
//! due".
//!
//! # What the user decided (2026-09-20), and this module holds to
//!
//! 1. **One alert per organization, never one per node.** The nodes are not what failed.
//! 2. **A node's state stays what it was** — nothing here touches the liveness state machine. What
//!    changes is that the stale state is *labelled*: `NodeStatus.collection_fault`.
//! 3. **Only the availability tier raises.** It is the tier liveness rides on. An uplink or traffic
//!    collect that fails while availability answers is shown on the organization's page and pages
//!    nobody — a licence that lacks one endpoint answers 403 on that tier forever.
//! 4. **Critical, after three failures in a row** — a monitoring blind spot, like a poller pool
//!    with no poller (`pool_coverage.rs`, which this is modelled on).
//! 5. The inventory sync's own failures stay where they were (`last_sync_error` on the row): that is
//!    core's path to the Dashboard, and a node goes stale through the *poller's*.
//!
//! # Where the signal comes from
//!
//! | Source | Means |
//! |---|---|
//! | the poller's collect report (`PollResult.meraki_collect`) | answered, or failed with a reason |
//! | an ordinary result of a collect job, with no report | answered — a poller from before 決定 18 |
//! | a collect flight whose lease ran out unanswered | `no_answer`: old poller + failed collect, no live poller in the pool, a crash |
//! | the scheduler could not open the key | `credential` |
//! | the scheduler could not read the organization's devices, or which networks it watches | `internal` |
//!
//! The last two rows are the ones **no poller can report**, because no job was sent: core stopped
//! before publishing one, and the organization's devices go unasked-about exactly as they do when
//! the Dashboard refuses. They are counted at the collect's own rate rather than the scheduler's —
//! see [`count_once_per_cadence`], without which a 15-second tick would reach three failures in
//! forty-five seconds. An organization that watches **nothing** is not among them: 決定 16 sends it
//! no collect on purpose, so there is nothing failing to report.
//!
//! # Closing
//!
//! 🚨 **On evidence, never on absence** (ADR-156 決定 3). The alert resolves when an availability
//! collect is *answered*, or when the configuration says there is nothing left to answer for — the
//! organization is gone or paused, or Meraki polling is switched off. A core that has just started
//! knows nothing about an organization until a report arrives, and knowing nothing resolves
//! nothing: [`CollectWatch::seed`] carries the restored alerts across the restart for exactly that.
//!
//! The record ([`MerakiCollectHealth`]) is written from the result-ingest hot path and therefore
//! does no I/O; deciding, persisting and dispatching belong to the leader-gated loop below.

use crate::meraki_sync::MerakiSyncFailure;
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use uuid::Uuid;
use yagra_common::{MerakiListing, MerakiTier};

/// Consecutive failed availability collects before the organization's alert is raised.
///
/// At the default 300 s cadence that is about a quarter of an hour. The liveness rule's own dwell is
/// the same number (`alerts/rules.rs::DEFAULT_LIVENESS_DWELL`), which is the intent: an
/// organization is not called unreachable on less evidence than a node is.
pub const RAISE_AFTER_FAILURES: u32 = 3;

/// The metric name the organization's alert carries. Not a collected series — a label for the
/// alert row, like `pool_coverage::COVERAGE_METRIC`.
pub const COLLECT_METRIC: &str = "meraki_api_collect";

/// One tier of one organization that is currently failing. Also the shape stored in
/// `meraki_orgs.collect_failures` (migration 0127).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TierFailure {
    /// The tier whose collects are failing.
    pub tier: MerakiTier,
    /// Why the most recent one failed.
    pub reason: MerakiSyncFailure,
    /// When the run of failures began.
    pub since_unix_ms: i64,
    /// How many in a row.
    pub failures: u32,
    /// Which of the tier's reads failed, when one did while the others answered (ADR-164 決定 25).
    /// Read leniently: a listing a newer core stored costs the label, never the entry.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient_listing"
    )]
    pub listing: Option<MerakiListing>,
}

/// A stored listing token, or `None` for one this build does not know.
fn lenient_listing<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Option<MerakiListing>, D::Error> {
    let token: Option<String> = serde::Deserialize::deserialize(d)?;
    Ok(token.as_deref().and_then(MerakiListing::from_token))
}

impl TierFailure {
    /// Read the stored JSON array **leniently**: an entry that does not decode — a tier or a reason
    /// a newer core wrote — is dropped, and anything that is not an array reads as none. The row
    /// this sits on is read by the collect scheduler; failing the row here would stop the very
    /// collects it describes.
    #[must_use]
    pub fn from_stored(value: serde_json::Value) -> Vec<Self> {
        let serde_json::Value::Array(entries) = value else {
            return Vec::new();
        };
        entries
            .into_iter()
            .filter_map(|e| serde_json::from_value(e).ok())
            .collect()
    }
}

#[derive(Debug, Clone, Copy)]
struct TierState {
    failures: u32,
    reason: MerakiSyncFailure,
    since_unix_ms: i64,
    listing: Option<MerakiListing>,
}

/// How each organization's collects have been ending, per tier. In memory, leader-local, and
/// rebuilt from the next round of reports after a restart.
///
/// An entry with `failures == 0` is **evidence**: it exists only because a collect of that tier was
/// answered. No entry at all is ignorance, and the two must never be confused — that distinction is
/// what keeps a restart from resolving an alert nobody has evidence against.
#[derive(Debug, Default)]
pub struct MerakiCollectHealth {
    tiers: Mutex<HashMap<(Uuid, MerakiTier), TierState>>,
}

impl MerakiCollectHealth {
    /// A collect of `tier` for `org` was answered.
    pub fn record_answered(&self, org: Uuid, tier: MerakiTier) {
        self.tiers
            .lock()
            .expect("meraki collect health poisoned")
            .insert(
                (org, tier),
                TierState {
                    failures: 0,
                    reason: MerakiSyncFailure::Internal,
                    since_unix_ms: 0,
                    listing: None,
                },
            );
    }

    /// A collect of `tier` for `org` failed for `reason`.
    pub fn record_failed(
        &self,
        org: Uuid,
        tier: MerakiTier,
        reason: MerakiSyncFailure,
        at_unix_ms: i64,
    ) {
        self.record_failed_in(org, tier, reason, None, at_unix_ms);
    }

    /// [`Self::record_failed`], naming the read of the tier that failed when the poller said which
    /// (ADR-164 決定 25). The most recent failure's listing is the one kept, like its reason.
    pub fn record_failed_in(
        &self,
        org: Uuid,
        tier: MerakiTier,
        reason: MerakiSyncFailure,
        listing: Option<MerakiListing>,
        at_unix_ms: i64,
    ) {
        let mut tiers = self.tiers.lock().expect("meraki collect health poisoned");
        let state = tiers.entry((org, tier)).or_insert(TierState {
            failures: 0,
            reason,
            since_unix_ms: at_unix_ms,
            listing,
        });
        if state.failures == 0 {
            state.since_unix_ms = at_unix_ms;
        }
        state.failures = state.failures.saturating_add(1);
        state.reason = reason;
        state.listing = listing;
    }

    /// Read one collect report off the bus. An unknown failure token costs the *reason* (it reads
    /// as `internal`), never the fact that the collect failed.
    pub fn record_report(&self, report: &yagra_bus::MerakiCollectReport, at_unix_ms: i64) {
        match report.failure.as_deref() {
            None => self.record_answered(report.org, report.tier),
            Some(token) => self.record_failed_in(
                report.org,
                report.tier,
                MerakiSyncFailure::from_token(token),
                report
                    .listing
                    .as_deref()
                    .and_then(MerakiListing::from_token),
                at_unix_ms,
            ),
        }
    }

    /// Whether the last collect of `tier` for `org` was answered. `None` when nothing is known.
    #[must_use]
    pub fn answered(&self, org: Uuid, tier: MerakiTier) -> Option<bool> {
        self.tiers
            .lock()
            .expect("meraki collect health poisoned")
            .get(&(org, tier))
            .map(|s| s.failures == 0)
    }

    /// Every tier of `org` that is failing right now, in tier order.
    #[must_use]
    pub fn failing(&self, org: Uuid) -> Vec<TierFailure> {
        let tiers = self.tiers.lock().expect("meraki collect health poisoned");
        MerakiTier::ALL
            .into_iter()
            .filter_map(|tier| {
                let s = tiers.get(&(org, tier))?;
                (s.failures > 0).then_some(TierFailure {
                    tier,
                    reason: s.reason,
                    since_unix_ms: s.since_unix_ms,
                    failures: s.failures,
                    listing: s.listing,
                })
            })
            .collect()
    }

    /// Drop everything known about organizations that no longer exist.
    pub fn retain_orgs(&self, keep: &HashSet<Uuid>) {
        self.tiers
            .lock()
            .expect("meraki collect health poisoned")
            .retain(|(org, _), _| keep.contains(org));
    }
}

/// Whether a failure **core itself** knows about should be counted for `(org, tier)` now, and
/// remember that it was.
///
/// Three things stop the scheduler before a job is sent, and no poller can report any of them: the
/// stored key cannot be opened, the organization's imported devices cannot be read, and which
/// networks it watches cannot be read. Each leaves the organization's devices unasked-about, which
/// is exactly what [`MerakiCollectHealth`] is for — but the scheduler ticks every 15 seconds while
/// a collect is due every `tier_cadence`, so counting one per tick would turn 45 seconds into the
/// three failures that raise the alert. Counting one per cadence makes a core-side failure cost the
/// same evidence as a collect that was sent and failed.
///
/// The tier stays due either way, so a repaired key or a healthy database is picked up on the next
/// tick rather than at the next cadence. Clear the entry with `counted.remove(&(org, tier))` when
/// the tier gets past all three.
pub fn count_once_per_cadence(
    counted: &mut HashMap<(Uuid, MerakiTier), std::time::Instant>,
    org: Uuid,
    tier: MerakiTier,
    cadence: std::time::Duration,
    now: std::time::Instant,
) -> bool {
    if counted
        .get(&(org, tier))
        .is_some_and(|&at| now.duration_since(at) < cadence)
    {
        return false;
    }
    counted.insert((org, tier), now);
    true
}

/// What the watch needs to know about one organization's configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OrgFacts {
    /// The `meraki_orgs` row id.
    pub id: Uuid,
    /// Whether the organization is enabled (a paused one is sent no collect).
    pub enabled: bool,
}

/// Why an organization's alert is being resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolveWhy {
    /// An availability collect was answered.
    Answered,
    /// There is nothing left to answer for: the organization is gone or paused, or Meraki polling
    /// is switched off. Configuration is evidence too — nobody is being collected, by decision.
    NotCollected,
}

/// One thing the watch decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transition {
    /// Raise the organization's alert.
    Raise {
        /// The organization.
        org: Uuid,
        /// The availability tier's run of failures.
        failure: TierFailure,
    },
    /// Resolve it.
    Resolve {
        /// The organization.
        org: Uuid,
        /// On what evidence.
        why: ResolveWhy,
    },
}

/// Which organizations have their alert raised, and the rule that moves them. Pure: no clock, no
/// store, no engine — the loop hands it facts and carries out what it answers.
#[derive(Debug, Default)]
pub struct CollectWatch {
    raised: HashSet<Uuid>,
}

impl CollectWatch {
    /// Start from the alerts that were open before this process began.
    ///
    /// Without this a restart during an outage would raise the same alert a second time after
    /// three more failures — and, worse, an outage that ended while core was down would leave an
    /// alert nothing owns and nothing can close (the 15-day-open pool alert, ADR-107 増分 5).
    #[must_use]
    pub fn seed(raised: impl IntoIterator<Item = Uuid>) -> Self {
        Self {
            raised: raised.into_iter().collect(),
        }
    }

    /// Whether `org`'s alert is raised. Only the tests ask: the loop acts on what `evaluate` returns.
    #[cfg(test)]
    #[must_use]
    pub fn is_raised(&self, org: Uuid) -> bool {
        self.raised.contains(&org)
    }

    /// Decide. `orgs` is every organization there is; `polling_enabled` is the global switch.
    pub fn evaluate(
        &mut self,
        orgs: &[OrgFacts],
        polling_enabled: bool,
        health: &MerakiCollectHealth,
    ) -> Vec<Transition> {
        let mut out = Vec::new();
        let collected: HashSet<Uuid> = orgs
            .iter()
            .filter(|o| o.enabled && polling_enabled)
            .map(|o| o.id)
            .collect();

        // Resolve first, so an organization cannot be raised and resolved in one pass.
        let mut raised: Vec<Uuid> = self.raised.iter().copied().collect();
        raised.sort_unstable();
        for org in raised {
            let why = if !collected.contains(&org) {
                Some(ResolveWhy::NotCollected)
            } else if health.answered(org, MerakiTier::Availability) == Some(true) {
                Some(ResolveWhy::Answered)
            } else {
                // Still failing — or nothing is known yet, which resolves nothing.
                None
            };
            if let Some(why) = why {
                self.raised.remove(&org);
                out.push(Transition::Resolve { org, why });
            }
        }

        let mut candidates: Vec<Uuid> = collected.into_iter().collect();
        candidates.sort_unstable();
        for org in candidates {
            if self.raised.contains(&org) {
                continue;
            }
            // Only the tier liveness rides on. An organization whose uplink collects fail while
            // availability answers is not blind.
            let Some(failure) = health
                .failing(org)
                .into_iter()
                .find(|f| f.tier == MerakiTier::Availability)
            else {
                continue;
            };
            if failure.failures >= RAISE_AFTER_FAILURES {
                self.raised.insert(org);
                out.push(Transition::Raise { org, failure });
            }
        }
        out
    }
}

/// The organizations whose collect alert is open in `active` — what [`CollectWatch::seed`] starts
/// from after a restart. Filtered on the subject **and** the metric, like
/// `pool_coverage::raised_pools`: a Meraki-organization subject may one day carry some other alert,
/// and this loop must not adopt (and then resolve) one it knows nothing about.
#[must_use]
pub fn raised_orgs(active: &[yagra_alert::Alert]) -> Vec<Uuid> {
    let mut orgs: Vec<Uuid> = active
        .iter()
        .filter(|a| a.metric == COLLECT_METRIC)
        .filter_map(|a| a.subject.meraki_org())
        .collect();
    orgs.sort_unstable();
    orgs.dedup();
    orgs
}

/// What the organization's row should say is failing, given what it says now and what is known.
///
/// A tier that is failing is written. A tier that was **answered** is removed. A tier nothing is
/// known about keeps what the row already says — after a restart that is every tier, and clearing
/// them all would have the page say "collecting" about an outage still in progress. An
/// organization nobody is collected for (`collected == false`) says nothing is failing: nothing is
/// being asked.
#[must_use]
pub fn row_failures(
    stored: &[TierFailure],
    org: Uuid,
    collected: bool,
    health: &MerakiCollectHealth,
) -> Vec<TierFailure> {
    if !collected {
        return Vec::new();
    }
    let failing = health.failing(org);
    MerakiTier::ALL
        .into_iter()
        .filter_map(|tier| {
            if let Some(now) = failing.iter().find(|f| f.tier == tier) {
                return Some(*now);
            }
            match health.answered(org, tier) {
                Some(true) => None,
                _ => stored.iter().find(|f| f.tier == tier).copied(),
            }
        })
        .collect()
}

/// How often the watch looks. The scheduler's own tick: a run of three failures cannot complete
/// faster than three collects, so looking more often would decide nothing sooner.
const WATCH_TICK: std::time::Duration = std::time::Duration::from_secs(15);

/// The leader-only loop: turn unanswered leases into failures, decide, dispatch, and keep each
/// organization's row saying what is failing.
///
/// **Leader-gated** (spawned from `LeaderTasks`), because it raises alerts: two cores deciding
/// independently would page twice, and the record it reads is fed by the result stream only the
/// leader consumes. It holds no `Notifier` — everything leaves through the [`AlertSink`], which
/// persists and notifies as one step (ADR-092).
pub(crate) async fn run_meraki_collect_watch(
    orgs: std::sync::Arc<crate::meraki::MerakiOrgRepo>,
    settings: std::sync::Arc<crate::repo::NodeRepo>,
    inflight: std::sync::Arc<crate::meraki::MerakiInflight>,
    alerts: std::sync::Arc<crate::alerts::AlertManager>,
    sink: std::sync::Arc<dyn crate::alerts::sink::AlertSink>,
) {
    // The alerts survived the restart (`alerts::restore`, awaited in `run_live`); the watch that can
    // close them did not. Without the seed an outage that ended while this core was down leaves an
    // alert nothing owns — ADR-107 増分 5's fifteen-day pool alert, in another subject.
    let reopened = raised_orgs(&alerts.active_alerts());
    if !reopened.is_empty() {
        tracing::info!(orgs = ?reopened, "resuming the meraki collect watch for organizations whose alert was already open");
    }
    let mut watch = CollectWatch::seed(reopened);

    loop {
        tokio::time::sleep(WATCH_TICK).await;

        for (org, tier) in inflight.take_unanswered(std::time::Instant::now()) {
            tracing::warn!(%org, tier = tier.as_str(), "meraki collect was never answered");
            inflight.health.record_failed(
                org,
                tier,
                MerakiSyncFailure::NoAnswer,
                crate::pool_coverage::now_unix_ms(),
            );
        }

        // A failed read decides nothing: an empty list would resolve every open alert as "the
        // organization is gone".
        let list = match orgs.list().await {
            Ok(list) => list,
            Err(e) => {
                tracing::warn!(error = %e, "meraki collect watch: listing organizations failed");
                continue;
            }
        };
        let polling = settings.get_meraki_polling_enabled().await;
        let facts: Vec<OrgFacts> = list
            .iter()
            .map(|o| OrgFacts {
                id: o.id,
                enabled: o.enabled,
            })
            .collect();
        inflight
            .health
            .retain_orgs(&facts.iter().map(|o| o.id).collect());

        for transition in watch.evaluate(&facts, polling, &inflight.health) {
            let action = match transition {
                Transition::Raise { org, failure } => {
                    tracing::warn!(
                        %org,
                        reason = failure.reason.as_str(),
                        failures = failure.failures,
                        "the meraki api is not answering this organization's collects — its nodes keep their last state"
                    );
                    metrics::counter!("yagra_meraki_collect_alerts_total", "event" => "raise")
                        .increment(1);
                    alerts.raise_meraki_collect_alert(
                        org,
                        failure.failures,
                        crate::pool_coverage::now_unix_ms(),
                    )
                }
                Transition::Resolve { org, why } => {
                    tracing::info!(%org, ?why, "meraki collect alert resolved");
                    metrics::counter!("yagra_meraki_collect_alerts_total", "event" => "resolve")
                        .increment(1);
                    alerts.resolve_meraki_collect_alert(org)
                }
            };
            if let Some(action) = action {
                sink.dispatch(action).await;
            }
        }

        for org in &list {
            let want = row_failures(
                &org.collect_failures,
                org.id,
                org.enabled && polling,
                &inflight.health,
            );
            if want != org.collect_failures {
                if let Err(e) = orgs.record_collect_failures(org.id, &want).await {
                    tracing::warn!(org = %org.org_id, error = %e, "recording meraki collect failures failed");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ORG: Uuid = Uuid::from_u128(0xACE);
    const OTHER: Uuid = Uuid::from_u128(0xBEE);

    fn on(id: Uuid) -> OrgFacts {
        OrgFacts { id, enabled: true }
    }

    fn fail(health: &MerakiCollectHealth, org: Uuid, tier: MerakiTier, times: u32) {
        for i in 0..times {
            health.record_failed(org, tier, MerakiSyncFailure::Auth, 1_000 + i64::from(i));
        }
    }

    /// The scheduler ticks four times per minute and a collect is due once a cadence, so a failure
    /// core itself knows about has to be counted at the collect's rate — or a key that cannot be
    /// opened would reach the three-in-a-row threshold in 45 seconds instead of three cadences.
    #[test]
    fn a_failure_core_knows_about_is_counted_once_per_cadence_and_per_tier() {
        use std::time::{Duration, Instant};
        let mut counted = HashMap::new();
        let cadence = Duration::from_secs(300);
        let tick = Duration::from_secs(15);
        let start = Instant::now();

        assert!(
            count_once_per_cadence(&mut counted, ORG, MerakiTier::Availability, cadence, start),
            "the first one is evidence"
        );
        for n in 1..20 {
            assert!(
                !count_once_per_cadence(
                    &mut counted,
                    ORG,
                    MerakiTier::Availability,
                    cadence,
                    start + tick * n
                ),
                "tick {n} inside the cadence counted a second failure"
            );
        }
        assert!(
            count_once_per_cadence(
                &mut counted,
                ORG,
                MerakiTier::Availability,
                cadence,
                start + cadence
            ),
            "a whole cadence of failing is one more collect that did not happen"
        );

        // Each (organization, tier) is counted on its own — one organization's broken key says
        // nothing about another's, and uplink failing is not availability failing.
        assert!(count_once_per_cadence(
            &mut counted,
            ORG,
            MerakiTier::Uplink,
            cadence,
            start
        ));
        assert!(count_once_per_cadence(
            &mut counted,
            OTHER,
            MerakiTier::Availability,
            cadence,
            start
        ));

        // Cleared when the tier gets past the failure: the next one is evidence again at once.
        counted.remove(&(ORG, MerakiTier::Availability));
        assert!(count_once_per_cadence(
            &mut counted,
            ORG,
            MerakiTier::Availability,
            cadence,
            start + tick
        ));
    }

    #[test]
    fn the_third_failed_availability_collect_in_a_row_raises_once_and_the_second_does_not() {
        let health = MerakiCollectHealth::default();
        let mut watch = CollectWatch::default();

        fail(&health, ORG, MerakiTier::Availability, 2);
        assert_eq!(
            watch.evaluate(&[on(ORG)], true, &health),
            vec![],
            "two failures raised an alert the rule asks three for"
        );

        fail(&health, ORG, MerakiTier::Availability, 1);
        let raised = watch.evaluate(&[on(ORG)], true, &health);
        assert_eq!(raised.len(), 1, "{raised:?}");
        let Transition::Raise { org, failure } = raised[0] else {
            panic!("expected a raise, got {raised:?}");
        };
        assert_eq!(org, ORG);
        assert_eq!(failure.reason, MerakiSyncFailure::Auth);
        assert_eq!(failure.failures, 3);
        assert_eq!(
            failure.since_unix_ms, 1_000,
            "the run began at its first failure"
        );

        // A fourth failure is the same outage, not a second alert.
        fail(&health, ORG, MerakiTier::Availability, 1);
        assert_eq!(watch.evaluate(&[on(ORG)], true, &health), vec![]);
    }

    /// 決定 18 (3): the uplink and traffic tiers decide nothing about liveness, so they page nobody
    /// — a licence without one endpoint answers 403 on that tier forever.
    #[test]
    fn a_tier_that_liveness_does_not_ride_on_never_raises() {
        let health = MerakiCollectHealth::default();
        let mut watch = CollectWatch::default();
        fail(&health, ORG, MerakiTier::Uplink, 10);
        fail(&health, ORG, MerakiTier::Traffic, 10);
        health.record_answered(ORG, MerakiTier::Availability);
        assert_eq!(watch.evaluate(&[on(ORG)], true, &health), vec![]);
        // …and they are still what the organization's page is told.
        let shown: Vec<MerakiTier> = health.failing(ORG).iter().map(|f| f.tier).collect();
        assert_eq!(shown, vec![MerakiTier::Uplink, MerakiTier::Traffic]);
    }

    #[test]
    fn an_answered_collect_breaks_the_run_and_resolves_a_raised_alert() {
        let health = MerakiCollectHealth::default();
        let mut watch = CollectWatch::default();
        fail(&health, ORG, MerakiTier::Availability, 2);
        health.record_answered(ORG, MerakiTier::Availability);
        fail(&health, ORG, MerakiTier::Availability, 2);
        assert_eq!(
            watch.evaluate(&[on(ORG)], true, &health),
            vec![],
            "two, an answer, then two is not three in a row"
        );

        fail(&health, ORG, MerakiTier::Availability, 1);
        assert_eq!(watch.evaluate(&[on(ORG)], true, &health).len(), 1);
        health.record_answered(ORG, MerakiTier::Availability);
        assert_eq!(
            watch.evaluate(&[on(ORG)], true, &health),
            vec![Transition::Resolve {
                org: ORG,
                why: ResolveWhy::Answered
            }]
        );
        assert!(!watch.is_raised(ORG));
    }

    /// 🚨 ADR-156 決定 3. After a restart the record is empty — and so is the record of an
    /// organization whose poller has gone quiet. Neither is an answer.
    #[test]
    fn knowing_nothing_resolves_nothing() {
        let health = MerakiCollectHealth::default();
        let mut watch = CollectWatch::seed([ORG]);
        assert_eq!(
            watch.evaluate(&[on(ORG)], true, &health),
            vec![],
            "an alert was closed with no evidence that anything answered"
        );
        assert!(watch.is_raised(ORG));

        // An answer on another tier is not an answer to the question either.
        health.record_answered(ORG, MerakiTier::Uplink);
        assert_eq!(watch.evaluate(&[on(ORG)], true, &health), vec![]);
    }

    /// A restored alert is not raised a second time, and can still be closed.
    #[test]
    fn a_restored_alert_is_neither_raised_again_nor_stranded() {
        let health = MerakiCollectHealth::default();
        let mut watch = CollectWatch::seed([ORG]);
        fail(&health, ORG, MerakiTier::Availability, 5);
        assert_eq!(watch.evaluate(&[on(ORG)], true, &health), vec![]);

        health.record_answered(ORG, MerakiTier::Availability);
        assert_eq!(
            watch.evaluate(&[on(ORG)], true, &health),
            vec![Transition::Resolve {
                org: ORG,
                why: ResolveWhy::Answered
            }]
        );
    }

    /// Configuration is evidence: nobody is collected for a paused or deleted organization, or
    /// while Meraki polling is off, so there is no outage left to report — and none may be raised.
    #[test]
    fn an_organization_that_is_not_collected_has_its_alert_resolved_and_none_raised() {
        let health = MerakiCollectHealth::default();
        fail(&health, ORG, MerakiTier::Availability, 9);
        fail(&health, OTHER, MerakiTier::Availability, 9);

        let paused = OrgFacts {
            id: ORG,
            enabled: false,
        };
        let mut watch = CollectWatch::seed([ORG, OTHER]);
        // ORG is paused; OTHER is gone from the list altogether.
        let mut got = watch.evaluate(&[paused], true, &health);
        got.sort_by_key(|t| match t {
            Transition::Raise { org, .. } | Transition::Resolve { org, .. } => *org,
        });
        assert_eq!(
            got,
            vec![
                Transition::Resolve {
                    org: ORG,
                    why: ResolveWhy::NotCollected
                },
                Transition::Resolve {
                    org: OTHER,
                    why: ResolveWhy::NotCollected
                },
            ]
        );
        assert_eq!(
            watch.evaluate(&[paused], true, &health),
            vec![],
            "a paused organization was raised"
        );

        // The global switch does the same for every organization.
        let mut watch = CollectWatch::seed([ORG]);
        assert_eq!(
            watch.evaluate(&[on(ORG)], false, &health),
            vec![Transition::Resolve {
                org: ORG,
                why: ResolveWhy::NotCollected
            }]
        );
        assert_eq!(watch.evaluate(&[on(ORG)], false, &health), vec![]);
    }

    /// A token this build has never heard of costs the reason, never the failure.
    #[test]
    fn a_report_with_an_unknown_failure_token_still_counts_as_a_failure() {
        let health = MerakiCollectHealth::default();
        let report = |failure: Option<&str>| yagra_bus::MerakiCollectReport {
            org: ORG,
            tier: MerakiTier::Availability,
            failure: failure.map(str::to_owned),
            listing: None,
        };
        health.record_report(&report(Some("a_token_from_the_future")), 5);
        let failing = health.failing(ORG);
        assert_eq!(failing.len(), 1);
        assert_eq!(failing[0].reason, MerakiSyncFailure::Internal);
        assert_eq!(health.answered(ORG, MerakiTier::Availability), Some(false));

        health.record_report(&report(None), 6);
        assert_eq!(health.answered(ORG, MerakiTier::Availability), Some(true));
        assert_eq!(health.failing(ORG), vec![]);
    }

    #[test]
    fn an_organization_that_is_gone_is_forgotten() {
        let health = MerakiCollectHealth::default();
        fail(&health, ORG, MerakiTier::Availability, 1);
        fail(&health, OTHER, MerakiTier::Availability, 1);
        health.retain_orgs(&HashSet::from([OTHER]));
        assert_eq!(health.answered(ORG, MerakiTier::Availability), None);
        assert_eq!(
            health.answered(OTHER, MerakiTier::Availability),
            Some(false)
        );
    }

    fn stored(tier: MerakiTier, reason: MerakiSyncFailure) -> TierFailure {
        TierFailure {
            tier,
            reason,
            since_unix_ms: 500,
            failures: 7,
            listing: None,
        }
    }

    /// What the organization's row says. The middle case is the one that matters: after a restart
    /// nothing is known about any tier, and clearing the row would have the page say "collecting"
    /// about an outage that is still going on.
    #[test]
    fn the_row_is_cleared_by_an_answer_and_never_by_knowing_nothing() {
        let on_the_row = [
            stored(MerakiTier::Availability, MerakiSyncFailure::Auth),
            stored(MerakiTier::Uplink, MerakiSyncFailure::Upstream),
        ];
        let health = MerakiCollectHealth::default();
        assert_eq!(
            row_failures(&on_the_row, ORG, true, &health),
            on_the_row.to_vec(),
            "a core that has just started knows nothing, and cleared the row anyway"
        );

        // An answer clears that tier only; a fresh failure replaces what was stored.
        health.record_answered(ORG, MerakiTier::Uplink);
        health.record_failed(
            ORG,
            MerakiTier::Availability,
            MerakiSyncFailure::Unreachable,
            9_000,
        );
        let now = row_failures(&on_the_row, ORG, true, &health);
        assert_eq!(now.len(), 1, "{now:?}");
        assert_eq!(now[0].tier, MerakiTier::Availability);
        assert_eq!(now[0].reason, MerakiSyncFailure::Unreachable);
        assert_eq!((now[0].since_unix_ms, now[0].failures), (9_000, 1));

        // Nobody is collected for a paused organization, so nothing is failing.
        assert_eq!(row_failures(&on_the_row, ORG, false, &health), vec![]);
    }

    /// The seed adopts this loop's own alerts and nothing else: another alert about the same
    /// organization, or the same metric name on a node, is not one it may later resolve.
    #[test]
    fn only_this_loops_own_alerts_are_adopted_after_a_restart() {
        use yagra_alert::{Alert, Subject};
        use yagra_common::{CheckId, NodeId, NodeState, Severity};
        let alert = |subject: Subject, metric: &str| Alert {
            subject,
            check: CheckId(Uuid::new_v4()),
            severity: Severity::Critical,
            state: NodeState::Critical,
            at_unix_ms: 1,
            root_cause: None,
            flapping: false,
            metric: metric.to_owned(),
            breach: None,
            ifindex: None,
            row: None,
            row_name: None,
        };
        let active = [
            alert(Subject::MerakiOrg(ORG), COLLECT_METRIC),
            alert(Subject::MerakiOrg(OTHER), "something_else"),
            alert(Subject::Node(NodeId::from(OTHER)), COLLECT_METRIC),
            alert(Subject::Pool("tokyo".to_owned()), COLLECT_METRIC),
        ];
        assert_eq!(raised_orgs(&active), vec![ORG]);
    }

    /// The stored array is read leniently — the row it sits on is read by the collect scheduler.
    #[test]
    fn an_entry_a_newer_core_wrote_costs_that_entry_and_not_the_organization() {
        let value = serde_json::json!([
            {"tier": "availability", "reason": "auth", "since_unix_ms": 1, "failures": 3},
            {"tier": "a_tier_from_the_future", "reason": "auth", "since_unix_ms": 1, "failures": 1},
            {"tier": "uplink", "reason": "a_reason_from_the_future", "since_unix_ms": 1, "failures": 1},
            "not an object",
        ]);
        let read = TierFailure::from_stored(value);
        assert_eq!(read.len(), 1, "{read:?}");
        assert_eq!(read[0].tier, MerakiTier::Availability);
        assert_eq!(
            TierFailure::from_stored(serde_json::json!({"not": "an array"})),
            vec![]
        );

        // What this build writes, it reads back whole.
        let written =
            serde_json::to_value([stored(MerakiTier::Traffic, MerakiSyncFailure::NoAnswer)])
                .expect("serialize");
        assert_eq!(
            TierFailure::from_stored(written),
            vec![stored(MerakiTier::Traffic, MerakiSyncFailure::NoAnswer)]
        );
    }

    /// ADR-164 決定 25: a failure names the read of the tier that failed. The stored listing is read
    /// leniently — one a newer core wrote costs the label, never the entry, and an entry from before
    /// the field existed reads as "the whole collect".
    #[test]
    fn a_failure_names_its_listing_and_an_unknown_listing_costs_only_the_label() {
        let read = TierFailure::from_stored(serde_json::json!([
            {"tier": "uplink", "reason": "upstream", "since_unix_ms": 1, "failures": 2,
             "listing": "appliance_vpn_statuses"},
            {"tier": "traffic", "reason": "upstream", "since_unix_ms": 1, "failures": 1,
             "listing": "a_listing_from_the_future"},
            {"tier": "availability", "reason": "auth", "since_unix_ms": 1, "failures": 3},
        ]));
        assert_eq!(read.len(), 3, "{read:?}");
        assert_eq!(read[0].listing, Some(MerakiListing::ApplianceVpnStatuses));
        assert_eq!(read[1].listing, None);
        assert_eq!(read[2].listing, None);

        // From the report to the row: the listing arrives, is kept, and is written back out.
        let health = MerakiCollectHealth::default();
        health.record_report(
            &yagra_bus::MerakiCollectReport {
                org: ORG,
                tier: MerakiTier::Uplink,
                failure: Some("upstream".to_owned()),
                listing: Some("appliance_vpn_statuses".to_owned()),
            },
            7,
        );
        let failing = health.failing(ORG);
        assert_eq!(failing.len(), 1);
        assert_eq!(
            failing[0].listing,
            Some(MerakiListing::ApplianceVpnStatuses)
        );
        let written = serde_json::to_value(&failing).expect("serialize");
        assert_eq!(written[0]["listing"], "appliance_vpn_statuses");
        assert_eq!(TierFailure::from_stored(written), failing);

        // The uplink tier failing still raises nothing — only availability does (決定 18).
        assert_eq!(health.answered(ORG, MerakiTier::Availability), None);
    }
}
