// SPDX-License-Identifier: AGPL-3.0-only
//! Holding an SNMP conversation: the v2c/v3 walker, and the scalar GET that rides it.
//!
//! [`SnmpWalker`] is the credential-shaped half of every SNMP check in this module — v2c community
//! or v3 USM parameters, chosen once and then invisible to the walk itself. Keeping it here is why
//! the table, optical, MAU and adjacency walks each exist once rather than twice (ADR-084).
//!
//! The scalar GET is the plainest use of it: named OIDs and configured columns in, [`Sample`]s out,
//! plus `snmp_up` — which is emitted on **every** path including the error one, because an alert
//! rule must not depend on *how* the agent failed.

use super::*;

const SYSDESCR_OID: &str = "1.3.6.1.2.1.1.1.0";

/// sysDescr column base — walking it yields the `.0` instance (the v2c path).
const SYSDESCR_BASE: &str = "1.3.6.1.2.1.1.1";

/// The credential half of an SNMP check that differs between v2c and v3 (community vs USM params).
/// Capturing it here lets everything above it — the scalar GET, the column walk, the interface
/// metadata fold, the identity probe — be written once instead of twice: v2c and v3 differ only in
/// which transport method carries the credential, never in what is done with the rows.
pub(super) enum SnmpWalker {
    V2c(String),
    V3(SnmpV3Params),
}

impl SnmpWalker {
    /// GET scalar OIDs via the appropriate protocol.
    async fn get(
        &self,
        transport: &dyn Transport,
        target: IpAddr,
        oids: &[String],
        timeout: Duration,
    ) -> Result<Vec<yagra_transport::SnmpSample>, TransportError> {
        match self {
            SnmpWalker::V2c(community) => {
                transport.snmp_get(target, community, oids, timeout).await
            }
            SnmpWalker::V3(params) => transport.snmp_v3_get(target, params, oids, timeout).await,
        }
    }

    /// Fetch `sysDescr.0` for the identity probe, so core can fill the node's maker/model.
    /// Best-effort: `None` on any transport error or an empty/missing value. The two protocols
    /// reach it differently — v2c walks the column base (its GET path returns numerics only),
    /// v3 does a scalar string GET.
    async fn fetch_sys_descr(
        &self,
        transport: &dyn Transport,
        target: IpAddr,
        timeout: Duration,
    ) -> Option<String> {
        let value = match self {
            SnmpWalker::V2c(community) => {
                let bases = [SYSDESCR_BASE.to_owned()];
                transport
                    .snmp_walk_strings(target, community, &bases, timeout)
                    .await
                    .ok()?
                    .into_iter()
                    .find(|r| r.oid_base == SYSDESCR_BASE)
                    .map(|r| r.value)
            }
            SnmpWalker::V3(params) => {
                let oids = [SYSDESCR_OID.to_owned()];
                transport
                    .snmp_v3_get_strings(target, params, &oids, timeout)
                    .await
                    .ok()?
                    .into_iter()
                    .find(|r| r.oid == SYSDESCR_OID)
                    .map(|r| r.value)
            }
        };
        value.filter(|v| !v.is_empty())
    }

    /// Walk numeric table columns via the appropriate protocol.
    pub(super) async fn walk(
        &self,
        transport: &dyn Transport,
        target: IpAddr,
        columns: &[String],
        timeout: Duration,
    ) -> Result<Vec<SnmpTableSample>, TransportError> {
        match self {
            SnmpWalker::V2c(community) => {
                transport
                    .snmp_walk(target, community, columns, timeout)
                    .await
            }
            SnmpWalker::V3(params) => {
                transport
                    .snmp_v3_walk(target, params, columns, timeout)
                    .await
            }
        }
    }

    /// Walk table columns keeping raw instance indices and raw octets (the neighbour walk).
    ///
    /// `max_rows` is the caller's budget for the whole walk — see `Transport::snmp_walk_instances`.
    /// Every caller states one; there is deliberately no default, because the tables this walker is
    /// pointed at range from tens of rows (`ipAddrTable`) to hundreds of thousands
    /// (`ipNetToPhysicalTable`) and no single number is right for both.
    pub(super) async fn walk_instances(
        &self,
        transport: &dyn Transport,
        target: IpAddr,
        columns: &[String],
        timeout: Duration,
        max_rows: usize,
    ) -> Result<Vec<yagra_transport::SnmpInstanceRow>, TransportError> {
        match self {
            SnmpWalker::V2c(community) => {
                transport
                    .snmp_walk_instances(target, community, columns, timeout, max_rows)
                    .await
            }
            SnmpWalker::V3(params) => {
                transport
                    .snmp_v3_walk_instances(target, params, columns, timeout, max_rows)
                    .await
            }
        }
    }

    /// Walk string-valued table columns (interface metadata) via the appropriate protocol.
    pub(super) async fn walk_strings(
        &self,
        transport: &dyn Transport,
        target: IpAddr,
        columns: &[String],
        timeout: Duration,
    ) -> Result<Vec<SnmpTableString>, TransportError> {
        match self {
            SnmpWalker::V2c(community) => {
                transport
                    .snmp_walk_strings(target, community, columns, timeout)
                    .await
            }
            SnmpWalker::V3(params) => {
                transport
                    .snmp_v3_walk_strings(target, params, columns, timeout)
                    .await
            }
        }
    }
}

/// Execute an SNMP scalar-GET check (v2c or v3, selected by `walker`): GET the bare OIDs and the
/// explicitly-named scalar columns together, name each sample (a configured column keeps its metric
/// name and kind; a bare OID falls back to the poller's built-in naming), and run the identity
/// probe when core asked for one.
///
/// The v2c and v3 arms of [`execute`] used to carry a copy of this each — ~48 lines apiece that
/// differed only in the credential type and which transport method was called. The table path had
/// already solved exactly that with [`SnmpWalker`]; this brings the scalar path in line, so an SNMP
/// behaviour change (a new outcome rule, a naming tweak) is one edit rather than two that can drift.
///
/// Every result carries [`METRIC_SNMP_UP`] (ADR-075) — `1` when the agent answered with at least
/// one value, `0` when it answered with nothing or the GET failed. This is the only signal that
/// distinguishes "the SNMP agent stopped" from "the device is fine", because the node-wide
/// liveness window is shared by every check on the node: with ICMP polling more often than SNMP,
/// an SNMP-only failure never reaches the consecutive-sample count and commits nothing. Being a
/// sample rather than an outcome, it drives its own threshold check with its own dwell window.
pub(super) async fn execute_scalar_get(
    job: &PollJob,
    transport: &dyn Transport,
    at_unix_ms: i64,
    oids: &[String],
    columns: &[SnmpColumn],
    timeout: Duration,
    walker: &SnmpWalker,
) -> PollResult {
    let col_by_oid: HashMap<&str, &SnmpColumn> =
        columns.iter().map(|c| (c.oid.as_str(), c)).collect();
    let mut all_oids = oids.to_vec();
    all_oids.extend(columns.iter().map(|c| c.oid.clone()));
    match walker.get(transport, job.target, &all_oids, timeout).await {
        Ok(samples) => {
            // No values back ⇒ treat as unreachable (agent down / wrong credential).
            let outcome = if samples.is_empty() {
                CheckOutcome::Unreachable
            } else {
                CheckOutcome::Reachable
            };
            let answered = f64::from(u8::from(!samples.is_empty()));
            let mut mapped: Vec<Sample> = samples
                .into_iter()
                .map(|s| match col_by_oid.get(s.oid.as_str()) {
                    // Configured column → honour its metric name and kind.
                    Some(col) => Sample {
                        metric: col.metric_name.clone(),
                        ifindex: None,
                        value: s.value,
                        kind: col.kind,
                    },
                    // Bare OID → the poller's built-in naming (gauge).
                    None => Sample::gauge(snmp_metric_name(&s.oid), s.value),
                })
                .collect();
            mapped.push(Sample::gauge(METRIC_SNMP_UP, answered));
            let mut r = result(job, at_unix_ms, outcome, mapped);
            if job.probe_identity && outcome == CheckOutcome::Reachable {
                r.sys_descr = walker.fetch_sys_descr(transport, job.target, timeout).await;
            }
            r
        }
        Err(err) => {
            tracing::warn!(job_id = %job.job_id, error = %err, "snmp get failed");
            // `snmp_up = 0` on the error path too: a GET that could not be issued is an agent the
            // operator cannot reach, and emitting nothing here would leave the rule with no
            // sample to evaluate — the alert would depend on *how* the agent failed.
            result(
                job,
                at_unix_ms,
                CheckOutcome::Error,
                vec![Sample::gauge(METRIC_SNMP_UP, 0.0)],
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::worker::testkit::*;
    use std::net::{IpAddr, Ipv4Addr};
    use uuid::Uuid;
    use yagra_bus::SnmpCheck;
    use yagra_common::{NodeId, SnmpV3Auth};
    use yagra_transport::{FakeTransport, SnmpSample};

    fn snmp_job() -> PollJob {
        PollJob::snmp(
            Uuid::nil(),
            NodeId::from(Uuid::nil()),
            IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
            SnmpCheck {
                community: "public".to_owned(),
                oids: vec!["1.3.6.1.2.1.1.3.0".to_owned()],
                columns: Vec::new(),
                timeout_ms: 2000,
            },
            30,
        )
    }

    #[tokio::test]
    async fn snmp_samples_map_to_named_metrics() {
        let t = FakeTransport::reachable(0.0).with_snmp(vec![SnmpSample {
            oid: "1.3.6.1.2.1.1.3.0".to_owned(),
            value: 123.0,
        }]);
        let r = execute(&snmp_job(), &t, 1_000).await;
        assert_eq!(r.outcome, CheckOutcome::Reachable);
        assert!(r
            .samples
            .iter()
            .any(|s| s.metric == "snmp_sys_uptime_ticks" && s.value == 123.0));
    }

    #[tokio::test]
    async fn snmp_no_values_is_unreachable() {
        // FakeTransport with no canned SNMP samples -> empty -> unreachable.
        let t = FakeTransport::reachable(0.0);
        let r = execute(&snmp_job(), &t, 1_000).await;
        assert_eq!(r.outcome, CheckOutcome::Unreachable);
        // The only sample is the agent-health gauge (ADR-075); nothing was read off the device.
        assert_eq!(r.samples.len(), 1);
        assert_eq!(sample(&r, METRIC_SNMP_UP), Some(0.0));
    }

    /// ADR-075. This gauge is the *only* thing that distinguishes "the SNMP agent stopped" from
    /// "the device is fine": the node-wide liveness window is shared by every check on the node,
    /// so with ICMP polling more often than SNMP an SNMP-only failure never reaches the
    /// consecutive-sample count and commits nothing. Both directions are asserted — a gauge that
    /// only ever reads 0 would satisfy a rejection-only test while alerting on every healthy node.
    #[tokio::test]
    async fn every_snmp_scalar_result_says_whether_the_agent_answered() {
        use yagra_transport::SnmpSample;
        let answered = FakeTransport::reachable(0.0).with_snmp(vec![SnmpSample {
            oid: "1.3.6.1.2.1.1.3.0".to_owned(),
            value: 123.0,
        }]);
        let r = execute(&snmp_job(), &answered, 1_000).await;
        assert_eq!(sample(&r, METRIC_SNMP_UP), Some(1.0));

        let silent = FakeTransport::reachable(0.0);
        let r = execute(&snmp_job(), &silent, 1_000).await;
        assert_eq!(sample(&r, METRIC_SNMP_UP), Some(0.0));
    }

    /// v3 goes through the same `execute_scalar_get`, but "the same function" is exactly the claim
    /// that stops being true when someone splits the arms again — so assert it rather than assume.
    #[tokio::test]
    async fn the_v3_scalar_path_reports_the_agent_the_same_way() {
        use yagra_bus::SnmpV3Check;
        use yagra_transport::SnmpSample;
        let job = PollJob::snmp_v3(
            Uuid::nil(),
            NodeId::from(Uuid::nil()),
            IpAddr::V4(Ipv4Addr::new(10, 0, 0, 3)),
            SnmpV3Check {
                auth: SnmpV3Auth {
                    user: "monitor".to_owned(),
                    security_level: "authpriv".to_owned(),
                    auth_protocol: Some("sha256".to_owned()),
                    auth_key: Some("auth-pass".to_owned()),
                    priv_protocol: Some("aes256".to_owned()),
                    priv_key: Some("priv-pass".to_owned()),
                },
                oids: vec!["1.3.6.1.2.1.1.3.0".to_owned()],
                columns: Vec::new(),
                timeout_ms: 2000,
            },
            30,
        );
        let answered = FakeTransport::reachable(0.0).with_snmp(vec![SnmpSample {
            oid: "1.3.6.1.2.1.1.3.0".to_owned(),
            value: 7.0,
        }]);
        assert_eq!(
            sample(&execute(&job, &answered, 1_000).await, METRIC_SNMP_UP),
            Some(1.0)
        );
        assert_eq!(
            sample(
                &execute(&job, &FakeTransport::reachable(0.0), 1_000).await,
                METRIC_SNMP_UP
            ),
            Some(0.0)
        );
    }

    /// The failure mode this closes: with no sample on the error path, whether the operator gets
    /// an alert would depend on *how* the agent failed — a refused connection would be silent
    /// while an empty answer alerted. Both must read 0.
    #[tokio::test]
    async fn a_failed_snmp_get_still_reports_the_agent_as_down() {
        let t = FakeTransport::reachable(0.0).with_snmp_get_error("snmp connect refused");
        let r = execute(&snmp_job(), &t, 1_000).await;
        assert_eq!(r.outcome, CheckOutcome::Error);
        assert_eq!(sample(&r, METRIC_SNMP_UP), Some(0.0));
    }

    #[tokio::test]
    async fn snmp_probe_identity_fetches_sysdescr() {
        use yagra_transport::{SnmpSample, SnmpTableString};
        let mut job = snmp_job();
        job.probe_identity = true;
        let t = FakeTransport::reachable(0.0)
            .with_snmp(vec![SnmpSample {
                oid: "1.3.6.1.2.1.1.3.0".to_owned(),
                value: 1.0,
            }])
            .with_snmp_table_strings(vec![SnmpTableString {
                oid_base: "1.3.6.1.2.1.1.1".to_owned(),
                ifindex: 0,
                value: "Huawei Versatile Routing Platform Software VRP USG6000".to_owned(),
            }]);
        let r = execute(&job, &t, 1_000).await;
        assert_eq!(r.outcome, CheckOutcome::Reachable);
        assert_eq!(
            r.sys_descr.as_deref(),
            Some("Huawei Versatile Routing Platform Software VRP USG6000")
        );
    }

    #[tokio::test]
    async fn snmp_without_probe_identity_has_no_sysdescr() {
        use yagra_transport::SnmpSample;
        // probe_identity defaults false on snmp_job(); even with a sysDescr available it's not sent.
        let t = FakeTransport::reachable(0.0).with_snmp(vec![SnmpSample {
            oid: "1.3.6.1.2.1.1.3.0".to_owned(),
            value: 1.0,
        }]);
        let r = execute(&snmp_job(), &t, 1_000).await;
        assert_eq!(r.outcome, CheckOutcome::Reachable);
        assert!(r.sys_descr.is_none());
    }

    #[tokio::test]
    async fn snmp_scalar_columns_use_configured_metric_name() {
        use yagra_bus::SnmpColumn;
        use yagra_common::MetricKind;
        use yagra_transport::SnmpSample;
        let job = PollJob::snmp(
            Uuid::nil(),
            NodeId::from(Uuid::nil()),
            IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
            SnmpCheck {
                community: "public".to_owned(),
                oids: Vec::new(),
                columns: vec![SnmpColumn {
                    metric_name: "cpu_util".to_owned(),
                    oid: "1.3.6.1.4.1.9.2.1.58.0".to_owned(),
                    kind: MetricKind::Gauge,
                }],
                timeout_ms: 2000,
            },
            30,
        );
        let t = FakeTransport::reachable(0.0).with_snmp(vec![SnmpSample {
            oid: "1.3.6.1.4.1.9.2.1.58.0".to_owned(),
            value: 42.0,
        }]);
        let r = execute(&job, &t, 1_000).await;
        // The configured metric name is used, not the built-in OID-derived fallback.
        assert!(r
            .samples
            .iter()
            .any(|s| s.metric == "cpu_util" && s.value == 42.0));
        assert!(!r.samples.iter().any(|s| s.metric.starts_with("snmp_oid_")));
    }
}
