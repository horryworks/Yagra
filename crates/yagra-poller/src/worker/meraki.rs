// SPDX-License-Identifier: AGPL-3.0-only
//! Cisco Meraki org-scoped collection: **one job in, many results out**.
//!
//! Unlike every other check here, the transport pages an org's Dashboard endpoints and answers with
//! per-device observations, so this emits one ordinary [`PollResult`] per device (attributed
//! through the inlined serial→node_id map) and the whole consume/write/alert spine works unchanged.
//! That shape is why it is dispatched from [`super::run_stream`] rather than from
//! [`super::execute`], which is one job → one result by construction.
//!
//! # Which tier speaks for liveness (ADR-164)
//!
//! Every result's `outcome` feeds the node's liveness state machine, and a Meraki node is never
//! pinged — so what these results say is all the engine ever hears about it. Only the
//! **availability** tier asks the question liveness is about, so only it answers: its outcome is
//! read from the `meraki_device_up` sample it carries ([`availability_outcome`]). The uplink and
//! traffic tiers are `observational`: they state nothing about reachability, and their samples are
//! still judged against threshold rules (`judge_samples`), exactly as before.
//!
//! This used to be `Reachable` on every result of every tier. A device the Dashboard reported
//! offline therefore read as healthy, and would have even with the availability tier fixed alone:
//! the next uplink result, minutes later, would have answered `Reachable` and cancelled the outage.

use super::*;
use yagra_common::{MerakiTier, METRIC_MERAKI_DEVICE_UP};

/// What an availability result says about the device, read from the sample the transport made.
///
/// The Dashboard's status word is read in exactly one place
/// (`yagra_transport::MerakiAvailability::is_up`), which is what decides `meraki_device_up`. Reading
/// that sample rather than the word again is what keeps the node's state and its chart from ever
/// disagreeing: `offline`, `dormant` and a word this build has never seen are all `0` there, and all
/// `Unreachable` here.
///
/// A result with no such sample is [`CheckOutcome::Error`] — the check could not decide — rather
/// than either answer. The availability collect cannot currently produce one, which is the reason
/// to say what it would mean instead of leaving it to a default.
fn availability_outcome(samples: &[Sample]) -> CheckOutcome {
    match samples
        .iter()
        .find(|s| s.metric == METRIC_MERAKI_DEVICE_UP && s.ifindex.is_none())
    {
        Some(up) if up.value >= 0.5 => CheckOutcome::Reachable,
        Some(_) => CheckOutcome::Unreachable,
        None => CheckOutcome::Error,
    }
}

/// How one tier's results take part in alerting: `(outcome, observational, judge_samples)`.
///
/// Exhaustive on purpose. A tier added later has to say whether it speaks for liveness, because
/// the default — a `Reachable` nobody decided on — is the defect this replaced.
fn tier_verdict(tier: MerakiTier, samples: &[Sample]) -> (CheckOutcome, bool, bool) {
    match tier {
        MerakiTier::Availability => (availability_outcome(samples), false, false),
        // The outcome is a placeholder the engine never reads for an observational result.
        MerakiTier::Uplink | MerakiTier::Traffic | MerakiTier::Inventory => {
            (CheckOutcome::Reachable, true, true)
        }
    }
}

/// Execute a Cisco Meraki org-scoped collect. Unlike the per-node checks, this fans **one** job out
/// to **many** results: the transport pages the org's Dashboard endpoints (read-only) and returns
/// per-device observations, and we emit one ordinary [`PollResult`] per device (attributed via the
/// inlined serial→node_id map) so the whole consume/write/alert spine works unchanged. A device the
/// API reports but that we didn't import is simply skipped (scope enforced at fan-out). Metrics are
/// gauges (ADR-012 exception — the source pre-aggregates); uplinks become interface inventory rows.
pub async fn execute_meraki(
    job: &PollJob,
    transport: &dyn Transport,
    at_unix_ms: i64,
) -> Vec<PollResult> {
    let CheckSpec::MerakiCollect(check) = &job.check else {
        return Vec::new();
    };
    let timeout = Duration::from_millis(u64::from(check.timeout_ms));
    let spec = MerakiCollectSpec {
        org_id: check.org_id.clone(),
        base_url: check.base_url.clone(),
        api_key: check.api_key.clone(),
        tier: check.tier,
        network_ids: check.network_ids.clone(),
        per_page: check.per_page,
        target_rps: check.target_rps,
    };
    let observations = match transport.collect_meraki(&spec, timeout).await {
        Ok(obs) => obs,
        Err(err) => {
            tracing::warn!(job_id = %job.job_id, org = %check.org_id, error = %err, "meraki collect failed");
            return Vec::new();
        }
    };

    let by_serial: HashMap<&str, NodeId> = check
        .devices
        .iter()
        .map(|d| (d.serial.as_str(), d.node_id))
        .collect();

    let mut results = Vec::new();
    for obs in observations {
        let Some(&node_id) = by_serial.get(obs.serial.as_str()) else {
            continue; // reported by the API but not imported → not in scope
        };
        let samples: Vec<Sample> = obs
            .samples
            .into_iter()
            .map(|s| match s.ifindex {
                Some(idx) => Sample::interface(s.metric, IfIndex(idx), s.value, MetricKind::Gauge),
                None => Sample::gauge(s.metric, s.value),
            })
            .collect();
        let (outcome, observational, judge_samples) = tier_verdict(check.tier, &samples);
        let interfaces = obs
            .uplinks
            .into_iter()
            .map(|u| DiscoveredInterface {
                ifindex: IfIndex(u.ifindex),
                if_name: Some(u.name),
                if_alias: None,
                if_speed: None,
                // The Meraki API reports no link mode either — these come from EtherLike-MIB.
                if_duplex: None,
                if_type: None,
                if_media: None,
                transceiver_model: None,
                // Meraki reports no transceiver diagnostics; the optical probe is SNMP-only.
                rx_power_low_dbm: None,
                rx_power_high_dbm: None,
                tx_power_low_dbm: None,
                tx_power_high_dbm: None,
            })
            .collect();
        results.push(PollResult {
            job_id: job.job_id,
            node_id,
            at_unix_ms,
            outcome,
            samples,
            interfaces,
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
            observational,
            judge_samples,
            poller_id: None,
            trace_context: Default::default(),
        });
    }
    results
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;
    use yagra_common::NodeId;
    use yagra_transport::FakeTransport;

    /// One imported device's results for `tier`, when the Dashboard reports `up` for it.
    async fn results_for(tier: MerakiTier, up: f64) -> Vec<PollResult> {
        use yagra_bus::{MerakiCollectCheck, MerakiDeviceRef};
        use yagra_transport::{MerakiObservation, MerakiSample};

        let transport = FakeTransport::reachable(1.0).with_meraki(vec![MerakiObservation {
            serial: "Q2-A".into(),
            samples: vec![MerakiSample {
                metric: METRIC_MERAKI_DEVICE_UP.into(),
                ifindex: None,
                value: up,
            }],
            uplinks: vec![],
        }]);
        let check = MerakiCollectCheck {
            org_id: "1".into(),
            meraki_org_uuid: Uuid::nil(),
            tier,
            base_url: "https://api.meraki.com".into(),
            api_key: "k".into(),
            devices: vec![MerakiDeviceRef {
                serial: "Q2-A".into(),
                node_id: NodeId::new(),
            }],
            network_ids: vec![],
            per_page: 1000,
            target_rps: 2.0,
            timeout_ms: 30_000,
        };
        let job = PollJob::meraki_collect(Uuid::nil(), check, 300);
        execute_meraki(&job, &transport, 42).await
    }

    /// 🚨 The defect (ADR-164): every result said `Reachable`, so a device the Dashboard reported
    /// offline read as healthy. A Meraki node is never pinged — this outcome is all the alert
    /// engine hears about it.
    #[tokio::test]
    async fn a_device_the_dashboard_reports_down_is_unreachable() {
        let results = results_for(MerakiTier::Availability, 0.0).await;
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].outcome, CheckOutcome::Unreachable);
        assert!(
            !results[0].observational,
            "the availability tier is the one that speaks for liveness"
        );
        assert!(
            results[0]
                .samples
                .iter()
                .any(|s| s.metric == METRIC_MERAKI_DEVICE_UP && s.value == 0.0),
            "the chart still gets the reading the state was decided from"
        );
    }

    /// The way back. Without it the fix above would hold a recovered device down forever.
    #[tokio::test]
    async fn a_device_the_dashboard_reports_up_is_reachable_and_says_so() {
        let results = results_for(MerakiTier::Availability, 1.0).await;
        assert_eq!(results[0].outcome, CheckOutcome::Reachable);
        assert!(!results[0].observational);
    }

    /// The other direction, and the one fixing the availability tier alone would have missed: an
    /// uplink or traffic result for a device that is **down** must not answer `Reachable` into the
    /// dwell window the availability tier is filling. Derived from `MerakiTier::ALL`, so a tier
    /// added later is asked the question instead of inheriting an answer.
    #[tokio::test]
    async fn no_other_tier_speaks_for_liveness_and_each_still_has_its_samples_judged() {
        let mut spoke = Vec::new();
        for tier in MerakiTier::ALL {
            let results = results_for(tier, 0.0).await;
            assert_eq!(results.len(), 1, "{tier:?}");
            let r = &results[0];
            if r.observational {
                assert!(
                    r.judge_samples,
                    "{tier:?}: its thresholds were judged before this change and must still be"
                );
            } else {
                spoke.push(tier);
            }
        }
        assert_eq!(spoke, vec![MerakiTier::Availability]);
    }

    /// Not reachable today — the availability collect builds each observation *from* this sample —
    /// so this pins what the absence would mean rather than leaving it to whichever arm came last.
    #[test]
    fn an_availability_result_without_the_reading_cannot_decide() {
        assert_eq!(availability_outcome(&[]), CheckOutcome::Error);
        // A per-uplink series of the same name would not be the device's reading either.
        let uplink = Sample::interface(METRIC_MERAKI_DEVICE_UP, IfIndex(1), 1.0, MetricKind::Gauge);
        assert_eq!(availability_outcome(&[uplink]), CheckOutcome::Error);
    }

    #[tokio::test]
    async fn meraki_collect_fans_out_to_mapped_nodes_only() {
        use yagra_bus::{MerakiCollectCheck, MerakiDeviceRef};
        use yagra_common::MerakiTier;
        use yagra_transport::{MerakiObservation, MerakiSample, MerakiUplink};

        let node_a = NodeId::new();
        let transport = FakeTransport::reachable(1.0).with_meraki(vec![
            MerakiObservation {
                serial: "Q2-A".into(),
                samples: vec![
                    MerakiSample {
                        metric: "meraki_device_up".into(),
                        ifindex: None,
                        value: 1.0,
                    },
                    MerakiSample {
                        metric: "meraki_uplink_loss_pct".into(),
                        ifindex: Some(1),
                        value: 0.5,
                    },
                ],
                uplinks: vec![MerakiUplink {
                    ifindex: 1,
                    name: "WAN1".into(),
                }],
            },
            // Reported by the API but not imported → must be skipped (scope at fan-out).
            MerakiObservation {
                serial: "Q2-UNMAPPED".into(),
                samples: vec![MerakiSample {
                    metric: "meraki_device_up".into(),
                    ifindex: None,
                    value: 1.0,
                }],
                uplinks: vec![],
            },
        ]);

        let check = MerakiCollectCheck {
            org_id: "1".into(),
            meraki_org_uuid: Uuid::nil(),
            tier: MerakiTier::Uplink,
            base_url: "https://api.meraki.com".into(),
            api_key: "k".into(),
            devices: vec![MerakiDeviceRef {
                serial: "Q2-A".into(),
                node_id: node_a,
            }],
            network_ids: vec![],
            per_page: 1000,
            target_rps: 2.0,
            timeout_ms: 30_000,
        };
        let job = PollJob::meraki_collect(Uuid::nil(), check, 300);

        let results = execute_meraki(&job, &transport, 42).await;
        assert_eq!(results.len(), 1, "only the imported device is emitted");
        let r = &results[0];
        assert_eq!(r.node_id, node_a);
        assert_eq!(r.at_unix_ms, 42);
        assert!(
            r.observational && r.judge_samples,
            "an uplink result says nothing about liveness, and its samples are still judged"
        );
        assert!(r
            .samples
            .iter()
            .any(|s| s.metric == "meraki_uplink_loss_pct" && s.ifindex == Some(IfIndex(1))));
        assert_eq!(
            r.interfaces,
            vec![DiscoveredInterface {
                ifindex: IfIndex(1),
                if_name: Some("WAN1".into()),
                if_alias: None,
                if_speed: None,
                if_duplex: None,
                if_type: None,
                if_media: None,
                transceiver_model: None,
                // A Meraki uplink is not an SNMP transceiver; the optical window is never filled
                // from this path, and `None` here leaves anything already stored untouched.
                rx_power_low_dbm: None,
                rx_power_high_dbm: None,
                tx_power_low_dbm: None,
                tx_power_high_dbm: None,
            }]
        );
    }
}
