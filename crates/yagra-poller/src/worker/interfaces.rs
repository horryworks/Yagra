// SPDX-License-Identifier: AGPL-3.0-only
//! The interface table walk: numbers keyed by ifIndex, and the metadata fold beside them.
//!
//! One combined walk answers two different questions. Numeric columns become per-interface
//! [`Sample`]s carrying the column's own metric name and kind — no OID-name guessing, and the only
//! place a counter can enter the system (`yagra-core`'s `api/metrics.rs` reads this file to check
//! that). Metadata columns become [`DiscoveredInterface`] rows for the PostgreSQL inventory, never
//! TSDB labels (ADR-011).
//!
//! ⚠️ The speed column is both: `ifHighSpeed` is published as a gauge *and* is the source
//! [`resolve_if_speed`] falls back to when the 32-bit `ifSpeed` has saturated at 4294967295. A
//! stored interface value never fixes itself, so getting that wrong needs a migration as well as a
//! poller change (`extensibility.md` §6).

use super::*;

/// Execute an SNMP table-walk check (v2c or v3, selected by `walker`): numeric columns become
/// per-interface samples (keyed by ifIndex, using the column's explicit metric name and kind — no
/// OID-name guessing), and metadata columns become [`DiscoveredInterface`] records (PostgreSQL
/// inventory, never TSDB labels — ADR-011). Shared by [`execute_snmp_table`] (v2c) and
/// [`execute_snmp_v3_table`] (v3).
async fn execute_table_walk(
    job: &PollJob,
    transport: &dyn Transport,
    at_unix_ms: i64,
    columns: &[SnmpColumn],
    meta_columns: &[SnmpMetaColumn],
    timeout: Duration,
    walker: &SnmpWalker,
) -> PollResult {
    let by_base: HashMap<&str, &SnmpColumn> = columns.iter().map(|c| (c.oid.as_str(), c)).collect();
    // Interface-speed columns (ifSpeed) declared in meta_columns; ifHighSpeed is walked poller-side.
    let speed_oids: Vec<String> = meta_columns
        .iter()
        .filter(|m| matches!(m.field, InterfaceField::Speed))
        .map(|m| m.oid.clone())
        .collect();

    // ONE numeric walk for the metric columns AND the interface-speed columns (ifSpeed +
    // ifHighSpeed), demuxed by column base. A table poll previously opened a fresh SNMP session
    // (UDP socket + client) per walk — metric, ifSpeed, ifHighSpeed — holding the poll's global
    // permit ~3× longer and amplifying permit exhaustion during a mass outage (S5). Folding the
    // numeric walks into one leaves just this walk + the string-metadata walk below (2 sessions).
    let mut numeric_oids: Vec<String> = columns.iter().map(|c| c.oid.clone()).collect();
    numeric_oids.extend(speed_oids.iter().cloned());
    if !speed_oids.is_empty() {
        numeric_oids.push(OID_IF_HIGH_SPEED.to_owned());
        // The link's negotiated mode rides along for free (ADR-063 Inc.1): both are INTEGER-valued
        // and indexed by ifIndex alone, so they cost extra GETBULK sequences on a session this poll
        // was opening anyway — not a session, which is what S5 was about. Gated on the same
        // condition as ifHighSpeed because it marks "this job is gathering interface metadata".
        //
        // ⚠️ They are appended here rather than declared as `InterfaceField` variants on purpose:
        // a new variant would make every N-1 poller drop the entire SnmpTable spec. The reasoning
        // is on `yagra_common::link_mode`, next to the constants.
        numeric_oids.push(OID_DOT3_DUPLEX_STATUS.to_owned());
        // Huawei YunShan implements neither EtherLike-MIB nor MAU-MIB, so both of ADR-063's
        // existing paths are dead there and the duplex column was permanently blank. This is
        // one more column on a walk already being issued, and it returns no rows on the
        // devices that answer the standard one. ⚠️ Its enumeration is NOT the standard one —
        // see `duplex_from_huawei`, which is why the fold below cannot share a mapper.
        numeric_oids.push(OID_HW_ETHERNET_DUPLEX.to_owned());
        // Whether the port is metal or optical (ADR-063 Inc.4), two columns from the duplex one in
        // the same Huawei table. The standard answer — `ifMauType` — is walked by the hourly media
        // job and is dead on this platform (`1.3.6.1.2.1.26` answers No Such Object), and the other
        // implemented source only ever names a pluggable, so a fixed RJ45 port had no source at all.
        numeric_oids.push(OID_HW_ETHERNET_PORT_TYPE.to_owned());
        numeric_oids.push(OID_IF_TYPE.to_owned());
    }

    let mut samples = Vec::new();
    let mut raw = RawInterfaceNumerics::default();
    match walker
        .walk(transport, job.target, &numeric_oids, timeout)
        .await
    {
        Ok(rows) => {
            for row in rows {
                // 🚨 `ifHighSpeed` is TWO things at once and must feed both.
                //
                // It is a declared metric column in the built-in interface template
                // (`if_high_speed`, a gauge) *and* the 64-bit source `resolve_if_speed` needs. It
                // used to sit in the `else if` chain below, where the metric arm matched first and
                // this insert was **unreachable on every node whose profile carries the standard
                // interface template** — i.e. every SNMP node. The visible effect was a permanently
                // empty speed column wherever `ifSpeed` could not answer: a device that reports only
                // ifXTable (measured: 19 of 21 lab devices) got nothing at all, and a real 10G+ port
                // whose 32-bit `ifSpeed` saturates got nothing either, because decision 7 refuses the
                // sentinel and then had no fallback left to reach.
                //
                // Hoisted out of the chain rather than reordered: the metric sample is still owed,
                // so this is an `and`, not an `or`. `the_high_speed_column_is_both_a_metric_and_the_
                // speed_source` pins the overlap so a catalog edit cannot quietly make this dead code.
                if row.oid_base == OID_IF_HIGH_SPEED {
                    raw.high.insert(row.ifindex, row.value);
                }
                if let Some(col) = by_base.get(row.oid_base.as_str()) {
                    samples.push(Sample::interface(
                        col.metric_name.clone(),
                        IfIndex(row.ifindex),
                        row.value,
                        col.kind,
                    ));
                } else if row.oid_base == OID_IF_HIGH_SPEED {
                    // Already captured above; kept as an explicit no-op arm so the chain still
                    // enumerates every OID this walk appends and nothing falls through to
                    // `speed_oids` by accident.
                } else if row.oid_base == OID_DOT3_DUPLEX_STATUS {
                    raw.duplex.insert(row.ifindex, row.value);
                } else if row.oid_base == OID_HW_ETHERNET_DUPLEX {
                    raw.hw_duplex.insert(row.ifindex, row.value);
                } else if row.oid_base == OID_HW_ETHERNET_PORT_TYPE {
                    raw.hw_port_type.insert(row.ifindex, row.value);
                } else if row.oid_base == OID_IF_TYPE {
                    raw.if_type.insert(row.ifindex, row.value);
                } else if speed_oids.iter().any(|o| o == &row.oid_base) {
                    raw.speed.insert(row.ifindex, row.value);
                }
            }
        }
        Err(err) => tracing::warn!(job_id = %job.job_id, error = %err, "snmp table walk failed"),
    }

    let interfaces =
        walk_interface_metadata(job, transport, walker, meta_columns, &raw, timeout).await;

    // Reachable iff the agent returned at least one value (matches the scalar SNMP arm).
    let outcome = if samples.is_empty() {
        CheckOutcome::Unreachable
    } else {
        CheckOutcome::Reachable
    };

    PollResult {
        job_id: job.job_id,
        node_id: job.node_id,
        at_unix_ms,
        outcome,
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
        // Stamped by `run_stream` from the poll span before publish (empty here = no trace).
        trace_context: Default::default(),
    }
}

/// Execute an SNMP v2c table-walk check — a thin wrapper over [`execute_table_walk`].
pub(super) async fn execute_snmp_table(
    job: &PollJob,
    transport: &dyn Transport,
    at_unix_ms: i64,
    table: &SnmpTableCheck,
    timeout: Duration,
) -> PollResult {
    let walker = SnmpWalker::V2c(table.community.clone());
    execute_table_walk(
        job,
        transport,
        at_unix_ms,
        &table.columns,
        &table.meta_columns,
        timeout,
        &walker,
    )
    .await
}

/// Execute an SNMP v3 (USM) table-walk check — the v3 analogue of [`execute_snmp_table`]. Maps the
/// USM params exactly as the scalar v3 arm does, then shares the walk/mapping logic.
pub(super) async fn execute_snmp_v3_table(
    job: &PollJob,
    transport: &dyn Transport,
    at_unix_ms: i64,
    table: &SnmpV3TableCheck,
    timeout: Duration,
) -> PollResult {
    let walker = SnmpWalker::V3(table.auth.clone());
    execute_table_walk(
        job,
        transport,
        at_unix_ms,
        &table.columns,
        &table.meta_columns,
        timeout,
        &walker,
    )
    .await
}

/// The per-ifIndex numeric readings the caller's single combined walk demuxed out, for the metadata
/// fold below.
///
/// A struct rather than four `&HashMap` parameters: clippy called it at nine arguments, and it was
/// right for the usual reason — four maps of the same type in a row are four chances to pass duplex
/// where ifType belongs, with nothing to catch it. Every field is raw as the agent reported it;
/// interpretation (`resolve_if_speed`, `duplex_from_dot3`, `if_type_from_snmp`) happens in the fold.
#[derive(Debug, Default)]
struct RawInterfaceNumerics {
    /// `ifSpeed` (32-bit, bits/sec) — saturates at ~4.29 Gbps, hence `high`.
    speed: HashMap<u32, f64>,
    /// `ifHighSpeed` (units of 1,000,000 bits/sec).
    high: HashMap<u32, f64>,
    /// `dot3StatsDuplexStatus` (`unknown(1)` / `halfDuplex(2)` / `fullDuplex(3)`).
    duplex: HashMap<u32, f64>,
    /// `hwEthernetDuplex` (`full(1)` / `half(2)`) — the Huawei fallback. **Kept in its own map
    /// rather than merged into `duplex`**: the two columns disagree on what `1` means, so a
    /// merged map would need to remember which column each row came from anyway.
    hw_duplex: HashMap<u32, f64>,
    /// `hwEthernetPortType` (`other(1)` / `copper(2)` / `fiber(3)`) — the only medium source any
    /// device in this lab supplies. ⚠️ Its own map for the same reason `hw_duplex` has one: it
    /// overlaps the duplex enumeration on every value and agrees with it on none.
    hw_port_type: HashMap<u32, f64>,
    /// `ifType` (IANAifType).
    if_type: HashMap<u32, f64>,
}

/// Fold interface metadata into [`DiscoveredInterface`]s: walk the `ifName`/`ifAlias` **string**
/// columns (the poll's second and only other SNMP session), and resolve `if_speed` from the
/// `ifSpeed`/`ifHighSpeed` values already gathered by the combined numeric walk in the caller (S5).
async fn walk_interface_metadata(
    job: &PollJob,
    transport: &dyn Transport,
    walker: &SnmpWalker,
    meta_columns: &[SnmpMetaColumn],
    raw: &RawInterfaceNumerics,
    timeout: Duration,
) -> Vec<DiscoveredInterface> {
    let RawInterfaceNumerics {
        speed: raw_speed,
        high: raw_high,
        duplex: raw_duplex,
        hw_duplex: raw_hw_duplex,
        hw_port_type: raw_hw_port_type,
        if_type: raw_iftype,
    } = raw;
    let field_by_base: HashMap<&str, InterfaceField> = meta_columns
        .iter()
        .map(|m| (m.oid.as_str(), m.field))
        .collect();
    let string_oids: Vec<String> = meta_columns
        .iter()
        .filter(|m| matches!(m.field, InterfaceField::Name | InterfaceField::Alias))
        .map(|m| m.oid.clone())
        .collect();

    let mut ifs: BTreeMap<u32, DiscoveredInterface> = BTreeMap::new();
    let blank = |ifindex: u32| DiscoveredInterface {
        ifindex: IfIndex(ifindex),
        if_name: None,
        if_alias: None,
        if_speed: None,
        if_duplex: None,
        if_type: None,
        if_media: None,
        transceiver_model: None,
        // The metadata walk never reads thresholds — they come from the optical probe, and core's
        // upsert COALESCEs, so leaving them None here preserves whatever that probe stored.
        rx_power_low_dbm: None,
        rx_power_high_dbm: None,
        tx_power_low_dbm: None,
        tx_power_high_dbm: None,
    };

    if !string_oids.is_empty() {
        match walker
            .walk_strings(transport, job.target, &string_oids, timeout)
            .await
        {
            Ok(rows) => {
                for row in rows {
                    let Some(field) = field_by_base.get(row.oid_base.as_str()) else {
                        continue;
                    };
                    let rec = ifs.entry(row.ifindex).or_insert_with(|| blank(row.ifindex));
                    match field {
                        InterfaceField::Name => rec.if_name = Some(row.value),
                        InterfaceField::Alias => rec.if_alias = Some(row.value),
                        InterfaceField::Speed => {}
                    }
                }
            }
            Err(err) => {
                tracing::debug!(job_id = %job.job_id, error = %err, "snmp ifName/ifAlias walk failed");
            }
        }
    }

    // Resolve the effective bandwidth from the pre-walked 32-bit `ifSpeed` and 64-bit `ifHighSpeed`
    // (Mbps), so links above the ~4.29 Gbps `ifSpeed` cap report their true rate. ifHighSpeed is
    // gathered poller-side (not a bus column) to keep the job contract N/N-1 compatible.
    for ifindex in raw_speed
        .keys()
        .chain(raw_high.keys())
        .copied()
        .collect::<BTreeSet<u32>>()
    {
        match resolve_if_speed(
            raw_speed.get(&ifindex).copied(),
            raw_high.get(&ifindex).copied(),
        ) {
            Some(bps) => {
                let rec = ifs.entry(ifindex).or_insert_with(|| blank(ifindex));
                rec.if_speed = Some(bps);
            }
            None => tracing::debug!(
                job_id = %job.job_id,
                ifindex,
                "no resolvable interface speed (ifSpeed/ifHighSpeed absent or out of range)"
            ),
        }
    }

    // Duplex and ifType, from the same numeric walk (ADR-063 Inc.1). Folded over their own union of
    // ifindexes rather than the speed one: a device may answer EtherLike-MIB for a port whose
    // ifSpeed it does not report, and vice versa.
    for ifindex in raw_duplex
        .keys()
        .chain(raw_hw_duplex.keys())
        .chain(raw_iftype.keys())
        .copied()
        .collect::<BTreeSet<u32>>()
    {
        // EtherLike wins when present: it is the standard, and a device answering both should
        // not have its duplex decided by which vendor MIB the poller happened to read second.
        let duplex = raw_duplex
            .get(&ifindex)
            .copied()
            .and_then(duplex_from_dot3)
            .or_else(|| {
                raw_hw_duplex
                    .get(&ifindex)
                    .copied()
                    .and_then(duplex_from_huawei)
            });
        let if_type = raw_iftype
            .get(&ifindex)
            .copied()
            .and_then(if_type_from_snmp);
        if duplex.is_none() && if_type.is_none() {
            // Nothing usable — do not materialise a row for an interface the metric walk never
            // saw either, or a device answering `unknown(1)` for every port would inflate the
            // inventory with index-only records.
            continue;
        }
        let rec = ifs.entry(ifindex).or_insert_with(|| blank(ifindex));
        rec.if_duplex = duplex;
        rec.if_type = if_type;
    }

    // Media, for the devices that name the medium (ADR-063 Inc.4). Last of the three folds because
    // a designation is medium × speed and neither half answers alone — this reads the `if_speed` the
    // first fold resolved.
    //
    // Only interfaces the walk already produced a record for are touched (`get_mut`, never
    // `or_insert`): a port with a medium but no speed has nothing to say, and materialising an
    // index-only row for it would put a blank line in the inventory.
    for (ifindex, reading) in raw_hw_port_type {
        // ⚠️ Fibre is read and deliberately dropped — `copper_designation` carries the reason
        // (two writers, one COALESCEd column, and only copper's designation is unique per speed).
        if !matches!(medium_from_huawei(*reading), Some(Medium::Copper)) {
            continue;
        }
        let Some(rec) = ifs.get_mut(ifindex) else {
            continue;
        };
        let Some(bps) = rec.if_speed else {
            continue;
        };
        if let Some(designation) = copper_designation(bps) {
            rec.if_media = Some(designation.to_owned());
        }
    }

    ifs.into_values().collect()
}

/// Resolve the effective interface bandwidth (bits/sec) from `ifSpeed` (32-bit) and `ifHighSpeed`
/// (units of 1,000,000 bits/sec).
///
/// Below the 32-bit `ifSpeed` saturation point (`u32::MAX`, ~4.29 Gbps) `ifSpeed` is authoritative
/// — it can express sub-Mbps links that `ifHighSpeed` rounds to 0. At/above the cap (or when
/// `ifSpeed` is missing/0) the 64-bit `ifHighSpeed` is used. Non-finite, negative, or
/// out-of-`i64`-range values are dropped rather than stored as a bogus saturated speed.
///
/// ⚠️ **A saturated `ifSpeed` with no usable `ifHighSpeed` resolves to `None`, not to the sentinel**
/// (ADR-063 decision 7). `4294967295` is the value the gauge reports when the real rate exceeds what
/// it can express — it is a "too big to say" marker, not a measurement. This used to fall through to
/// it, and the lab's down 10G ports are stored that way today: harmless while the only reader was
/// the chart's bandwidth line, wrong the moment a speed column renders it as "4.29 Gbps". The same
/// value is also `in_util_pct`'s denominator, so utilisation was being computed against a rate no
/// interface has.
fn resolve_if_speed(if_speed: Option<f64>, if_high_speed: Option<f64>) -> Option<i64> {
    let sane = |v: f64| v.is_finite() && (0.0..=i64::MAX as f64).contains(&v);
    let speed = if_speed.filter(|v| sane(*v));
    let high_bps = if_high_speed
        .filter(|v| v.is_finite() && *v > 0.0)
        .map(|mbps| mbps * 1_000_000.0)
        .filter(|bps| sane(*bps));

    // 4_294_967_295: the value a 32-bit `ifSpeed` reports once the real rate exceeds it.
    const IF_SPEED_CAP: f64 = u32::MAX as f64;

    match speed {
        Some(s) if s > 0.0 && s < IF_SPEED_CAP => Some(s as i64),
        // Saturated or absent: only ifHighSpeed can answer. Falling back to `speed` here would
        // store the sentinel itself — see the ⚠️ on this function.
        _ => high_bps.map(|bps| bps as i64),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};
    use uuid::Uuid;
    use yagra_common::{NodeId, SnmpV3Auth};
    use yagra_transport::FakeTransport;

    fn snmp_table_job() -> PollJob {
        use yagra_bus::{SnmpColumn, SnmpMetaColumn, SnmpTableCheck};
        use yagra_common::{InterfaceField, MetricKind};
        PollJob::snmp_table(
            Uuid::nil(),
            NodeId::from(Uuid::nil()),
            IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
            SnmpTableCheck {
                community: "public".to_owned(),
                columns: vec![
                    SnmpColumn {
                        metric_name: "if_hc_in_octets".to_owned(),
                        oid: "1.3.6.1.2.1.31.1.1.1.6".to_owned(),
                        kind: MetricKind::Counter,
                    },
                    SnmpColumn {
                        metric_name: "if_oper_status".to_owned(),
                        oid: "1.3.6.1.2.1.2.2.1.8".to_owned(),
                        kind: MetricKind::Gauge,
                    },
                ],
                meta_columns: vec![
                    SnmpMetaColumn {
                        field: InterfaceField::Name,
                        oid: "1.3.6.1.2.1.31.1.1.1.1".to_owned(),
                    },
                    SnmpMetaColumn {
                        field: InterfaceField::Speed,
                        oid: "1.3.6.1.2.1.2.2.1.5".to_owned(),
                    },
                ],
                timeout_ms: 2000,
            },
            60,
        )
    }

    /// A table job built from the **real built-in catalog**, not from a hand-written fixture.
    ///
    /// 🚨 The hand-written `snmp_table_job` declares two metric columns and neither is
    /// `if_high_speed`, which is precisely why nothing caught the demux bug it was supposed to
    /// cover: the fake was narrower than the thing it stood for, so every test agreed while every
    /// real node behaved differently. Anything asserting how metric columns and interface-metadata
    /// columns interact has to be built from the catalog that actually ships.
    fn catalog_table_job() -> PollJob {
        use yagra_bus::{SnmpColumn, SnmpMetaColumn, SnmpTableCheck};
        use yagra_common::{builtin_catalog, builtin_interface_meta_columns, CollectionKind};
        PollJob::snmp_table(
            Uuid::nil(),
            NodeId::from(Uuid::nil()),
            IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
            SnmpTableCheck {
                community: "public".to_owned(),
                columns: builtin_catalog()
                    .into_iter()
                    .filter(|i| i.kind == CollectionKind::Table)
                    .map(|i| SnmpColumn {
                        metric_name: i.metric_name,
                        oid: i.oid,
                        kind: i.metric_kind,
                    })
                    .collect(),
                meta_columns: builtin_interface_meta_columns()
                    .into_iter()
                    .map(|(field, oid)| SnmpMetaColumn {
                        field,
                        oid: oid.to_owned(),
                    })
                    .collect(),
                timeout_ms: 2000,
            },
            60,
        )
    }

    fn snmp_v3_table_job() -> PollJob {
        use yagra_bus::{SnmpColumn, SnmpMetaColumn, SnmpV3TableCheck};
        use yagra_common::{InterfaceField, MetricKind};
        PollJob::snmp_v3_table(
            Uuid::nil(),
            NodeId::from(Uuid::nil()),
            IpAddr::V4(Ipv4Addr::new(10, 0, 0, 3)),
            SnmpV3TableCheck {
                auth: SnmpV3Auth {
                    user: "monitor".to_owned(),
                    security_level: "authpriv".to_owned(),
                    auth_protocol: Some("sha256".to_owned()),
                    auth_key: Some("auth-pass".to_owned()),
                    priv_protocol: Some("aes256".to_owned()),
                    priv_key: Some("priv-pass".to_owned()),
                },
                columns: vec![SnmpColumn {
                    metric_name: "if_hc_in_octets".to_owned(),
                    oid: "1.3.6.1.2.1.31.1.1.1.6".to_owned(),
                    kind: MetricKind::Counter,
                }],
                meta_columns: vec![SnmpMetaColumn {
                    field: InterfaceField::Name,
                    oid: "1.3.6.1.2.1.31.1.1.1.1".to_owned(),
                }],
                timeout_ms: 2000,
            },
            60,
        )
    }

    #[tokio::test]
    async fn snmp_table_maps_columns_to_per_interface_samples() {
        use yagra_common::MetricKind;
        use yagra_transport::SnmpTableSample;
        let t = FakeTransport::reachable(0.0).with_snmp_table(vec![
            SnmpTableSample {
                oid_base: "1.3.6.1.2.1.31.1.1.1.6".to_owned(),
                ifindex: 1,
                value: 1000.0,
            },
            SnmpTableSample {
                oid_base: "1.3.6.1.2.1.31.1.1.1.6".to_owned(),
                ifindex: 2,
                value: 2000.0,
            },
            SnmpTableSample {
                oid_base: "1.3.6.1.2.1.2.2.1.8".to_owned(),
                ifindex: 1,
                value: 1.0,
            },
        ]);
        let r = execute(&snmp_table_job(), &t, 1_000).await;
        assert_eq!(r.outcome, CheckOutcome::Reachable);
        // The in-octets counter for ifIndex 2 is mapped by name, ifindex, and kind.
        let octets2 = r
            .samples
            .iter()
            .find(|s| s.metric == "if_hc_in_octets" && s.ifindex == Some(IfIndex(2)))
            .expect("if_hc_in_octets ifIndex 2 present");
        assert_eq!(octets2.value, 2000.0);
        assert_eq!(octets2.kind, MetricKind::Counter);
        // The oper-status gauge for ifIndex 1.
        assert!(r.samples.iter().any(|s| s.metric == "if_oper_status"
            && s.ifindex == Some(IfIndex(1))
            && s.kind == MetricKind::Gauge));
    }

    /// The overlap this walk has to survive, stated as its own assertion.
    ///
    /// `ifHighSpeed` is a metric the catalog charts **and** the only 64-bit source for the speed
    /// column. If a future edit drops it from the catalog, the hoisted capture in the demux becomes
    /// ordinary code rather than a deliberate exception — and the comment explaining why it is
    /// hoisted becomes a lie. Failing here is how that gets noticed.
    #[test]
    fn the_high_speed_column_is_both_a_metric_and_the_speed_source() {
        use yagra_common::{builtin_catalog, builtin_interface_meta_columns, InterfaceField};
        assert!(
            builtin_catalog().iter().any(|i| i.oid == OID_IF_HIGH_SPEED),
            "ifHighSpeed must still be a declared metric column, or the demux hoist is pointless",
        );
        // …and it must NOT be the declared meta column, which is the 32-bit ifSpeed. If these two
        // ever became the same OID, `resolve_if_speed` would read Mbps as bits/sec and store a
        // 1 Gbps link as 1000 bps — a wrong number, which is worse than the empty cell this fixes.
        let meta_speed = builtin_interface_meta_columns()
            .into_iter()
            .find(|(f, _)| matches!(f, InterfaceField::Speed))
            .map(|(_, oid)| oid)
            .expect("a Speed meta column exists");
        assert_ne!(meta_speed, OID_IF_HIGH_SPEED);
    }

    /// A device that answers **only** ifXTable still gets a speed.
    ///
    /// This is the shape 19 of the 21 lab devices actually have — `ifSpeed` absent, `ifHighSpeed`
    /// present — and the shape every 10G+ port has once its 32-bit gauge saturates. Before the
    /// demux hoist this stored `None` for all of them.
    #[tokio::test]
    async fn if_high_speed_feeds_the_speed_column_as_well_as_the_metric() {
        use yagra_transport::SnmpTableSample;
        let t = FakeTransport::reachable(0.0).with_snmp_table(vec![
            // ifHighSpeed only — no ifSpeed row at all, exactly as the lab captures answer.
            SnmpTableSample {
                oid_base: OID_IF_HIGH_SPEED.to_owned(),
                ifindex: 7,
                value: 10_000.0,
            },
            SnmpTableSample {
                oid_base: OID_IF_HIGH_SPEED.to_owned(),
                ifindex: 8,
                value: 1_000.0,
            },
        ]);
        let r = execute(&catalog_table_job(), &t, 1_000).await;

        let speed = |ifindex: u32| {
            r.interfaces
                .iter()
                .find(|i| i.ifindex == IfIndex(ifindex))
                .unwrap_or_else(|| panic!("ifIndex {ifindex} must have an interface row"))
                .if_speed
        };
        assert_eq!(speed(7), Some(10_000_000_000), "10 Gbps from ifHighSpeed");
        assert_eq!(speed(8), Some(1_000_000_000), "1 Gbps from ifHighSpeed");

        // …and the metric is still charted. The fix is an `and`: reordering the chain instead of
        // hoisting would have swapped one silent loss for another.
        for ifindex in [7u32, 8] {
            assert!(
                r.samples.iter().any(|s| s.metric == "if_high_speed"
                    && s.ifindex == Some(IfIndex(ifindex))
                    && s.kind == MetricKind::Gauge),
                "if_high_speed sample for ifIndex {ifindex} must still be emitted",
            );
        }
    }

    /// A saturated 32-bit `ifSpeed` resolves through `ifHighSpeed` when both arrive together.
    ///
    /// ADR-063 decision 7 refuses to store the `4294967295` sentinel. That refusal is only correct
    /// if the 64-bit column can still answer — otherwise it turns a wrong number into no number,
    /// which is what the lab's two real 10G ports had.
    #[tokio::test]
    async fn a_saturated_if_speed_still_resolves_through_the_high_speed_column() {
        use yagra_transport::SnmpTableSample;
        let t = FakeTransport::reachable(0.0).with_snmp_table(vec![
            SnmpTableSample {
                oid_base: "1.3.6.1.2.1.2.2.1.5".to_owned(),
                ifindex: 4,
                value: u32::MAX as f64,
            },
            SnmpTableSample {
                oid_base: OID_IF_HIGH_SPEED.to_owned(),
                ifindex: 4,
                value: 10_000.0,
            },
        ]);
        let r = execute(&catalog_table_job(), &t, 1_000).await;
        let iface = r
            .interfaces
            .iter()
            .find(|i| i.ifindex == IfIndex(4))
            .expect("ifIndex 4");
        assert_eq!(iface.if_speed, Some(10_000_000_000));
    }

    /// The lab's real Huawei USG, port for port (ADR-063 Inc.4).
    ///
    /// Its two live ports are metal and run at 100 Mbit/s and 1 Gbit/s; its two 10GE ports are
    /// optical. Before this increment every one of the sixteen media cells was empty, because the
    /// standard source (`ifMauType`) answers No Such Object on this platform and the other one only
    /// ever names a pluggable.
    #[tokio::test]
    async fn a_huawei_port_gets_its_media_from_the_medium_and_the_speed() {
        use yagra_transport::SnmpTableSample;
        let sample = |oid: &str, ifindex: u32, value: f64| SnmpTableSample {
            oid_base: oid.to_owned(),
            ifindex,
            value,
        };
        let t = FakeTransport::reachable(0.0).with_snmp_table(vec![
            // GE0/0/1 — copper at 100 Mbit/s.
            sample(OID_IF_HIGH_SPEED, 7, 100.0),
            sample(OID_HW_ETHERNET_PORT_TYPE, 7, 2.0),
            // GE0/0/2 — copper at 1 Gbit/s.
            sample(OID_IF_HIGH_SPEED, 8, 1_000.0),
            sample(OID_HW_ETHERNET_PORT_TYPE, 8, 2.0),
            // 10GE0/0/0 — optical. Read, and deliberately left without a designation.
            sample(OID_IF_HIGH_SPEED, 4, 10_000.0),
            sample(OID_HW_ETHERNET_PORT_TYPE, 4, 3.0),
            // A port the agent will not classify: other(1) is "not known", not a third medium.
            sample(OID_IF_HIGH_SPEED, 9, 1_000.0),
            sample(OID_HW_ETHERNET_PORT_TYPE, 9, 1.0),
        ]);
        let r = execute(&catalog_table_job(), &t, 1_000).await;
        let media = |ifindex: u32| {
            r.interfaces
                .iter()
                .find(|i| i.ifindex == IfIndex(ifindex))
                .unwrap_or_else(|| panic!("ifIndex {ifindex}"))
                .if_media
                .clone()
        };
        assert_eq!(media(7).as_deref(), Some("100BASE-TX"));
        assert_eq!(media(8).as_deref(), Some("1000BASE-T"));
        assert_eq!(
            media(4),
            None,
            "a fibre port must not be given a designation"
        );
        assert_eq!(media(9), None, "other(1) must not become a medium");
    }

    /// A medium with no speed, and a speed with no medium, both stay empty.
    ///
    /// Stated because the fold reads two maps and the failure would be silent either way: a
    /// designation invented from half the inputs is a wrong value in a column whose whole point is
    /// that it never guesses.
    #[tokio::test]
    async fn media_needs_both_halves_and_declines_when_it_has_one() {
        use yagra_transport::SnmpTableSample;
        let sample = |oid: &str, ifindex: u32, value: f64| SnmpTableSample {
            oid_base: oid.to_owned(),
            ifindex,
            value,
        };
        let t = FakeTransport::reachable(0.0).with_snmp_table(vec![
            // Copper, but the device never reported a speed for it.
            sample(OID_HW_ETHERNET_PORT_TYPE, 11, 2.0),
            // A speed, but no medium column at all — every non-Huawei device in the lab.
            sample(OID_IF_HIGH_SPEED, 12, 1_000.0),
            // Copper at a speed with no transcribed twisted-pair registration (2.5GBASE-T).
            sample(OID_HW_ETHERNET_PORT_TYPE, 13, 2.0),
            sample(OID_IF_HIGH_SPEED, 13, 2_500.0),
        ]);
        let r = execute(&catalog_table_job(), &t, 1_000).await;
        let iface = |ifindex: u32| r.interfaces.iter().find(|i| i.ifindex == IfIndex(ifindex));
        // No speed ⇒ no row is materialised for it at all, and certainly no media.
        assert!(iface(11).is_none_or(|i| i.if_media.is_none()));
        assert_eq!(iface(12).expect("ifIndex 12").if_media, None);
        assert_eq!(iface(13).expect("ifIndex 13").if_media, None);
        // …and the speed itself is still stored for the two that reported one.
        assert_eq!(iface(12).unwrap().if_speed, Some(1_000_000_000));
    }

    #[tokio::test]
    async fn snmp_table_metadata_folds_into_discovered_interfaces() {
        use yagra_transport::{SnmpTableSample, SnmpTableString};
        let t = FakeTransport::reachable(0.0)
            .with_snmp_table(vec![
                // One numeric sample so the poll counts as reachable.
                SnmpTableSample {
                    oid_base: "1.3.6.1.2.1.31.1.1.1.6".to_owned(),
                    ifindex: 1,
                    value: 10.0,
                },
                // ifSpeed (numeric meta column) for ifIndex 1.
                SnmpTableSample {
                    oid_base: "1.3.6.1.2.1.2.2.1.5".to_owned(),
                    ifindex: 1,
                    value: 1_000_000_000.0,
                },
            ])
            .with_snmp_table_strings(vec![SnmpTableString {
                oid_base: "1.3.6.1.2.1.31.1.1.1.1".to_owned(),
                ifindex: 1,
                value: "Gi0/1".to_owned(),
            }]);
        let r = execute(&snmp_table_job(), &t, 1_000).await;
        assert_eq!(r.interfaces.len(), 1);
        let iface = &r.interfaces[0];
        assert_eq!(iface.ifindex, IfIndex(1));
        assert_eq!(iface.if_name.as_deref(), Some("Gi0/1"));
        assert_eq!(iface.if_speed, Some(1_000_000_000));
        // ifSpeed must NOT have leaked into the TSDB samples (it's metadata, not a metric).
        assert!(!r.samples.iter().any(|s| s.metric == "1.3.6.1.2.1.2.2.1.5"));
    }

    #[tokio::test]
    async fn snmp_v3_table_walks_columns_and_metadata_like_v2c() {
        use yagra_common::MetricKind;
        use yagra_transport::{SnmpTableSample, SnmpTableString};
        // The v3 table path drives the same walk/fold logic as v2c (shared `execute_table_walk`);
        // the fake returns its canned rows for the v3 walk too. This proves a v3 node now collects
        // per-interface metrics + interface metadata instead of being silently limited to scalars.
        let t = FakeTransport::reachable(0.0)
            .with_snmp_table(vec![SnmpTableSample {
                oid_base: "1.3.6.1.2.1.31.1.1.1.6".to_owned(),
                ifindex: 7,
                value: 4242.0,
            }])
            .with_snmp_table_strings(vec![SnmpTableString {
                oid_base: "1.3.6.1.2.1.31.1.1.1.1".to_owned(),
                ifindex: 7,
                value: "Gi0/7".to_owned(),
            }]);
        let r = execute(&snmp_v3_table_job(), &t, 1_000).await;
        assert_eq!(r.outcome, CheckOutcome::Reachable);
        let octets = r
            .samples
            .iter()
            .find(|s| s.metric == "if_hc_in_octets" && s.ifindex == Some(IfIndex(7)))
            .expect("v3 table produced the per-interface counter");
        assert_eq!(octets.value, 4242.0);
        assert_eq!(octets.kind, MetricKind::Counter);
        // Interface metadata is folded from the v3 string walk (PostgreSQL inventory, ADR-011).
        assert_eq!(r.interfaces.len(), 1);
        assert_eq!(r.interfaces[0].ifindex, IfIndex(7));
        assert_eq!(r.interfaces[0].if_name.as_deref(), Some("Gi0/7"));
    }

    #[tokio::test]
    async fn snmp_table_ignores_out_of_range_if_speed() {
        use yagra_transport::{SnmpTableSample, SnmpTableString};
        let t = FakeTransport::reachable(0.0)
            .with_snmp_table(vec![
                // One numeric metric sample so the poll counts as reachable.
                SnmpTableSample {
                    oid_base: "1.3.6.1.2.1.31.1.1.1.6".to_owned(),
                    ifindex: 1,
                    value: 10.0,
                },
                // A non-finite ifSpeed must be dropped, not silently saturated to i64::MAX.
                SnmpTableSample {
                    oid_base: "1.3.6.1.2.1.2.2.1.5".to_owned(),
                    ifindex: 1,
                    value: f64::INFINITY,
                },
            ])
            .with_snmp_table_strings(vec![SnmpTableString {
                oid_base: "1.3.6.1.2.1.31.1.1.1.1".to_owned(),
                ifindex: 1,
                value: "Gi0/1".to_owned(),
            }]);
        let r = execute(&snmp_table_job(), &t, 1_000).await;
        assert_eq!(r.interfaces.len(), 1);
        let iface = &r.interfaces[0];
        assert_eq!(iface.if_name.as_deref(), Some("Gi0/1"));
        assert_eq!(
            iface.if_speed, None,
            "out-of-range ifSpeed must be dropped rather than saturated"
        );
    }

    #[tokio::test]
    async fn snmp_table_no_values_is_unreachable() {
        let t = FakeTransport::reachable(0.0); // no canned table rows
        let r = execute(&snmp_table_job(), &t, 1_000).await;
        assert_eq!(r.outcome, CheckOutcome::Unreachable);
        assert!(r.samples.is_empty());
        assert!(r.interfaces.is_empty());
    }

    #[test]
    fn resolve_if_speed_prefers_ifspeed_below_cap() {
        // A 1 Gbps link: ifSpeed is exact and below the 32-bit cap, so it wins.
        assert_eq!(
            resolve_if_speed(Some(1_000_000_000.0), Some(1000.0)),
            Some(1_000_000_000)
        );
    }

    #[test]
    fn resolve_if_speed_uses_high_speed_when_saturated() {
        // 10 Gbps: ifSpeed saturates at u32::MAX, ifHighSpeed (10000 Mbps) gives the true rate.
        assert_eq!(
            resolve_if_speed(Some(u32::MAX as f64), Some(10_000.0)),
            Some(10_000_000_000)
        );
        // 100 Gbps with ifSpeed absent entirely → ifHighSpeed (100000 Mbps).
        assert_eq!(
            resolve_if_speed(None, Some(100_000.0)),
            Some(100_000_000_000)
        );
    }

    #[test]
    fn resolve_if_speed_keeps_sub_mbps_precision() {
        // A 64 kbps link: ifHighSpeed rounds to 0, so the exact ifSpeed must be kept.
        assert_eq!(resolve_if_speed(Some(64_000.0), Some(0.0)), Some(64_000));
    }

    #[test]
    fn resolve_if_speed_drops_invalid_and_handles_absence() {
        assert_eq!(resolve_if_speed(None, None), None);
        // Non-finite / negative ifSpeed is dropped; falls back to ifHighSpeed when present.
        assert_eq!(resolve_if_speed(Some(f64::INFINITY), None), None);
        assert_eq!(resolve_if_speed(Some(-1.0), None), None);
        assert_eq!(
            resolve_if_speed(Some(f64::NAN), Some(40_000.0)),
            Some(40_000_000_000)
        );
    }

    /// A saturated `ifSpeed` with no usable `ifHighSpeed` is **not** a speed (ADR-063 decision 7).
    ///
    /// This assertion is inverted from what it used to be, and the reversal is the point: the old
    /// behaviour stored `u32::MAX` itself as a "best-effort" rate. `4294967295` is the gauge's way
    /// of saying *"the real rate is larger than I can express"* — keeping it means a 10 Gbps port
    /// is recorded as 4.29 Gbps, and `in_util_pct` is then computed against a rate no interface
    /// has. It was invisible while the only reader was the throughput chart's bandwidth line on a
    /// hand-selected (therefore up) interface; a speed column renders it for every port.
    ///
    /// The lab's down 10G ports store the sentinel today, so this is a live wrong value, not a
    /// hypothetical one.
    #[test]
    fn a_saturated_if_speed_with_no_high_speed_is_unknown_not_the_sentinel() {
        assert_eq!(resolve_if_speed(Some(u32::MAX as f64), None), None);
        // Same when the device answers ifHighSpeed but with the "no idea" zero, which is what a
        // down port typically reports — measured on the lab's 10GE0/0/0.
        assert_eq!(resolve_if_speed(Some(u32::MAX as f64), Some(0.0)), None);
        // ⚠️ But one bit below the cap is a real 4.29 Gbps reading and must survive.
        assert_eq!(
            resolve_if_speed(Some(u32::MAX as f64 - 1.0), None),
            Some(u32::MAX as i64 - 1)
        );
    }

    /// ifHighSpeed (walked poller-side, not a bus column) overrides a saturated ifSpeed so a
    /// 10 Gbps interface stores its true rate, not the 32-bit cap.
    #[tokio::test]
    async fn snmp_table_high_speed_overrides_saturated_if_speed() {
        use yagra_transport::SnmpTableSample;
        let t = FakeTransport::reachable(0.0).with_snmp_table(vec![
            // One numeric metric sample so the poll counts as reachable.
            SnmpTableSample {
                oid_base: "1.3.6.1.2.1.31.1.1.1.6".to_owned(),
                ifindex: 1,
                value: 10.0,
            },
            // ifSpeed saturated at the 32-bit cap.
            SnmpTableSample {
                oid_base: "1.3.6.1.2.1.2.2.1.5".to_owned(),
                ifindex: 1,
                value: u32::MAX as f64,
            },
            // ifHighSpeed = 10000 Mbps (walked from OID_IF_HIGH_SPEED).
            SnmpTableSample {
                oid_base: OID_IF_HIGH_SPEED.to_owned(),
                ifindex: 1,
                value: 10_000.0,
            },
        ]);
        let r = execute(&snmp_table_job(), &t, 1_000).await;
        let iface = r
            .interfaces
            .iter()
            .find(|i| i.ifindex == IfIndex(1))
            .expect("ifIndex 1 discovered");
        assert_eq!(iface.if_speed, Some(10_000_000_000));
    }

    /// Duplex and ifType ride the same numeric walk and land on the right ifIndex (ADR-063 Inc.1).
    ///
    /// The accepting case is the load-bearing one: everything about this feature — the OIDs being
    /// appended, the demux arms, the fold — fails *silently* into "column always empty", which is
    /// indistinguishable from a device that does not implement EtherLike-MIB. A test that only
    /// checked the rejecting cases would pass against a poller that walks neither OID.
    #[tokio::test]
    async fn snmp_table_walk_carries_duplex_and_if_type() {
        use yagra_transport::SnmpTableSample;
        let t = FakeTransport::reachable(0.0).with_snmp_table(vec![
            // Two interfaces' worth of a metric column, so the poll is reachable and so the
            // per-ifIndex demux has something to get wrong.
            SnmpTableSample {
                oid_base: "1.3.6.1.2.1.31.1.1.1.6".to_owned(),
                ifindex: 1,
                value: 10.0,
            },
            SnmpTableSample {
                oid_base: "1.3.6.1.2.1.31.1.1.1.6".to_owned(),
                ifindex: 2,
                value: 20.0,
            },
            // ifIndex 1: a copper port, full duplex, ethernetCsmacd.
            SnmpTableSample {
                oid_base: OID_DOT3_DUPLEX_STATUS.to_owned(),
                ifindex: 1,
                value: 3.0,
            },
            SnmpTableSample {
                oid_base: OID_IF_TYPE.to_owned(),
                ifindex: 1,
                value: 6.0,
            },
            // ifIndex 2: a loopback answering `unknown(1)` — the shape that must NOT become a
            // stored duplex. `if_type` still lands, and it is what lets a reader say "does not
            // apply" rather than "could not read".
            SnmpTableSample {
                oid_base: OID_DOT3_DUPLEX_STATUS.to_owned(),
                ifindex: 2,
                value: 1.0,
            },
            SnmpTableSample {
                oid_base: OID_IF_TYPE.to_owned(),
                ifindex: 2,
                value: 24.0,
            },
        ]);
        let r = execute(&snmp_table_job(), &t, 1_000).await;
        let find = |ix: u32| {
            r.interfaces
                .iter()
                .find(|i| i.ifindex == IfIndex(ix))
                .unwrap_or_else(|| panic!("ifIndex {ix} discovered"))
        };

        let copper = find(1);
        assert_eq!(
            copper.if_duplex,
            Some(yagra_common::Duplex::Full),
            "full duplex on ifIdx 1"
        );
        assert_eq!(copper.if_type, Some(6));

        let loopback = find(2);
        assert_eq!(
            loopback.if_duplex, None,
            "`unknown(1)` must store as unknown, not as a duplex"
        );
        assert_eq!(loopback.if_type, Some(24));
    }

    /// A device with no EtherLike-MIB still reports duplex, via Huawei's column (ADR-063 Inc.3).
    ///
    /// 🚨 The assertion on ifIndex 1 is the one that matters: `hwEthernetDuplex` says `full(1)`,
    /// and the standard mapper reads `1` as `unknown`. If the Huawei rows were ever fed through
    /// `duplex_from_dot3` — the obvious "reuse" — this would be `None`, which is byte-identical to
    /// the behaviour before this feature existed. **The bug would look like the feature simply not
    /// working**, on a device nobody can compare against, so it has to be pinned here.
    #[tokio::test]
    async fn a_device_without_etherlike_mib_gets_duplex_from_the_huawei_column() {
        use yagra_transport::SnmpTableSample;
        let metric = |ifindex: u32| SnmpTableSample {
            oid_base: "1.3.6.1.2.1.31.1.1.1.6".to_owned(),
            ifindex,
            value: 10.0,
        };
        let hw = |ifindex: u32, value: f64| SnmpTableSample {
            oid_base: OID_HW_ETHERNET_DUPLEX.to_owned(),
            ifindex,
            value,
        };
        let t = FakeTransport::reachable(0.0).with_snmp_table(vec![
            metric(1),
            metric(2),
            metric(3),
            metric(4),
            // ifIndex 1: the lab USG's shape — Huawei column only, `full(1)` on every port.
            hw(1, 1.0),
            hw(2, 2.0),
            // ifIndex 3: both columns present and *disagreeing*. EtherLike is the standard and
            // must win, so the answer is Half even though the Huawei column says full.
            SnmpTableSample {
                oid_base: OID_DOT3_DUPLEX_STATUS.to_owned(),
                ifindex: 3,
                value: 2.0,
            },
            hw(3, 1.0),
            // ifIndex 4: `3` is a value the Huawei enumeration does not define. It must not be
            // read as `fullDuplex(3)` from the other MIB.
            hw(4, 3.0),
        ]);
        let r = execute(&snmp_table_job(), &t, 1_000).await;
        let row = |ix: u32| r.interfaces.iter().find(|i| i.ifindex == IfIndex(ix));
        let duplex = |ix: u32| {
            row(ix)
                .unwrap_or_else(|| panic!("ifIndex {ix} discovered"))
                .if_duplex
        };

        assert_eq!(
            duplex(1),
            Some(yagra_common::Duplex::Full),
            "hwEthernetDuplex full(1) — reusing the dot3 mapper here would silently give None"
        );
        assert_eq!(duplex(2), Some(yagra_common::Duplex::Half));
        assert_eq!(
            duplex(3),
            Some(yagra_common::Duplex::Half),
            "dot3StatsDuplexStatus wins when a device answers both"
        );
        // 3 is not a value in Huawei's enumeration. It must not borrow `fullDuplex(3)` from the
        // standard one — and because that leaves the row with nothing usable, the fold declines to
        // materialise the interface at all rather than adding an index-only record (the same guard
        // that stops a device answering `unknown(1)` everywhere from inflating the inventory).
        assert!(
            row(4).is_none(),
            "an unmappable duplex reading must not conjure an interface row"
        );
    }

    /// A device that implements neither OID still gets its names and speed — the ADR-063 columns
    /// are additive, and a poller that walks two OIDs the agent ignores must not lose the rest.
    #[tokio::test]
    async fn a_device_without_etherlike_mib_still_reports_its_interfaces() {
        use yagra_transport::SnmpTableSample;
        let t = FakeTransport::reachable(0.0)
            .with_snmp_table(vec![SnmpTableSample {
                oid_base: "1.3.6.1.2.1.31.1.1.1.6".to_owned(),
                ifindex: 1,
                value: 10.0,
            }])
            .with_snmp_table_strings(vec![SnmpTableString {
                oid_base: "1.3.6.1.2.1.31.1.1.1.1".to_owned(),
                ifindex: 1,
                value: "GE0/0/1".to_owned(),
            }]);
        let r = execute(&snmp_table_job(), &t, 1_000).await;
        let iface = r
            .interfaces
            .iter()
            .find(|i| i.ifindex == IfIndex(1))
            .expect("ifIndex 1 discovered");
        assert_eq!(iface.if_name.as_deref(), Some("GE0/0/1"));
        assert_eq!(iface.if_duplex, None);
        assert_eq!(iface.if_type, None);
    }
}
