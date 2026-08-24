// SPDX-License-Identifier: AGPL-3.0-only
//! Cisco Meraki org-scoped collection: **one job in, many results out**.
//!
//! Unlike every other check here, the transport pages an org's Dashboard endpoints and answers with
//! per-device observations, so this emits one ordinary [`PollResult`] per device (attributed
//! through the inlined serial→node_id map) and the whole consume/write/alert spine works unchanged.
//! That shape is why it is dispatched from [`super::run_stream`] rather than from
//! [`super::execute`], which is one job → one result by construction.

use super::*;

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
        let samples = obs
            .samples
            .into_iter()
            .map(|s| match s.ifindex {
                Some(idx) => Sample::interface(s.metric, IfIndex(idx), s.value, MetricKind::Gauge),
                None => Sample::gauge(s.metric, s.value),
            })
            .collect();
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
            outcome: CheckOutcome::Reachable,
            samples,
            interfaces,
            sys_descr: None,
            dns_chain: None,
            neighbors: None,
            l3: None,
            arp: None,
            routing: None,
            observational: false,
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
        assert_eq!(r.outcome, CheckOutcome::Reachable);
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
