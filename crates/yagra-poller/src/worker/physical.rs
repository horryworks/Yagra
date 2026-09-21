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
    let mut entity: Option<EntityIndexWalk> = None;

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
        //
        // "Reaches no interface" is a conclusion from what the index did NOT contain, so it is drawn
        // only from an index read whole (ADR-158 A6). An incomplete one publishes no chassis
        // temperature at all this poll: a gap in the chart, rather than an SFP's 45 °C drawn as the
        // chassis's.
        if let (Some(metric), Some(walk)) = (probe.temp_metric.as_ref(), entity.as_ref()) {
            let mut kept = 0usize;
            for (ent, celsius) in &temps {
                if !walk.complete || walk.index.ifindex_for(*ent).is_some() {
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
                    // Every sensor is withheld when the index is incomplete, so without this the
                    // log would call them all port-attached.
                    index_complete = walk.complete,
                    port_attached = temps.len() - kept,
                    "chassis temperature sensors",
                );
            }
        }

        let (resolved, resolved_windows) = if probe.flavor.is_ifindex_keyed() {
            (readings, raw_windows)
        } else {
            // A partial index is used as it is: a row it DID return is true (`EntityIndexWalk`).
            let idx = &entity
                .as_ref()
                .expect("built above for a non-ifindex-keyed dialect")
                .index;
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
        // …but its readings are readings: a band on a light level or a chassis temperature is
        // judged like any other (ADR-158 A10). Core schedules this job at the node's own poll
        // interval (`scheduler/assemble.rs`), which is the condition the field's doc sets.
        judge_samples: true,
        poller_id: None,
        trace_context: Default::default(),
        meraki_collect: None,
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
    // The truncation verdict is ignored here on purpose: this walk is observational (ADR-110
    // Increment 6 reports completeness for the interface table walk, which carries the node's
    // configured collection set; a short optical or sensor walk costs a gap in an optional reading).
    // Its budget is still Increment 3's — Increment 11 is where the optical job gets its own.
    let limits = WalkLimits::per_round_trip(timeout);
    let rows = match walker.walk(transport, job.target, &columns, limits).await {
        Ok(walk) => walk.rows,
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
    // The truncation verdict is ignored here on purpose: this walk is observational (ADR-110
    // Increment 6 reports completeness for the interface table walk, which carries the node's
    // configured collection set; a short optical or sensor walk costs a gap in an optional reading).
    let limits = WalkLimits::per_round_trip(timeout);
    let rows = match walker.walk(transport, job.target, &columns, limits).await {
        Ok(walk) => walk.rows,
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
        .walk_strings(
            transport,
            job.target,
            &columns,
            WalkLimits::per_round_trip(timeout),
        )
        .await
    {
        Ok(walk) => {
            for row in walk.rows {
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

/// Execute a media-type walk (v2c or v3, selected by `walker`) — ADR-063 Inc.2, ADR-110 Inc.4.
///
/// Three sources, and the order they are consulted in is the point:
///
/// 1. **`ifMauTable`**, which answers with a registry designation covering copper *and* optics. Its
///    `(ifIndex, ifMauIndex)` index is why this cannot ride the interface walk — the ordinary
///    walkers fold a multi-subid tail into a hash. The instance walker preserves it.
/// 2. **CISCO-STACK-MIB's `portType`** (ADR-063 Inc.7), for ports `ifMauType` did not answer: the
///    device *stating* the medium, rather than a part number to pattern-match.
/// 3. **ENTITY-MIB**, only for ports neither of those answered and only when `entity_fallback` is
///    on. It returns a vendor part string, which is a *different fact*: it is stored as
///    `transceiver_model` and only promotes to `if_media` when it demonstrably contains a
///    designation. It reaches pluggables alone — a fixed copper port has no entity to describe, so
///    a device with no MAU-MIB gets nothing for its RJ45 ports and that is honest.
///
/// **Sources 1 and 2 share one walk; source 3 is its own** — see the comment on that walk for why
/// merging the first two is what makes a silent device cheap, and why merging the third would break
/// `mau::entity_text`.
///
/// The result is **observational**, like the optical and neighbour walks: most devices do not
/// implement MAU-MIB, and a silent one is not an unreachable one. Reporting otherwise would page
/// someone about a healthy box. Every exit goes through [`mau_result`], which is where that is said
/// once.
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
    // **One walk, not two** (ADR-110 Increment 4). `ifMauType` on its own is a *single* column, and
    // a single column can never trip `WalkBudget`'s two-consecutive-failures rule — so against a
    // silent device this walk paid a whole timeout and then returned `Ok(empty)`, which is
    // indistinguishable from "the device does not implement MAU-MIB". Asking for both column
    // families in one call gives the budget two columns to judge the device on, and the walker then
    // says so with `TransportError::Silent` instead of leaving it to be guessed at. That is what
    // put this check on the 4,002 ms every other silent-device check had already converged on;
    // measured before it, `snmp_mau` alone sat at 10,006 ms (= 2 + 4 + 4).
    //
    // A healthy device is asked for exactly the same four columns as before, over one UDP session
    // instead of two — the old `media.len() < MAU_MAX_ROWS` gate was true for anything short of a
    // 4,096-port chassis, and the row budget it stood in for is enforced inside the walker anyway.
    //
    // ⚠️ **The ENTITY-MIB columns below are deliberately NOT merged in.** `mau::entity_text` treats
    // every column that is not one of its yardsticks as a describing candidate, so MAU and Cisco
    // rows arriving in the same vector would be read as part numbers.
    let columns = vec![
        yagra_common::OID_IF_MAU_TYPE.to_owned(),
        yagra_common::OID_CISCO_PORT_TYPE.to_owned(),
        yagra_common::OID_CISCO_PORT_IFINDEX.to_owned(),
        // Three `portType` values name a capability rather than a rate; see `cisco_media_by_ifindex`.
        yagra_common::OID_IF_HIGH_SPEED.to_owned(),
    ];
    let rows = match walker
        .walk_instances(transport, job.target, &columns, timeout, MAU_MAX_ROWS)
        .await
    {
        Ok(rows) => rows,
        Err(err) => {
            // `TransportError::Silent` lands here, and returning on it is the point: the two ENTITY
            // walks below would each buy another four seconds of the same answer.
            tracing::debug!(job_id = %job.job_id, error = %err, "media walk failed");
            return mau_result(job, at_unix_ms, Vec::new());
        }
    };

    let (mut media, unknown) = crate::mau::media_by_ifindex(&rows, yagra_common::OID_IF_MAU_TYPE);
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
    // not answer. Read between MAU and the ENTITY text on purpose — it is the device *stating* the
    // medium, where the ENTITY string is a part number this code pattern-matches.
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

    if entity_fallback {
        let text = walk_entity_media_text(job, transport, walker, timeout).await;
        if !text.is_empty() {
            // Only what the index FOUND is used here — a part attached to a port — so a partial
            // index is safe (see `EntityIndexWalk`).
            let walk = walk_entity_index(job, transport, walker, timeout).await;
            crate::mau::merge_entity_fallback(&mut media, &text, |ent| walk.index.ifindex_for(ent));
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

    mau_result(job, at_unix_ms, interfaces)
}

/// The shape every media-walk answer takes: observational, no samples, only the interface rows.
///
/// A function rather than a literal because [`execute_mau`] now has **two** exits — the walk that
/// found nothing because the device said nothing returns early — and two copies of a nineteen-field
/// struct is exactly how one of them ends up claiming reachability the walk never established.
fn mau_result(job: &PollJob, at_unix_ms: i64, interfaces: Vec<DiscoveredInterface>) -> PollResult {
    PollResult {
        job_id: job.job_id,
        node_id: job.node_id,
        at_unix_ms,
        outcome: CheckOutcome::Reachable,
        samples: Vec::new(),
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
        poller_id: None,
        // Never a liveness statement — see [`execute_mau`]'s doc comment.
        row_names: Vec::new(),
        observational: true,
        // Hourly, and it carries no samples anyway.
        judge_samples: false,
        trace_context: Default::default(),
        meraki_collect: None,
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

/// Counts every ENTITY-MIB index walk by whether it was read whole (`result`: `complete` or
/// `incomplete`). Both series are touched on every walk, so a poller that has walked once publishes
/// the incomplete one at zero rather than first creating it on a failure (ADR-108 Increment 3).
const ENTITY_INDEX_WALKS_METRIC: &str = "yagra_optical_entity_index_walks_total";

/// What [`walk_entity_index`] learned, and whether it learned all of it.
///
/// **Not an `Option`, because the two uses of the index need different evidence** (ADR-158 A6).
/// Mapping a reading to its port uses what was *found*: a row that attaches a sensor to a port is
/// true however much of the table went unread, so a partial index is still safe to map through.
/// Calling a sensor a chassis sensor uses what was *not found* — "it reaches no port" — and that
/// conclusion is only as good as the table it was drawn from. A failed walk used to leave an empty
/// index, under which every sensor reached no port and each SFP's own temperature was published
/// as the chassis's.
struct EntityIndexWalk {
    index: optical::EntityIndex,
    /// Every column was walked to its end and the row budget did not cut the table.
    complete: bool,
}

/// Walk the two ENTITY-MIB relations that attach a physical entity to an interface.
async fn walk_entity_index(
    job: &PollJob,
    transport: &dyn Transport,
    walker: &SnmpWalker,
    timeout: Duration,
) -> EntityIndexWalk {
    let mut index = optical::EntityIndex::default();
    let columns = vec![
        optical::ENT_ALIAS_MAPPING.to_owned(),
        optical::ENT_PHYSICAL_CONTAINED_IN.to_owned(),
    ];
    let complete = match walker
        .walk_instance_columns(
            transport,
            job.target,
            &columns,
            timeout,
            OPTICAL_ENTITY_MAX_ROWS,
        )
        .await
    {
        Ok(walk) => {
            // A column the row budget cut still ends as answered — the walker stops asking, it
            // does not fail — so a table that reached the cap is judged by its size.
            let complete = walk.every_column_answered && walk.rows.len() < OPTICAL_ENTITY_MAX_ROWS;
            let (alias, parent): (Vec<_>, Vec<_>) = walk
                .rows
                .into_iter()
                .partition(|r| r.oid_base == optical::ENT_ALIAS_MAPPING);
            index.add_alias_rows(&alias);
            index.add_parent_rows(&parent);
            complete
        }
        Err(err) => {
            tracing::debug!(job_id = %job.job_id, error = %err, "entity index walk failed");
            false
        }
    };
    let (seen, other) = if complete {
        ("complete", "incomplete")
    } else {
        tracing::debug!(
            job_id = %job.job_id,
            "entity index incomplete: no sensor will be reported as a chassis sensor this poll"
        );
        ("incomplete", "complete")
    };
    metrics::counter!(ENTITY_INDEX_WALKS_METRIC, "result" => seen).increment(1);
    metrics::counter!(ENTITY_INDEX_WALKS_METRIC, "result" => other).increment(0);
    EntityIndexWalk { index, complete }
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

    /// One ENTITY-MIB containment row: `ent` sits inside `parent`.
    fn contained_in(ent: u32, parent: u32) -> yagra_transport::SnmpInstanceRow {
        yagra_transport::SnmpInstanceRow {
            oid_base: optical::ENT_PHYSICAL_CONTAINED_IN.to_owned(),
            instance: vec![ent],
            value: yagra_transport::SnmpValue::Int(i64::from(parent)),
        }
    }

    /// A Cisco switch as the lab N3K/N9K answer it: one live receive sensor on Ethernet1/1, that
    /// SFP's own temperature, one chassis sensor (`module-1 FRONT`), and a 0 dBm "no module"
    /// transmit sensor — with the ENTITY-MIB rows that tell the port-attached sensors from the
    /// chassis one. Returned with the optical job that asks for all three metrics.
    fn cisco_sensor_switch() -> (FakeTransport, PollJob) {
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
        fake.snmp_instances = vec![
            contained_in(100, 10),
            contained_in(101, 10),
            alias(10, 1), // port Ethernet1/1
            contained_in(300, 20),
            alias(20, 2), // port Ethernet1/2
            // The chassis sensor climbs to a module that owns no interface — a dead end, exactly
            // as `module-1 FRONT` does on the real N9K (four hops, no alias).
            contained_in(200, 900),
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
        (fake, job)
    }

    /// `(ifindex, value)` of every sample in `r` named `metric`.
    fn samples_of(r: &PollResult, metric: &str) -> Vec<(Option<u32>, f64)> {
        r.samples
            .iter()
            .filter(|s| s.metric == metric)
            .map(|s| (s.ifindex.map(|i| i.0), s.value))
            .collect()
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
        let (fake, job) = cisco_sensor_switch();
        let r = execute(&job, &fake, 1_000).await;

        // The Cisco columns were walked at all — if the dialect were still hardcoded to the
        // standard root this would be empty, which is the pre-ADR-070 behaviour on every Catalyst.
        assert_eq!(
            samples_of(&r, yagra_common::METRIC_IF_RX_POWER_DBM),
            vec![(Some(1), -13.187)],
            "the live receive level, translated to its ifIndex"
        );
        // The 0 dBm marker must not become the strongest reading on the switch.
        assert!(
            samples_of(&r, yagra_common::METRIC_IF_TX_POWER_DBM).is_empty(),
            "a 0 dBm sensor is 'no module', not a measurement"
        );
        // Exactly one temperature: the chassis one. The SFP's own temperature (ent 101, 45 °C) is
        // excluded *structurally* — it reaches a port — not by matching its description.
        assert_eq!(
            samples_of(&r, yagra_common::METRIC_CISCO_TEMP_C),
            vec![(Some(200), 31.0)],
            "only the sensor that belongs to no port becomes a chassis temperature"
        );
    }

    /// 🚨 **A chassis temperature is a claim that a sensor reaches no port, and an index that was
    /// not read cannot support it** (ADR-158 A6). Before this, a failed ENTITY-MIB walk left an
    /// empty index, every sensor "reached no port", and the SFP's own 45 °C was published as a
    /// chassis temperature beside the real one.
    #[tokio::test]
    async fn a_failed_entity_index_walk_publishes_no_chassis_temperature() {
        let (fake, job) = cisco_sensor_switch();
        let fake = fake.with_silent_instance_walks();
        let r = execute(&job, &fake, 1_000).await;

        assert!(
            samples_of(&r, yagra_common::METRIC_CISCO_TEMP_C).is_empty(),
            "nothing was learned about which sensor sits on a port, so none is a chassis sensor: \
             {:?}",
            samples_of(&r, yagra_common::METRIC_CISCO_TEMP_C)
        );
        assert!(
            r.observational,
            "a failed index walk still states nothing about liveness"
        );
    }

    /// The optical result states nothing about liveness and still asks for its readings to be
    /// judged (ADR-158 A10) — the two halves core reads separately.
    #[tokio::test]
    async fn an_optical_result_is_observational_and_asks_for_its_samples_to_be_judged() {
        let (fake, job) = cisco_sensor_switch();
        let r = execute(&job, &fake, 1_000).await;
        assert!(r.observational);
        assert!(r.judge_samples);
        assert!(!r.samples.is_empty(), "…and it has readings to judge");
    }

    /// A walk that returned rows but did not hear every column out is half an index. The rows it
    /// did return are still true, so a reading that finds its port through them keeps it — only the
    /// "reaches no port" conclusion is withheld.
    #[tokio::test]
    async fn an_index_missing_its_alias_column_publishes_no_chassis_temperature() {
        let (mut fake, job) = cisco_sensor_switch();
        // Ethernet1/2's alias row is what the unanswered column lost.
        fake.snmp_instances
            .retain(|r| !(r.oid_base == optical::ENT_ALIAS_MAPPING && r.instance == vec![20, 0]));
        let fake = fake.with_unanswered_instance_columns();
        let r = execute(&job, &fake, 1_000).await;

        assert!(
            samples_of(&r, yagra_common::METRIC_CISCO_TEMP_C).is_empty(),
            "a partial index cannot prove a sensor reaches no port: {:?}",
            samples_of(&r, yagra_common::METRIC_CISCO_TEMP_C)
        );
        assert_eq!(
            samples_of(&r, yagra_common::METRIC_IF_RX_POWER_DBM),
            vec![(Some(1), -13.187)],
            "a reading whose port WAS found keeps it — finding is evidence even in half an index"
        );
    }

    /// The row budget ends a column as answered (the walker stops asking, it does not fail), so the
    /// column flag alone says nothing about a table cut at its cap.
    #[tokio::test]
    async fn an_index_walk_that_filled_its_row_budget_is_not_trusted() {
        let (mut fake, job) = cisco_sensor_switch();
        let real = fake.snmp_instances.len();
        let filler = u32::try_from(OPTICAL_ENTITY_MAX_ROWS - real).expect("fits");
        fake.snmp_instances
            .extend((0..filler).map(|i| contained_in(100_000 + i, 900)));
        let r = execute(&job, &fake, 1_000).await;

        assert!(
            samples_of(&r, yagra_common::METRIC_CISCO_TEMP_C).is_empty(),
            "a table that filled the budget may have lost the rows that attach a sensor to its \
             port: {:?}",
            samples_of(&r, yagra_common::METRIC_CISCO_TEMP_C)
        );
    }

    /// A media job aimed at a device, with the ENTITY fallback on — the shape core always builds
    /// (`build_snmp_mau_check` hardcodes `entity_fallback: true`).
    fn mau_job() -> PollJob {
        PollJob::snmp_mau(
            Uuid::nil(),
            NodeId::from(Uuid::nil()),
            IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
            yagra_bus::SnmpMauCheck {
                community: "public".to_owned(),
                entity_fallback: true,
                timeout_ms: 2_000,
            },
            3600,
        )
    }

    /// **A device that said nothing is asked once, not three times** (ADR-110 Increment 4).
    ///
    /// The measured defect: `snmp_mau` sat at 10,006 ms against a silent device while every other
    /// SNMP check converged on 4,002 ms, because `execute_mau` fired three walks and each one
    /// started a fresh `WalkBudget`. The first asked for a *single* column, which can never trip the
    /// two-consecutive-failures rule, so it returned `Ok(empty)` — indistinguishable from "this
    /// device does not implement MAU-MIB" — and the other two paid their own timeouts to learn the
    /// same thing.
    ///
    /// 🚨 This asserts what the poller **asked for**, not what came back. Every assertion about the
    /// result already passed against the broken version: three walks against a silent device return
    /// exactly what one walk returns.
    #[tokio::test]
    async fn a_device_that_said_nothing_is_asked_once_not_three_times() {
        let fake = FakeTransport::reachable(1.0).with_silent_instance_walks();
        let r = execute(&mau_job(), &fake, 1_000).await;

        assert!(r.interfaces.is_empty(), "nothing was learned");
        assert_eq!(
            r.outcome,
            CheckOutcome::Reachable,
            "a media walk never states liveness, silence included"
        );
        assert!(r.observational, "…and says so");

        let asked = fake
            .asked
            .lock()
            .expect("the ask log is not poisoned")
            .clone();
        assert_eq!(
            asked.len(),
            1,
            "one walk, not three — each extra one buys another timeout to learn the same thing: \
             {asked:?}"
        );
        let first = &asked[0];
        assert!(
            first.iter().any(|c| c == yagra_common::OID_IF_MAU_TYPE)
                && first.iter().any(|c| c == yagra_common::OID_CISCO_PORT_TYPE),
            "the one walk must carry both column families, or the budget has only one column to \
             judge the device on and cannot trip: {first:?}"
        );
        assert!(
            !asked
                .iter()
                .flatten()
                .any(|c| c == ENT_PHYSICAL_MODEL_NAME || c == optical::ENT_ALIAS_MAPPING),
            "no ENTITY column may be asked for after the device was reported silent: {asked:?}"
        );
    }

    /// **Merging the two column families changes nothing about what is reported.**
    ///
    /// The regression for ADR-110 Increment 4's second half: `ifMauType` and Cisco's `portTable`
    /// now arrive in one row vector, so both readers have to pick their own columns out of it.
    /// `media_by_ifindex` used to take every row it was handed and lean on "only `ifMauType` is an
    /// OBJECT IDENTIFIER" — true today, and a coincidence of value types rather than a rule.
    #[tokio::test]
    async fn one_walk_carrying_both_column_families_reports_what_two_walks_did() {
        use yagra_transport::{SnmpInstanceRow, SnmpValue};
        const CISCO_TYPE: &str = "1.3.6.1.4.1.9.5.1.4.1.1.5";
        const CISCO_IFX: &str = "1.3.6.1.4.1.9.5.1.4.1.1.11";
        const HIGH_SPEED: &str = "1.3.6.1.2.1.31.1.1.1.15";

        let mau = |ifindex: u32, subid: u32| SnmpInstanceRow {
            oid_base: yagra_common::OID_IF_MAU_TYPE.to_owned(),
            instance: vec![ifindex, 1],
            value: SnmpValue::Oid(format!("1.3.6.1.2.1.26.4.{subid}")),
        };
        let port = |oid: &str, module: u32, port: u32, v: i64| SnmpInstanceRow {
            oid_base: oid.to_owned(),
            instance: vec![module, port],
            value: SnmpValue::Int(v),
        };
        let speed = |ifindex: u32, mbps: i64| SnmpInstanceRow {
            oid_base: HIGH_SPEED.to_owned(),
            instance: vec![ifindex],
            value: SnmpValue::Int(mbps),
        };

        let mut fake = FakeTransport::reachable(1.0);
        fake.snmp_instances = vec![
            // ifIndex 7 answers MAU: 1000BASE-T (registration 30).
            mau(7, 30),
            // (1,1) → ifIndex 10101 answers only Cisco's portTable, at 1 Gbit/s copper.
            port(CISCO_IFX, 1, 1, 10101),
            port(CISCO_TYPE, 1, 1, 61),
            speed(10101, 1000),
        ];
        let r = execute(&mau_job(), &fake, 1_000).await;

        let media: Vec<(u32, Option<String>)> = r
            .interfaces
            .iter()
            .map(|i| (i.ifindex.0, i.if_media.clone()))
            .collect();
        assert!(
            media.contains(&(7, Some("1000BASE-T".to_owned()))),
            "the MAU registration must survive the merge: {media:?}"
        );
        assert!(
            media.contains(&(10101, Some("1000BASE-T".to_owned()))),
            "…and so must the Cisco translation, which now shares the walk: {media:?}"
        );
        assert_eq!(
            fake.asked
                .lock()
                .expect("the ask log is not poisoned")
                .first()
                .map(Vec::len),
            Some(4),
            "one walk asking for all four columns"
        );
    }
}
