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
use yagra_discovery::os_version;

/// Identity probes run, by what they found: `version`, `no_version` (the device answered but the
/// table does not cover it or it reports none), `unread` (the device answered but the patch table
/// the version's row walks did not answer to its end, so the version went out without its patch, in
/// a field core writes only where it cannot strip one — ADR-138 Increments 3 and 4) or `no_answer`
/// (not even `sysDescr` came back).
/// The ratio of the first two is the table's real coverage of a fleet (ADR-138).
pub(super) const IDENTITY_PROBES_METRIC: &str = "yagra_poll_identity_probes_total";

/// The most rows the identity probe takes from the table columns it walks whole. A patch table
/// holds a handful per slot; the cap is what stops a device that answers with thousands of rows
/// from turning an hourly probe into a table dump.
const IDENTITY_COLUMN_ROWS: usize = 256;

/// The shortest per-round-trip wait the identity probe gives the table columns it walks whole
/// (ADR-138 Increment 4).
///
/// **Why not the job's own 2 s.** Measured on a Huawei S5731 (VRP 5.170) from the PoC box: a
/// GETBULK on its empty `hwPatchTable` took **2.35 s** to answer with 20 repetitions and 1.12 s with
/// one, while `sysDescr` took 0.02 s. At 2 s every hourly walk timed out, and all 43 such switches
/// there never showed a version. Five seconds is twice the measured answer.
///
/// ⚠️ The cost is bounded and hourly: the walk runs only on a device that already answered
/// `sysDescr`, and a device that ignores the table holds its single-flight slot for at most two
/// columns of this, once an hour.
const IDENTITY_COLUMN_TIMEOUT: Duration = Duration::from_secs(5);

/// The wait for the identity probe's whole-column walks: [`IDENTITY_COLUMN_TIMEOUT`], or the job's
/// own timeout when that is longer — a job already more patient is never made less so.
fn identity_column_timeout(job_timeout: Duration) -> Duration {
    job_timeout.max(IDENTITY_COLUMN_TIMEOUT)
}

/// What one identity probe learned (ADR-138).
pub(super) struct IdentityProbe {
    pub(super) sys_descr: Option<String>,
    pub(super) os_version: Option<String>,
    /// The version without its running-patch suffix, set only when `unread` is — the table did not
    /// answer, so [`os_version`](Self::os_version) is withheld (ADR-138 Increment 4).
    pub(super) os_version_without_patch: Option<String>,
    /// Always read by the first GET, to pick the version rows; kept since ADR-140 so core can
    /// re-run the classification rules on an existing node.
    pub(super) sys_object_id: Option<String>,
    /// A column the version's row walks did not answer, so the full version was withheld
    /// (ADR-138 Increment 3) and only [`os_version_without_patch`](Self::os_version_without_patch)
    /// can carry one (Increment 4).
    pub(super) unread: bool,
}

impl IdentityProbe {
    fn outcome(&self) -> &'static str {
        match (&self.os_version, &self.sys_descr) {
            (Some(_), _) => "version",
            (None, _) if self.unread => "unread",
            (None, Some(_)) => "no_version",
            (None, None) => "no_answer",
        }
    }
}

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

    /// The identity probe: `sysDescr` (so core can fill the node's maker/model) and the OS version
    /// (ADR-138).
    ///
    /// The first read takes `sysDescr` and `sysObjectID` together, because which OIDs hold the
    /// version depends on what the device is; the next ones take those, and are skipped when the
    /// table keeps this device's version in `sysDescr` or does not know the device at all. Only a
    /// Huawei adds a walk of whole columns, for the patch that is running (ADR-138 Inc.2).
    /// Best-effort throughout: an error or a missing value is simply absent. The one exception is
    /// that walk of whole columns: when it did not finish, the full version is withheld rather than
    /// built from half a table (ADR-138 Increment 3), and the version without its patch goes out in
    /// its own field, which core writes only where no patch can be stripped (Increment 4). That walk
    /// also waits longer per round trip than the job does — see [`IDENTITY_COLUMN_TIMEOUT`].
    async fn fetch_identity(
        &self,
        transport: &dyn Transport,
        target: IpAddr,
        timeout: Duration,
    ) -> IdentityProbe {
        let first = self
            .read_strings(
                transport,
                target,
                &[os_version::OID_SYS_DESCR, os_version::OID_SYS_OBJECT_ID],
                timeout,
            )
            .await;
        let sys_descr = first
            .get(os_version::OID_SYS_DESCR)
            .filter(|v| !v.is_empty())
            .cloned();
        let sys_object_id = first.get(os_version::OID_SYS_OBJECT_ID).map(String::as_str);
        let reads = os_version::oids_to_read(sys_object_id, sys_descr.as_deref());
        let mut answers = os_version::Answers::default();
        if !reads.strings.is_empty() {
            answers.strings = self
                .read_strings(transport, target, &reads.strings, timeout)
                .await;
        }
        if !reads.integers.is_empty() {
            answers.integers = self
                .read_integers(transport, target, &reads.integers, timeout)
                .await;
        }
        if !reads.columns.is_empty() {
            let columns = self
                .read_columns(
                    transport,
                    target,
                    &reads.columns,
                    identity_column_timeout(timeout),
                )
                .await;
            answers.strings.extend(columns.strings);
            answers.integers.extend(columns.integers);
            answers.unanswered_columns = columns.unanswered_columns;
        }
        let os_version = os_version::resolve(sys_object_id, sys_descr.as_deref(), &answers);
        let unread = os_version.is_none() && answers.unanswered_columns;
        // Withholding the version protects a patched value already stored (Increment 3), but on a
        // node that never had one it left the row empty for good — every VRP 5.170 switch on the
        // PoC box. So the bare version still goes out, in the field core will not let strip a patch.
        let os_version_without_patch = if unread {
            os_version::resolve_without_patch(sys_object_id, sys_descr.as_deref(), &answers)
        } else {
            None
        };
        if unread {
            // The span carries the target and node. Without this line the case is invisible: the
            // walk logs a failed column at debug only, and the node page shows no patch.
            tracing::info!(
                "identity probe sent the OS version without its patch: the patch table did not answer"
            );
        }
        IdentityProbe {
            os_version,
            os_version_without_patch,
            sys_object_id: sys_object_id.and_then(yagra_discovery::normalize_sys_object_id),
            sys_descr,
            unread,
        }
    }

    /// Read integer-valued instance OIDs, keyed by the instance OID — the scalar GET, which both
    /// protocols already carry. The version table needs it for the values its string readers drop:
    /// Cisco's `entPhysicalContainedIn.1` gate and TiMOS's Gauge32 version numbers.
    async fn read_integers(
        &self,
        transport: &dyn Transport,
        target: IpAddr,
        oids: &[&str],
        timeout: Duration,
    ) -> HashMap<String, i64> {
        let asked: Vec<String> = oids.iter().map(|oid| (*oid).to_owned()).collect();
        let Ok(samples) = self.get(transport, target, &asked, timeout).await else {
            return HashMap::new();
        };
        samples
            .into_iter()
            .filter(|s| oids.contains(&s.oid.as_str()))
            // An SNMP integer, counter or gauge is whole; the transport widened it to `f64`.
            .map(|s| (s.oid, s.value as i64))
            .collect()
    }

    /// Walk whole table columns for the version table, keyed `column.instance` with the instance
    /// left **unfolded**: Huawei's `hwPatchTable` is indexed by slot and patch (`128.2`), and the
    /// version row is chosen by the state row at the same index — which a folded index could not
    /// pair (ADR-138 Inc.2). Strings and integers go to separate maps, as the table reads them.
    ///
    /// Whether the walk finished travels with the rows as `unanswered_columns`: a failed walk, or
    /// one that returned rows but did not hear every column out, is not the same answer as a table
    /// with no running patch (ADR-138 Increment 3).
    async fn read_columns(
        &self,
        transport: &dyn Transport,
        target: IpAddr,
        columns: &[&str],
        timeout: Duration,
    ) -> os_version::Answers {
        let mut read = os_version::Answers::default();
        let asked: Vec<String> = columns.iter().map(|c| (*c).to_owned()).collect();
        let Ok(walk) = self
            .walk_instance_columns(transport, target, &asked, timeout, IDENTITY_COLUMN_ROWS)
            .await
        else {
            read.unanswered_columns = true;
            return read;
        };
        read.unanswered_columns = !walk.every_column_answered;
        for row in walk.rows {
            let instance: Vec<String> = row.instance.iter().map(u32::to_string).collect();
            let key = format!("{}.{}", row.oid_base, instance.join("."));
            match row.value {
                yagra_transport::SnmpValue::Int(value) => {
                    read.integers.insert(key, value);
                }
                yagra_transport::SnmpValue::Bytes(bytes) => {
                    read.strings
                        .insert(key, String::from_utf8_lossy(&bytes).into_owned());
                }
                yagra_transport::SnmpValue::Oid(oid) => {
                    read.strings.insert(key, oid);
                }
            }
        }
        read
    }

    /// Read string-valued **instance** OIDs, keyed by the instance OID.
    ///
    /// The two protocols reach a string scalar differently: v2c walks each OID's column (its GET
    /// path returns numbers only) and keeps just the instances asked for — an ENTITY-MIB column can
    /// return hundreds of rows to find one — while v3 GETs them directly. A value the agent types as
    /// something other than a string or an OID is dropped by both readers, which is why the version
    /// table has no integer-valued sources (`os_version`'s module doc).
    async fn read_strings(
        &self,
        transport: &dyn Transport,
        target: IpAddr,
        oids: &[&str],
        timeout: Duration,
    ) -> HashMap<String, String> {
        match self {
            SnmpWalker::V2c(community) => {
                let mut columns: Vec<String> = Vec::new();
                for (column, _) in oids.iter().filter_map(|oid| os_version::walk_column(oid)) {
                    if !columns.iter().any(|c| c == column) {
                        columns.push(column.to_owned());
                    }
                }
                let Ok(rows) = transport
                    .snmp_walk_strings(target, community, &columns, timeout)
                    .await
                else {
                    return HashMap::new();
                };
                rows.into_iter()
                    .map(|r| (format!("{}.{}", r.oid_base, r.ifindex), r.value))
                    .filter(|(instance, _)| oids.contains(&instance.as_str()))
                    .collect()
            }
            SnmpWalker::V3(params) => {
                let asked: Vec<String> = oids.iter().map(|oid| (*oid).to_owned()).collect();
                let Ok(rows) = transport
                    .snmp_v3_get_strings(target, params, &asked, timeout)
                    .await
                else {
                    return HashMap::new();
                };
                rows.into_iter().map(|r| (r.oid, r.value)).collect()
            }
        }
    }

    /// Walk numeric table columns via the appropriate protocol.
    ///
    /// The second half of the answer is whether the walk got to ask for every column — see
    /// [`Transport::snmp_walk`]. Passed straight through rather than consumed here: this type is
    /// the shared funnel, and what a truncation *means* differs per caller (ADR-110 Increment 6).
    pub(super) async fn walk(
        &self,
        transport: &dyn Transport,
        target: IpAddr,
        columns: &[String],
        timeout: Duration,
    ) -> Result<(Vec<SnmpTableSample>, Option<Truncation>), TransportError> {
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
        self.walk_instance_columns(transport, target, columns, timeout, max_rows)
            .await
            .map(|walk| walk.rows)
    }

    /// As [`Self::walk_instances`], keeping whether every column answered. Only a caller that pairs
    /// rows across columns needs that — the identity probe's patch table (ADR-138 Increment 3) — so
    /// the neighbour, address, ARP, routing and media walks keep taking the rows alone.
    async fn walk_instance_columns(
        &self,
        transport: &dyn Transport,
        target: IpAddr,
        columns: &[String],
        timeout: Duration,
        max_rows: usize,
    ) -> Result<yagra_transport::InstanceWalk, TransportError> {
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
                let probe = walker.fetch_identity(transport, job.target, timeout).await;
                metrics::counter!(IDENTITY_PROBES_METRIC, "result" => probe.outcome()).increment(1);
                r.sys_descr = probe.sys_descr;
                r.os_version = probe.os_version;
                r.os_version_without_patch = probe.os_version_without_patch;
                r.sys_object_id = probe.sys_object_id;
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
        assert!(r.os_version.is_none());
    }

    fn string_row(column: &str, index: u32, value: &str) -> yagra_transport::SnmpTableString {
        yagra_transport::SnmpTableString {
            oid_base: column.to_owned(),
            ifindex: index,
            value: value.to_owned(),
        }
    }

    /// The identity probe reads the OS version from where the table says this device keeps it
    /// (ADR-138) — a FortiGate's is only in its vendor MIB, so this is the two-read path.
    ///
    /// 🚨 The `asked` assertion is not decoration: a probe that resolved the version out of rows it
    /// happened to be handed would pass the first assertion without ever asking the device.
    #[tokio::test]
    async fn the_identity_probe_reads_the_os_version_from_the_vendor_mib() {
        let mut job = snmp_job();
        job.probe_identity = true;
        let t = FakeTransport::reachable(0.0)
            .with_snmp(vec![SnmpSample {
                oid: "1.3.6.1.2.1.1.3.0".to_owned(),
                value: 1.0,
            }])
            .with_snmp_table_strings(vec![
                string_row("1.3.6.1.2.1.1.1", 0, "FGT_1500D"),
                string_row("1.3.6.1.2.1.1.2", 0, "1.3.6.1.4.1.12356.101.1.15000"),
                string_row(
                    "1.3.6.1.4.1.12356.101.4.1.1",
                    0,
                    "v7.2.6,build1575,230926 (GA.F)",
                ),
            ]);
        let r = execute(&job, &t, 1_000).await;
        assert_eq!(r.sys_descr.as_deref(), Some("FGT_1500D"));
        // ADR-140: the sysObjectID the first read already held now reaches core.
        assert_eq!(
            r.sys_object_id.as_deref(),
            Some("1.3.6.1.4.1.12356.101.1.15000")
        );
        assert_eq!(
            r.os_version.as_deref(),
            Some("v7.2.6,build1575,230926 (GA.F)")
        );
        let asked = t.asked();
        assert!(
            asked
                .iter()
                .any(|call| call.iter().any(|o| o == "1.3.6.1.4.1.12356.101.4.1.1")),
            "the vendor column was never walked: {asked:?}"
        );
    }

    /// A device the table does not cover costs what the identity probe always cost — the second
    /// read is not made — and reports its `sysDescr` with no version.
    #[tokio::test]
    async fn an_unknown_device_is_not_asked_a_second_time() {
        let mut job = snmp_job();
        job.probe_identity = true;
        let t = FakeTransport::reachable(0.0)
            .with_snmp(vec![SnmpSample {
                oid: "1.3.6.1.2.1.1.3.0".to_owned(),
                value: 1.0,
            }])
            .with_snmp_table_strings(vec![
                string_row("1.3.6.1.2.1.1.1", 0, "Acme Widget Controller rev B"),
                string_row("1.3.6.1.2.1.1.2", 0, "1.3.6.1.4.1.99999.1.7"),
            ]);
        let r = execute(&job, &t, 1_000).await;
        assert_eq!(r.os_version, None);
        assert_eq!(r.sys_descr.as_deref(), Some("Acme Widget Controller rev B"));
        // The scalar GET, then one identity walk — nothing else.
        assert_eq!(t.asked().len(), 2, "{:?}", t.asked());
    }

    /// A Huawei on YunShan OS keeps its version in `sysDescr` and its running patch in
    /// `hwPatchTable`, whose row index (`128.2`) is the device's own. So the probe walks the version
    /// and state columns whole, pairs them by the unfolded index, and shows only the running patch —
    /// not the loaded one beside it (ADR-138 Inc.2).
    const HW_PATCH_VERSION: &str = "1.3.6.1.4.1.2011.5.25.19.1.8.5.1.1.4";
    const HW_PATCH_OPERATE_STATE: &str = "1.3.6.1.4.1.2011.5.25.19.1.8.5.1.1.14";

    /// The real USG6530F-D the lab watches: its `sysDescr` and `sysObjectID`, and the patch-table
    /// rows it was read with on 2026-09-13 — a loaded patch in `128.1`, the running one in `128.2`.
    /// `with_state` false drops the state column, the shape a VRP recording has.
    fn huawei_usg(with_state: bool) -> FakeTransport {
        use yagra_transport::{SnmpInstanceRow, SnmpValue};
        let mut t = FakeTransport::reachable(0.0)
            .with_snmp(vec![SnmpSample {
                oid: "1.3.6.1.2.1.1.3.0".to_owned(),
                value: 1.0,
            }])
            .with_snmp_table_strings(vec![
                string_row(
                    "1.3.6.1.2.1.1.1",
                    0,
                    "Huawei YunShan OS \r\nVersion 1.24.0.1 (USG V600R024C00SPC100) \r\nHUAWEI USG6530F-D \r\n",
                ),
                string_row("1.3.6.1.2.1.1.2", 0, "1.3.6.1.4.1.2011.2.321.1.406"),
            ]);
        let cell = |column: &str, instance: &[u32], value: SnmpValue| SnmpInstanceRow {
            oid_base: column.to_owned(),
            instance: instance.to_vec(),
            value,
        };
        t.snmp_instances = vec![
            cell(
                HW_PATCH_VERSION,
                &[128, 1],
                SnmpValue::Bytes(b"V600R023SPH120".to_vec()),
            ),
            cell(
                HW_PATCH_VERSION,
                &[128, 2],
                SnmpValue::Bytes(b"V600R024SPH120".to_vec()),
            ),
        ];
        if with_state {
            t.snmp_instances
                .push(cell(HW_PATCH_OPERATE_STATE, &[128, 1], SnmpValue::Int(3)));
            t.snmp_instances
                .push(cell(HW_PATCH_OPERATE_STATE, &[128, 2], SnmpValue::Int(1)));
        }
        t
    }

    fn walked_the_patch_table(t: &FakeTransport) -> bool {
        t.asked().iter().any(|call| {
            call.iter().any(|o| o == HW_PATCH_VERSION)
                && call.iter().any(|o| o == HW_PATCH_OPERATE_STATE)
        })
    }

    #[tokio::test]
    async fn the_identity_probe_appends_the_running_huawei_patch() {
        let mut job = snmp_job();
        job.probe_identity = true;
        let t = huawei_usg(true);
        let r = execute(&job, &t, 1_000).await;
        assert_eq!(
            r.os_version.as_deref(),
            Some("V600R024C00SPC100 [V600R024SPH120]")
        );
        assert!(
            walked_the_patch_table(&t),
            "the patch table was never walked: {:?}",
            t.asked()
        );
    }

    /// 🚨 The defect ADR-138 Increment 3 closes, as `.210` and `.211` showed it: the patch table's
    /// walk did not finish, so the probe sent the bare version and core overwrote the patched one.
    /// Now nothing is sent — `None` is what makes core keep the stored value — while the device's
    /// own identity still arrives. The rows are all in hand here on purpose: a probe that decided
    /// from the rows would still append the patch, and only the walk's own verdict can stop it.
    #[tokio::test]
    async fn the_identity_probe_leaves_no_version_when_the_patch_table_did_not_answer() {
        let t = huawei_usg(true).with_unanswered_instance_columns();
        let walker = SnmpWalker::V2c("public".to_owned());
        let target = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));
        let probe = walker
            .fetch_identity(&t, target, Duration::from_secs(2))
            .await;
        assert_eq!(probe.os_version, None);
        assert!(probe.unread);
        assert_eq!(probe.outcome(), "unread");
        // ADR-138 Increment 4: the version still goes out, without the patch the rows would give.
        assert_eq!(
            probe.os_version_without_patch.as_deref(),
            Some("V600R024C00SPC100")
        );
        assert!(probe.sys_descr.is_some(), "the device did answer");
        assert_eq!(
            probe.sys_object_id.as_deref(),
            Some("1.3.6.1.4.1.2011.2.321.1.406")
        );
        assert!(
            walked_the_patch_table(&t),
            "the patch table was never walked: {:?}",
            t.asked()
        );

        let silent = huawei_usg(true).with_silent_instance_walks();
        let probe = walker
            .fetch_identity(&silent, target, Duration::from_secs(2))
            .await;
        assert_eq!(
            probe.os_version, None,
            "a walk that failed outright is no better"
        );
        assert_eq!(probe.outcome(), "unread");
        assert_eq!(
            probe.os_version_without_patch.as_deref(),
            Some("V600R024C00SPC100"),
            "the version lives in sysDescr, which did answer"
        );
    }

    /// A walk that answered sends the full version and nothing on the side — the field core
    /// treats more cautiously is for the unread case only, or every Huawei would be written twice.
    #[tokio::test]
    async fn a_patch_table_that_answered_sends_nothing_without_its_patch() {
        let probe = SnmpWalker::V2c("public".to_owned())
            .fetch_identity(
                &huawei_usg(true),
                IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
                Duration::from_secs(2),
            )
            .await;
        assert_eq!(
            probe.os_version.as_deref(),
            Some("V600R024C00SPC100 [V600R024SPH120]")
        );
        assert_eq!(probe.os_version_without_patch, None);
    }

    /// ADR-138 Increment 4's wait: the patch table is walked with five seconds per round trip though
    /// the job carries two — a VRP 5.170 switch took 2.35 s to answer it — and a job that is already
    /// more patient keeps its own wait.
    #[tokio::test]
    async fn the_patch_table_is_walked_with_a_longer_wait_than_the_job() {
        let target = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));
        let walker = SnmpWalker::V2c("public".to_owned());

        let t = huawei_usg(true);
        walker
            .fetch_identity(&t, target, Duration::from_secs(2))
            .await;
        assert!(walked_the_patch_table(&t), "{:?}", t.asked());
        assert_eq!(t.instance_walk_timeouts(), vec![IDENTITY_COLUMN_TIMEOUT]);

        let patient = huawei_usg(true);
        walker
            .fetch_identity(&patient, target, Duration::from_secs(8))
            .await;
        assert_eq!(
            patient.instance_walk_timeouts(),
            vec![Duration::from_secs(8)]
        );
    }

    /// The scalar GET carries the side field onto the result it sends core — the probe computing it
    /// is not enough if the poll drops it on the way out.
    #[tokio::test]
    async fn a_poll_whose_patch_table_did_not_answer_carries_the_version_without_its_patch() {
        let mut job = snmp_job();
        job.probe_identity = true;
        let t = huawei_usg(true).with_unanswered_instance_columns();
        let r = execute(&job, &t, 1_000).await;
        assert_eq!(r.os_version, None);
        assert_eq!(
            r.os_version_without_patch.as_deref(),
            Some("V600R024C00SPC100")
        );
    }

    /// The other side of the rule, and the one that protects `.210`'s two simulated VRP devices: a
    /// table that **answered** without a state column is a device with no running patch, not an
    /// unfinished read, so the bare version is still sent.
    #[tokio::test]
    async fn a_patch_table_that_answered_without_a_state_column_still_gives_the_version() {
        let t = huawei_usg(false);
        let probe = SnmpWalker::V2c("public".to_owned())
            .fetch_identity(
                &t,
                IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
                Duration::from_secs(2),
            )
            .await;
        assert_eq!(probe.os_version.as_deref(), Some("V600R024C00SPC100"));
        assert!(!probe.unread);
        assert_eq!(probe.outcome(), "version");
    }

    /// Over v3 the version instance is fetched with a GET, not by walking its column, and a value
    /// longer than the cap is cut rather than refused.
    #[tokio::test]
    async fn the_v3_identity_probe_gets_the_version_instance_directly() {
        use yagra_bus::SnmpV3Check;
        use yagra_transport::SnmpStringSample;
        let mut job = PollJob::snmp_v3(
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
        job.probe_identity = true;
        let long = format!("v7.4.1,{}", "b".repeat(300));
        let mut t = FakeTransport::reachable(0.0).with_snmp(vec![SnmpSample {
            oid: "1.3.6.1.2.1.1.3.0".to_owned(),
            value: 1.0,
        }]);
        t.snmp_v3_strings = vec![
            SnmpStringSample {
                oid: "1.3.6.1.2.1.1.1.0".to_owned(),
                value: "FGT_60F".to_owned(),
            },
            SnmpStringSample {
                oid: "1.3.6.1.2.1.1.2.0".to_owned(),
                value: "1.3.6.1.4.1.12356.101.1.60".to_owned(),
            },
            SnmpStringSample {
                oid: "1.3.6.1.4.1.12356.101.4.1.1.0".to_owned(),
                value: long,
            },
        ];
        let r = execute(&job, &t, 1_000).await;
        let version = r.os_version.expect("a version");
        assert!(version.starts_with("v7.4.1,"), "{version}");
        assert_eq!(
            version.chars().count(),
            yagra_discovery::os_version::OS_VERSION_MAX_CHARS
        );
        let asked = t.asked();
        assert!(
            asked
                .iter()
                .any(|call| call.iter().any(|o| o == "1.3.6.1.4.1.12356.101.4.1.1.0")),
            "v3 must GET the instance: {asked:?}"
        );
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
