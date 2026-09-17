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
use yagra_common::{WlanFlavor, METRIC_WLAN_AP_WALK_COMPLETE};

/// Walk `flavor`'s AP table on the job's target and build the controller's result.
pub(super) async fn execute_wlan(
    job: &PollJob,
    transport: &dyn Transport,
    at_unix_ms: i64,
    flavor: WlanFlavor,
    max_aps: u32,
    timeout: Duration,
    walker: &SnmpWalker,
) -> PollResult {
    let columns = crate::wlan::columns(flavor);
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
        Ok(walk) if walk.every_column_answered => (
            CheckOutcome::Reachable,
            Some(crate::wlan::inventory(flavor, &walk.rows, max_aps)),
        ),
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
    let mut r = result(
        job,
        at_unix_ms,
        outcome,
        vec![Sample::gauge(METRIC_WLAN_AP_WALK_COMPLETE, complete)],
    );
    r.wlan = inventory;
    // Never a liveness statement, but its one sample is a reading taken at the node's interval.
    r.observational = true;
    r.judge_samples = true;
    r
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
                timeout_ms: 1000,
            }),
            300,
        )
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
}
