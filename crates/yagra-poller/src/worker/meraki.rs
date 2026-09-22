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
//!
//! # How a collect ended (ADR-164 決定 18)
//!
//! Beside the per-device results, every collect publishes **one report** — a result of its own
//! that carries [`PollResult::meraki_collect`] and nothing else. A collect that failed used to
//! publish nothing, so core could not tell "the Dashboard is not answering" from "nothing is due":
//! every node of the organization kept its last state, nothing alerted, and the single flight
//! core holds per organization stayed taken for its whole lease. The report is sent on success as
//! well, because an alert may only be closed on evidence — and "the Dashboard answered" is it.

use super::*;
use yagra_bus::{MerakiCollectReport, RowName};
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
    let spec = spec_for(job, check);
    // `failure` is the closed reason core is told. A collect that was refused outright has one;
    // so does one that stopped early with nothing in hand — a Dashboard outage, a 429 storm and
    // a dropped connection all used to arrive here as an `Ok` with no observations, log nothing,
    // and leave every node of the organization at its last state. A partial answer is a success:
    // the Dashboard did answer.
    // `listing` names which of the tier's reads failed when one did while the others answered
    // (ADR-164 決定 25): those readings still go out below, and the report says what is missing.
    let (observations, failure, listing) = match transport.collect_meraki(&spec, timeout).await {
        Ok(collected) => {
            let failure = collected.failure();
            let listing = collected.failed_listing.map(|(l, _)| l);
            (collected.observations, failure, listing)
        }
        Err(err) => (Vec::new(), Some(err), None),
    };
    if let Some(why) = failure {
        tracing::warn!(job_id = %job.job_id, org = %check.org_id, tier = ?check.tier, listing = listing.map(yagra_common::MerakiListing::as_str), error = %why, "meraki collect failed");
    }

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
        let row_names = uplink_row_names(&obs.samples, &obs.uplinks);
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
            hardware_model: None,
            sys_object_id: None,
            dns_chain: None,
            neighbors: None,
            l3: None,
            arp: None,
            routing: None,
            wlan: None,
            row_names,
            observational,
            judge_samples,
            poller_id: None,
            trace_context: Default::default(),
            meraki_collect: None,
        });
    }
    results.push(collect_report(job, check, failure, listing, at_unix_ms));
    results
}

/// What the transport is asked for this job. The collect's interval travels on the job, not on
/// the check, and the traffic tier needs it: its usage window is the interval, so consecutive
/// collects tile time (ADR-164 決定 23).
fn spec_for(job: &PollJob, check: &yagra_bus::MerakiCollectCheck) -> MerakiCollectSpec {
    MerakiCollectSpec {
        org_id: check.org_id.clone(),
        base_url: check.base_url.clone(),
        api_key: check.api_key.clone(),
        tier: check.tier,
        network_ids: check.network_ids.clone(),
        per_page: check.per_page,
        target_rps: check.target_rps,
        interval_secs: job.interval_secs,
    }
}

/// Names for the per-uplink rows of one device's samples (ADR-164 決定 24): every metric that
/// carries an uplink key gets that uplink's name (WAN1 / WAN2 / cellular). An alert on one uplink
/// then says which, and a rule can be narrowed to one uplink with a row pattern (ADR-143).
///
/// Built from the samples this result carries, so no name is sent for a row without a reading. The
/// name comes from the uplinks the transport reported beside them, falling back to the synthetic
/// index's canonical name.
fn uplink_row_names(
    samples: &[yagra_transport::MerakiSample],
    uplinks: &[yagra_transport::MerakiUplink],
) -> Vec<RowName> {
    let mut names: Vec<RowName> = Vec::new();
    for s in samples {
        let Some(row) = s.ifindex else {
            continue;
        };
        if names.iter().any(|n| n.metric == s.metric && n.row == row) {
            continue;
        }
        let name = uplinks
            .iter()
            .find(|u| u.ifindex == row)
            .map(|u| u.name.clone())
            .or_else(|| yagra_common::uplink_name(row).map(str::to_owned));
        if let Some(name) = name {
            names.push(RowName {
                metric: s.metric.clone(),
                row,
                name,
            });
        }
    }
    RowName::cleaned(&names)
}

/// The one result that says how this collect ended.
///
/// It is addressed to `job.node_id`, which for a collect job is the organization's own uuid
/// ([`PollJob::meraki_collect`] set it as a sentinel long before this existed) — no node has that
/// id, so nothing can mistake the report for a reading of a device. `observational` with no
/// samples is what makes it inert to a core from before 決定 18: that core persists nothing for
/// it and returns before the alert engine, having released the organization's single flight on
/// the way, which is the one effect worth having there.
fn collect_report(
    job: &PollJob,
    check: &yagra_bus::MerakiCollectCheck,
    failure: Option<yagra_transport::MerakiFetchError>,
    listing: Option<yagra_common::MerakiListing>,
    at_unix_ms: i64,
) -> PollResult {
    PollResult {
        job_id: job.job_id,
        node_id: job.node_id,
        at_unix_ms,
        // A placeholder the engine never reads for an observational result.
        outcome: CheckOutcome::Reachable,
        samples: Vec::new(),
        interfaces: Vec::new(),
        sys_descr: None,
        os_version: None,
        os_version_without_patch: None,
        serial_number: None,
        hardware_model: None,
        sys_object_id: None,
        dns_chain: None,
        neighbors: None,
        l3: None,
        arp: None,
        routing: None,
        wlan: None,
        row_names: Vec::new(),
        observational: true,
        judge_samples: false,
        poller_id: None,
        trace_context: Default::default(),
        meraki_collect: Some(MerakiCollectReport {
            org: check.meraki_org_uuid,
            tier: check.tier,
            failure: failure.map(|why| why.token().to_owned()),
            listing: listing.map(|l| l.as_str().to_owned()),
        }),
    }
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
        device_results(execute_meraki(&job, &transport, 42).await)
    }

    /// The per-device results of a collect, without the report that says how it ended.
    fn device_results(results: Vec<PollResult>) -> Vec<PollResult> {
        results
            .into_iter()
            .filter(|r| r.meraki_collect.is_none())
            .collect()
    }

    /// A collect of `tier` for organization `org` against `transport`: every result it published.
    async fn collect_with(
        transport: &FakeTransport,
        org: Uuid,
        tier: MerakiTier,
    ) -> Vec<PollResult> {
        use yagra_bus::{MerakiCollectCheck, MerakiDeviceRef};
        let check = MerakiCollectCheck {
            org_id: "1".into(),
            meraki_org_uuid: org,
            tier,
            base_url: "https://api.meraki.com".into(),
            api_key: "k".into(),
            devices: vec![MerakiDeviceRef {
                serial: "Q2-A".into(),
                node_id: NodeId::new(),
            }],
            network_ids: vec!["N_1".into()],
            per_page: 1000,
            target_rps: 2.0,
            timeout_ms: 30_000,
        };
        let job = PollJob::meraki_collect(Uuid::from_u128(7), check, 300);
        execute_meraki(&job, transport, 42).await
    }

    fn one_device_up() -> Vec<yagra_transport::MerakiObservation> {
        vec![yagra_transport::MerakiObservation {
            serial: "Q2-A".into(),
            samples: vec![yagra_transport::MerakiSample {
                metric: METRIC_MERAKI_DEVICE_UP.into(),
                ifindex: None,
                value: 1.0,
            }],
            uplinks: vec![],
        }]
    }

    /// The reports among `results` — there must be exactly one per collect.
    fn reports(results: &[PollResult]) -> Vec<&PollResult> {
        results
            .iter()
            .filter(|r| r.meraki_collect.is_some())
            .collect()
    }

    /// The traffic tier's usage window is the collect's interval, which travels on the job and not
    /// on the check (ADR-164 決定 23) — dropped here, every window would be the transport's floor.
    #[test]
    fn the_transport_is_told_the_jobs_interval() {
        use yagra_bus::MerakiCollectCheck;
        let check = MerakiCollectCheck {
            org_id: "1".into(),
            meraki_org_uuid: Uuid::nil(),
            tier: MerakiTier::Traffic,
            base_url: "https://api.meraki.com".into(),
            api_key: "k".into(),
            devices: vec![],
            network_ids: vec!["N_1".into()],
            per_page: 1000,
            target_rps: 2.0,
            timeout_ms: 30_000,
        };
        let job = PollJob::meraki_collect(Uuid::nil(), check.clone(), 1_800);
        let spec = spec_for(&job, &check);
        assert_eq!(spec.interval_secs, 1_800);
        assert_eq!(spec.tier, MerakiTier::Traffic);
        assert_eq!(spec.network_ids, ["N_1"]);
    }

    /// Per-uplink readings carry their uplink's name, once per (metric, row); device-level ones
    /// carry none (ADR-164 決定 24).
    #[tokio::test]
    async fn per_uplink_samples_carry_their_uplinks_name() {
        use yagra_transport::{MerakiObservation, MerakiSample, MerakiUplink};
        let sample = |metric: &str, ifindex: Option<u32>| MerakiSample {
            metric: metric.into(),
            ifindex,
            value: 1.0,
        };
        let transport = FakeTransport::reachable(1.0).with_meraki(vec![MerakiObservation {
            serial: "Q2-A".into(),
            samples: vec![
                sample("meraki_uplink_sent_bps", Some(1)),
                sample("meraki_uplink_recv_bps", Some(1)),
                sample("meraki_uplink_sent_bps", Some(3)),
                sample(METRIC_MERAKI_DEVICE_UP, None),
            ],
            uplinks: vec![
                MerakiUplink {
                    ifindex: 1,
                    name: "WAN1".into(),
                },
                MerakiUplink {
                    ifindex: 3,
                    name: "cellular".into(),
                },
            ],
        }]);
        let results =
            device_results(collect_with(&transport, Uuid::from_u128(1), MerakiTier::Traffic).await);
        assert_eq!(results.len(), 1);
        let mut names: Vec<_> = results[0]
            .row_names
            .iter()
            .map(|n| (n.metric.as_str(), n.row, n.name.as_str()))
            .collect();
        names.sort_unstable();
        assert_eq!(
            names,
            [
                ("meraki_uplink_recv_bps", 1, "WAN1"),
                ("meraki_uplink_sent_bps", 1, "WAN1"),
                ("meraki_uplink_sent_bps", 3, "cellular"),
            ]
        );
    }

    /// ADR-164 決定 25: one read of the uplink tier failed while the others answered — the readings
    /// that did arrive still go out, and the report says the tier failed and which read it was.
    #[tokio::test]
    async fn a_collect_with_one_failed_listing_keeps_its_readings_and_names_the_listing() {
        use yagra_common::MerakiListing;
        use yagra_transport::MerakiFetchError;
        let transport = FakeTransport::reachable(1.0)
            .with_meraki(one_device_up())
            .with_meraki_listing_failed(
                MerakiListing::ApplianceVpnStatuses,
                MerakiFetchError::Status(400),
            );
        let results = collect_with(&transport, Uuid::from_u128(3), MerakiTier::Uplink).await;
        assert_eq!(
            device_results(results.clone()).len(),
            1,
            "the readings still go out"
        );
        let reports = reports(&results);
        assert_eq!(reports.len(), 1);
        let report = reports[0].meraki_collect.as_ref().expect("the report");
        assert_eq!(report.failure.as_deref(), Some("upstream"));
        assert_eq!(report.listing.as_deref(), Some("appliance_vpn_statuses"));
    }

    /// 🚨 The defect (ADR-164 決定 18): a refused key published **nothing**, so core heard nothing,
    /// every node of the organization kept its last state, and no alert was possible.
    #[tokio::test]
    async fn a_collect_the_dashboard_refused_says_so_instead_of_saying_nothing() {
        use yagra_transport::MerakiFetchError;
        let org = Uuid::from_u128(0xACE);
        let transport =
            FakeTransport::reachable(1.0).with_meraki_refused(MerakiFetchError::Auth(401));
        let results = collect_with(&transport, org, MerakiTier::Availability).await;

        assert_eq!(
            results.len(),
            1,
            "a failed collect still publishes its report"
        );
        let report = results[0].meraki_collect.as_ref().expect("the report");
        assert_eq!(report.org, org);
        assert_eq!(report.tier, MerakiTier::Availability);
        assert_eq!(report.failure.as_deref(), Some("auth"));
    }

    /// The three causes that never reached the `Err` arm at all: an outage, a 429 storm and a
    /// dropped connection came back as an `Ok` with no observations, and were not even logged.
    #[tokio::test]
    async fn a_collect_that_stopped_with_nothing_in_hand_is_reported_as_failed() {
        use yagra_transport::MerakiFetchError;
        for (why, token) in [
            (MerakiFetchError::Network, "unreachable"),
            (MerakiFetchError::RateLimited, "rate_limited"),
            (MerakiFetchError::Status(503), "upstream"),
        ] {
            let transport = FakeTransport::reachable(1.0).with_meraki_stopped(why);
            let results = collect_with(&transport, Uuid::nil(), MerakiTier::Availability).await;
            let reports = reports(&results);
            assert_eq!(reports.len(), 1, "{why:?}");
            assert_eq!(
                reports[0]
                    .meraki_collect
                    .as_ref()
                    .and_then(|r| r.failure.as_deref()),
                Some(token),
                "{why:?} read as a collect that simply found no devices"
            );
        }
    }

    /// A success is reported too — an alert may only be closed on evidence, and this is it. A
    /// partial answer counts: pages did arrive, so the Dashboard is answering.
    #[tokio::test]
    async fn a_collect_that_was_answered_reports_no_failure_even_when_it_was_cut_short() {
        use yagra_transport::MerakiFetchError;
        let complete = FakeTransport::reachable(1.0).with_meraki(one_device_up());
        let partial = FakeTransport::reachable(1.0)
            .with_meraki(one_device_up())
            .with_meraki_stopped(MerakiFetchError::RateLimited);
        for transport in [complete, partial] {
            let results = collect_with(&transport, Uuid::nil(), MerakiTier::Availability).await;
            assert_eq!(results.len(), 2, "one device, one report");
            let reports = reports(&results);
            assert_eq!(reports.len(), 1);
            assert_eq!(
                reports[0].meraki_collect.as_ref().expect("report").failure,
                None
            );
        }
    }

    /// What keeps the report harmless to a core from before it existed: it is about no node (the
    /// id is the organization's, which the job already used as its sentinel), it is observational
    /// so the liveness state machine never sees it, and it carries nothing to store or judge.
    #[tokio::test]
    async fn the_report_is_inert_to_a_core_that_has_never_heard_of_it() {
        let transport = FakeTransport::reachable(1.0).with_meraki(one_device_up());
        let results =
            collect_with(&transport, Uuid::from_u128(0xACE), MerakiTier::Availability).await;
        let report = reports(&results)[0];
        assert_eq!(
            report.job_id,
            Uuid::from_u128(7),
            "it must release the flight its job took"
        );
        assert_eq!(report.node_id, NodeId::from(Uuid::from_u128(0xACE)));
        assert!(report.observational && !report.judge_samples);
        assert!(report.samples.is_empty() && report.interfaces.is_empty());
        assert!(
            results
                .iter()
                .all(|r| r.node_id != report.node_id || r.meraki_collect.is_some()),
            "a device result was addressed to the organization's id"
        );
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

        let results = device_results(execute_meraki(&job, &transport, 42).await);
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
