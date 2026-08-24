// SPDX-License-Identifier: AGPL-3.0-only
//! The physical layer: optical transceiver readings and interface media (ADR-062, ADR-063).
//!
//! Both walks ask an agent about the *parts* rather than the interfaces, so both end up indexed by
//! `entPhysicalIndex` and both need [`walk_entity_index`] and the ENTITY-MIB columns beside it to
//! translate back to ifIndex. That shared dependency is why they are one file: splitting them would
//! put a boundary through the translation they both exist on the far side of.
//!
//! ⚠️ **Everything worth testing is in [`crate::optical`] and [`crate::mau`], and deliberately so.**
//! Those modules are pure — already-walked rows in, a reading out — because the scale factors, the
//! transmit/receive split and the index translation are where a mistake draws a smooth, believable,
//! wrong line. What is left here is the SNMP session: which columns to walk, in what order, and what
//! to do when a dialect answers nothing.

use super::*;

/// Row budget for one ENTITY-MIB walk.
///
/// `entPhysicalTable` is one row per part — a fully populated chassis switch runs to a few
/// thousand — and the alias and containment columns are walked once per poll. The bound is what
/// stops a pathological agent from turning an optical probe into an unbounded read.
const OPTICAL_ENTITY_MAX_ROWS: usize = 8192;

/// Execute an optical-transceiver (DDM/DOM) probe — ADR-062.
///
/// Two shapes, chosen per dialect by [`optical::simple_dialect`]:
///
/// - **Plain columns** (Huawei / Juniper / H3C): one numeric walk of two columns and a fixed
///   multiplier. Juniper and H3C key their rows by ifIndex already; Huawei keys them by
///   `entPhysicalIndex` and needs the same translation as the dialect below.
/// - **ENTITY-SENSOR-MIB** (Cisco / Arista and anything else standards-based): four numeric
///   columns correlated per sensor, plus the entity's free text to say which direction it is.
///
/// The result is **observational**. A device whose transceivers will not answer — because it has
/// none, because the MIB is unimplemented, or because the view excludes it — is not unreachable,
/// and reporting it as such would page someone about a healthy box. Everything else here is best
/// effort by design: an unreadable dialect logs and contributes nothing.
pub(super) async fn execute_optical(
    job: &PollJob,
    transport: &dyn Transport,
    at_unix_ms: i64,
    probes: &[yagra_bus::OpticalProbe],
    timeout: Duration,
    walker: &SnmpWalker,
) -> PollResult {
    let mut samples = Vec::new();
    // Keyed by the *resolved* ifIndex, so an interface seen through two dialects merges into one
    // row rather than giving core two to upsert in an arbitrary order.
    let mut windows: BTreeMap<u32, optical::OpticalWindow> = BTreeMap::new();
    // Built lazily and at most once per poll: both dialects that need it walk the same two
    // ENTITY-MIB columns, and a node bound to two vendor profiles must not walk them twice.
    let mut entity: Option<optical::EntityIndex> = None;

    for probe in probes {
        if probe.rx_metric.is_none() && probe.tx_metric.is_none() && probe.temp_metric.is_none() {
            continue;
        }
        let (readings, raw_windows, temps) = match optical::simple_dialect(probe.flavor) {
            Some(dialect) => {
                let (r, w) = walk_simple_optical(job, transport, walker, timeout, &dialect).await;
                (r, w, Vec::new())
            }
            // The correlated dialects have no threshold objects at all — RFC 3433 defines none,
            // and Cisco's live in a separate table with a different row shape. So they draw their
            // two lines and no band, which is a stated degradation rather than a missing case.
            None => match optical::sensor_dialect(probe.flavor) {
                Some(dialect) => {
                    let (r, t) = walk_entity_sensor_optical(
                        job,
                        transport,
                        walker,
                        timeout,
                        &dialect,
                        probe.temp_metric.is_some(),
                    )
                    .await;
                    (r, HashMap::new(), t)
                }
                // Unreachable today — every flavour is served by one of the two tables, and
                // `every_flavor_is_served_by_exactly_one_dialect_kind` pins that. Skipping rather
                // than panicking is what an older poller meeting a newer dialect must do anyway.
                None => continue,
            },
        };
        if readings.is_empty() && temps.is_empty() {
            continue;
        }

        // Translate entPhysicalIndex → ifIndex for the dialects that need it. A row that does not
        // translate is DROPPED: emitting it under its raw entity index would land the series on
        // `MetricDimension::Entity`, which costs storage and appears on no chart (decision 3).
        // The thresholds travel through exactly the same translation — they are keyed by the same
        // row, so a window that survives while its reading did not would be a band with no line.
        // Both the readings and the temperatures are keyed by entPhysicalIndex and need the same
        // index — the readings to *find* their port, the temperatures to prove they have none.
        if !probe.flavor.is_ifindex_keyed() && entity.is_none() {
            entity = Some(walk_entity_index(job, transport, walker, timeout).await);
        }

        // Chassis temperature (ADR-070 decision 2). The rows kept here are the ones that reach no
        // interface: an SFP's own sensors climb ENTITY-MIB to their port (measured 42 of 42), a
        // chassis sensor dead-ends. That is the whole of the SFP/chassis split — no free-text rule
        // is added, and ADR-062 Issue #66's exclusion of a module's own temperature holds by
        // construction rather than by a list of strings.
        if let (Some(metric), Some(idx)) = (probe.temp_metric.as_ref(), entity.as_ref()) {
            let mut kept = 0usize;
            for (ent, celsius) in &temps {
                if idx.ifindex_for(*ent).is_some() {
                    continue;
                }
                kept += 1;
                samples.push(Sample::interface(
                    metric.clone(),
                    IfIndex(*ent),
                    *celsius,
                    MetricKind::Gauge,
                ));
            }
            if !temps.is_empty() {
                tracing::debug!(
                    job_id = %job.job_id,
                    flavor = ?probe.flavor,
                    kept,
                    port_attached = temps.len() - kept,
                    "chassis temperature sensors",
                );
            }
        }

        let (resolved, resolved_windows) = if probe.flavor.is_ifindex_keyed() {
            (readings, raw_windows)
        } else {
            let idx = entity
                .as_ref()
                .expect("built above for a non-ifindex-keyed dialect");
            let before = readings.len();
            let mapped: Vec<optical::OpticalSample> = readings
                .into_iter()
                .filter_map(|s| {
                    idx.ifindex_for(s.ifindex)
                        .map(|ifindex| optical::OpticalSample { ifindex, ..s })
                })
                .collect();
            if mapped.len() < before {
                tracing::debug!(
                    job_id = %job.job_id,
                    flavor = ?probe.flavor,
                    dropped = before - mapped.len(),
                    "optical rows dropped: no interface maps to their physical entity"
                );
            }
            let mapped_windows = raw_windows
                .into_iter()
                .filter_map(|(ent, w)| idx.ifindex_for(ent).map(|ifindex| (ifindex, w)))
                .collect();
            (mapped, mapped_windows)
        };
        windows.extend(resolved_windows);

        for s in optical::dedupe_readings(resolved) {
            let metric = match s.reading {
                optical::OpticalReading::Rx => probe.rx_metric.as_ref(),
                optical::OpticalReading::Tx => probe.tx_metric.as_ref(),
            };
            if let Some(name) = metric {
                samples.push(Sample::interface(
                    name.clone(),
                    IfIndex(s.ifindex),
                    s.dbm,
                    MetricKind::Gauge,
                ));
            }
        }
    }

    PollResult {
        job_id: job.job_id,
        node_id: job.node_id,
        at_unix_ms,
        // Never a liveness statement — see the doc comment.
        outcome: CheckOutcome::Reachable,
        samples,
        // ⚠️ These carry the thresholds and NOTHING else — no name, no alias, no speed, no link
        // mode. Core's interface upsert COALESCEs every column against its existing value, so the
        // `None`s preserve whatever the metadata walk stored rather than blanking it. That property
        // is what makes it safe to write the same row from two different probes, and it has a test.
        interfaces: windows
            .into_iter()
            .map(|(ifindex, w)| DiscoveredInterface {
                ifindex: IfIndex(ifindex),
                if_name: None,
                if_alias: None,
                if_speed: None,
                if_duplex: None,
                if_type: None,
                if_media: None,
                transceiver_model: None,
                rx_power_low_dbm: w.rx_low,
                rx_power_high_dbm: w.rx_high,
                tx_power_low_dbm: w.tx_low,
                tx_power_high_dbm: w.tx_high,
            })
            .collect(),
        sys_descr: None,
        dns_chain: None,
        neighbors: None,
        l3: None,
        arp: None,
        routing: None,
        observational: true,
        poller_id: None,
        trace_context: Default::default(),
    }
}

/// Walk a two-column optical dialect and scale it to dBm, together with the module's own
/// acceptable window when the dialect publishes one. Indices are whatever the dialect keys on;
/// the caller translates them if needed.
///
/// One walk for readings and thresholds together: they live in the same table, and the thresholds
/// are what turn a dBm figure into something an operator can act on, so splitting them would
/// double the SNMP sessions to draw one chart.
async fn walk_simple_optical(
    job: &PollJob,
    transport: &dyn Transport,
    walker: &SnmpWalker,
    timeout: Duration,
    dialect: &optical::SimpleDialect,
) -> (
    Vec<optical::OpticalSample>,
    HashMap<u32, optical::OpticalWindow>,
) {
    let mut columns = vec![dialect.rx_oid.to_owned(), dialect.tx_oid.to_owned()];
    if let Some(l) = dialect.limits {
        columns.extend([
            l.rx_low_oid.to_owned(),
            l.rx_high_oid.to_owned(),
            l.tx_low_oid.to_owned(),
            l.tx_high_oid.to_owned(),
        ]);
    }
    let rows = match walker.walk(transport, job.target, &columns, timeout).await {
        Ok(rows) => rows,
        Err(err) => {
            tracing::debug!(job_id = %job.job_id, error = %err, "optical walk failed");
            return (Vec::new(), HashMap::new());
        }
    };

    let mut samples = Vec::new();
    // Raw, unvalidated bounds per row key; validated in one pass below so a bad pair is refused
    // as a pair rather than half-kept.
    let mut raw: HashMap<u32, optical::OpticalWindow> = HashMap::new();
    for row in rows {
        let base = row.oid_base.as_str();
        // A port with no transceiver still answers, with the vendor's placeholder. Drop the row
        // here — before the scale turns the marker into an ordinary number — so a dark port is a
        // gap in the chart rather than a flat line at whatever the marker happens to scale to.
        // Applied to the bound columns too: they only escaped before because all four carrying
        // the same marker made `low == high`, which is an accident of the pair, not a guard.
        if dialect.no_module == Some(row.value) {
            continue;
        }
        let scaled = row.value * dialect.scale;
        if base == dialect.rx_oid || base == dialect.tx_oid {
            samples.push(optical::OpticalSample {
                ifindex: row.ifindex,
                reading: if base == dialect.rx_oid {
                    optical::OpticalReading::Rx
                } else {
                    optical::OpticalReading::Tx
                },
                dbm: scaled,
            });
            continue;
        }
        let Some(l) = dialect.limits else { continue };
        let w = raw.entry(row.ifindex).or_default();
        if base == l.rx_low_oid {
            w.rx_low = Some(scaled);
        } else if base == l.rx_high_oid {
            w.rx_high = Some(scaled);
        } else if base == l.tx_low_oid {
            w.tx_low = Some(scaled);
        } else if base == l.tx_high_oid {
            w.tx_high = Some(scaled);
        }
    }

    let windows = raw
        .into_iter()
        .filter_map(|(ifindex, w)| {
            let (rx_low, rx_high) = optical::validated_window(w.rx_low, w.rx_high);
            let (tx_low, tx_high) = optical::validated_window(w.tx_low, w.tx_high);
            let out = optical::OpticalWindow {
                rx_low,
                rx_high,
                tx_low,
                tx_high,
            };
            (!out.is_empty()).then_some((ifindex, out))
        })
        .collect();
    (samples, windows)
}

/// Walk a correlated sensor table and pull two different things out of one pass: the optical
/// readings, and — when asked — the chassis temperatures.
///
/// Four numeric columns in one session, then the entity text in a second — the same two-session
/// shape the interface walk uses, and for the same reason: the numeric and string walkers are
/// separate transports.
///
/// The `dialect` argument is the whole of ADR-070 decision 1 on this side: Cisco does not implement
/// RFC 3433, but it implements the identical table at its own root, so the columns move and nothing
/// else does.
///
/// Returns `(optical readings, (entPhysicalIndex, °C) candidates)`. The temperatures are
/// **candidates** because "is this a chassis sensor or an SFP's own?" is answered by whether the
/// entity resolves to an interface, and that index belongs to the caller.
async fn walk_entity_sensor_optical(
    job: &PollJob,
    transport: &dyn Transport,
    walker: &SnmpWalker,
    timeout: Duration,
    dialect: &optical::SensorDialect,
    want_temperature: bool,
) -> (Vec<optical::OpticalSample>, Vec<(u32, f64)>) {
    let columns = vec![
        dialect.type_oid.to_owned(),
        dialect.scale_oid.to_owned(),
        dialect.precision_oid.to_owned(),
        dialect.value_oid.to_owned(),
    ];
    let rows = match walker.walk(transport, job.target, &columns, timeout).await {
        Ok(rows) => rows,
        Err(err) => {
            tracing::debug!(job_id = %job.job_id, error = %err, "entity-sensor walk failed");
            return (Vec::new(), Vec::new());
        }
    };
    let mut types: HashMap<u32, i64> = HashMap::new();
    let mut scales: HashMap<u32, i64> = HashMap::new();
    let mut precisions: HashMap<u32, i64> = HashMap::new();
    let mut values: HashMap<u32, i64> = HashMap::new();
    for row in rows {
        let v = row.value as i64;
        // Not a `match`: the arms are runtime values now, so the compiler cannot help here. The
        // `else` arm is the one that matters — a column this dialect did not ask for is skipped
        // rather than folded into whichever bucket happened to come last.
        let base = row.oid_base.as_str();
        let bucket = if base == dialect.type_oid {
            &mut types
        } else if base == dialect.scale_oid {
            &mut scales
        } else if base == dialect.precision_oid {
            &mut precisions
        } else if base == dialect.value_oid {
            &mut values
        } else {
            continue;
        };
        bucket.insert(row.ifindex, v);
    }
    if values.is_empty() {
        return (Vec::new(), Vec::new());
    }

    // units(9) and no decimals are the MIB's own defaults for an agent that omits either column.
    let scale_of = |ent: &u32| scales.get(ent).copied().unwrap_or(9);
    let precision_of = |ent: &u32| precisions.get(ent).copied().unwrap_or(0);

    // Chassis temperature, from the same rows (ADR-070 decision 2). Deliberately computed before
    // the text walk: a temperature needs no free text, so a device with nothing but chassis
    // sensors still reports them.
    let temps: Vec<(u32, f64)> = if want_temperature {
        let mut t: Vec<(u32, f64)> = values
            .iter()
            .filter_map(|(ent, value)| {
                let celsius = optical::entity_sensor_celsius(
                    *value,
                    *types.get(ent)?,
                    scale_of(ent),
                    precision_of(ent),
                )?;
                Some((*ent, celsius))
            })
            .collect();
        t.sort_unstable_by_key(|(ent, _)| *ent);
        t
    } else {
        Vec::new()
    };

    // Only now walk the text, and only for the entities that produced a candidate reading.
    let text = walk_entity_text(job, transport, walker, timeout).await;

    // Ascending entity order so "first lane wins" in `dedupe_readings` is deterministic.
    let mut ents: Vec<u32> = values.keys().copied().collect();
    ents.sort_unstable();
    let readings = ents
        .into_iter()
        .filter_map(|ent| {
            let dbm = optical::entity_sensor_dbm(
                *values.get(&ent)?,
                *types.get(&ent)?,
                scale_of(&ent),
                precision_of(&ent),
            )?;
            let reading = optical::reading_from_text(text.get(&ent)?)?;
            Some(optical::OpticalSample {
                ifindex: ent,
                reading,
                dbm,
            })
        })
        .collect();
    (readings, temps)
}

/// `entPhysicalIndex` → the best free text describing it (`entPhysicalName` preferred, falling
/// back to `entPhysicalDescr`).
///
/// Both are walked because vendors disagree on which one carries the direction: Cisco puts it in
/// `entPhysicalDescr`, and some agents leave that generic and put the useful string in
/// `entPhysicalName`. Whichever parses wins — `reading_from_text` refuses anything ambiguous, so
/// preferring one cannot silently pick a wrong direction.
async fn walk_entity_text(
    job: &PollJob,
    transport: &dyn Transport,
    walker: &SnmpWalker,
    timeout: Duration,
) -> HashMap<u32, String> {
    let columns = vec![
        optical::ENT_PHYSICAL_DESCR.to_owned(),
        optical::ENT_PHYSICAL_NAME.to_owned(),
    ];
    let mut out: HashMap<u32, String> = HashMap::new();
    match walker
        .walk_strings(transport, job.target, &columns, timeout)
        .await
    {
        Ok(rows) => {
            for row in rows {
                // Device-supplied text: kept only to classify, never rendered or used as a label.
                if optical::reading_from_text(&row.value).is_some() {
                    out.insert(row.ifindex, row.value);
                } else {
                    out.entry(row.ifindex).or_insert(row.value);
                }
            }
        }
        Err(err) => {
            tracing::debug!(job_id = %job.job_id, error = %err, "entity text walk failed");
        }
    }
    out
}

/// Row budget for the MAU walk. `ifMauTable` holds roughly one row per Ethernet port, so a few
/// thousand is generous for any single device; the bound exists so a pathological agent cannot turn
/// an hourly attribute read into an unbounded one. Stated rather than defaulted, like every other
/// `walk_instances` caller.
const MAU_MAX_ROWS: usize = 4096;

/// Execute a media-type walk (v2c or v3, selected by `walker`) — ADR-063 Inc.2.
///
/// Two sources, tried in order, and the order is the point:
///
/// 1. **`ifMauTable`**, which answers with a registry designation covering copper *and* optics. Its
///    `(ifIndex, ifMauIndex)` index is why this cannot ride the interface walk — the ordinary
///    walkers fold a multi-subid tail into a hash. The instance walker preserves it.
/// 2. **ENTITY-MIB**, only for ports MAU did not answer and only when `entity_fallback` is on. It
///    returns a vendor part string, which is a *different fact*: it is stored as
///    `transceiver_model` and only promotes to `if_media` when it demonstrably contains a
///    designation. It reaches pluggables alone — a fixed copper port has no entity to describe, so
///    a device with no MAU-MIB gets nothing for its RJ45 ports and that is honest.
///
/// The result is **observational**, like the optical and neighbour walks: most devices do not
/// implement MAU-MIB, and a silent one is not an unreachable one. Reporting otherwise would page
/// someone about a healthy box.
///
/// Every field except the media pair is `None` on the way out. Core's interface upsert COALESCEs
/// each column, so these rows fill their own columns and leave the name, alias, speed, duplex and
/// optical window exactly as the other probes wrote them.
pub(super) async fn execute_mau(
    job: &PollJob,
    transport: &dyn Transport,
    at_unix_ms: i64,
    entity_fallback: bool,
    timeout: Duration,
    walker: &SnmpWalker,
) -> PollResult {
    let columns = vec![yagra_common::OID_IF_MAU_TYPE.to_owned()];
    let (mut media, unknown) = match walker
        .walk_instances(transport, job.target, &columns, timeout, MAU_MAX_ROWS)
        .await
    {
        Ok(rows) => crate::mau::media_by_ifindex(&rows),
        Err(err) => {
            tracing::debug!(job_id = %job.job_id, error = %err, "ifMauTable walk failed");
            (BTreeMap::new(), Vec::new())
        }
    };
    if !unknown.is_empty() {
        // A *number*, not an OID string — it is what someone extending `MAU_TYPES` needs, and the
        // only way a gap in a hand-transcribed registry becomes visible from a running deployment.
        tracing::debug!(
            job_id = %job.job_id,
            subids = ?unknown,
            "ifMauType registrations not in the transcribed table; media left unknown",
        );
    }

    // Second source: CISCO-STACK-MIB's `portType` (ADR-063 Inc.7), for the ports `ifMauType` did
    // not answer. Between MAU and the ENTITY text on purpose — it is the device *stating* the
    // medium, where the ENTITY string is a part number this code pattern-matches — and skipped
    // entirely when MAU already covered everything, so a device that answers the standard MIB pays
    // nothing for it.
    if media.is_empty() || media.len() < MAU_MAX_ROWS {
        let columns = vec![
            yagra_common::OID_CISCO_PORT_TYPE.to_owned(),
            yagra_common::OID_CISCO_PORT_IFINDEX.to_owned(),
            // Three `portType` values name a capability rather than a rate; see `cisco_media_by_ifindex`.
            yagra_common::OID_IF_HIGH_SPEED.to_owned(),
        ];
        match walker
            .walk_instances(transport, job.target, &columns, timeout, MAU_MAX_ROWS)
            .await
        {
            Ok(rows) => {
                let cisco = crate::mau::cisco_media_by_ifindex(
                    &rows,
                    yagra_common::OID_CISCO_PORT_TYPE,
                    yagra_common::OID_CISCO_PORT_IFINDEX,
                    yagra_common::OID_IF_HIGH_SPEED,
                );
                for (ifindex, m) in cisco {
                    // MAU wins: a registry designation is never replaced by a translated one.
                    media.entry(ifindex).or_insert(crate::mau::MediaRow {
                        media: Some(m),
                        duplex: None,
                        transceiver_model: None,
                    });
                }
            }
            Err(err) => {
                tracing::debug!(job_id = %job.job_id, error = %err, "cisco portTable walk failed");
            }
        }
    }

    if entity_fallback {
        let text = walk_entity_media_text(job, transport, walker, timeout).await;
        if !text.is_empty() {
            let index = walk_entity_index(job, transport, walker, timeout).await;
            crate::mau::merge_entity_fallback(&mut media, &text, |ent| index.ifindex_for(ent));
        }
    }

    let interfaces = media
        .into_iter()
        .map(|(ifindex, row)| DiscoveredInterface {
            ifindex: IfIndex(ifindex),
            if_name: None,
            if_alias: None,
            if_speed: None,
            // MAU's duplex is secondary: `dot3StatsDuplexStatus` runs on the fast path and wins by
            // arriving first, because the upsert COALESCEs rather than overwrites. This fills the
            // column only on a device that implements MAU-MIB but not EtherLike-MIB.
            if_duplex: row.duplex,
            if_type: None,
            if_media: row.media,
            transceiver_model: row.transceiver_model,
            rx_power_low_dbm: None,
            rx_power_high_dbm: None,
            tx_power_low_dbm: None,
            tx_power_high_dbm: None,
        })
        .collect();

    PollResult {
        job_id: job.job_id,
        node_id: job.node_id,
        at_unix_ms,
        outcome: CheckOutcome::Reachable,
        samples: Vec::new(),
        interfaces,
        sys_descr: None,
        dns_chain: None,
        neighbors: None,
        l3: None,
        arp: None,
        routing: None,
        poller_id: None,
        // Never a liveness statement — see the doc comment.
        observational: true,
        trace_context: Default::default(),
    }
}

/// Walk the ENTITY-MIB text columns that can name a pluggable.
///
/// Two describing columns because which one carries a designation varies by vendor, plus
/// `entPhysicalName` — which is **not** a candidate but the yardstick: `mau::entity_text` throws
/// away any description that merely restates the component's own name. Without that third column
/// this walk reported every port as its own transceiver (see that function's 🚨).
async fn walk_entity_media_text(
    job: &PollJob,
    transport: &dyn Transport,
    walker: &SnmpWalker,
    timeout: Duration,
) -> BTreeMap<u32, String> {
    let columns = vec![
        ENT_PHYSICAL_MODEL_NAME.to_owned(),
        optical::ENT_PHYSICAL_DESCR.to_owned(),
        optical::ENT_PHYSICAL_NAME.to_owned(),
        // Not describing columns either — the two yardsticks. See `mau::entity_text`'s 🚨.
        ENT_PHYSICAL_IS_FRU.to_owned(),
        ENT_PHYSICAL_CLASS.to_owned(),
    ];
    match walker
        .walk_instances(
            transport,
            job.target,
            &columns,
            timeout,
            OPTICAL_ENTITY_MAX_ROWS,
        )
        .await
    {
        Ok(rows) => crate::mau::entity_text(
            &rows,
            optical::ENT_PHYSICAL_NAME,
            ENT_PHYSICAL_IS_FRU,
            ENT_PHYSICAL_CLASS,
        ),
        Err(err) => {
            tracing::debug!(job_id = %job.job_id, error = %err, "entity media text walk failed");
            BTreeMap::new()
        }
    }
}

/// `entPhysicalModelName` — ENTITY-MIB's vendor part number for a physical component. The column
/// `optical.rs` had no use for, since a part number says nothing about a light level.
const ENT_PHYSICAL_MODEL_NAME: &str = "1.3.6.1.2.1.47.1.1.1.1.13";

/// `entPhysicalIsFRU` — whether the component can be pulled out and replaced. A transceiver can; a
/// soldered port cannot, which is what makes this the structural half of "is this text a module?".
const ENT_PHYSICAL_IS_FRU: &str = "1.3.6.1.2.1.47.1.1.1.1.16";

/// `entPhysicalClass` — what kind of component this is. Read only to exclude `sensor(8)`, which
/// reaches a port through the same containment chain a transceiver does and whose *name* reads like
/// a module's. See `mau::entity_text`'s 🚨.
const ENT_PHYSICAL_CLASS: &str = "1.3.6.1.2.1.47.1.1.1.1.5";

/// Walk the two ENTITY-MIB relations that attach a physical entity to an interface.
async fn walk_entity_index(
    job: &PollJob,
    transport: &dyn Transport,
    walker: &SnmpWalker,
    timeout: Duration,
) -> optical::EntityIndex {
    let mut idx = optical::EntityIndex::default();
    let columns = vec![
        optical::ENT_ALIAS_MAPPING.to_owned(),
        optical::ENT_PHYSICAL_CONTAINED_IN.to_owned(),
    ];
    match walker
        .walk_instances(
            transport,
            job.target,
            &columns,
            timeout,
            OPTICAL_ENTITY_MAX_ROWS,
        )
        .await
    {
        Ok(rows) => {
            let (alias, parent): (Vec<_>, Vec<_>) = rows
                .into_iter()
                .partition(|r| r.oid_base == optical::ENT_ALIAS_MAPPING);
            idx.add_alias_rows(&alias);
            idx.add_parent_rows(&parent);
        }
        Err(err) => {
            tracing::debug!(job_id = %job.job_id, error = %err, "entity index walk failed");
        }
    }
    idx
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};
    use uuid::Uuid;
    use yagra_common::NodeId;
    use yagra_transport::FakeTransport;

    /// A dark port answers the walk with the vendor's placeholder on every column. It must produce
    /// no reading and no band — while a real row on the same walk still comes through, which is the
    /// half that stops "skip everything" from passing as a fix.
    #[tokio::test]
    async fn a_no_module_row_yields_neither_a_reading_nor_a_band() {
        let d = optical::simple_dialect(yagra_common::OpticalFlavor::Huawei).expect("huawei");
        let row = |oid: &str, ifindex: u32, value: f64| yagra_transport::SnmpTableSample {
            oid_base: oid.to_owned(),
            ifindex,
            value,
        };
        let limits = d.limits.expect("huawei publishes a window");
        let mut fake = FakeTransport::reachable(1.0);
        fake.snmp_table = vec![
            // ifIndex 1 — no transceiver: every column reads the marker, as the lab USG does.
            row(d.rx_oid, 1, -1.0),
            row(d.tx_oid, 1, -1.0),
            row(limits.rx_low_oid, 1, -1.0),
            row(limits.rx_high_oid, 1, -1.0),
            // ifIndex 2 — a live module, with a window that is a genuine pair.
            row(d.rx_oid, 2, -1005.0),
            row(d.tx_oid, 2, -950.0),
            row(limits.rx_low_oid, 2, -1410.0),
            row(limits.rx_high_oid, 2, 200.0),
        ];
        let job = PollJob::snmp_optical(
            Uuid::nil(),
            NodeId::from(Uuid::nil()),
            IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
            yagra_bus::SnmpOpticalCheck {
                community: "public".to_owned(),
                probes: Vec::new(),
                timeout_ms: 1_000,
            },
            30,
        );
        let walker = SnmpWalker::V2c("public".to_owned());
        let (readings, windows) =
            walk_simple_optical(&job, &fake, &walker, Duration::from_secs(1), &d).await;

        assert!(
            readings.iter().all(|r| r.ifindex == 2),
            "the dark port must contribute no reading, got {readings:?}"
        );
        assert_eq!(readings.len(), 2, "the live port still reports rx and tx");
        assert!(
            readings.iter().any(|r| (r.dbm - -10.05).abs() < 1e-9),
            "the live receive level survives unchanged, got {readings:?}"
        );
        assert!(
            !windows.contains_key(&1),
            "a marker row must not become a band either"
        );
        assert!(windows.contains_key(&2), "the live port keeps its band");
    }

    /// **The Cisco sensor dialect end to end: optical readings, the sentinel, and the SFP/chassis
    /// split** (ADR-070 decisions 1 and 2).
    ///
    /// There was no test of the correlated path at all before this — every optical test exercised
    /// a `SimpleDialect`. That gap is why the shape of this one matters more than its size: the
    /// four things asserted here each fail *silently* into "no series", which on a real device is
    /// indistinguishable from "this switch has no optics".
    #[tokio::test]
    async fn the_cisco_sensor_dialect_splits_optical_readings_from_chassis_temperature() {
        use yagra_transport::{SnmpInstanceRow, SnmpTableSample, SnmpTableString, SnmpValue};
        let d = optical::sensor_dialect(yagra_common::OpticalFlavor::CiscoEntitySensor)
            .expect("cisco is a correlated dialect");
        let num = |oid: &str, ent: u32, value: f64| SnmpTableSample {
            oid_base: oid.to_owned(),
            ifindex: ent,
            value,
        };
        // type / scale / precision / value for one entity, in the shape a real Nexus sends.
        let sensor = |ent: u32, ty: f64, scale: f64, prec: f64, value: f64| {
            vec![
                num(d.type_oid, ent, ty),
                num(d.scale_oid, ent, scale),
                num(d.precision_oid, ent, prec),
                num(d.value_oid, ent, value),
            ]
        };
        let text = |ent: u32, s: &str| SnmpTableString {
            oid_base: optical::ENT_PHYSICAL_NAME.to_owned(),
            ifindex: ent,
            value: s.to_owned(),
        };

        let mut fake = FakeTransport::reachable(1.0);
        fake.snmp_table = [
            // ent 100 — a live receive sensor on Ethernet1/1. The exact row from the lab N3K.
            sensor(100, 14.0, 8.0, 0.0, -13187.0),
            // ent 101 — the SFP's *own* temperature, sitting in the same table. Excluded by
            // ADR-062 Issue #66, and excluded here because it reaches a port.
            sensor(101, 8.0, 9.0, 0.0, 45.0),
            // ent 200 — `module-1 FRONT`, a chassis sensor. Reaches no port, so it is the one
            // temperature that survives.
            sensor(200, 8.0, 9.0, 0.0, 31.0),
            // ent 300 — a transmit sensor reading 0, which is what an N9K sends for all fourteen
            // of its dBm sensors when nothing is plugged in. 0 dBm is 1 mW — stronger than the
            // live port above — so it must produce nothing.
            sensor(300, 14.0, 8.0, 0.0, 0.0),
        ]
        .concat();
        fake.snmp_table_strings = vec![
            text(100, "Ethernet1/1 Lane 1 Transceiver Receive Power Sensor"),
            text(101, "Ethernet1/1 Lane 1 Transceiver Temperature Sensor"),
            text(200, "module-1 FRONT"),
            text(300, "Ethernet1/2 Lane 1 Transceiver Transmit Power Sensor"),
        ];
        // ENTITY-MIB: the two optical sensors and the SFP temperature hang off ports; the chassis
        // sensor has a parent that leads nowhere. This is the whole SFP/chassis discriminator.
        let alias = |ent: u32, ifindex: u32| SnmpInstanceRow {
            oid_base: optical::ENT_ALIAS_MAPPING.to_owned(),
            instance: vec![ent, 0],
            value: SnmpValue::Oid(format!("1.3.6.1.2.1.2.2.1.1.{ifindex}")),
        };
        let parent = |ent: u32, p: u32| SnmpInstanceRow {
            oid_base: optical::ENT_PHYSICAL_CONTAINED_IN.to_owned(),
            instance: vec![ent],
            value: SnmpValue::Int(i64::from(p)),
        };
        fake.snmp_instances = vec![
            parent(100, 10),
            parent(101, 10),
            alias(10, 1), // port Ethernet1/1
            parent(300, 20),
            alias(20, 2), // port Ethernet1/2
            // The chassis sensor climbs to a module that owns no interface — a dead end, exactly
            // as `module-1 FRONT` does on the real N9K (four hops, no alias).
            parent(200, 900),
        ];

        let job = PollJob::snmp_optical(
            Uuid::nil(),
            NodeId::from(Uuid::nil()),
            IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
            yagra_bus::SnmpOpticalCheck {
                community: "public".to_owned(),
                probes: vec![yagra_bus::OpticalProbe {
                    flavor: yagra_common::OpticalFlavor::CiscoEntitySensor,
                    rx_metric: Some(yagra_common::METRIC_IF_RX_POWER_DBM.to_owned()),
                    tx_metric: Some(yagra_common::METRIC_IF_TX_POWER_DBM.to_owned()),
                    temp_metric: Some(yagra_common::METRIC_CISCO_TEMP_C.to_owned()),
                }],
                timeout_ms: 1_000,
            },
            30,
        );
        let r = execute(&job, &fake, 1_000).await;
        let of = |metric: &str| -> Vec<(Option<u32>, f64)> {
            r.samples
                .iter()
                .filter(|s| s.metric == metric)
                .map(|s| (s.ifindex.map(|i| i.0), s.value))
                .collect()
        };

        // The Cisco columns were walked at all — if the dialect were still hardcoded to the
        // standard root this would be empty, which is the pre-ADR-070 behaviour on every Catalyst.
        assert_eq!(
            of(yagra_common::METRIC_IF_RX_POWER_DBM),
            vec![(Some(1), -13.187)],
            "the live receive level, translated to its ifIndex"
        );
        // The 0 dBm marker must not become the strongest reading on the switch.
        assert!(
            of(yagra_common::METRIC_IF_TX_POWER_DBM).is_empty(),
            "a 0 dBm sensor is 'no module', not a measurement"
        );
        // Exactly one temperature: the chassis one. The SFP's own temperature (ent 101, 45 °C) is
        // excluded *structurally* — it reaches a port — not by matching its description.
        assert_eq!(
            of(yagra_common::METRIC_CISCO_TEMP_C),
            vec![(Some(200), 31.0)],
            "only the sensor that belongs to no port becomes a chassis temperature"
        );
    }
}
