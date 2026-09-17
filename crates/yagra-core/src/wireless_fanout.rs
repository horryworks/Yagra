// SPDX-License-Identifier: AGPL-3.0-only
//! A controller's AP inventory, turned into one poll result per imported AP node (ADR-064 increment
//! B2, 改訂 R13).
//!
//! The poller walks a controller's AP table and publishes **one** result for the controller, carrying
//! every AP in `PollResult.wlan`. It knows nothing about AP nodes: which APs are imported, and as
//! which node, is decided here in core. So core fans the inventory out — one synthesized result per
//! imported AP — before anything is stored or judged, and the AP node then goes through the same
//! ingest as every other node: its samples to the TSDB, its liveness to the alert engine.
//!
//! ## What an AP's result says
//!
//! | The controller says (and its word stands)  | The AP node gets |
//! |---|---|
//! | associated — it serves the AP              | `Reachable`, `wlan_ap_up = 1`, clients / CPU / memory / temperature |
//! | not associated — down, failed, not joined  | `Unreachable`, `wlan_ap_up = 0` |
//! | backup — an HA standby's view              | **nothing** |
//!
//! "Its word stands" is [`crate::wireless::ownership`], the same function the inventory writer asks.
//! Measured on the PoC's AC6508 pair: the standby reports every AP the active serves as `standby`
//! with CPU and memory of 0, and without the rule those zeros would replace the readings on every
//! poll — and an AP the active serves would read as down whenever the standby's view arrived last.
//!
//! ## What an AP's result never says
//!
//! **Absence is not a statement** (ADR-156 決定 3). An AP missing from an inventory, and an inventory
//! that did not arrive — the walk missed a column, the controller stopped answering — produce no
//! result at all. The AP's liveness is then simply not refreshed; nothing concludes it went down.
//! The controller's own `wlan_ap_walk_complete` is what says the walk failed.
//!
//! ## Where the state lives
//!
//! Who serves each AP is kept **in memory**, updated by every live inventory, because the rule has
//! to be applied per result on the hot path, where no query may run. It is seeded from PostgreSQL
//! before the consumer starts (the writer keeps the same column), and the node bindings are
//! refreshed from there every [`BINDINGS_REFRESH`] — that is how an AP imported a moment ago starts
//! getting results, and how a deleted AP node stops.

use std::collections::HashMap;
use std::sync::{Arc, PoisonError, RwLock};
use std::time::Duration;

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use uuid::Uuid;
use yagra_bus::{CheckOutcome, PollResult, Sample};
use yagra_common::{
    ap_id, NodeId, WlanApObservation, WlanApState, METRIC_WLAN_AP_CLIENT_COUNT,
    METRIC_WLAN_AP_CPU_PCT, METRIC_WLAN_AP_MEM_PCT, METRIC_WLAN_AP_TEMP_C, METRIC_WLAN_AP_UP,
};

use crate::wireless::{ownership, ApBinding, Ownership, WirelessRepo, OWNER_STALE_AFTER_SECS};

/// How often the node bindings are re-read. An AP imported (or deleted) takes at most this long to
/// start (or stop) receiving results.
pub(crate) const BINDINGS_REFRESH: Duration = Duration::from_secs(15);

/// How many refreshes pass between importer runs: once a minute.
const IMPORT_EVERY_REFRESHES: u32 = 4;

/// Which ingest path a result arrived on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Replay {
    /// The live consumer: the inventory is news, so it moves who serves each AP.
    Live,
    /// Store-and-forward replay, possibly hours old: judged against what is known now, and never
    /// allowed to move it — an old "associated" must not take an AP back from the controller
    /// serving it today.
    Backfill,
}

#[derive(Debug, Clone, Copy, Default)]
struct ApEntry {
    node: Option<NodeId>,
    owner: Option<Ownership>,
}

/// The fan-out's state: every AP it has heard of, its node when imported, and who serves it.
#[derive(Debug, Default)]
pub(crate) struct ApFanout {
    aps: RwLock<HashMap<Uuid, ApEntry>>,
}

impl ApFanout {
    #[must_use]
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Install the imported APs as PostgreSQL has them.
    ///
    /// The node binding is PostgreSQL's to decide — an AP it no longer lists has had its node deleted,
    /// and stops getting results. Who serves an AP is memory's, once memory has an opinion: the live
    /// consumer updates it on every inventory, while the stored column trails it by a writer flush.
    /// So a stored owner only fills an AP memory knows no owner for — which is every AP at startup.
    pub(crate) fn install(&self, bindings: &[ApBinding]) {
        let mut aps = self.aps.write().unwrap_or_else(PoisonError::into_inner);
        for entry in aps.values_mut() {
            entry.node = None;
        }
        for b in bindings {
            let entry = aps.entry(b.ap_id).or_default();
            entry.node = Some(NodeId::from(b.node_id));
            if entry.owner.is_none() {
                entry.owner = b.owner;
            }
        }
    }

    /// The results `result`'s AP inventory stands for, one per imported AP whose controller's word
    /// stands and says something. Empty for a result that carries no inventory.
    pub(crate) fn results_for(&self, result: &PollResult, replay: Replay) -> Vec<PollResult> {
        let Some(inventory) = &result.wlan else {
            return Vec::new();
        };
        let controller = result.node_id.as_uuid();
        let at = DateTime::<Utc>::from_timestamp_millis(result.at_unix_ms).unwrap_or_else(Utc::now);
        let stale_after = ChronoDuration::seconds(OWNER_STALE_AFTER_SECS);
        let mut out = Vec::new();
        let mut decide = |entry: ApEntry, ap: &WlanApObservation| {
            let verdict = ownership(entry.owner, controller, ap.state, at, stale_after);
            if let (Some(node), true) = (entry.node, verdict.take_state) {
                if let Some(r) = ap_result(result, node, ap) {
                    out.push(r);
                }
            }
            verdict.owner
        };
        match replay {
            Replay::Live => {
                let mut aps = self.aps.write().unwrap_or_else(PoisonError::into_inner);
                for ap in &inventory.aps {
                    let entry = aps.entry(ap_id(ap.mac)).or_default();
                    entry.owner = decide(*entry, ap);
                }
            }
            Replay::Backfill => {
                let aps = self.aps.read().unwrap_or_else(PoisonError::into_inner);
                for ap in &inventory.aps {
                    let entry = aps.get(&ap_id(ap.mac)).copied().unwrap_or_default();
                    decide(entry, ap);
                }
            }
        }
        if !out.is_empty() {
            metrics::counter!("yagra_wlan_ap_results_total").increment(out.len() as u64);
        }
        out
    }
}

/// One AP's result, or `None` for a standby's view, which says nothing about the AP.
fn ap_result(controller: &PollResult, node: NodeId, ap: &WlanApObservation) -> Option<PollResult> {
    let (outcome, samples) = match ap.state {
        WlanApState::Associated => {
            let mut samples = vec![Sample::gauge(METRIC_WLAN_AP_UP, 1.0)];
            let readings = [
                (METRIC_WLAN_AP_CLIENT_COUNT, ap.clients.map(f64::from)),
                (METRIC_WLAN_AP_CPU_PCT, ap.cpu_pct.map(f64::from)),
                (METRIC_WLAN_AP_MEM_PCT, ap.mem_pct.map(f64::from)),
                (METRIC_WLAN_AP_TEMP_C, ap.temp_c.map(f64::from)),
            ];
            samples.extend(
                readings
                    .into_iter()
                    .filter_map(|(metric, value)| value.map(|v| Sample::gauge(metric, v))),
            );
            (CheckOutcome::Reachable, samples)
        }
        WlanApState::NotAssociated => (
            CheckOutcome::Unreachable,
            vec![Sample::gauge(METRIC_WLAN_AP_UP, 0.0)],
        ),
        WlanApState::Backup => return None,
    };
    Some(PollResult {
        job_id: controller.job_id,
        node_id: node,
        at_unix_ms: controller.at_unix_ms,
        outcome,
        samples,
        interfaces: Vec::new(),
        sys_descr: None,
        os_version: None,
        os_version_without_patch: None,
        serial_number: None,
        sys_object_id: None,
        dns_chain: None,
        neighbors: None,
        l3: None,
        arp: None,
        routing: None,
        wlan: None,
        row_names: Vec::new(),
        // A liveness result: this is the only statement anything makes about whether the AP is up.
        observational: false,
        judge_samples: false,
        // Not a poller's result — the controller's is, and it is counted once, there.
        poller_id: None,
        trace_context: controller.trace_context.clone(),
    })
}

/// Leader-only upkeep of the fan-out: re-read the node bindings every [`BINDINGS_REFRESH`], and run
/// the importer once a minute (ADR-064 決定 8).
///
/// Started by the ingest pipeline after it has installed the first bindings, so the two share one
/// [`ApFanout`]. A failed read keeps the bindings already installed — an AP that stops getting
/// results because a query failed would look exactly like an AP that stopped being reported.
///
/// An import that created nodes bumps the configuration generation: the scheduler's cached round
/// must be rebuilt to learn the new nodes are APs, and until it is they are simply not polled, since
/// a cached round holds only the nodes it was built from.
pub(crate) async fn run_upkeep(fanout: Arc<ApFanout>, repo: Arc<WirelessRepo>) {
    let mut tick = tokio::time::interval(BINDINGS_REFRESH);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut n: u32 = 0;
    loop {
        tick.tick().await;
        n = n.wrapping_add(1);
        if n.is_multiple_of(IMPORT_EVERY_REFRESHES) {
            match repo.import_pending(Utc::now()).await {
                Ok(pass) => {
                    if pass.imported > 0 {
                        tracing::info!(
                            imported = pass.imported,
                            over_cap = pass.over_cap,
                            "imported wireless access points as nodes"
                        );
                        metrics::counter!("yagra_wlan_aps_imported_total")
                            .increment(u64::from(pass.imported));
                        crate::config_gen::bump();
                    }
                }
                Err(e) => tracing::warn!(error = %e, "wireless AP import pass failed"),
            }
        }
        match repo.ap_bindings().await {
            Ok(bindings) => fanout.install(&bindings),
            Err(e) => {
                tracing::warn!(error = %e, "wireless AP bindings refresh failed; keeping the last ones");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use yagra_common::{ApMac, WlanFlavor, WlanInventory};

    const ACTIVE: u128 = 1;
    const STANDBY: u128 = 2;

    fn mac(n: u8) -> ApMac {
        ApMac::new([0x54, 0xf6, 0xe2, 0, 0, n])
    }

    fn ap(n: u8, state: WlanApState, cpu: u32) -> WlanApObservation {
        WlanApObservation {
            mac: mac(n),
            name: Some(format!("ap-{n}")),
            serial: None,
            model: None,
            sw_version: None,
            ip: None,
            vendor_group: None,
            run_state: match state {
                WlanApState::Associated => "normal",
                WlanApState::Backup => "standby",
                WlanApState::NotAssociated => "fault",
            }
            .to_owned(),
            state,
            clients: Some(4),
            cpu_pct: Some(cpu),
            mem_pct: Some(30),
            temp_c: None,
        }
    }

    fn inventory_result(controller: u128, at_secs: i64, aps: Vec<WlanApObservation>) -> PollResult {
        PollResult {
            job_id: Uuid::new_v4(),
            node_id: NodeId::from(Uuid::from_u128(controller)),
            at_unix_ms: at_secs * 1000,
            outcome: CheckOutcome::Reachable,
            samples: Vec::new(),
            interfaces: Vec::new(),
            sys_descr: None,
            os_version: None,
            os_version_without_patch: None,
            serial_number: None,
            sys_object_id: None,
            dns_chain: None,
            neighbors: None,
            l3: None,
            arp: None,
            routing: None,
            wlan: Some(WlanInventory::bounded(WlanFlavor::Huawei, aps, 1024)),
            row_names: Vec::new(),
            observational: true,
            judge_samples: true,
            poller_id: Some("p1".into()),
            trace_context: Default::default(),
        }
    }

    fn node_of(n: u8) -> Uuid {
        Uuid::from_u128(1000 + u128::from(n))
    }

    fn imported(aps: &[u8]) -> ApFanout {
        let fanout = ApFanout::new();
        let bindings: Vec<ApBinding> = aps
            .iter()
            .map(|n| ApBinding {
                ap_id: ap_id(mac(*n)),
                node_id: node_of(*n),
                owner: None,
            })
            .collect();
        fanout.install(&bindings);
        fanout
    }

    fn sample(r: &PollResult, metric: &str) -> Option<f64> {
        r.samples
            .iter()
            .find(|s| s.metric == metric)
            .map(|s| s.value)
    }

    #[test]
    fn an_associated_ap_is_up_with_its_readings_and_only_imported_aps_get_a_result() {
        let fanout = imported(&[1]);
        let out = fanout.results_for(
            &inventory_result(
                ACTIVE,
                0,
                vec![
                    ap(1, WlanApState::Associated, 7),
                    ap(2, WlanApState::Associated, 9),
                ],
            ),
            Replay::Live,
        );
        assert_eq!(out.len(), 1, "AP 2 has no node and must produce nothing");
        let r = &out[0];
        assert_eq!(r.node_id.as_uuid(), node_of(1));
        assert_eq!(r.outcome, CheckOutcome::Reachable);
        assert!(!r.observational, "the AP's result is its liveness");
        assert_eq!(
            r.poller_id, None,
            "the controller's result is the one a poller is credited"
        );
        assert_eq!(sample(r, METRIC_WLAN_AP_UP), Some(1.0));
        assert_eq!(sample(r, METRIC_WLAN_AP_CPU_PCT), Some(7.0));
        assert_eq!(sample(r, METRIC_WLAN_AP_CLIENT_COUNT), Some(4.0));
        assert_eq!(
            sample(r, METRIC_WLAN_AP_TEMP_C),
            None,
            "an AP with no sensor gets no temperature, not a zero"
        );
    }

    #[test]
    fn the_standby_of_a_pair_never_publishes_its_zeros_or_takes_the_ap_down() {
        let fanout = imported(&[1, 2]);
        fanout.results_for(
            &inventory_result(
                ACTIVE,
                0,
                vec![
                    ap(1, WlanApState::Associated, 7),
                    ap(2, WlanApState::Associated, 8),
                ],
            ),
            Replay::Live,
        );
        // The standby's view of the same APs, a second later — and, for AP 2, a claim it is down
        // that must not stand while the active serves it.
        let out = fanout.results_for(
            &inventory_result(
                STANDBY,
                1,
                vec![
                    ap(1, WlanApState::Backup, 0),
                    ap(2, WlanApState::NotAssociated, 0),
                ],
            ),
            Replay::Live,
        );
        assert!(out.is_empty(), "the standby published {out:?}");
    }

    #[test]
    fn an_ap_down_on_both_members_is_down() {
        let fanout = imported(&[3]);
        let out = fanout.results_for(
            &inventory_result(ACTIVE, 0, vec![ap(3, WlanApState::NotAssociated, 0)]),
            Replay::Live,
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].outcome, CheckOutcome::Unreachable);
        assert_eq!(sample(&out[0], METRIC_WLAN_AP_UP), Some(0.0));
        assert_eq!(out[0].samples.len(), 1, "a down AP carries no readings");
    }

    #[test]
    fn a_switchover_moves_the_ap_to_the_member_that_now_serves_it() {
        let fanout = imported(&[1]);
        fanout.results_for(
            &inventory_result(ACTIVE, 0, vec![ap(1, WlanApState::Associated, 7)]),
            Replay::Live,
        );
        // Roles swap: the old standby now serves the AP, the old active holds it as a backup.
        let out = fanout.results_for(
            &inventory_result(STANDBY, 300, vec![ap(1, WlanApState::Associated, 6)]),
            Replay::Live,
        );
        assert_eq!(out.len(), 1);
        assert_eq!(sample(&out[0], METRIC_WLAN_AP_CPU_PCT), Some(6.0));
        let out = fanout.results_for(
            &inventory_result(ACTIVE, 301, vec![ap(1, WlanApState::NotAssociated, 0)]),
            Replay::Live,
        );
        assert!(
            out.is_empty(),
            "the former active's view must not take the AP down: {out:?}"
        );
    }

    #[test]
    fn a_backfilled_inventory_is_judged_but_moves_nothing() {
        let fanout = imported(&[1]);
        fanout.results_for(
            &inventory_result(ACTIVE, 1_000, vec![ap(1, WlanApState::Associated, 7)]),
            Replay::Live,
        );
        // An hour-old replay from the other member, saying it served the AP then.
        let replayed = fanout.results_for(
            &inventory_result(
                STANDBY,
                1_000 - 3_600,
                vec![ap(1, WlanApState::Associated, 5)],
            ),
            Replay::Backfill,
        );
        assert_eq!(
            replayed.len(),
            1,
            "what it said then is stored at its own time"
        );
        // …and it did not take the AP: the active still serves it, so the other member's live
        // "down" does not stand.
        let out = fanout.results_for(
            &inventory_result(STANDBY, 1_001, vec![ap(1, WlanApState::NotAssociated, 0)]),
            Replay::Live,
        );
        assert!(out.is_empty(), "the replay moved ownership: {out:?}");
    }

    #[test]
    fn a_stored_owner_seeds_memory_and_a_deleted_node_stops_getting_results() {
        let fanout = ApFanout::new();
        let now = Utc::now();
        fanout.install(&[ApBinding {
            ap_id: ap_id(mac(1)),
            node_id: node_of(1),
            owner: Some(Ownership {
                controller: Uuid::from_u128(ACTIVE),
                last_associated_at: now,
            }),
        }]);
        // Straight after a restart the standby's view arrives first: the stored owner keeps it out.
        let standby = inventory_result(
            STANDBY,
            now.timestamp() + 1,
            vec![ap(1, WlanApState::NotAssociated, 0)],
        );
        assert!(fanout.results_for(&standby, Replay::Live).is_empty());

        // The node is deleted: PostgreSQL no longer binds the AP.
        fanout.install(&[]);
        let active = inventory_result(
            ACTIVE,
            now.timestamp() + 2,
            vec![ap(1, WlanApState::Associated, 7)],
        );
        assert!(fanout.results_for(&active, Replay::Live).is_empty());
    }

    #[test]
    fn a_result_without_an_inventory_fans_out_to_nothing() {
        let fanout = imported(&[1]);
        let mut r = inventory_result(ACTIVE, 0, Vec::new());
        r.wlan = None;
        assert!(fanout.results_for(&r, Replay::Live).is_empty());
    }
}
