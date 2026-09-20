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
use yagra_bus::DiscoveredInterface;
use yagra_bus::{CheckOutcome, PollResult, Sample};
use yagra_common::{
    ap_id, IfIndex, MetricKind, NodeId, WlanApObservation, WlanApState, WlanRadioObservation,
    METRIC_IF_HC_IN_OCTETS, METRIC_IF_HC_OUT_OCTETS, METRIC_IF_OPER_STATUS,
    METRIC_WLAN_AP_CLIENT_COUNT, METRIC_WLAN_AP_CPU_PCT, METRIC_WLAN_AP_CPU_TEMP_C,
    METRIC_WLAN_AP_MEM_PCT, METRIC_WLAN_AP_POWER_STATE, METRIC_WLAN_AP_TEMP_C, METRIC_WLAN_AP_UP,
    METRIC_WLAN_RADIO_CHANNEL, METRIC_WLAN_RADIO_CHANNEL_UTIL_PCT, METRIC_WLAN_RADIO_CLIENT_COUNT,
    METRIC_WLAN_RADIO_CLIENT_SIGNAL_DBM, METRIC_WLAN_RADIO_INTERFERENCE_PCT,
    METRIC_WLAN_RADIO_NOISE_DBM, METRIC_WLAN_RADIO_TX_POWER_DBM,
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
                (METRIC_WLAN_AP_CPU_TEMP_C, ap.cpu_temp_c.map(f64::from)),
                // Only reachable from this arm, which is the point: a down AP answers `invalid(4)`
                // and would publish a power fault where the real fact is that the AP is down —
                // `wlan_ap_up` says that, and says it once.
                (METRIC_WLAN_AP_POWER_STATE, ap.power_state.map(f64::from)),
            ];
            samples.extend(
                readings
                    .into_iter()
                    .filter_map(|(metric, value)| value.map(|v| Sample::gauge(metric, v))),
            );
            for radio in &ap.radios {
                samples.extend(radio_samples(radio));
            }
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
        // The AP node has no ifTable walk of its own, so its radios are the only rows it ever
        // gets. They exist for three independent reasons: the Interfaces tab and
        // `get_interface_series` read this table, `dimension_of` falls back to it for a node with
        // no collection items of its own, and the threshold editor picks a port out of it.
        interfaces: radio_interfaces(ap),
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
        meraki_collect: None,
    })
}

/// One radio's samples, keyed by its slot so the AP node reads it as a port (ADR-064 R6/R9).
///
/// The traffic counters and the operational status go out under the **IF-MIB** names, which is
/// what makes a radio draw on every screen that already draws a port — throughput, the up/down
/// dot, the interface-tier threshold rules — without any of them learning what a radio is.
///
/// ⚠️ A reading the controller did not have is absent, never zero: `radio.noise_dbm` is `None`
/// where the dialect answered its invalid marker, and a 0 dBm noise floor would read as a radio
/// being drowned (ADR-064 改訂 R10).
fn radio_samples(radio: &WlanRadioObservation) -> Vec<Sample> {
    let slot = IfIndex(radio.slot);
    let gauge = |metric: &'static str, value: Option<f64>| {
        value.map(move |v| Sample::interface(metric, slot, v, MetricKind::Gauge))
    };
    let mut out: Vec<Sample> = [
        gauge(METRIC_WLAN_RADIO_CLIENT_COUNT, radio.clients.map(f64::from)),
        gauge(
            METRIC_WLAN_RADIO_CHANNEL_UTIL_PCT,
            radio.channel_util_pct.map(f64::from),
        ),
        gauge(
            METRIC_WLAN_RADIO_INTERFERENCE_PCT,
            radio.interference_pct.map(f64::from),
        ),
        gauge(METRIC_WLAN_RADIO_NOISE_DBM, radio.noise_dbm.map(f64::from)),
        gauge(
            METRIC_WLAN_RADIO_CLIENT_SIGNAL_DBM,
            radio.client_signal_dbm.map(f64::from),
        ),
        gauge(
            METRIC_WLAN_RADIO_TX_POWER_DBM,
            radio.tx_power_dbm.map(f64::from),
        ),
        gauge(METRIC_WLAN_RADIO_CHANNEL, radio.channel.map(f64::from)),
        radio.up.map(|up| {
            Sample::interface(
                METRIC_IF_OPER_STATUS,
                slot,
                if up { 1.0 } else { 2.0 },
                MetricKind::Gauge,
            )
        }),
    ]
    .into_iter()
    .flatten()
    .collect();
    #[allow(clippy::cast_precision_loss)]
    for (metric, value) in [
        (METRIC_IF_HC_IN_OCTETS, radio.in_octets),
        (METRIC_IF_HC_OUT_OCTETS, radio.out_octets),
    ] {
        if let Some(v) = value {
            out.push(Sample::interface(
                metric,
                slot,
                v as f64,
                MetricKind::Counter,
            ));
        }
    }
    out
}

/// The `interfaces` rows an AP's radios stand for.
///
/// 🚨 **`if_speed` stays `None`, and the channel width must never be put there.** A radio reports
/// a 20 MHz channel, which is not a line rate: stored as a speed it would make `if_in_util_pct`
/// out of twenty bits per second, and every radio would read as thousands of percent utilised.
/// `if_type` is IANAifType 71, `ieee80211`, so a reader can tell "this is not an Ethernet port"
/// from "we could not read it".
fn radio_interfaces(ap: &WlanApObservation) -> Vec<DiscoveredInterface> {
    ap.radios
        .iter()
        .map(|r| DiscoveredInterface {
            ifindex: IfIndex(r.slot),
            if_name: Some(r.band.label().to_owned()),
            if_alias: r.channel.map(|c| format!("channel {c}")),
            if_speed: None,
            if_duplex: None,
            if_type: Some(IF_TYPE_IEEE80211),
            // A radio has no pluggable and no optical window; every one of these is a question
            // about a wired port, and answering it would be inventing an answer.
            if_media: None,
            transceiver_model: None,
            rx_power_low_dbm: None,
            rx_power_high_dbm: None,
            tx_power_low_dbm: None,
            tx_power_high_dbm: None,
        })
        .collect()
}

/// IANAifType for a radio: `ieee80211(71)`.
const IF_TYPE_IEEE80211: i32 = 71;

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
            cpu_temp_c: None,
            power_state: None,
            radios: Vec::new(),
        }
    }

    /// One AP with the two radios a measured AP reports.
    fn ap_with_radios(n: u8, state: WlanApState) -> WlanApObservation {
        let mut a = ap(n, state, 5);
        a.radios = [
            (yagra_common::WlanBand::Band2G4, 1, 11),
            (yagra_common::WlanBand::Band5G, 2, 44),
        ]
        .into_iter()
        .map(|(band, slot, channel)| yagra_common::WlanRadioObservation {
            slot,
            band,
            up: Some(true),
            clients: Some(3),
            channel: Some(channel),
            channel_util_pct: Some(14),
            interference_pct: Some(2),
            noise_dbm: Some(-96),
            client_signal_dbm: Some(-77),
            tx_power_dbm: Some(23),
            in_octets: Some(2_555_209_504),
            out_octets: Some(5_819_880_561),
        })
        .collect();
        a
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
            meraki_collect: None,
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
    /// ADR-064 増分 C: a radio reaches the AP node as a **port** — a row in `interfaces` and a
    /// series keyed by its slot — so every screen that already draws a port draws a radio.
    #[tokio::test]
    async fn an_aps_radios_arrive_as_ports_on_its_node() {
        let fanout = imported(&[1]);
        let out = fanout.results_for(
            &inventory_result(1, 1_000, vec![ap_with_radios(1, WlanApState::Associated)]),
            Replay::Live,
        );
        let r = out.first().expect("the AP gets a result");

        // Two interface rows, named by band, with no speed and the ieee80211 type.
        let slots: Vec<u32> = r.interfaces.iter().map(|i| i.ifindex.0).collect();
        assert_eq!(slots, vec![1, 2]);
        assert_eq!(r.interfaces[0].if_name.as_deref(), Some("2.4 GHz"));
        assert_eq!(r.interfaces[1].if_alias.as_deref(), Some("channel 44"));
        for i in &r.interfaces {
            assert_eq!(
                i.if_speed, None,
                "a channel width is not a line rate and must never become one"
            );
            assert_eq!(i.if_type, Some(71));
        }

        // The radio-specific readings are keyed by the slot, not by the node.
        let at = |metric: &str, slot: u32| {
            r.samples
                .iter()
                .find(|s| s.metric == metric && s.ifindex == Some(yagra_common::IfIndex(slot)))
                .map(|s| s.value)
        };
        assert_eq!(at("wlan_radio_client_count", 1), Some(3.0));
        assert_eq!(at("wlan_radio_noise_dbm", 2), Some(-96.0));
        assert_eq!(at("wlan_radio_channel_util_pct", 2), Some(14.0));
        // …and the traffic and status ride the IF-MIB names, which is what makes a radio draw
        // like a port without any screen learning what a radio is.
        assert_eq!(at("if_oper_status", 1), Some(1.0));
        assert_eq!(at("if_hc_in_octets", 1), Some(2_555_209_504.0));

        // The AP's own readings are still node-level, not attributed to a radio.
        let clients = r
            .samples
            .iter()
            .find(|s| s.metric == "wlan_ap_client_count")
            .expect("the AP client count is still published");
        assert_eq!(clients.ifindex, None);
    }

    /// The standby rule covers radios for free, which is the point of carrying them inside the AP:
    /// a standby reports every radio value as 0, and none of it may reach the node.
    #[tokio::test]
    async fn a_standbys_radios_are_not_recorded_either() {
        let fanout = imported(&[1]);
        // The active says it serves the AP, so the standby is the one whose word does not stand.
        fanout.results_for(
            &inventory_result(1, 1_000, vec![ap_with_radios(1, WlanApState::Associated)]),
            Replay::Live,
        );
        let mut standby = ap_with_radios(1, WlanApState::Backup);
        for r in &mut standby.radios {
            r.clients = Some(0);
            r.channel_util_pct = Some(0);
        }
        let out = fanout.results_for(&inventory_result(2, 1_010, vec![standby]), Replay::Live);
        assert!(
            out.is_empty(),
            "a standby's view produces no result at all, radios included"
        );
    }
}
