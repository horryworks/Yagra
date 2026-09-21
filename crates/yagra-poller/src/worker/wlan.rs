// SPDX-License-Identifier: AGPL-3.0-only
//! Walking a wireless controller's AP table (ADR-064).
//!
//! One walk, one result for the controller node. The result carries the inventory — every AP the
//! controller manages, as it described them — and one sample, `wlan_ap_walk_complete`. What each AP
//! *becomes* (listed, imported as a node, which controller owns it in an HA pair) is core's to
//! decide, because those decisions need state no poller holds.
//!
//! 🚨 **A walk that did not get every column publishes no inventory** (ADR-064 決定 9b). The columns
//! arrive separately, so a column that timed out between two that answered would read as APs with
//! no state, or no address, or no clients — and a controller whose table stopped half way would read
//! as APs disappearing. The sample says the walk was incomplete; the stored list stays as it was.
//!
//! The result is observational (an AP table says nothing about whether the controller is up) and its
//! sample is judged (`judge_samples`): the walk runs at the node's own interval, which is the
//! condition ADR-158 A10 sets.

use super::*;
use std::collections::BTreeMap;
use yagra_common::{
    WlanBand, WlanFlavor, METRIC_WLAN_AP_WALK_COMPLETE, METRIC_WLAN_CONTROLLER_APS_JOINED,
    METRIC_WLAN_CONTROLLER_AP_CAPACITY, METRIC_WLAN_CONTROLLER_CLIENTS,
    METRIC_WLAN_CONTROLLER_SSID_COUNT, METRIC_WLAN_SSID_WALK_COMPLETE,
};

/// What one wireless-controller job is asked to read.
///
/// A struct rather than five arguments: the SSID flag made the sixth, and the parameters were
/// already one fact about the controller spread across a signature.
#[derive(Debug, Clone, Copy)]
pub(super) struct WlanPlan {
    pub(super) flavor: WlanFlavor,
    pub(super) max_aps: u32,
    pub(super) walk_ssids: bool,
    pub(super) walk_radios: bool,
    pub(super) timeout: Duration,
}

/// Walk `flavor`'s AP table on the job's target and build the controller's result.
pub(super) async fn execute_wlan(
    job: &PollJob,
    transport: &dyn Transport,
    at_unix_ms: i64,
    plan: WlanPlan,
    walker: &SnmpWalker,
) -> PollResult {
    let WlanPlan {
        flavor,
        max_aps,
        walk_ssids,
        walk_radios,
        timeout,
    } = plan;
    let columns = crate::wlan::columns(flavor);
    let counts_totals = crate::wlan::counts_controller_totals(flavor);
    // A Cisco controller's joined count comes out of this walk (ADR-064 増分 F, F8), counted before
    // the inventory is cut to its cap. `None` for a dialect whose own scalars say it, and for a walk
    // that did not finish — the same rule as the inventory.
    let mut joined: Option<usize> = None;
    // And its clients per band out of the radio walk (増分 H, H4), by the same rule: only a
    // complete radio walk may say, because a partial one would publish smaller numbers.
    let mut per_band: Option<BTreeMap<WlanBand, u32>> = None;
    let (outcome, inventory) = match walker
        .walk_instance_columns(
            transport,
            job.target,
            &columns,
            timeout,
            crate::wlan::walk_row_budget(flavor),
        )
        .await
    {
        Ok(walk) if walk.every_column_answered => {
            if counts_totals {
                joined = Some(crate::wlan::joined_count(flavor, &walk.rows));
            }
            let no_aps = walk.rows.is_empty();
            // Whether it finished is not asked of the optional walk: a column it misses costs that
            // column's readings and nothing else.
            let (optional, _) = extra_rows(
                job,
                transport,
                timeout,
                walker,
                no_aps,
                Extra::Optional(flavor),
            )
            .await;
            let radio = if walk_radios {
                let (rows, complete) = extra_rows(
                    job,
                    transport,
                    timeout,
                    walker,
                    no_aps,
                    Extra::Radio(flavor),
                )
                .await;
                if counts_totals && complete {
                    per_band = if no_aps {
                        // No AP, so no radio and no client: that is an answer, and it is zero.
                        Some(WlanBand::ALL.iter().map(|b| (*b, 0)).collect())
                    } else {
                        crate::wlan::clients_per_band(flavor, &rows)
                    };
                }
                rows
            } else {
                Vec::new()
            };
            (
                CheckOutcome::Reachable,
                Some(crate::wlan::inventory(
                    flavor, &walk.rows, &optional, &radio, max_aps,
                )),
            )
        }
        Ok(walk) => {
            tracing::warn!(
                job_id = %job.job_id,
                target = %job.target,
                rows = walk.rows.len(),
                "wireless controller AP walk did not answer every column; no inventory published"
            );
            (CheckOutcome::Reachable, None)
        }
        Err(TransportError::Silent(_)) => {
            tracing::debug!(job_id = %job.job_id, "wireless controller answered nothing");
            (CheckOutcome::Unreachable, None)
        }
        Err(err @ (TransportError::Io(_) | TransportError::Unimplemented(_))) => {
            tracing::warn!(job_id = %job.job_id, error = %err, "wireless controller AP walk failed");
            (CheckOutcome::Error, None)
        }
    };
    // A dialect whose table lists only the APs joined to it also says how long the controller has
    // been up, because an AP missing from the table is taken as down only past the grace after a
    // boot (ADR-064 増分 F, F11) — asked only when there is an inventory to go on. And how many APs
    // its platform supports (増分 H, H5), asked of any controller that answered at all. One GET for
    // both, so an SNMPv3 controller sets up one session rather than two.
    let mut inventory = inventory;
    let wants_uptime = inventory.is_some() && flavor.lists_only_joined_aps();
    let capacity_oids: &[&str] = if outcome == CheckOutcome::Unreachable {
        &[]
    } else {
        crate::wlan::capacity_oids(flavor)
    };
    let scalars =
        controller_scalars(job, transport, walker, timeout, wants_uptime, capacity_oids).await;
    if let Some(inv) = inventory.as_mut() {
        if wants_uptime {
            inv.controller_uptime_secs = scalars.uptime_secs;
        }
    }
    let capacity = crate::wlan::ap_capacity(flavor, &scalars.answered);
    if let Some(inv) = &inventory {
        if let Some(reported) = inv.truncated_at {
            metrics::counter!("yagra_wlan_ap_rows_truncated_total")
                .increment(u64::from(reported).saturating_sub(inv.aps.len() as u64));
            tracing::warn!(
                job_id = %job.job_id,
                reported,
                kept = inv.aps.len(),
                "wireless controller reported more APs than it may publish; the list was cut"
            );
        }
    }
    let complete = f64::from(u8::from(inventory.is_some()));
    let mut samples = vec![Sample::gauge(METRIC_WLAN_AP_WALK_COMPLETE, complete)];
    if let Some(joined) = joined {
        #[allow(clippy::cast_precision_loss)]
        samples.push(Sample::gauge(
            METRIC_WLAN_CONTROLLER_APS_JOINED,
            joined as f64,
        ));
    }
    for (band, clients) in per_band.into_iter().flatten() {
        samples.push(Sample::gauge(
            band.controller_clients_metric(),
            f64::from(clients),
        ));
    }
    if let Some(capacity) = capacity {
        samples.push(Sample::gauge(
            METRIC_WLAN_CONTROLLER_AP_CAPACITY,
            f64::from(capacity),
        ));
    }
    let mut row_names = Vec::new();
    if walk_ssids && outcome != CheckOutcome::Unreachable {
        let (ssid_samples, names, ssid_complete) =
            ssid_readings(job, transport, flavor, timeout, walker).await;
        samples.extend(ssid_samples);
        row_names = names;
        samples.push(Sample::gauge(
            METRIC_WLAN_SSID_WALK_COMPLETE,
            f64::from(u8::from(ssid_complete.is_some())),
        ));
        // Only a complete walk may say how many SSIDs there are, or how many clients they carry
        // together: a partial one would publish a smaller number, which reads as SSIDs (or
        // clients) having gone rather than as a failed read.
        if let Some(complete) = ssid_complete {
            if let Some(count) = complete.count {
                #[allow(clippy::cast_precision_loss)]
                samples.push(Sample::gauge(
                    METRIC_WLAN_CONTROLLER_SSID_COUNT,
                    count as f64,
                ));
            }
            if counts_totals {
                if let Some(clients) = complete.clients {
                    samples.push(Sample::gauge(
                        METRIC_WLAN_CONTROLLER_CLIENTS,
                        f64::from(clients),
                    ));
                }
            }
        }
    }
    let mut r = result(job, at_unix_ms, outcome, samples);
    r.row_names = row_names;
    r.wlan = inventory;
    // Never a liveness statement, but its one sample is a reading taken at the node's interval.
    r.observational = true;
    r.judge_samples = true;
    r
}

/// What the one scalar GET after the walks answered.
struct ControllerScalars {
    /// `sysUpTime.0` in whole seconds, or `None` when it was not asked or not answered — which core
    /// reads as "not up long enough", never as "up for ever" (ADR-064 増分 F, F11).
    uptime_secs: Option<u64>,
    /// Every sample the GET returned, for the readers that pick their own OIDs out of it.
    answered: Vec<yagra_transport::SnmpSample>,
}

/// One GET for the controller's scalars: `sysUpTime.0` when `uptime` is asked for, plus `extra`.
/// No request at all when there is nothing to ask — a Huawei AC, or a Cisco controller that did
/// not answer the walk.
///
/// Every value is picked out by its OID: an agent may answer in any order, and one that does not
/// implement an object leaves it out, which costs that reading and nothing else.
async fn controller_scalars(
    job: &PollJob,
    transport: &dyn Transport,
    walker: &SnmpWalker,
    timeout: Duration,
    uptime: bool,
    extra: &[&str],
) -> ControllerScalars {
    const SYS_UPTIME: &str = "1.3.6.1.2.1.1.3.0";
    let asked: Vec<String> = uptime
        .then_some(SYS_UPTIME)
        .into_iter()
        .chain(extra.iter().copied())
        .map(str::to_owned)
        .collect();
    if asked.is_empty() {
        return ControllerScalars {
            uptime_secs: None,
            answered: Vec::new(),
        };
    }
    let answered = match walker.get(transport, job.target, &asked, timeout).await {
        Ok(samples) => samples,
        Err(err) => {
            tracing::debug!(job_id = %job.job_id, error = %err, "wireless controller scalars unread");
            Vec::new()
        }
    };
    let uptime_secs = uptime
        .then(|| {
            answered
                .iter()
                .find(|s| s.oid.trim_start_matches('.') == SYS_UPTIME)
                // TimeTicks, hundredths of a second, widened to `f64` by the transport.
                .filter(|s| s.value.is_finite() && s.value >= 0.0)
                .map(|s| {
                    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                    let secs = (s.value / 100.0) as u64;
                    secs
                })
        })
        .flatten();
    ControllerScalars {
        uptime_secs,
        answered,
    }
}

/// Which secondary walk of a controller this is.
#[derive(Debug, Clone, Copy)]
enum Extra {
    /// The AP table columns whose absence must cost a reading and nothing more.
    Optional(WlanFlavor),
    /// The radio table (ADR-064 増分 C).
    Radio(WlanFlavor),
}

impl Extra {
    fn columns(self) -> Vec<String> {
        match self {
            Self::Optional(f) => crate::wlan::optional_columns(f),
            Self::Radio(f) => crate::wlan::radio_columns(f),
        }
    }

    fn row_budget(self) -> usize {
        match self {
            Self::Optional(f) => crate::wlan::optional_walk_row_budget(f),
            Self::Radio(f) => crate::wlan::radio_walk_row_budget(f),
        }
    }

    fn what(self) -> &'static str {
        match self {
            Self::Optional(_) => "optional AP columns",
            Self::Radio(_) => "radio table",
        }
    }
}

/// A secondary walk's rows and whether it heard every column out, or an empty list if it did not
/// work out (ADR-064 増分 C/E). The flag matters only to a reader that **counts** the rows — the
/// per-band client totals (増分 H) — never to the readings attached per AP.
///
/// 🚨 **Every failure here returns empty rather than propagating.** That asymmetry is the whole
/// point of walking these apart from the AP table: they carry readings, and a controller that
/// does not implement one must cost that reading and nothing else. Folded into the required
/// walk they would hand `every_column_answered` a veto over the AP list — measured on the PoC's
/// AC6508, six columns of `hwWlanApEntry` are asked for and never answered, so "a Huawei model
/// that skips one" is the normal case, not the edge.
///
/// Skipped entirely when the required walk found no APs: there is nothing to attach readings to,
/// and the device's time is better left to the next job. That is reported as complete — a table of
/// no APs has no radios, and that is an answer.
async fn extra_rows(
    job: &PollJob,
    transport: &dyn Transport,
    timeout: Duration,
    walker: &SnmpWalker,
    no_aps: bool,
    extra: Extra,
) -> (Vec<yagra_transport::SnmpInstanceRow>, bool) {
    if no_aps {
        return (Vec::new(), true);
    }
    match walker
        .walk_instance_columns(
            transport,
            job.target,
            &extra.columns(),
            timeout,
            extra.row_budget(),
        )
        .await
    {
        // The rows are good whether or not every column answered: a column this controller does not
        // implement is the expected answer, and the rows of the ones it does implement stand.
        Ok(walk) => (walk.rows, walk.every_column_answered),
        Err(err) => {
            tracing::debug!(
                job_id = %job.job_id,
                target = %job.target,
                error = %err,
                what = extra.what(),
                "wireless controller secondary walk unread; the AP list is unaffected"
            );
            (Vec::new(), false)
        }
    }
}
/// What only a **complete** SSID walk may say: how many SSIDs there are — unless a WLAN answered
/// with no name, when that is not known (ADR-064 増分 H, H3) — and, when any row answered a client
/// count, how many clients the table carries together.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SsidTotals {
    count: Option<usize>,
    clients: Option<u32>,
}

/// The SSID table's samples and row names, and the totals a **complete** walk found.
///
/// Unlike the AP table there is no all-or-nothing rule here (決定 9b): each SSID is an independent
/// series rather than a member of a list that replaces a stored one, so a column that did not
/// answer costs that column and the rest are published. What a partial walk may not do is *count*
/// — hence the `Option`.
async fn ssid_readings(
    job: &PollJob,
    transport: &dyn Transport,
    flavor: WlanFlavor,
    timeout: Duration,
    walker: &SnmpWalker,
) -> (Vec<Sample>, Vec<yagra_bus::RowName>, Option<SsidTotals>) {
    let columns = crate::wlan::ssid_columns(flavor);
    match walker
        .walk_instance_columns(
            transport,
            job.target,
            &columns,
            timeout,
            crate::wlan::ssid_walk_row_budget(flavor),
        )
        .await
    {
        Ok(walk) => {
            let table = crate::wlan::ssid_table(flavor, &walk.rows);
            let (samples, names) = crate::wlan::ssid_samples(&table.readings);
            let totals = SsidTotals {
                // A WLAN we saw and could not name would make a smaller count, which reads as an
                // SSID having been removed — the shape the 9800's "0 SSIDs" took.
                count: (table.unnamed_rows == 0).then_some(table.readings.len()),
                clients: table.clients_total,
            };
            (samples, names, walk.every_column_answered.then_some(totals))
        }
        Err(err) => {
            tracing::warn!(
                job_id = %job.job_id,
                target = %job.target,
                error = %err,
                "wireless controller SSID walk failed"
            );
            (Vec::new(), Vec::new(), None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use yagra_transport::{FakeTransport, SnmpInstanceRow, SnmpValue};

    const ROOT: &str = "1.3.6.1.4.1.2011.6.139.13.3.3.1";

    fn job() -> PollJob {
        PollJob::for_spec(
            uuid::Uuid::nil(),
            NodeId::from(uuid::Uuid::nil()),
            "192.0.2.1".parse().unwrap(),
            CheckSpec::SnmpWlanAp(yagra_bus::SnmpWlanApCheck {
                community: "public".into(),
                flavor: WlanFlavor::Huawei,
                max_aps: 1024,
                walk_ssids: false,
                walk_radios: false,
                timeout_ms: 1000,
            }),
            300,
        )
    }

    const SSID_ROOT: &str = "1.3.6.1.4.1.2011.6.139.17.1.2.1";

    /// One SSID column cell, keyed by the SSID name the way the controller indexes it.
    fn ssid_cell(column: u32, name: &str, value: i64) -> SnmpInstanceRow {
        let mut instance = vec![u32::try_from(name.len()).unwrap()];
        instance.extend(name.bytes().map(u32::from));
        SnmpInstanceRow {
            oid_base: format!("{SSID_ROOT}.{column}"),
            instance,
            value: SnmpValue::Int(value),
        }
    }

    fn ssid_job(walk_ssids: bool) -> PollJob {
        PollJob::for_spec(
            uuid::Uuid::nil(),
            NodeId::from(uuid::Uuid::nil()),
            "192.0.2.1".parse().unwrap(),
            CheckSpec::SnmpWlanAp(yagra_bus::SnmpWlanApCheck {
                community: "public".into(),
                flavor: WlanFlavor::Huawei,
                max_aps: 1024,
                walk_ssids,
                walk_radios: false,
                timeout_ms: 1000,
            }),
            300,
        )
    }

    fn with_ssids() -> FakeTransport {
        FakeTransport {
            snmp_instances: vec![
                instance(6, [1, 2, 3, 4, 5, 6], SnmpValue::Int(8)),
                instance(4, [1, 2, 3, 4, 5, 6], SnmpValue::Bytes(b"ap-1".to_vec())),
                ssid_cell(2, "guest", 2),
                ssid_cell(3, "guest", 11),
                ssid_cell(4, "guest", 30),
            ],
            ..FakeTransport::reachable(1.0)
        }
    }

    fn value(r: &PollResult, metric: &str) -> Option<f64> {
        r.samples
            .iter()
            .find(|s| s.metric == metric)
            .map(|s| s.value)
    }

    /// The SSID rows ride the controller own result as samples plus row names — no new message
    /// type, no new store (ADR-064 R17).
    #[tokio::test]
    async fn an_asked_for_ssid_walk_publishes_a_row_and_its_name() {
        let r = execute(&ssid_job(true), &with_ssids(), 1).await;
        assert_eq!(value(&r, "wlan_ssid_walk_complete"), Some(1.0));
        assert_eq!(value(&r, "wlan_controller_ssid_count"), Some(1.0));
        let row = yagra_common::ssid_row_key("guest");
        let clients = r
            .samples
            .iter()
            .find(|s| s.metric == "wlan_ssid_clients")
            .expect("the SSID client count is published");
        assert_eq!(clients.ifindex, Some(yagra_common::IfIndex(row)));
        assert_eq!(clients.value, 13.0, "2 on 2.4 GHz plus 11 on 5 GHz");
        assert!(
            r.row_names
                .iter()
                .any(|n| n.row == row && n.name == "guest" && n.metric == "wlan_ssid_clients"),
            "{:?}",
            r.row_names
        );
    }

    /// The flag is the whole switch: a controller whose collection set has no SSID item is never
    /// asked, so attaching the template is what turns the second walk on.
    #[tokio::test]
    async fn an_unasked_ssid_walk_is_not_made_at_all() {
        let t = with_ssids();
        let r = execute(&ssid_job(false), &t, 1).await;
        assert_eq!(value(&r, "wlan_ssid_walk_complete"), None);
        assert!(r.row_names.is_empty());
        for asked in t.asked() {
            assert!(
                !asked.iter().any(|c| c.starts_with(SSID_ROOT)),
                "the SSID table was walked without being asked for: {asked:?}"
            );
        }
    }

    /// 🚨 Unlike the AP table there is no all-or-nothing rule: a column that did not answer costs
    /// that column, and the SSIDs still publish. What a partial walk may **not** do is count them
    /// — a smaller number reads as SSIDs having been removed rather than as a failed read.
    #[tokio::test]
    async fn an_incomplete_ssid_walk_still_publishes_its_rows_but_will_not_count_them() {
        let t = with_ssids().with_unanswered_instance_column(format!("{SSID_ROOT}.17"));
        let r = execute(&ssid_job(true), &t, 1).await;
        assert_eq!(value(&r, "wlan_ssid_walk_complete"), Some(0.0));
        assert_eq!(value(&r, "wlan_controller_ssid_count"), None);
        assert_eq!(value(&r, "wlan_ssid_clients"), Some(13.0));
        assert!(!r.row_names.is_empty());
        assert_eq!(
            value(&r, "wlan_ap_walk_complete"),
            Some(1.0),
            "the AP list is not affected by the SSID table"
        );
        assert!(r.wlan.is_some());
    }
    fn instance(column: u32, mac: [u32; 6], value: SnmpValue) -> SnmpInstanceRow {
        SnmpInstanceRow {
            oid_base: format!("{ROOT}.{column}"),
            instance: mac.to_vec(),
            value,
        }
    }

    fn sample(r: &PollResult) -> f64 {
        r.samples
            .iter()
            .find(|s| s.metric == METRIC_WLAN_AP_WALK_COMPLETE)
            .expect("the walk-complete sample is always published")
            .value
    }

    #[tokio::test]
    async fn a_complete_walk_publishes_the_inventory_and_says_so() {
        let t = FakeTransport {
            snmp_instances: vec![
                instance(6, [1, 2, 3, 4, 5, 6], SnmpValue::Int(8)),
                instance(4, [1, 2, 3, 4, 5, 6], SnmpValue::Bytes(b"ap-1".to_vec())),
            ],
            ..FakeTransport::reachable(1.0)
        };
        let r = execute(&job(), &t, 1).await;
        assert!(r.observational && r.judge_samples);
        assert_eq!(r.outcome, CheckOutcome::Reachable);
        assert_eq!(sample(&r), 1.0);
        let inv = r.wlan.expect("an inventory");
        assert_eq!(inv.aps.len(), 1);
        assert_eq!(inv.aps[0].name.as_deref(), Some("ap-1"));
    }

    /// A controller that manages no APs is an answer, not a failure: `Some(empty)`.
    #[tokio::test]
    async fn a_controller_with_no_aps_publishes_an_empty_inventory() {
        let t = FakeTransport::reachable(1.0);
        let r = execute(&job(), &t, 1).await;
        assert_eq!(sample(&r), 1.0);
        assert_eq!(r.wlan.map(|i| i.aps.len()), Some(0));
    }

    /// 🚨 The regression this file's second walk exists for (ADR-064 増分 E).
    ///
    /// A controller whose CPU-temperature column fails still gets its AP list — the reading is
    /// simply absent. Move `.83` back into [`crate::wlan::columns`] and this goes red, because the
    /// failure is then injected into the walk the inventory depends on.
    ///
    /// 🚨 **The injection has to be per column, and the first version of this test was wrong about
    /// that.** `with_unanswered_instance_columns` fails every walk at the device, so with the
    /// columns moved back it failed both walks — which the old assertions could not distinguish
    /// from nothing failing, and the test stayed green over exactly the defect it names.
    #[tokio::test]
    async fn a_failing_optional_column_costs_its_reading_and_not_the_ap_list() {
        let t = FakeTransport {
            snmp_instances: vec![
                instance(6, [1, 2, 3, 4, 5, 6], SnmpValue::Int(8)),
                instance(4, [1, 2, 3, 4, 5, 6], SnmpValue::Bytes(b"ap-1".to_vec())),
                instance(83, [1, 2, 3, 4, 5, 6], SnmpValue::Int(66)),
            ],
            ..FakeTransport::reachable(1.0)
        }
        .with_unanswered_instance_column(format!("{ROOT}.83"));
        let r = execute(&job(), &t, 1).await;
        assert_eq!(sample(&r), 1.0, "the required walk was complete");
        let inv = r
            .wlan
            .expect("the AP list survives a column the inventory does not depend on");
        assert_eq!(inv.aps.len(), 1);
        assert_eq!(
            t.asked().len(),
            2,
            "two walks: the one the inventory depends on, and the one it does not"
        );
    }

    /// The other half: a controller that simply does not implement the optional columns answers an
    /// empty walk, and the readings are absent rather than zero.
    #[tokio::test]
    async fn an_unimplemented_optional_column_leaves_its_reading_absent() {
        let t = FakeTransport {
            snmp_instances: vec![
                instance(6, [1, 2, 3, 4, 5, 6], SnmpValue::Int(8)),
                instance(4, [1, 2, 3, 4, 5, 6], SnmpValue::Bytes(b"ap-1".to_vec())),
            ],
            ..FakeTransport::reachable(1.0)
        };
        let inv = execute(&job(), &t, 1).await.wlan.expect("an inventory");
        assert_eq!(inv.aps[0].cpu_temp_c, None);
        assert_eq!(inv.aps[0].power_state, None);
    }

    /// The two column sets are disjoint, and the optional one holds exactly the columns whose
    /// absence must not cost the AP list. Structural, and deliberately so: the test above cannot
    /// tell "walked separately" from "walked together and happened to answer".
    #[test]
    fn the_optional_columns_are_not_the_ones_the_inventory_depends_on() {
        let required = crate::wlan::columns(WlanFlavor::Huawei);
        let optional = crate::wlan::optional_columns(WlanFlavor::Huawei);
        assert!(!optional.is_empty());
        for oid in &optional {
            assert!(
                !required.contains(oid),
                "{oid} is in the required walk, so a device that skips it loses its whole AP list"
            );
        }
        assert!(optional.iter().any(|o| o.ends_with(".83")), "{optional:?}");
        assert!(optional.iter().any(|o| o.ends_with(".80")), "{optional:?}");
    }

    /// 決定 9b, the side that matters: a walk that missed a column publishes nothing, so the stored
    /// list is not replaced by a half-read one.
    #[tokio::test]
    async fn an_incomplete_walk_publishes_no_inventory() {
        let t = FakeTransport {
            snmp_instances: vec![instance(6, [1, 2, 3, 4, 5, 6], SnmpValue::Int(8))],
            ..FakeTransport::reachable(1.0)
        }
        .with_unanswered_instance_columns();
        let r = execute(&job(), &t, 1).await;
        assert_eq!(sample(&r), 0.0);
        assert!(r.wlan.is_none());
        assert!(
            r.observational,
            "an incomplete walk still says nothing about liveness"
        );
    }

    #[tokio::test]
    async fn a_silent_controller_publishes_no_inventory() {
        let t = FakeTransport::reachable(1.0).with_silent_instance_walks();
        let r = execute(&job(), &t, 1).await;
        assert_eq!(sample(&r), 0.0);
        assert!(r.wlan.is_none());
        assert_eq!(r.outcome, CheckOutcome::Unreachable);
        assert!(r.observational);
    }

    #[tokio::test]
    async fn the_walk_asks_for_the_dialect_columns_with_a_row_budget() {
        let t = FakeTransport::reachable(1.0);
        let _ = execute(&job(), &t, 1).await;
        let asked: Vec<String> = t.asked().concat();
        assert!(asked.contains(&format!("{ROOT}.6")), "{asked:?}");
        assert!(asked.contains(&format!("{ROOT}.44")), "{asked:?}");
    }

    // ─── Cisco (ADR-064 増分 F) ─────────────────────────────────────────────────────────

    const CISCO_AP: &str = "1.3.6.1.4.1.14179.2.2.1.1";
    const CISCO_RADIO: &str = "1.3.6.1.4.1.14179.2.2.2.1";
    const CISCO_SSID: &str = "1.3.6.1.4.1.14179.2.1.1.1";

    fn cisco_job() -> PollJob {
        cisco_job_with(true)
    }

    fn cisco_job_with(walk_radios: bool) -> PollJob {
        PollJob::for_spec(
            uuid::Uuid::nil(),
            NodeId::from(uuid::Uuid::nil()),
            "192.0.2.1".parse().unwrap(),
            CheckSpec::SnmpWlanAp(yagra_bus::SnmpWlanApCheck {
                community: "public".into(),
                flavor: WlanFlavor::CiscoAirespace,
                max_aps: 1024,
                walk_ssids: true,
                walk_radios,
                timeout_ms: 1000,
            }),
            300,
        )
    }

    /// `cLWlanSsid` (CISCO-LWAPP-WLAN-MIB), the SSID name a 9800 answers.
    const CLW_SSID: &str = "1.3.6.1.4.1.9.9.512.1.1.1.1.4";
    /// AireOS's and the 9800's platform AP capacity.
    const AIREOS_CAPACITY: &str = "1.3.6.1.4.1.14179.1.1.1.18.0";
    const C9800_CAPACITY: &str = "1.3.6.1.4.1.9.9.513.1.3.28.0";

    fn scalar(oid: &str, value: f64) -> yagra_transport::SnmpSample {
        yagra_transport::SnmpSample {
            oid: oid.to_owned(),
            value,
        }
    }

    fn cell(table: &str, column: u32, instance: &[u32], value: SnmpValue) -> SnmpInstanceRow {
        SnmpInstanceRow {
            oid_base: format!("{table}.{column}"),
            instance: instance.to_vec(),
            value,
        }
    }

    /// A controller shaped like the lab's 9800 recording: the required columns and nothing of the
    /// optional walk — no `.30`, no cLApTable. One AP serving with two radios, one downloading, and
    /// two SSIDs. Names and addresses are made up.
    ///
    /// 🚨 **No SSID column `.2`, and that is the recording's shape** (ADR-064 増分 H). The SSID names
    /// come from `cLWlanSsid` only. This fixture used to answer `.2`, which is why a 9800 publishing
    /// no SSIDs and no client count passed every test here.
    fn cisco_controller() -> FakeTransport {
        let serving = [0, 60, 16, 104, 153, 160];
        let upgrading = [0, 60, 16, 104, 153, 176];
        let mut rows = Vec::new();
        for (mac, state) in [(serving, 1), (upgrading, 3)] {
            rows.push(cell(CISCO_AP, 6, &mac, SnmpValue::Int(state)));
            for column in [3, 8, 16, 17] {
                rows.push(cell(
                    CISCO_AP,
                    column,
                    &mac,
                    SnmpValue::Bytes(b"x".to_vec()),
                ));
            }
            rows.push(cell(
                CISCO_AP,
                19,
                &mac,
                SnmpValue::Bytes(vec![192, 0, 2, 9]),
            ));
        }
        for (slot, kind, channel, clients) in [(0, 1, 11, 2), (1, 2, 36, 5)] {
            let mut index = serving.to_vec();
            index.push(slot);
            rows.push(cell(CISCO_RADIO, 2, &index, SnmpValue::Int(kind)));
            rows.push(cell(CISCO_RADIO, 4, &index, SnmpValue::Int(channel)));
            rows.push(cell(CISCO_RADIO, 15, &index, SnmpValue::Int(clients)));
        }
        for (wlan, name, clients) in [(1, "corp", 6), (2, "guest", 1)] {
            rows.push(SnmpInstanceRow {
                oid_base: CLW_SSID.to_owned(),
                instance: vec![wlan],
                value: SnmpValue::Bytes(name.as_bytes().to_vec()),
            });
            rows.push(cell(CISCO_SSID, 38, &[wlan], SnmpValue::Int(clients)));
        }
        FakeTransport {
            snmp_instances: rows,
            ..FakeTransport::reachable(1.0)
        }
    }

    /// Every GET the job made, as the OIDs each asked for.
    fn gets(t: &FakeTransport) -> Vec<Vec<String>> {
        t.asked()
            .into_iter()
            .filter(|asked| asked.iter().any(|oid| oid.ends_with(".0")))
            .collect()
    }

    /// The whole Cisco path on the poller: an inventory with no optional column answered at all —
    /// the 9800's shape — and the controller's two totals counted out of the walks (F3, F8).
    #[tokio::test]
    async fn a_cisco_controller_publishes_its_aps_and_counts_its_own_totals() {
        let t = cisco_controller().with_snmp(vec![scalar(C9800_CAPACITY, 250.0)]);
        let r = execute(&cisco_job(), &t, 1).await;
        let inv = r
            .wlan
            .as_ref()
            .expect("an inventory, with no optional column answered");
        assert_eq!(inv.flavor, WlanFlavor::CiscoAirespace);
        assert_eq!(inv.aps.len(), 2);
        assert_eq!(inv.aps[0].clients, Some(7), "its two radios' clients");
        assert_eq!(value(&r, "wlan_ap_walk_complete"), Some(1.0));
        assert_eq!(
            value(&r, "wlan_controller_aps_joined"),
            Some(1.0),
            "the downloading AP is not joined"
        );
        assert_eq!(
            value(&r, "wlan_controller_ssid_count"),
            Some(2.0),
            "named from cLWlanSsid, the SSID table's own name column unanswered"
        );
        assert_eq!(
            value(&r, "wlan_controller_clients"),
            Some(7.0),
            "6 on corp + 1 on guest"
        );
        assert!(r
            .row_names
            .iter()
            .any(|n| n.name == "corp" && n.metric == "wlan_ssid_clients"));
        // The radios' clients by band (H4), every band present.
        assert_eq!(value(&r, "wlan_controller_clients_2g4"), Some(2.0));
        assert_eq!(value(&r, "wlan_controller_clients_5g"), Some(5.0));
        assert_eq!(value(&r, "wlan_controller_clients_6g"), Some(0.0));
        // The platform's AP capacity from the 9800's object (H5), asked in the one GET that also
        // asks the uptime and AireOS's object.
        assert_eq!(value(&r, "wlan_controller_ap_capacity"), Some(250.0));
        let gets = gets(&t);
        assert_eq!(gets.len(), 1, "{gets:?}");
        for oid in ["1.3.6.1.2.1.1.3.0", AIREOS_CAPACITY, C9800_CAPACITY] {
            assert!(
                gets[0].iter().any(|o| o == oid),
                "{oid} not asked: {gets:?}"
            );
        }
        assert!(r.observational && r.judge_samples);
    }

    /// AireOS keeps its capacity in AIRESPACE-SWITCHING-MIB (150 on the PoC's AIR-CT3504), and an
    /// AireOS answers the SSID table's own name column, which then names the SSID.
    #[tokio::test]
    async fn an_aireos_controller_reads_its_capacity_from_its_own_object() {
        let mut t = cisco_controller().with_snmp(vec![scalar(AIREOS_CAPACITY, 150.0)]);
        t.snmp_instances.push(cell(
            CISCO_SSID,
            2,
            &[1],
            SnmpValue::Bytes(b"corp".to_vec()),
        ));
        let r = execute(&cisco_job(), &t, 1).await;
        assert_eq!(value(&r, "wlan_controller_ap_capacity"), Some(150.0));
        assert_eq!(
            value(&r, "wlan_controller_ssid_count"),
            Some(2.0),
            "never counted twice"
        );
    }

    /// 🚨 A WLAN that answered neither name is nobody's SSID, and while there is one the SSID count
    /// is not known — publishing 2 of 3 would read as an SSID having been removed, which is the 9800
    /// bug in its general form (H3). Its clients are still the controller's (H2).
    #[tokio::test]
    async fn an_unnamed_wlan_withholds_the_ssid_count_but_not_the_client_total() {
        let mut t = cisco_controller();
        t.snmp_instances
            .push(cell(CISCO_SSID, 38, &[3], SnmpValue::Int(4)));
        let r = execute(&cisco_job(), &t, 1).await;
        assert_eq!(value(&r, "wlan_ssid_walk_complete"), Some(1.0));
        assert_eq!(value(&r, "wlan_controller_ssid_count"), None);
        assert_eq!(
            value(&r, "wlan_controller_clients"),
            Some(11.0),
            "6 + 1 + 4"
        );
    }

    /// Per-band totals are counted, so only a finished radio walk may give them — and a controller
    /// whose set does not ask for radios is never walked for them at all.
    #[tokio::test]
    async fn a_radio_walk_that_did_not_finish_publishes_no_per_band_total() {
        let t = cisco_controller().with_unanswered_instance_column(format!("{CISCO_RADIO}.15"));
        let r = execute(&cisco_job(), &t, 1).await;
        assert!(
            r.wlan.is_some(),
            "the AP list does not depend on the radios"
        );
        for band in ["2g4", "5g", "6g"] {
            assert_eq!(value(&r, &format!("wlan_controller_clients_{band}")), None);
        }
        let r = execute(&cisco_job_with(false), &cisco_controller(), 1).await;
        assert_eq!(value(&r, "wlan_controller_clients_2g4"), None);
    }

    /// A controller that did not answer the walk is not asked for anything else, and a Huawei AC is
    /// never asked for a capacity its own scalars' template says differently.
    #[tokio::test]
    async fn a_silent_cisco_and_any_huawei_make_no_capacity_request() {
        let t = cisco_controller().with_silent_instance_walks();
        let r = execute(&cisco_job(), &t, 1).await;
        assert_eq!(r.outcome, CheckOutcome::Unreachable);
        assert!(gets(&t).is_empty(), "{:?}", gets(&t));
        assert_eq!(value(&r, "wlan_controller_ap_capacity"), None);

        let t = with_ssids();
        let _ = execute(&ssid_job(true), &t, 1).await;
        assert!(gets(&t).is_empty(), "{:?}", gets(&t));
    }

    /// A Cisco inventory says how long its controller has been up — what core waits on before an AP
    /// missing from the table counts (F11) — and a controller that does not answer `sysUpTime` sends
    /// none, which core reads as "not long enough". A Huawei is never asked.
    #[tokio::test]
    async fn a_cisco_inventory_carries_its_controllers_uptime() {
        use yagra_transport::SnmpSample;
        // 162 days, the PoC controller's own uptime when it was walked.
        let t = cisco_controller().with_snmp(vec![SnmpSample {
            oid: "1.3.6.1.2.1.1.3.0".into(),
            value: 1_406_883_500.0,
        }]);
        let r = execute(&cisco_job(), &t, 1).await;
        assert_eq!(r.wlan.unwrap().controller_uptime_secs, Some(14_068_835));

        let silent = FakeTransport {
            snmp: Vec::new(),
            ..cisco_controller()
        };
        let r = execute(&cisco_job(), &silent, 1).await;
        assert_eq!(r.wlan.unwrap().controller_uptime_secs, None);

        let t = with_ssids().with_snmp(vec![SnmpSample {
            oid: "1.3.6.1.2.1.1.3.0".into(),
            value: 1_406_883_500.0,
        }]);
        let r = execute(&ssid_job(false), &t, 1).await;
        assert_eq!(
            r.wlan.unwrap().controller_uptime_secs,
            None,
            "a Huawei is not asked"
        );
    }

    /// 🚨 A walk that did not finish counts nothing: no inventory, and no joined count — a smaller
    /// number would read as APs having left (決定 9b's rule, applied to the total). And a Huawei
    /// never publishes the two totals from its walk: its scalars do, and two sources would draw two
    /// lines under one name.
    #[tokio::test]
    async fn an_unfinished_walk_counts_nothing_and_a_huawei_counts_from_its_scalars() {
        let t = cisco_controller().with_unanswered_instance_column(format!("{CISCO_AP}.17"));
        let r = execute(&cisco_job(), &t, 1).await;
        assert!(r.wlan.is_none());
        assert_eq!(value(&r, "wlan_ap_walk_complete"), Some(0.0));
        assert_eq!(value(&r, "wlan_controller_aps_joined"), None);

        let r = execute(&ssid_job(true), &with_ssids(), 1).await;
        assert!(r.wlan.is_some(), "the Huawei walk itself worked");
        assert_eq!(value(&r, "wlan_controller_aps_joined"), None);
        assert_eq!(value(&r, "wlan_controller_clients"), None);
    }
}
