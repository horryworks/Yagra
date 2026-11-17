// SPDX-License-Identifier: AGPL-3.0-only
//! The adjacency walks: who this device is next to, at layer 2 and layer 3 (ADR-038, ADR-043), and
//! how its ports are configured to carry VLANs (ADR-201).
//!
//! Five checks — CDP/LLDP neighbours, interface addresses, ARP / IPv6 neighbours, routing
//! adjacency and port VLANs — that share one contract: **they are observational**. Each sets
//! `PollResult.observational`, so the result is persisted and never reaches the liveness state
//! machine. That is not tidiness. An hourly neighbour walk that failed would otherwise page someone
//! for a healthy device, and one that succeeded would cancel a real outage ICMP had already found.
//!
//! ⚠️ Like [`super::physical`], the interpretation lives in pure siblings — [`crate::neighbors`],
//! [`crate::l3`], [`crate::arp`], [`crate::routing`], [`crate::vlans`] — which take walked rows and
//! return a normalized set. This file holds the sessions and nothing else.
//!
//! Each also reports `Some(empty)` and `None` differently on purpose: a device with no neighbours
//! replaces the stored set (a real observation), while a failed walk sends nothing at all, so core
//! never records "every link disappeared" because a walk timed out.

use super::*;

/// Execute a CDP/LLDP neighbour walk (v2c or v3, selected by `walker`) — ADR-038.
///
/// Three properties distinguish this from every other arm:
///
/// * The result is **observational**: it says nothing about the node's reachability. `outcome` is
///   fixed at `Reachable` and core ignores it, because either alternative is a real bug — reporting
///   `Unreachable` on a failed hourly walk pages someone for a healthy device, and reporting
///   `Reachable` unconditionally would cancel a genuine outage ICMP had detected.
/// * `neighbors` is `Some` **only when the walk actually produced rows to interpret**. A transport
///   failure sends `None`, so core writes nothing rather than recording "every link disappeared".
/// * A device that simply has no neighbours sends `Some(empty)`, which *does* replace the stored
///   set — that is a real observation, and it is how an unplugged switch stops showing stale peers.
pub(super) async fn execute_neighbors(
    job: &PollJob,
    transport: &dyn Transport,
    at_unix_ms: i64,
    columns: &[yagra_bus::SnmpNeighborColumn],
    timeout: Duration,
    walker: &SnmpWalker,
) -> PollResult {
    // The declared columns plus the ones this poller walks on its own (ADR-180). Both the walk and
    // the assembly read the widened list, or the appended rows would be walked and then ignored.
    let columns = &crate::neighbors::with_poller_columns(columns);
    let bases: Vec<String> = columns.iter().map(|c| c.oid.clone()).collect();
    let mut r = result(job, at_unix_ms, CheckOutcome::Reachable, Vec::new());
    r.observational = true;
    match walker
        .walk_instances(
            transport,
            job.target,
            &bases,
            timeout,
            yagra_common::MAX_NEIGHBOR_WALK_ROWS,
        )
        .await
    {
        Ok(rows) => {
            let set = crate::neighbors::assemble(columns, &rows);
            if set.truncated {
                metrics::counter!("yagra_neighbor_rows_truncated_total").increment(1);
                tracing::warn!(
                    job_id = %job.job_id,
                    kept = set.len(),
                    "neighbour set exceeded the per-node cap; the excess was dropped"
                );
            }
            r.samples.push(Sample::gauge(
                yagra_common::METRIC_SNMP_NEIGHBOR_COUNT,
                set.len() as f64,
            ));
            r.neighbors = Some(set);
        }
        Err(err) => {
            // No set, no count sample: the poll observed nothing, and saying "0 neighbours" here
            // would be a claim the walk never made.
            tracing::debug!(job_id = %job.job_id, error = %err, "neighbour walk failed");
        }
    }
    r
}

/// Execute an interface-address walk (v2c or v3, selected by `walker`) — ADR-043.
///
/// Structurally identical to [`execute_neighbors`], and for the same three reasons:
///
/// * The result is **observational**. An hourly address walk that timed out must not push
///   `Unreachable` into the dwell window ICMP owns, and must not report `Reachable` either — that
///   would cancel a genuine outage. It has nothing to say about liveness, so it says nothing.
/// * `l3` is `Some` **only when the walk actually produced rows**. A transport failure sends `None`,
///   so core writes nothing rather than recording that the device lost its addressing — which, one
///   derivation later, would read as every link through that node disappearing.
/// * A device with no addresses to report sends `Some(empty)`, which *does* replace the stored
///   snapshot. That is a real observation.
pub(super) async fn execute_l3(
    job: &PollJob,
    transport: &dyn Transport,
    at_unix_ms: i64,
    columns: &[yagra_bus::SnmpL3Column],
    timeout: Duration,
    walker: &SnmpWalker,
) -> PollResult {
    let bases: Vec<String> = columns.iter().map(|c| c.oid.clone()).collect();
    let mut r = result(job, at_unix_ms, CheckOutcome::Reachable, Vec::new());
    r.observational = true;
    match walker
        .walk_instances(
            transport,
            job.target,
            &bases,
            timeout,
            yagra_common::MAX_L3_WALK_ROWS,
        )
        .await
    {
        Ok(rows) => {
            let snapshot = crate::l3::assemble(columns, &rows);
            if snapshot.truncated {
                metrics::counter!("yagra_l3_rows_truncated_total").increment(1);
                tracing::warn!(
                    job_id = %job.job_id,
                    kept = snapshot.len(),
                    "interface-address set exceeded the per-node cap; the excess was dropped"
                );
            }
            r.samples.push(Sample::gauge(
                yagra_common::METRIC_SNMP_L3_ADDRESS_COUNT,
                snapshot.len() as f64,
            ));
            r.l3 = Some(snapshot);
        }
        Err(err) => {
            // No snapshot, no count sample: the poll observed nothing, and saying "0 addresses"
            // here would be a claim the walk never made.
            tracing::debug!(job_id = %job.job_id, error = %err, "interface-address walk failed");
        }
    }
    r
}

/// Execute a port VLAN walk (v2c or v3, selected by `walker`) — ADR-201.
///
/// Observational like the walks above, with one difference that matters: the snapshot is sent
/// **only when every column answered**. The other walks read each column on its own, but this one
/// reads a port across several (Cisco's status beside its allowed list, Huawei's port number beside
/// its ifIndex), and a column that timed out between two that answered reads exactly like a device
/// that does not implement it — a whole switch of trunks would then read as access ports, or as
/// no switch ports at all. An agent answering that it does not implement a column still counts as
/// answered, which is what lets YunShan, with no operating-mode column, through.
pub(super) async fn execute_vlans(
    job: &PollJob,
    transport: &dyn Transport,
    at_unix_ms: i64,
    dialect: yagra_common::VlanDialect,
    timeout: Duration,
    walker: &SnmpWalker,
) -> PollResult {
    let mut r = result(job, at_unix_ms, CheckOutcome::Reachable, Vec::new());
    r.observational = true;
    let columns = crate::vlans::columns(dialect);
    if columns.is_empty() {
        tracing::debug!(job_id = %job.job_id, "VLAN walk for a dialect this poller does not know");
        return r;
    }
    let timeout = vlan_column_timeout(timeout);
    let started = std::time::Instant::now();
    match walker
        .walk_instance_columns(transport, job.target, &columns, timeout, MAX_VLAN_WALK_ROWS)
        .await
    {
        // The row budget ends a column as answered (the walker stops asking, it does not fail), so
        // the column flag alone cannot tell a full table from one cut at the cap — the same
        // reasoning as the ENTITY index walk in `physical.rs`.
        Ok(walk) if walk.every_column_answered && walk.rows.len() < MAX_VLAN_WALK_ROWS => {
            let ports = crate::vlans::assemble(dialect, &walk.rows);
            let names = walk_vlan_names(job, transport, dialect, timeout, walker).await;
            r.vlans = Some(ports.with_names(names));
        }
        Ok(walk) => {
            // A walk that filled its row budget lands here too, whichever column it was cut in.
            metrics::counter!("yagra_vlan_walk_incomplete_total").increment(1);
            // Info, not debug: this is the line that says how far short a slow device fell, and it
            // comes at most once an hour per node (ADR-201 Increment 2).
            tracing::info!(
                job_id = %job.job_id,
                rows = walk.rows.len(),
                elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
                "VLAN walk incomplete; nothing sent"
            );
        }
        Err(err) => {
            tracing::debug!(job_id = %job.job_id, error = %err, "VLAN walk failed");
        }
    }
    r
}

/// Walk the VLAN names a device defines — ADR-257 decision 2.
///
/// A walk of its own, after the port walk, so a device without the name table (or one that times
/// out on it) still sends its ports. `None` — "names not read" — whenever this walk cannot vouch
/// for a whole table: a column that did not answer, the row budget reached, an error, or an empty
/// table (see `vlans::assemble_names`). A surface marks a VLAN "not defined" only on `Some`.
async fn walk_vlan_names(
    job: &PollJob,
    transport: &dyn Transport,
    dialect: yagra_common::VlanDialect,
    timeout: Duration,
    walker: &SnmpWalker,
) -> Option<Vec<yagra_common::VlanName>> {
    let columns = crate::vlans::name_columns(dialect);
    if columns.is_empty() {
        return None;
    }
    let timeout = vlan_column_timeout(timeout);
    match walker
        .walk_instance_columns(transport, job.target, &columns, timeout, MAX_VLAN_NAME_ROWS)
        .await
    {
        Ok(walk) if walk.every_column_answered && walk.rows.len() < MAX_VLAN_NAME_ROWS => {
            crate::vlans::assemble_names(dialect, &walk.rows)
        }
        Ok(walk) => {
            metrics::counter!("yagra_vlan_name_walk_incomplete_total").increment(1);
            tracing::debug!(
                job_id = %job.job_id,
                rows = walk.rows.len(),
                "VLAN name walk incomplete; names not sent"
            );
            None
        }
        Err(err) => {
            tracing::debug!(job_id = %job.job_id, error = %err, "VLAN name walk failed");
            None
        }
    }
}

/// The shortest per-round-trip wait the VLAN walks use (ADR-201 Increment 2). The walk's budget is
/// eight of these, so this also takes it from 16 s to 40 s.
///
/// **Why not the job's own 2 s.** Measured on the PoC: a two-member Huawei S6730 stack (VRP 5.170,
/// 230 interfaces) spent the whole 16 s budget on the first six of Huawei's twelve columns, every
/// hour, so its ports never once showed a VLAN — the walk sends nothing unless every column
/// answered. VRP is slow on Huawei's own tables (the patch table in ADR-138 Increment 4 is the
/// same habit), and the trunk and hybrid columns carry 256 bytes per port each.
///
/// ⚠️ 40 s is an estimate, not a measurement: the six columns not reached include four of those
/// heavy ones. It also stays inside `table_plan`'s 60 s single-flight floor, so a one-minute job
/// queued behind this walk on the same device waits rather than being shed.
const VLAN_COLUMN_TIMEOUT: Duration = Duration::from_secs(5);

/// The wait for the VLAN walks: [`VLAN_COLUMN_TIMEOUT`], or the job's own when that is longer — a
/// job already more patient is never made less so.
fn vlan_column_timeout(job_timeout: Duration) -> Duration {
    job_timeout.max(VLAN_COLUMN_TIMEOUT)
}

/// The most rows the name walk may collect: three columns over every usable VLAN ID, under one VTP
/// domain, is 12,282. A device answering more is out of spec, and its names are not sent.
const MAX_VLAN_NAME_ROWS: usize = 16_384;

/// The most rows one VLAN walk may collect across all its columns. Estimated, not measured: the
/// largest device walked for ADR-201 (a four-member Huawei stack, 220 interfaces) has about 2,000
/// rows across these columns, about ten per port, so this leaves room for a chassis of 3,000 ports.
const MAX_VLAN_WALK_ROWS: usize = 32_768;

/// Execute an ARP / IPv6-neighbour walk (v2c or v3, selected by `walker`) — ADR-043 Increment 3.
///
/// Shares the three properties of [`execute_neighbors`] and [`execute_l3`] — observational, `Some`
/// only when the walk produced rows, `Some(empty)` is a real answer — and adds a fourth that is
/// specific to this check:
///
/// * **The row budget comes from the job**, and the truncation flag is derived from it here rather
///   than inside the assembler. Only this layer knows how many rows it asked for, so only this
///   layer can tell a full answer from a walk that ran out — and `truncated` is what stops a
///   partial read of a large table from being published as the whole picture.
pub(super) async fn execute_arp(
    job: &PollJob,
    transport: &dyn Transport,
    at_unix_ms: i64,
    columns: &[yagra_bus::SnmpArpColumn],
    max_rows: u32,
    timeout: Duration,
    walker: &SnmpWalker,
) -> PollResult {
    let bases: Vec<String> = columns.iter().map(|c| c.oid.clone()).collect();
    // Core decides the fleet-wide budget, but a core that sent nonsense (or a field an N-1 core
    // never sent at all) must not turn into "walk the whole table": the transport's own ceiling is
    // the backstop and this is the floor under it.
    let budget = usize::try_from(max_rows)
        .unwrap_or(yagra_common::MAX_ARP_WALK_ROWS)
        .clamp(1, yagra_common::MAX_ARP_WALK_ROWS);
    let mut r = result(job, at_unix_ms, CheckOutcome::Reachable, Vec::new());
    r.observational = true;
    match walker
        .walk_instances(transport, job.target, &bases, timeout, budget)
        .await
    {
        Ok(rows) => {
            // The walk stops *at* the budget, so hitting it exactly is the signal that there was
            // more table behind it. One row short is a complete answer.
            let walk_truncated = rows.len() >= budget;
            let summary = crate::arp::assemble(columns, &rows, walk_truncated);
            if summary.truncated {
                metrics::counter!("yagra_arp_rows_truncated_total").increment(1);
                tracing::warn!(
                    job_id = %job.job_id,
                    kept = summary.len(),
                    observed = summary.observed,
                    "ARP cache exceeded the walk or sample cap; the excess was dropped"
                );
            }
            r.samples.push(Sample::gauge(
                yagra_common::METRIC_SNMP_ARP_ENTRY_COUNT,
                f64::from(summary.observed),
            ));
            r.arp = Some(summary);
        }
        Err(err) => {
            // No summary, no count sample: the poll observed nothing, and saying "0 endpoints" here
            // would be a claim the walk never made — one that would then age every discovered
            // endpoint behind this router out of the table.
            tracing::debug!(job_id = %job.job_id, error = %err, "ARP walk failed");
        }
    }
    r
}

/// What one routing job asks for (ADR-043 Increment 4, ADR-191 Inc.10).
pub(super) struct RoutingAsk<'a> {
    pub columns: &'a [yagra_bus::SnmpRoutingColumn],
    pub probes: &'a [yagra_bus::SnmpRouteProbe],
    pub default_route: bool,
}

/// Where the IPv4 default route points: the RFC 4292 table first, the deprecated RFC 2096 one when
/// the first gives no IPv4 next hop (ADR-191 Inc.10). `Some(empty)` is an answer — the device has no
/// default route — and it is given only when the table that decides it answered: the RFC 2096 one,
/// since on some IOS releases it is the only table holding the route at all. When that read fails
/// the answer is `None` (not known), whatever the RFC 4292 table said.
async fn read_default_route(
    job: &PollJob,
    transport: &dyn Transport,
    timeout: Duration,
    walker: &SnmpWalker,
) -> Option<Vec<std::net::IpAddr>> {
    let read = |root: &'static str| async move {
        match walker
            .walk_instances(
                transport,
                job.target,
                &[root.to_owned()],
                timeout,
                yagra_common::MAX_DEFAULT_ROUTE_ROWS,
            )
            .await
        {
            Ok(rows) => Some(crate::routing::default_next_hops(&rows)),
            Err(err) => {
                tracing::debug!(job_id = %job.job_id, error = %err, root, "default route read failed");
                None
            }
        }
    };
    if let Some(hops) = read(yagra_common::OID_DEFAULT_ROUTE_INET).await {
        if !hops.is_empty() {
            return Some(hops);
        }
    }
    read(yagra_common::OID_DEFAULT_ROUTE_IPCIDR).await
}

/// Execute a routing-adjacency collection (v2c or v3, selected by `walker`) — ADR-043 Increment 4.
///
/// Shares the three properties of [`execute_neighbors`], [`execute_l3`] and [`execute_arp`] —
/// observational, `Some` only when something was actually collected, `Some(empty)` is a real answer
/// — and adds a fourth specific to this check:
///
/// * **Two calls, two budgets.** The adjacency walk reads tables sized by the device's peering
///   mesh; the probes read one destination each. Sharing a budget would let a route reflector's
///   hundreds of iBGP peers consume it before the probes ran, and which half lost would depend on
///   the order the bases happened to be listed in — a silent, configuration-dependent gap.
///
/// A failure of *either* call leaves that half's rows out and lets the other half through: a device
/// that answers `bgpPeerState` but has no `inetCidrRouteTable` is ordinary, and refusing the whole
/// observation because one table is absent would collect nothing from most of the fleet.
///
/// Since ADR-191 Inc.10 a job may also ask where the IPv4 default route points. That read rides on
/// an observation the two halves above already made — it never makes one on its own, because a
/// snapshot sent when neither half answered would erase the node's stored adjacency.
pub(super) async fn execute_routing(
    job: &PollJob,
    transport: &dyn Transport,
    at_unix_ms: i64,
    ask: RoutingAsk<'_>,
    timeout: Duration,
    walker: &SnmpWalker,
) -> PollResult {
    let RoutingAsk {
        columns,
        probes,
        default_route,
    } = ask;
    let mut r = result(job, at_unix_ms, CheckOutcome::Reachable, Vec::new());
    r.observational = true;

    let mut rows = Vec::new();
    let mut answered = false;
    let mut truncated = false;

    if !columns.is_empty() {
        let bases: Vec<String> = columns.iter().map(|c| c.oid.clone()).collect();
        match walker
            .walk_instances(
                transport,
                job.target,
                &bases,
                timeout,
                yagra_common::MAX_ROUTING_WALK_ROWS,
            )
            .await
        {
            Ok(found) => {
                truncated |= found.len() >= yagra_common::MAX_ROUTING_WALK_ROWS;
                rows.extend(found);
                answered = true;
            }
            Err(err) => {
                tracing::debug!(job_id = %job.job_id, error = %err, "routing adjacency walk failed");
            }
        }
    }

    if !probes.is_empty() {
        // Every probe is its own subtree root, so they go in one call: the transport walks each in
        // turn and the shared budget is the *probe* budget, sized for exactly this many roots.
        let bases: Vec<String> = probes.iter().map(|p| p.oid.clone()).collect();
        match walker
            .walk_instances(
                transport,
                job.target,
                &bases,
                timeout,
                yagra_common::MAX_ROUTE_PROBE_ROWS,
            )
            .await
        {
            Ok(found) => {
                truncated |= found.len() >= yagra_common::MAX_ROUTE_PROBE_ROWS;
                rows.extend(found);
                answered = true;
            }
            Err(err) => {
                tracing::debug!(job_id = %job.job_id, error = %err, "route probes failed");
            }
        }
    }

    if !answered {
        // Neither half produced anything, so nothing was observed. Sending `Some(empty)` here would
        // erase the node's stored adjacency on a transport failure — and, one derivation later,
        // every link that node was in.
        return r;
    }

    let mut snapshot = crate::routing::assemble(columns, probes, &rows, truncated);
    if default_route {
        if let Some(hops) = read_default_route(job, transport, timeout, walker).await {
            snapshot = snapshot.with_default_next_hops(hops);
        }
    }
    if snapshot.truncated {
        metrics::counter!("yagra_routing_rows_truncated_total").increment(1);
        tracing::warn!(
            job_id = %job.job_id,
            kept = snapshot.len(),
            "routing adjacency exceeded a walk or sample cap; the excess was dropped"
        );
    }
    r.samples.push(Sample::gauge(
        yagra_common::METRIC_SNMP_ROUTING_ADJACENCY_COUNT,
        snapshot.len() as f64,
    ));
    r.routing = Some(snapshot);
    r
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};
    use uuid::Uuid;
    use yagra_common::{NodeId, SnmpV3Auth};
    use yagra_transport::{FakeTransport, SnmpSample};

    /// A neighbour job's own columns, as core sends them.
    fn neighbor_columns() -> Vec<yagra_bus::SnmpNeighborColumn> {
        yagra_common::builtin_neighbor_columns()
            .into_iter()
            .map(|(field, oid)| yagra_bus::SnmpNeighborColumn {
                field,
                oid: oid.to_owned(),
            })
            .collect()
    }

    fn neighbor_job() -> PollJob {
        PollJob::snmp_neighbors(
            Uuid::nil(),
            NodeId::from(Uuid::nil()),
            IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
            yagra_bus::SnmpNeighborCheck {
                community: "public".into(),
                columns: neighbor_columns(),
                timeout_ms: 2000,
            },
            3600,
        )
    }

    fn l3_job() -> PollJob {
        PollJob::snmp_l3(
            Uuid::nil(),
            NodeId::from(Uuid::nil()),
            IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
            yagra_bus::SnmpL3Check {
                community: "public".into(),
                columns: yagra_common::builtin_l3_columns()
                    .into_iter()
                    .map(|(field, oid)| yagra_bus::SnmpL3Column {
                        field,
                        oid: oid.to_owned(),
                    })
                    .collect(),
                timeout_ms: 2000,
            },
            3600,
        )
    }

    /// The safety property from ADR-038: a neighbour result must never reach the liveness state
    /// machine. `outcome` feeds it on every result, so a device that speaks no LLDP/CDP — an
    /// entirely normal state — would otherwise register as either an outage or a recovery.
    #[tokio::test]
    async fn a_neighbor_result_is_observational_and_states_nothing_about_liveness() {
        let transport = FakeTransport::reachable(1.0);
        let r = execute(&neighbor_job(), &transport, 0).await;
        assert!(
            r.observational,
            "core keys its skip-the-alert-engine branch off this flag"
        );
        // Its count sample is not judged either: the walk runs hourly, slower than the freshness
        // sweep and every dwell assume (ADR-158 A10).
        assert!(!r.judge_samples);
        // A device with nothing to report still made a real observation: an empty set, which
        // replaces whatever was stored (so an unplugged switch stops showing stale peers).
        assert_eq!(
            r.neighbors.as_ref().map(yagra_common::NeighborSet::len),
            Some(0)
        );
        assert_eq!(
            r.samples.len(),
            1,
            "one bounded node-level count sample, and no per-adjacency series"
        );
        assert_eq!(
            r.samples[0].metric,
            yagra_common::METRIC_SNMP_NEIGHBOR_COUNT
        );
    }

    /// The same safety property ADR-038 established, now for ADR-043's walk. `outcome` feeds the
    /// liveness state machine on *every* result, so an hourly address walk must be able to say
    /// nothing about reachability — reporting a failure would page someone for a healthy device,
    /// and reporting success would cancel a genuine outage ICMP had detected.
    #[tokio::test]
    async fn an_l3_result_is_observational_and_states_nothing_about_liveness() {
        let transport = FakeTransport::reachable(1.0);
        let r = execute(&l3_job(), &transport, 0).await;
        assert!(
            r.observational,
            "core keys its skip-the-alert-engine branch off this flag"
        );
        // A device with no addresses to report still made a real observation: an empty snapshot,
        // which replaces whatever was stored.
        assert_eq!(r.l3.as_ref().map(yagra_common::L3Snapshot::len), Some(0));
        assert_eq!(
            r.samples.len(),
            1,
            "one bounded node-level count sample, and no per-address series — an IP in a label is \
             the cardinality explosion CLAUDE.md §7.1 names"
        );
        assert_eq!(
            r.samples[0].metric,
            yagra_common::METRIC_SNMP_L3_ADDRESS_COUNT
        );
    }

    /// A failed walk must send **no** set. Sending `Some(empty)` would tell core the device has no
    /// neighbours, wiping a correct stored adjacency because one SNMP request timed out.
    #[tokio::test]
    async fn a_failed_neighbor_walk_reports_no_set_rather_than_an_empty_one() {
        /// A transport whose instance walk always fails; everything else delegates to the fake.
        struct WalkFails(FakeTransport);
        #[async_trait::async_trait]
        impl Transport for WalkFails {
            async fn snmp_walk_instances(
                &self,
                _t: IpAddr,
                _c: &str,
                _o: &[String],
                _to: Duration,
                _max: usize,
            ) -> Result<yagra_transport::InstanceWalk, TransportError> {
                Err(TransportError::Io("snmp connect refused".into()))
            }
            async fn snmp_v3_walk_instances(
                &self,
                _t: IpAddr,
                _p: &SnmpV3Params,
                _o: &[String],
                _to: Duration,
                _max: usize,
            ) -> Result<yagra_transport::InstanceWalk, TransportError> {
                Err(TransportError::Io("snmp connect refused".into()))
            }
            async fn probe_icmp(
                &self,
                t: IpAddr,
                c: u8,
                to: Duration,
            ) -> Result<yagra_transport::IcmpProbe, TransportError> {
                self.0.probe_icmp(t, c, to).await
            }
            async fn snmp_get(
                &self,
                t: IpAddr,
                c: &str,
                o: &[String],
                to: Duration,
            ) -> Result<Vec<SnmpSample>, TransportError> {
                self.0.snmp_get(t, c, o, to).await
            }
            async fn snmp_v3_get(
                &self,
                t: IpAddr,
                p: &SnmpV3Params,
                o: &[String],
                to: Duration,
            ) -> Result<Vec<SnmpSample>, TransportError> {
                self.0.snmp_v3_get(t, p, o, to).await
            }
            async fn snmp_v3_get_strings(
                &self,
                t: IpAddr,
                p: &SnmpV3Params,
                o: &[String],
                to: Duration,
            ) -> Result<Vec<yagra_transport::SnmpStringSample>, TransportError> {
                self.0.snmp_v3_get_strings(t, p, o, to).await
            }
            async fn snmp_walk(
                &self,
                t: IpAddr,
                c: &str,
                o: &[String],
                l: WalkLimits,
            ) -> Result<TableWalk<SnmpTableSample>, TransportError> {
                self.0.snmp_walk(t, c, o, l).await
            }
            async fn snmp_walk_strings(
                &self,
                t: IpAddr,
                c: &str,
                o: &[String],
                l: WalkLimits,
            ) -> Result<TableWalk<SnmpTableString>, TransportError> {
                self.0.snmp_walk_strings(t, c, o, l).await
            }
            async fn snmp_v3_walk(
                &self,
                t: IpAddr,
                p: &SnmpV3Params,
                o: &[String],
                l: WalkLimits,
            ) -> Result<TableWalk<SnmpTableSample>, TransportError> {
                self.0.snmp_v3_walk(t, p, o, l).await
            }
            async fn snmp_v3_walk_strings(
                &self,
                t: IpAddr,
                p: &SnmpV3Params,
                o: &[String],
                l: WalkLimits,
            ) -> Result<TableWalk<SnmpTableString>, TransportError> {
                self.0.snmp_v3_walk_strings(t, p, o, l).await
            }
            async fn probe_http(
                &self,
                s: &HttpProbeSpec,
                to: Duration,
            ) -> Result<yagra_transport::HttpProbe, TransportError> {
                self.0.probe_http(s, to).await
            }
            async fn resolve_dns(
                &self,
                s: &yagra_transport::DnsProbeSpec,
                to: Duration,
            ) -> Result<yagra_transport::DnsChain, TransportError> {
                self.0.resolve_dns(s, to).await
            }
            async fn collect_meraki(
                &self,
                s: &yagra_transport::MerakiCollectSpec,
                to: Duration,
            ) -> Result<yagra_transport::MerakiCollected, yagra_transport::MerakiFetchError>
            {
                self.0.collect_meraki(s, to).await
            }
        }

        let r = execute(
            &neighbor_job(),
            &WalkFails(FakeTransport::reachable(1.0)),
            0,
        )
        .await;
        assert!(r.observational);
        assert!(
            r.neighbors.is_none(),
            "a failed walk must not read as 'this device has no neighbours'"
        );
        assert!(
            r.samples.is_empty(),
            "no count sample either — the poll observed nothing to count"
        );
    }

    #[tokio::test]
    async fn a_v3_neighbor_job_takes_the_same_path() {
        let job = PollJob::snmp_v3_neighbors(
            Uuid::nil(),
            NodeId::from(Uuid::nil()),
            IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
            yagra_bus::SnmpV3NeighborCheck {
                auth: SnmpV3Auth {
                    user: "monitor".into(),
                    security_level: "authpriv".into(),
                    auth_protocol: Some("sha256".into()),
                    auth_key: Some("auth-pass-12345".into()),
                    priv_protocol: Some("aes256".into()),
                    priv_key: Some("priv-pass-12345".into()),
                },
                columns: neighbor_columns(),
                timeout_ms: 2000,
            },
            3600,
        );
        let r = execute(&job, &FakeTransport::reachable(1.0), 0).await;
        assert!(r.observational);
        assert!(r.neighbors.is_some());
    }

    /// A Cisco access-port VLAN job, as core builds it for a v2c node.
    fn vlan_job() -> PollJob {
        PollJob::snmp_vlans(
            Uuid::nil(),
            NodeId::from(Uuid::nil()),
            IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
            yagra_bus::SnmpVlanCheck {
                community: "public".into(),
                dialect: yagra_common::VlanDialect::Cisco,
                timeout_ms: 2000,
            },
            3600,
        )
    }

    /// `vmVlan` rows (CISCO-VLAN-MEMBERSHIP-MIB): `count` access ports, each in VLAN 10.
    fn access_ports(count: u32) -> Vec<yagra_transport::SnmpInstanceRow> {
        (1..=count)
            .map(|ifindex| yagra_transport::SnmpInstanceRow {
                oid_base: "1.3.6.1.4.1.9.9.68.1.2.2.1.2".into(),
                instance: vec![ifindex],
                value: yagra_transport::SnmpValue::Int(10),
            })
            .collect()
    }

    /// ADR-201 Increment 2: both VLAN walks — the ports and the names — wait 5 s per round trip,
    /// not the job's 2 s. At 2 s a VRP stack spent its whole budget on half the columns and never
    /// sent a VLAN.
    #[tokio::test]
    async fn the_vlan_walks_wait_longer_than_the_job() {
        let mut fake = FakeTransport::reachable(1.0);
        fake.snmp_instances = access_ports(3);
        let r = execute(&vlan_job(), &fake, 0).await;
        assert!(r.vlans.is_some());
        assert_eq!(
            fake.instance_walk_timeouts(),
            vec![VLAN_COLUMN_TIMEOUT; 2],
            "the port walk and the name walk"
        );
    }

    /// A job already more patient than [`VLAN_COLUMN_TIMEOUT`] keeps its own wait.
    #[test]
    fn a_longer_job_timeout_is_kept() {
        assert_eq!(
            vlan_column_timeout(Duration::from_secs(2)),
            VLAN_COLUMN_TIMEOUT
        );
        assert_eq!(
            vlan_column_timeout(Duration::from_secs(9)),
            Duration::from_secs(9)
        );
    }

    /// The ADR-201 property in the direction that sends: every column answered, under the cap.
    #[tokio::test]
    async fn a_complete_vlan_walk_sends_its_snapshot() {
        let mut fake = FakeTransport::reachable(1.0);
        fake.snmp_instances = access_ports(3);
        let r = execute(&vlan_job(), &fake, 0).await;
        assert!(r.observational);
        assert_eq!(r.vlans.map(|s| s.ports.len()), Some(3));
    }

    /// `vtpVlanName` rows under VTP domain 1.
    fn vtp_names(vlans: &[u32]) -> Vec<yagra_transport::SnmpInstanceRow> {
        vlans
            .iter()
            .map(|v| yagra_transport::SnmpInstanceRow {
                oid_base: "1.3.6.1.4.1.9.9.46.1.3.1.1.4".into(),
                instance: vec![1, *v],
                value: yagra_transport::SnmpValue::Bytes(format!("V{v}").into_bytes()),
            })
            .collect()
    }

    /// ADR-257 decision 2 in the direction that sends: the port walk and the name walk both
    /// complete, and the names ride the same snapshot.
    #[tokio::test]
    async fn a_complete_name_walk_rides_the_port_snapshot() {
        let mut fake = FakeTransport::reachable(1.0);
        fake.snmp_instances = access_ports(2);
        fake.snmp_instances.extend(vtp_names(&[1, 10]));
        let s = execute(&vlan_job(), &fake, 0)
            .await
            .vlans
            .expect("snapshot");
        assert_eq!(s.ports.len(), 2);
        let ids: Vec<u16> = s.names.expect("names read").iter().map(|n| n.id).collect();
        assert_eq!(ids, vec![1, 10]);
    }

    /// The point of a separate walk: a name table that does not answer costs the names, never the
    /// ports. Only the name column is made to fail, so the port walk beside it stays complete
    /// (`failure-injection-must-break-only-the-read-under-test`).
    #[tokio::test]
    async fn a_name_walk_that_did_not_finish_still_sends_the_ports_and_no_names() {
        let mut fake = FakeTransport::reachable(1.0)
            .with_unanswered_instance_column("1.3.6.1.4.1.9.9.46.1.3.1.1.4");
        fake.snmp_instances = access_ports(2);
        fake.snmp_instances.extend(vtp_names(&[1, 10]));
        let s = execute(&vlan_job(), &fake, 0)
            .await
            .vlans
            .expect("ports still sent");
        assert_eq!(s.ports.len(), 2);
        assert_eq!(
            s.names, None,
            "an unfinished name walk is 'not read', never 'defines none'"
        );
    }

    /// A device that answers no name row at all does not implement the table: not read.
    #[tokio::test]
    async fn an_empty_name_table_is_sent_as_not_read() {
        let mut fake = FakeTransport::reachable(1.0);
        fake.snmp_instances = access_ports(2);
        let s = execute(&vlan_job(), &fake, 0)
            .await
            .vlans
            .expect("snapshot");
        assert_eq!(s.names, None);
    }

    /// The row budget ends a column as answered (the walker stops asking, it does not fail), so a
    /// walk cut at the cap would otherwise pass as complete — and the stored snapshot would be
    /// replaced by one missing whatever the last column had left to say.
    #[tokio::test]
    async fn a_vlan_walk_that_filled_its_row_budget_sends_nothing() {
        let mut fake = FakeTransport::reachable(1.0);
        fake.snmp_instances = access_ports(u32::try_from(MAX_VLAN_WALK_ROWS).expect("fits"));
        let r = execute(&vlan_job(), &fake, 0).await;
        assert!(
            r.vlans.is_none(),
            "a table that filled the budget may have lost rows; no snapshot replaces the stored one"
        );
    }
}
