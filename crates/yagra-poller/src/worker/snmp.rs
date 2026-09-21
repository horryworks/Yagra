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
use yagra_discovery::{os_version, serial};

/// Identity probes run, by what they found: `version`, `no_version` (the device answered but the
/// table does not cover it or it reports none), `unread` (the device answered but the patch table
/// the version's row walks did not answer to its end, so the version went out without its patch, in
/// a field core writes only where it cannot strip one — ADR-138 Increments 3 and 4) or `no_answer`
/// (not even `sysDescr` came back). Since Increment 5 the probe also runs on a device that answered
/// its scalar GET with no value at all, so `no_answer` counts real traffic: an agent that is there
/// and implements neither the profile's scalars nor `sysDescr`.
/// The ratio of the first two is the table's real coverage of a fleet (ADR-138).
pub(super) const IDENTITY_PROBES_METRIC: &str = "yagra_poll_identity_probes_total";

/// The most rows the identity probe takes from the table columns it walks whole. A patch table
/// holds a handful per slot; the cap is what stops a device that answers with thousands of rows
/// from turning an hourly probe into a table dump.
const IDENTITY_COLUMN_ROWS: usize = 256;

/// Serial-number reads (ADR-147), one per identity probe, by what they found — `serial` (a row
/// carried one), `none` (every walk finished and nothing did: no ENTITY-MIB, or an empty serial) or
/// `unread` (a walk failed or did not finish, so nothing was sent) — and by `source`, the read that
/// decided it: a vendor's own MIB (`juniper`, Increment 2), a Huawei's main boards (`huawei`,
/// Increment 4, counted only when the boards decided), `entity`, or `os_row` — the serial the
/// device's OS-version row names, taken when the chassis rule found none (Increment 6, only
/// `serial`: when it too has none the count stays `entity`/`none`). The first result against the
/// second, per source, is how much of a fleet each rule actually covers.
pub(super) const SERIAL_PROBES_METRIC: &str = "yagra_poll_serial_probes_total";

/// The most rows the serial read takes from ENTITY-MIB's columns together — two, or four on a Huawei
/// (Increments 4 and 5). A chassis with every
/// line card, power supply and transceiver is some hundreds of rows per column; the cap is what
/// keeps an hourly read from turning into a dump of a device listing thousands of sensors. A walk it
/// stops sends no serial rather than a partial list (ADR-147 decision 4).
const SERIAL_ENTITY_ROWS: usize = 4096;

/// The most rows the serial read takes from a vendor's own MIB (ADR-147 Increment 2). A Juniper
/// Virtual Chassis has at most ten members and the box serial is one row, so this is room to spare,
/// not a limit a real device reaches. A walk it stops sends nothing, as the ENTITY-MIB one does.
const SERIAL_VENDOR_ROWS: usize = 64;

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
    /// The device's serial number (ADR-147) — from its vendor's own MIB (a Juniper Virtual Chassis
    /// lists its members, Increment 2) or ENTITY-MIB's chassis rows, a stack's members joined with
    /// `, `. `None` when `sysDescr` did not answer, the walk that decided did not finish, or nothing
    /// carries one.
    pub(super) serial_number: Option<String>,
    /// The device's own model name, at the OID its OS-version row names (ADR-147 Increment 6) —
    /// only Cisco AireOS today. `None` for every other device.
    pub(super) hardware_model: Option<String>,
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
    pub(super) async fn get(
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
    ///
    /// A device that answered `sysDescr` is also asked for its serial number, out of its vendor's own
    /// MIB or ENTITY-MIB's chassis rows (ADR-147) — see [`Self::read_serial_number`].
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
        // Read in the same round trips as the version, so a row naming them costs no extra walk.
        let chassis = os_version::resolve_chassis(sys_object_id, sys_descr.as_deref(), &answers);
        // Only a device that answered `sysDescr` is walked for a serial: one that did not is not
        // going to answer a table, and the walk would spend the probe's time waiting on silence.
        let serial_number = if sys_descr.is_some() {
            self.read_serial_number(
                transport,
                target,
                sys_object_id,
                chassis.serial,
                identity_column_timeout(timeout),
            )
            .await
        } else {
            None
        };
        IdentityProbe {
            os_version,
            os_version_without_patch,
            sys_object_id: sys_object_id.and_then(yagra_discovery::normalize_sys_object_id),
            sys_descr,
            serial_number,
            hardware_model: chassis.model,
            unread,
        }
    }

    /// The serial number (ADR-147): from the vendor's own MIB when [`serial::vendor_read`] knows the
    /// vendor (Increment 2), and otherwise — or when those columns are empty — from ENTITY-MIB,
    /// walking `entPhysicalClass` and `entPhysicalSerialNum` together and letting
    /// [`serial::resolve`] take the chassis rows.
    ///
    /// Nothing is sent unless the walk that decided heard every column out — a stack read halfway
    /// would replace three members' serials with one (decision 4) — and a vendor walk that did not
    /// finish does not fall back to ENTITY-MIB, for the same reason (decision 12). The wait is the
    /// one the patch table gets: a whole-column walk on a slow agent is not a scalar GET.
    ///
    /// A Huawei is walked for `entPhysicalName` and `entPhysicalContainedIn` as well, and its members'
    /// main boards are tried before the chassis rows (Increments 4 and 5) — out of the same walk, so an
    /// unfinished one still sends nothing.
    ///
    /// `row_serial` is what the device's OS-version row names as its serial
    /// ([`os_version::resolve_chassis`]), and it is the last resort (Increment 6): taken only when the
    /// ENTITY-MIB walk was heard out and its chassis rule found nothing. A Cisco AireOS controller is
    /// that case — no class column, and its access points' serials beside its own. A walk that did
    /// not finish still sends nothing, so the fallback cannot overwrite a stack's list either.
    async fn read_serial_number(
        &self,
        transport: &dyn Transport,
        target: IpAddr,
        sys_object_id: Option<&str>,
        row_serial: Option<String>,
        timeout: Duration,
    ) -> Option<String> {
        // One count per probe, under the read that decided it.
        let count = |source: &'static str, result: &'static str| {
            metrics::counter!(SERIAL_PROBES_METRIC, "source" => source, "result" => result)
                .increment(1);
        };
        if let Some(read) = serial::vendor_read(sys_object_id) {
            let columns: Vec<String> = read.columns.iter().map(|c| c.oid().to_owned()).collect();
            let walk = match self
                .walk_instance_columns(transport, target, &columns, timeout, SERIAL_VENDOR_ROWS)
                .await
            {
                Ok(walk) if walk.every_column_answered => walk,
                Ok(_) | Err(_) => {
                    count(read.name, "unread");
                    return None;
                }
            };
            let mut rows: std::collections::BTreeMap<
                String,
                std::collections::BTreeMap<u32, String>,
            > = std::collections::BTreeMap::new();
            for row in walk.rows {
                // A vendor serial column is indexed by one number — a member id, or `0`.
                let [index] = row.instance.as_slice() else {
                    continue;
                };
                let index = *index;
                if let yagra_transport::SnmpValue::Bytes(bytes) = row.value {
                    rows.entry(row.oid_base)
                        .or_default()
                        .insert(index, String::from_utf8_lossy(&bytes).into_owned());
                }
            }
            if let Some(found) = serial::resolve_vendor(read, &rows) {
                count(read.name, "serial");
                return Some(found);
            }
        }
        let mut columns = vec![
            serial::OID_ENT_PHYSICAL_CLASS.to_owned(),
            serial::OID_ENT_PHYSICAL_SERIAL_NUM.to_owned(),
        ];
        if serial::reads_board_names(sys_object_id) {
            columns.push(serial::OID_ENT_PHYSICAL_NAME.to_owned());
            columns.push(serial::OID_ENT_PHYSICAL_CONTAINED_IN.to_owned());
        }
        let walk = match self
            .walk_instance_columns(transport, target, &columns, timeout, SERIAL_ENTITY_ROWS)
            .await
        {
            Ok(walk) if walk.every_column_answered => walk,
            Ok(_) | Err(_) => {
                count("entity", "unread");
                return None;
            }
        };
        let mut classes = std::collections::BTreeMap::new();
        let mut serials = std::collections::BTreeMap::new();
        let mut names = std::collections::BTreeMap::new();
        let mut parents = std::collections::BTreeMap::new();
        for row in walk.rows {
            // Every column is indexed by `entPhysicalIndex` alone; a longer index is not a row of
            // this table.
            let [index] = row.instance.as_slice() else {
                continue;
            };
            let index = *index;
            match row.value {
                yagra_transport::SnmpValue::Int(class)
                    if row.oid_base == serial::OID_ENT_PHYSICAL_CLASS =>
                {
                    classes.insert(index, class);
                }
                yagra_transport::SnmpValue::Bytes(bytes)
                    if row.oid_base == serial::OID_ENT_PHYSICAL_SERIAL_NUM =>
                {
                    serials.insert(index, String::from_utf8_lossy(&bytes).into_owned());
                }
                yagra_transport::SnmpValue::Bytes(bytes)
                    if row.oid_base == serial::OID_ENT_PHYSICAL_NAME =>
                {
                    names.insert(index, String::from_utf8_lossy(&bytes).into_owned());
                }
                // A parent index outside `u32` is not an `entPhysicalIndex`, so it names no row.
                yagra_transport::SnmpValue::Int(parent)
                    if row.oid_base == serial::OID_ENT_PHYSICAL_CONTAINED_IN =>
                {
                    if let Ok(parent) = u32::try_from(parent) {
                        parents.insert(index, parent);
                    }
                }
                // A class that is not an integer, or a serial that is not a string, says nothing.
                _ => {}
            }
        }
        // Only a Huawei was walked for names and parents, so for every other device there are none
        // and this is `None` (Increments 4 and 5). Out of the same walk as the chassis rows, so
        // decision 4 still holds.
        if let Some(found) = serial::resolve_huawei_boards(&classes, &names, &parents, &serials) {
            count("huawei", "serial");
            return Some(found);
        }
        if let Some(found) = serial::resolve(&classes, &serials) {
            count("entity", "serial");
            return Some(found);
        }
        if let Some(found) = row_serial {
            count("os_row", "serial");
            return Some(found);
        }
        count("entity", "none");
        None
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
                let Ok(walk) = transport
                    .snmp_walk_strings(
                        target,
                        community,
                        &columns,
                        WalkLimits::per_round_trip(timeout),
                    )
                    .await
                else {
                    return HashMap::new();
                };
                walk.rows
                    .into_iter()
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

    /// Walk numeric table columns via the appropriate protocol, within `limits`.
    ///
    /// Whether the walk got to ask for every column, and how each ended, travel with the rows — see
    /// [`Transport::snmp_walk`]. Passed straight through rather than consumed here: this type is the
    /// shared funnel, and what a truncation *means* differs per caller (ADR-110 Increment 6).
    pub(super) async fn walk(
        &self,
        transport: &dyn Transport,
        target: IpAddr,
        columns: &[String],
        limits: WalkLimits,
    ) -> Result<TableWalk<SnmpTableSample>, TransportError> {
        match self {
            SnmpWalker::V2c(community) => {
                transport
                    .snmp_walk(target, community, columns, limits)
                    .await
            }
            SnmpWalker::V3(params) => {
                transport
                    .snmp_v3_walk(target, params, columns, limits)
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
    /// rows across columns needs that — the identity probe's patch table (ADR-138 Increment 3), and
    /// the ENTITY-MIB index when it concludes a sensor reaches no port (ADR-158) — so the neighbour,
    /// address, ARP, routing and media walks keep taking the rows alone.
    pub(super) async fn walk_instance_columns(
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

    /// Walk string-valued table columns (interface metadata) via the appropriate protocol, within
    /// `limits`.
    pub(super) async fn walk_strings(
        &self,
        transport: &dyn Transport,
        target: IpAddr,
        columns: &[String],
        limits: WalkLimits,
    ) -> Result<TableWalk<SnmpTableString>, TransportError> {
        match self {
            SnmpWalker::V2c(community) => {
                transport
                    .snmp_walk_strings(target, community, columns, limits)
                    .await
            }
            SnmpWalker::V3(params) => {
                transport
                    .snmp_v3_walk_strings(target, params, columns, limits)
                    .await
            }
        }
    }
}

/// Execute an SNMP scalar-GET check (v2c or v3, selected by `walker`): GET the bare OIDs and the
/// explicitly-named scalar columns together, name each sample (a configured column keeps its metric
/// name and kind; a bare OID falls back to the poller's built-in naming), and run the identity
/// probe when it was asked for and the agent answered — with values or without (ADR-138
/// Increment 5). The transport tells the two apart from a device that answered nothing
/// ([`TransportError::Silent`]), which gets the same `Unreachable` and no probe.
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
            // The agent answered. No values back — every OID it was asked is one it does not
            // implement — is still `Unreachable` and `snmp_up = 0` (ADR-075 decision 3): the
            // scalar set is dead, and the rule that says so is what an operator corrects the
            // profile from. What an empty answer no longer withholds is the identity probe below.
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
            // Whenever the agent answered, not only when it answered with a value (ADR-138
            // Increment 5). A device that implements none of its profile's scalars still says what
            // it is — `sysDescr`, its OS version, its serial — and that is the device an operator
            // most needs identified, to correct its profile. A silent agent never reaches here, so
            // this adds no wait to an outage.
            if job.probe_identity {
                let probe = walker.fetch_identity(transport, job.target, timeout).await;
                metrics::counter!(IDENTITY_PROBES_METRIC, "result" => probe.outcome()).increment(1);
                r.sys_descr = probe.sys_descr;
                r.os_version = probe.os_version;
                r.os_version_without_patch = probe.os_version_without_patch;
                r.sys_object_id = probe.sys_object_id;
                r.serial_number = probe.serial_number;
                r.hardware_model = probe.hardware_model;
            }
            r
        }
        Err(TransportError::Silent(_)) => {
            // Not one OID answered, not even with "no such object": nobody is home. The same
            // `Unreachable` and `snmp_up = 0` an empty answer gets — what a silent agent means for
            // liveness has not changed — but no identity probe, which would only wait on the same
            // silence again. Debug rather than warn: an outage is every device in this state.
            tracing::debug!(job_id = %job.job_id, "snmp agent answered nothing");
            result(
                job,
                at_unix_ms,
                CheckOutcome::Unreachable,
                vec![Sample::gauge(METRIC_SNMP_UP, 0.0)],
            )
        }
        Err(err @ (TransportError::Io(_) | TransportError::Unimplemented(_))) => {
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
        // FakeTransport with no canned SNMP samples: the agent answered and implements none of
        // what it was asked — the real transport's `Ok(vec![])` — which is still unreachable.
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
    /// The two ways an agent gives no value — answering with none, and not answering — read 0
    /// alike (ADR-138 Increment 5 tells them apart for the identity probe, not for this gauge).
    #[tokio::test]
    async fn every_snmp_scalar_result_says_whether_the_agent_answered() {
        use yagra_transport::SnmpSample;
        let answered = FakeTransport::reachable(0.0).with_snmp(vec![SnmpSample {
            oid: "1.3.6.1.2.1.1.3.0".to_owned(),
            value: 123.0,
        }]);
        let r = execute(&snmp_job(), &answered, 1_000).await;
        assert_eq!(sample(&r, METRIC_SNMP_UP), Some(1.0));

        let empty = FakeTransport::reachable(0.0);
        let r = execute(&snmp_job(), &empty, 1_000).await;
        assert_eq!(sample(&r, METRIC_SNMP_UP), Some(0.0));

        let silent = FakeTransport::reachable(0.0).with_silent_snmp_gets();
        let r = execute(&snmp_job(), &silent, 1_000).await;
        assert_eq!(sample(&r, METRIC_SNMP_UP), Some(0.0));
    }

    /// A v3 scalar job with one OID, the shape of the v2c [`snmp_job`].
    fn snmp_v3_job() -> PollJob {
        use yagra_bus::SnmpV3Check;
        PollJob::snmp_v3(
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
        )
    }

    /// v3 goes through the same `execute_scalar_get`, but "the same function" is exactly the claim
    /// that stops being true when someone splits the arms again — so assert it rather than assume.
    #[tokio::test]
    async fn the_v3_scalar_path_reports_the_agent_the_same_way() {
        use yagra_transport::SnmpSample;
        let job = snmp_v3_job();
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
        assert_eq!(
            sample(
                &execute(
                    &job,
                    &FakeTransport::reachable(0.0).with_silent_snmp_gets(),
                    1_000
                )
                .await,
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

    /// ADR-138 Increment 5, the shape of `.210`'s `sim-cisco-n9k`: the agent answers, and
    /// implements none of the profile's scalars. `snmp_up` still says the scalar set is dead, and
    /// the identity probe still runs — the device it would otherwise never identify is exactly the
    /// one whose profile an operator has to correct.
    #[tokio::test]
    async fn an_agent_that_answers_nothing_it_was_asked_is_still_probed_for_identity() {
        let mut job = snmp_job();
        job.probe_identity = true;
        let t = with_entity_rows(
            FakeTransport::reachable(0.0).with_snmp_table_strings(vec![
                string_row(
                    "1.3.6.1.2.1.1.1",
                    0,
                    "Cisco NX-OS(tm) Nexus9000 C93180YC-FX3, Software (NXOS 64-bit), Version 10.5(2)",
                ),
                string_row("1.3.6.1.2.1.1.2", 0, "1.3.6.1.4.1.9.12.3.1.3.2193"),
            ]),
            &[(149, 3, "FDO2750P")],
        );
        let r = execute(&job, &t, 1_000).await;
        assert_eq!(r.outcome, CheckOutcome::Unreachable);
        assert_eq!(sample(&r, METRIC_SNMP_UP), Some(0.0));
        assert!(
            r.sys_descr
                .as_deref()
                .is_some_and(|d| d.starts_with("Cisco NX-OS")),
            "{:?}",
            r.sys_descr
        );
        assert_eq!(r.serial_number.as_deref(), Some("FDO2750P"));
        assert!(walked_for_a_serial(&t), "{:?}", t.asked());
    }

    /// The other side of the same increment: a device that answered nothing is not asked for its
    /// identity — one silent GET is the whole cost of a device that is not there, and the probe
    /// would only add its own timeouts to it. The rows are all there, so only the probe not being
    /// sent explains the missing identity.
    #[tokio::test]
    async fn a_silent_agent_is_not_asked_for_its_identity() {
        let mut job = snmp_job();
        job.probe_identity = true;
        let t = with_entity_rows(
            FakeTransport::reachable(0.0)
                .with_snmp_table_strings(vec![string_row("1.3.6.1.2.1.1.1", 0, "Anything")])
                .with_silent_snmp_gets(),
            &[(1, 3, "SN1")],
        );
        let r = execute(&job, &t, 1_000).await;
        assert_eq!(
            r.outcome,
            CheckOutcome::Unreachable,
            "silence is not an error"
        );
        assert_eq!(sample(&r, METRIC_SNMP_UP), Some(0.0));
        assert!(r.sys_descr.is_none());
        assert_eq!(r.serial_number, None);
        let asked = t.asked();
        assert_eq!(
            asked.len(),
            1,
            "the scalar GET and nothing after it: {asked:?}"
        );
        assert!(!walked_for_a_serial(&t));
    }

    /// A silent agent is `Unreachable` on both protocols, never `Error`: the alert engine reads
    /// `Error` as unknown, and a device that is not there is not unknown — it is down, which is
    /// what an empty answer already said before the transport could tell the two apart.
    #[tokio::test]
    async fn a_silent_snmp_get_is_unreachable_not_an_error() {
        let silent = FakeTransport::reachable(0.0).with_silent_snmp_gets();
        let r = execute(&snmp_job(), &silent, 1_000).await;
        assert_eq!(r.outcome, CheckOutcome::Unreachable);
        assert_eq!(r.samples.len(), 1, "only the agent-health gauge");
        let r = execute(&snmp_v3_job(), &silent, 1_000).await;
        assert_eq!(r.outcome, CheckOutcome::Unreachable);
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

    /// A device the version table does not cover is not asked a second time for a version, and
    /// reports its `sysDescr` with no version. It is still walked once for a serial (ADR-147), which
    /// does not depend on that table.
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
        // The scalar GET, one identity walk and the serial walk (ADR-147) — no version read.
        assert_eq!(t.asked().len(), 3, "{:?}", t.asked());
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
        // The patch table, then the serial read (ADR-147), which takes the same wait.
        assert_eq!(
            t.instance_walk_timeouts(),
            vec![IDENTITY_COLUMN_TIMEOUT, IDENTITY_COLUMN_TIMEOUT]
        );

        let patient = huawei_usg(true);
        walker
            .fetch_identity(&patient, target, Duration::from_secs(8))
            .await;
        assert_eq!(
            patient.instance_walk_timeouts(),
            vec![Duration::from_secs(8), Duration::from_secs(8)]
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

    /// ENTITY-MIB's class and serial columns as `(entPhysicalIndex, class, serial)`, put where the
    /// fake answers instance walks from — the two columns the serial read walks (ADR-147).
    fn with_entity_rows(mut t: FakeTransport, rows: &[(u32, i64, &str)]) -> FakeTransport {
        use yagra_transport::{SnmpInstanceRow, SnmpValue};
        for (index, class, serial_number) in rows {
            t.snmp_instances.push(SnmpInstanceRow {
                oid_base: serial::OID_ENT_PHYSICAL_CLASS.to_owned(),
                instance: vec![*index],
                value: SnmpValue::Int(*class),
            });
            t.snmp_instances.push(SnmpInstanceRow {
                oid_base: serial::OID_ENT_PHYSICAL_SERIAL_NUM.to_owned(),
                instance: vec![*index],
                value: SnmpValue::Bytes(serial_number.as_bytes().to_vec()),
            });
        }
        t
    }

    /// A Catalyst 2960X stack as LibreNMS recorded it (`ios_2960x`, which `.210` replays): the stack
    /// row at index 1 carries no serial, and the three members are the chassis rows.
    fn catalyst_stack() -> FakeTransport {
        let t = FakeTransport::reachable(0.0)
            .with_snmp(vec![SnmpSample {
                oid: "1.3.6.1.2.1.1.3.0".to_owned(),
                value: 1.0,
            }])
            .with_snmp_table_strings(vec![
                string_row(
                    "1.3.6.1.2.1.1.1",
                    0,
                    "Cisco IOS Software, C2960X Software (C2960X-UNIVERSALK9-M), Version 15.0(2a)EX5, RELEASE SOFTWARE (fc3)",
                ),
                string_row("1.3.6.1.2.1.1.2", 0, "1.3.6.1.4.1.9.1.1208"),
            ]);
        with_entity_rows(
            t,
            &[
                (1, 11, ""),
                (1001, 3, "FCW1929B68S"),
                (1002, 9, ""),
                (2001, 3, "FCW1931A06Z"),
                (3001, 3, "FCW1929B6BP"),
            ],
        )
    }

    fn walked_for_a_serial(t: &FakeTransport) -> bool {
        t.asked().iter().any(|call| {
            call.iter()
                .any(|o| o == serial::OID_ENT_PHYSICAL_SERIAL_NUM)
        })
    }

    /// The whole path of ADR-147 on the poller: the probe walks the two columns and the result it
    /// sends core carries every member of the stack, in index order.
    #[tokio::test]
    async fn the_identity_probe_lists_every_member_of_a_stack() {
        let mut job = snmp_job();
        job.probe_identity = true;
        let t = catalyst_stack();
        let r = execute(&job, &t, 1_000).await;
        assert_eq!(
            r.serial_number.as_deref(),
            Some("FCW1929B68S, FCW1931A06Z, FCW1929B6BP")
        );
        assert!(walked_for_a_serial(&t), "{:?}", t.asked());
    }

    /// 🚨 ADR-147 decision 4. The rows are all in hand here on purpose: a read that decided from the
    /// rows would still send three serials, and only the walk's own verdict can stop a half-read
    /// stack from replacing a full one.
    #[tokio::test]
    async fn a_serial_walk_that_did_not_finish_sends_no_serial() {
        let mut job = snmp_job();
        job.probe_identity = true;
        let r = execute(
            &job,
            &catalyst_stack().with_unanswered_instance_columns(),
            1_000,
        )
        .await;
        assert_eq!(r.serial_number, None);
        assert!(r.sys_descr.is_some(), "the device did answer");

        let r = execute(&job, &catalyst_stack().with_silent_instance_walks(), 1_000).await;
        assert_eq!(r.serial_number, None, "a walk that failed outright");
    }

    /// No serial walk for a device that did not answer `sysDescr`, nor for a poll that was not asked
    /// to probe identity — the rows are there both times, so only the walk not being made explains
    /// the missing serial.
    #[tokio::test]
    async fn no_serial_walk_without_an_identity_answer() {
        let mut job = snmp_job();
        job.probe_identity = true;
        // Answers its scalar, but not `sysDescr`.
        let no_sysdescr = with_entity_rows(
            FakeTransport::reachable(0.0).with_snmp(vec![SnmpSample {
                oid: "1.3.6.1.2.1.1.3.0".to_owned(),
                value: 1.0,
            }]),
            &[(1, 3, "SN1")],
        );
        let r = execute(&job, &no_sysdescr, 1_000).await;
        assert_eq!(r.serial_number, None);
        assert!(
            !walked_for_a_serial(&no_sysdescr),
            "{:?}",
            no_sysdescr.asked()
        );

        let unasked = catalyst_stack();
        let r = execute(&snmp_job(), &unasked, 1_000).await;
        assert_eq!(r.serial_number, None);
        assert!(!walked_for_a_serial(&unasked), "{:?}", unasked.asked());
    }

    /// ADR-147 Increment 3, the shape of `.210`'s `sim-cisco-c3560`: the recording has no class
    /// column and one serial — and, like the n9k, no scalar the profile asks for, so this is the
    /// ADR-138 Increment 5 gate too. The fake answers a column with no rows as answered, which is
    /// what the real walker does for a column the agent does not implement, so the walk finishes
    /// and decision 16 applies. The serial row goes in by hand: [`with_entity_rows`] would put a
    /// class row beside it, which is the whole thing this device does not have.
    #[tokio::test]
    async fn a_device_with_no_class_column_and_one_serial_gets_it() {
        use yagra_transport::{SnmpInstanceRow, SnmpValue};
        let mut job = snmp_job();
        job.probe_identity = true;
        let mut t = FakeTransport::reachable(0.0).with_snmp_table_strings(vec![
            string_row(
                "1.3.6.1.2.1.1.1",
                0,
                "Cisco IOS Software, C3560 Software (C3560-IPSERVICESK9-M), Version 12.2(55)SE",
            ),
            string_row("1.3.6.1.2.1.1.2", 0, "1.3.6.1.4.1.9.1.634"),
        ]);
        t.snmp_instances.push(SnmpInstanceRow {
            oid_base: serial::OID_ENT_PHYSICAL_SERIAL_NUM.to_owned(),
            instance: vec![1001],
            value: SnmpValue::Bytes(b"CAT0912N0CU".to_vec()),
        });
        let r = execute(&job, &t, 1_000).await;
        assert_eq!(r.outcome, CheckOutcome::Unreachable, "no scalar answered");
        assert_eq!(r.serial_number.as_deref(), Some("CAT0912N0CU"));
        assert!(walked_for_a_serial(&t), "{:?}", t.asked());
    }

    /// The PoC's Cisco AireOS controller as the identity probe meets it (ADR-147 Increment 6): the
    /// shape walked from it on 2026-09-21, with its serials replaced by made-up ones of the same
    /// form. Row 1 is the controller and rows 8 and 16 two of its
    /// access points, all listed with no `entPhysicalClass` column: the string reads answer the OS
    /// row's instances, and the instance walk answers the serial column alone — which the fake, like
    /// the real walker, treats as heard out.
    fn aireos_controller() -> FakeTransport {
        use yagra_transport::{SnmpInstanceRow, SnmpValue};
        let entity = "1.3.6.1.2.1.47.1.1.1.1";
        let mut t = FakeTransport::reachable(0.0)
            .with_snmp(vec![SnmpSample {
                oid: "1.3.6.1.2.1.1.3.0".to_owned(),
                value: 1.0,
            }])
            .with_snmp_table_strings(vec![
                string_row("1.3.6.1.2.1.1.1", 0, "Cisco Controller"),
                string_row("1.3.6.1.2.1.1.2", 0, "1.3.6.1.4.1.9.1.2427"),
                string_row(&format!("{entity}.10"), 1, "8.5.140.0"),
                string_row(&format!("{entity}.10"), 8, "8.5.140.0"),
                string_row(&format!("{entity}.13"), 1, "AIR-CT3504-K9"),
                string_row(&format!("{entity}.13"), 8, "AIR-AP2802I-Q-K9"),
                string_row(&format!("{entity}.11"), 1, "FCW0000A0AA"),
                string_row(&format!("{entity}.11"), 8, "FGL0000A0AB"),
            ]);
        for (index, serial_number) in [(1, "FCW0000A0AA"), (8, "FGL0000A0AB"), (16, "FGL0000A0AC")]
        {
            t.snmp_instances.push(SnmpInstanceRow {
                oid_base: serial::OID_ENT_PHYSICAL_SERIAL_NUM.to_owned(),
                instance: vec![index],
                value: SnmpValue::Bytes(serial_number.as_bytes().to_vec()),
            });
        }
        t
    }

    /// The chassis rule finds nothing here — three distinct serials and no class column, so
    /// decision 16 declines — and the OS row's `entPhysicalSerialNum.1` is what the node gets. The
    /// model rides in the same probe.
    #[tokio::test]
    async fn an_aireos_controller_gets_the_model_and_serial_its_os_row_names() {
        let mut job = snmp_job();
        job.probe_identity = true;
        let t = aireos_controller();
        let r = execute(&job, &t, 1_000).await;
        assert_eq!(r.os_version.as_deref(), Some("8.5.140.0"));
        assert_eq!(r.hardware_model.as_deref(), Some("AIR-CT3504-K9"));
        assert_eq!(r.serial_number.as_deref(), Some("FCW0000A0AA"));
        assert!(walked_for_a_serial(&t), "{:?}", t.asked());
    }

    /// 🚨 The fallback is a fallback. A serial walk that did not finish sends nothing, row serial or
    /// not (decision 4) — the model still goes, since it never depended on that walk — and a chassis
    /// row the rule can pick wins over the OS row's instance.
    #[tokio::test]
    async fn the_os_rows_serial_never_overrides_the_chassis_rule() {
        let mut job = snmp_job();
        job.probe_identity = true;
        let r = execute(
            &job,
            &aireos_controller().with_unanswered_instance_columns(),
            1_000,
        )
        .await;
        assert_eq!(r.serial_number, None);
        assert_eq!(r.hardware_model.as_deref(), Some("AIR-CT3504-K9"));

        // The same controller, had it a class column naming a chassis row with a serial of its own.
        let t = with_entity_rows(aireos_controller(), &[(2, 3, "CHASSIS-RULE-SN")]);
        let r = execute(&job, &t, 1_000).await;
        assert_eq!(r.serial_number.as_deref(), Some("CHASSIS-RULE-SN"));

        // And a device whose OS row names nothing gets no model from row 1, whatever row 1 says.
        // Added to the stack's own answers (the builder would replace them, and a probe with no
        // `sysDescr` proves nothing about the row).
        let mut t = catalyst_stack();
        t.snmp_table_strings.push(string_row(
            "1.3.6.1.2.1.47.1.1.1.1.13",
            1,
            "WS-C2960X-48FPD-L",
        ));
        let r = execute(&job, &t, 1_000).await;
        assert!(r.sys_descr.is_some(), "the probe ran");
        assert_eq!(r.hardware_model, None);
    }

    /// A Huawei S6730 as the identity probe meets it (ADR-147 Increment 4): `sysDescr` and
    /// `sysObjectID` answered, and ENTITY-MIB rows as `(index, class, name, serial)` — with no
    /// `entPhysicalContainedIn` rows, because S90003CO01's walk did not read that column.
    fn huawei(rows: &[(u32, i64, &str, &str)]) -> FakeTransport {
        huawei_device(
            "S6730-H48X6C\r\nHuawei Versatile Routing Platform Software\r\nVRP (R) software, Version 5.170 (S6730 V200R020C10SPC500)",
            "1.3.6.1.4.1.2011.2.23.291",
            rows,
            &[],
        )
    }

    /// A Huawei with the identity it answers, its ENTITY-MIB rows as `(index, class, name, serial)`,
    /// and `entPhysicalContainedIn` as `(index, parent)` — an `i64`, as the agent sends it.
    fn huawei_device(
        sys_descr: &str,
        sys_object_id: &str,
        rows: &[(u32, i64, &str, &str)],
        parents: &[(u32, i64)],
    ) -> FakeTransport {
        use yagra_transport::{SnmpInstanceRow, SnmpValue};
        let mut t = FakeTransport::reachable(0.0)
            .with_snmp(vec![SnmpSample {
                oid: "1.3.6.1.2.1.1.3.0".to_owned(),
                value: 1.0,
            }])
            .with_snmp_table_strings(vec![
                string_row("1.3.6.1.2.1.1.1", 0, sys_descr),
                string_row("1.3.6.1.2.1.1.2", 0, sys_object_id),
            ]);
        for (index, parent) in parents {
            t.snmp_instances.push(SnmpInstanceRow {
                oid_base: serial::OID_ENT_PHYSICAL_CONTAINED_IN.to_owned(),
                instance: vec![*index],
                value: SnmpValue::Int(*parent),
            });
        }
        for (index, class, name, serial_number) in rows {
            t.snmp_instances.push(SnmpInstanceRow {
                oid_base: serial::OID_ENT_PHYSICAL_CLASS.to_owned(),
                instance: vec![*index],
                value: SnmpValue::Int(*class),
            });
            t.snmp_instances.push(SnmpInstanceRow {
                oid_base: serial::OID_ENT_PHYSICAL_NAME.to_owned(),
                instance: vec![*index],
                value: SnmpValue::Bytes(name.as_bytes().to_vec()),
            });
            t.snmp_instances.push(SnmpInstanceRow {
                oid_base: serial::OID_ENT_PHYSICAL_SERIAL_NUM.to_owned(),
                instance: vec![*index],
                value: SnmpValue::Bytes(serial_number.as_bytes().to_vec()),
            });
        }
        t
    }

    /// The PoC's S90003CO01 as it was walked: one chassis row repeating member 0's serial, and each
    /// member's main board.
    fn huawei_stack() -> FakeTransport {
        huawei(&[
            (67_108_867, 3, "HUAWEI S6730 Routing Switch", "1021A0000448"),
            (67_108_873, 9, "MPU Board 0", "1021A0000448"),
            (67_190_797, 9, "POWER Card 0/PWR1", "21021317408NM0000774"),
            (68_157_449, 9, "MPU Board 1", "1021A0000352"),
        ])
    }

    /// The PoC's S90002ds011 as it was walked on 2026-09-16 (ADR-147 Increment 5): a two-member
    /// CloudEngine S5735-L-V2 on YunShan OS, one chassis row repeating member 1's serial, each
    /// member's main board named for its model and sitting in `MPU slot N`, an empty slot, and a
    /// port carrying its transceiver's serial.
    const CLOUDENGINE_ROWS: &[(u32, i64, &str, &str)] = &[
        (16_777_216, 3, "CloudEngine S5735-L-V2", "QU23C6000037"),
        (16_842_752, 5, "MPU slot 1", ""),
        (16_842_753, 9, "S5735-L8P2T4X-A-V2 1", "QU23C6000037"),
        (16_850_178, 10, "10GE1/0/1", "2000000000529"),
        (16_908_288, 5, "MPU slot 2", ""),
        (16_908_289, 9, "S5735-L8P2T4X-A-V2 2", "QU23C6000056"),
        (16_973_824, 5, "MPU slot 3", ""),
    ];

    fn cloudengine_stack(parents: &[(u32, i64)]) -> FakeTransport {
        huawei_device(
            "Huawei YunShan OS \r\nVersion 1.24.0.1 (S5700 V600R024C00SPC500) \r\nCopyright (C) 2021-2024 Huawei Technologies Co., Ltd. \r\nHUAWEI CloudEngine S5735-L-V2 \r\n",
            "1.3.6.1.4.1.2011.2.23.1078",
            CLOUDENGINE_ROWS,
            parents,
        )
    }

    /// Where each of [`CLOUDENGINE_ROWS`] sits, as walked.
    const CLOUDENGINE_PARENTS: &[(u32, i64)] = &[
        (16_777_216, 0),
        (16_842_752, 16_777_216),
        (16_842_753, 16_842_752),
        (16_850_178, 16_842_753),
        (16_908_288, 16_777_216),
        (16_908_289, 16_908_288),
        (16_973_824, 16_777_216),
    ];

    /// The serial walk asked for `column` — in the same call as the serial column, so a walk made
    /// for some other reason cannot answer for it.
    fn walked_beside_the_serial(t: &FakeTransport, column: &str) -> bool {
        t.asked().iter().any(|call| {
            call.iter().any(|o| o == column)
                && call
                    .iter()
                    .any(|o| o == serial::OID_ENT_PHYSICAL_SERIAL_NUM)
        })
    }

    /// ADR-147 Increment 4: a Huawei stack lists every member's main board, where the chassis rule
    /// showed member 0 alone.
    #[tokio::test]
    async fn a_huawei_stack_lists_every_members_main_board() {
        let mut job = snmp_job();
        job.probe_identity = true;
        let t = huawei_stack();
        let r = execute(&job, &t, 1_000).await;
        assert_eq!(
            r.serial_number.as_deref(),
            Some("1021A0000448, 1021A0000352")
        );
        assert!(
            walked_beside_the_serial(&t, serial::OID_ENT_PHYSICAL_NAME),
            "{:?}",
            t.asked()
        );
    }

    /// ADR-147 Increment 5: a CloudEngine stack, whose boards no name rule recognises, lists every
    /// member by the slot each board sits in — and the containment it needed came out of the same
    /// walk as the serials (decision 22).
    #[tokio::test]
    async fn a_cloudengine_stack_lists_every_members_main_board() {
        let mut job = snmp_job();
        job.probe_identity = true;
        let t = cloudengine_stack(CLOUDENGINE_PARENTS);
        let r = execute(&job, &t, 1_000).await;
        assert_eq!(
            r.serial_number.as_deref(),
            Some("QU23C6000037, QU23C6000056")
        );
        for column in [
            serial::OID_ENT_PHYSICAL_NAME,
            serial::OID_ENT_PHYSICAL_CONTAINED_IN,
        ] {
            assert!(
                walked_beside_the_serial(&t, column),
                "{column}: {:?}",
                t.asked()
            );
        }
    }

    /// A parent the poller cannot read as an `entPhysicalIndex` names no row, so the board is not
    /// seen in its slot and the chassis row decides, as it did before Increment 5.
    #[tokio::test]
    async fn a_containment_outside_the_index_range_leaves_the_chassis_serial() {
        let mut job = snmp_job();
        job.probe_identity = true;
        let unreadable: Vec<(u32, i64)> = CLOUDENGINE_PARENTS
            .iter()
            .map(|(index, parent)| {
                let parent = if *index == 16_842_753 || *index == 16_908_289 {
                    -1
                } else {
                    *parent
                };
                (*index, parent)
            })
            .collect();
        let r = execute(&job, &cloudengine_stack(&unreadable), 1_000).await;
        assert_eq!(r.serial_number.as_deref(), Some("QU23C6000037"));
    }

    /// Decisions 17 and 22: only a Huawei pays for the name and containment columns.
    #[tokio::test]
    async fn a_device_that_is_not_a_huawei_is_not_walked_for_names() {
        let mut job = snmp_job();
        job.probe_identity = true;
        let t = catalyst_stack();
        let r = execute(&job, &t, 1_000).await;
        assert!(r.serial_number.is_some());
        assert!(walked_for_a_serial(&t), "{:?}", t.asked());
        for column in [
            serial::OID_ENT_PHYSICAL_NAME,
            serial::OID_ENT_PHYSICAL_CONTAINED_IN,
        ] {
            assert!(
                !walked_beside_the_serial(&t, column),
                "{column}: {:?}",
                t.asked()
            );
        }
    }

    /// 🚨 Decision 20 (decision 4 again): every row is in hand, so only the walk's own verdict stops
    /// a half-read stack from sending one member's serial — in either board shape.
    #[tokio::test]
    async fn an_unfinished_huawei_walk_sends_no_serial() {
        let mut job = snmp_job();
        job.probe_identity = true;
        for (shape, device) in [
            ("MPU Board", huawei_stack as fn() -> FakeTransport),
            ("MPU slot", || cloudengine_stack(CLOUDENGINE_PARENTS)),
        ] {
            let r = execute(&job, &device().with_unanswered_instance_columns(), 1_000).await;
            assert_eq!(r.serial_number, None, "{shape}");
            assert!(r.sys_descr.is_some(), "{shape}: the device did answer");

            let r = execute(&job, &device().with_silent_instance_walks(), 1_000).await;
            assert_eq!(
                r.serial_number, None,
                "{shape}: a walk that failed outright"
            );
        }
    }

    /// A Huawei whose main board carries no serial is read by the chassis rule, as before.
    #[tokio::test]
    async fn a_huawei_with_no_main_board_serial_keeps_its_chassis_serial() {
        let mut job = snmp_job();
        job.probe_identity = true;
        let t = huawei(&[
            (
                67_108_867,
                3,
                "HUAWEI S5720 Routing Switch",
                "2102359576DMHC000120",
            ),
            (67_108_873, 9, "MPU Board 0", ""),
        ]);
        let r = execute(&job, &t, 1_000).await;
        assert_eq!(r.serial_number.as_deref(), Some("2102359576DMHC000120"));
    }

    /// A Juniper device as the identity probe meets it (ADR-147 Increment 2): `sysDescr` and
    /// `sysObjectID` answered, the Virtual Chassis serial column as `(member id, serial)` rows, and
    /// the box serial at `.0` when there is one.
    fn juniper(
        sys_object_id: &str,
        members: &[(u32, &str)],
        box_serial: Option<&str>,
    ) -> FakeTransport {
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
                    "Juniper Networks, Inc. vmx internet router, kernel JUNOS 18.2R1.9, Build date: 2018-06-28 04:23:52 UTC Copyright (c) 1996-2018 Juniper Networks, Inc.",
                ),
                string_row("1.3.6.1.2.1.1.2", 0, sys_object_id),
            ]);
        let row = |oid_base: &str, index: u32, value: &str| SnmpInstanceRow {
            oid_base: oid_base.to_owned(),
            instance: vec![index],
            value: SnmpValue::Bytes(value.as_bytes().to_vec()),
        };
        for (member, serial_number) in members {
            t.snmp_instances.push(row(
                serial::OID_JNX_VC_MEMBER_SERIAL,
                *member,
                serial_number,
            ));
        }
        if let Some(serial_number) = box_serial {
            t.snmp_instances
                .push(row(serial::OID_JNX_BOX_SERIAL, 0, serial_number));
        }
        t
    }

    /// LibreNMS's `junos_ex4600mp`, which `.210` replays as `sim-juniper-ex`: eight members, and a
    /// box serial that is only member 0's.
    fn juniper_virtual_chassis() -> FakeTransport {
        let members: Vec<(u32, String)> = (0..8)
            .map(|m| (m, format!("XR01234567{}", 89 + m)))
            .collect();
        let borrowed: Vec<(u32, &str)> = members.iter().map(|(m, s)| (*m, s.as_str())).collect();
        juniper(
            "1.3.6.1.4.1.2636.1.1.1.4.63.9",
            &borrowed,
            Some("XR0123456789"),
        )
    }

    fn walked_juniper_columns(t: &FakeTransport) -> bool {
        t.asked()
            .iter()
            .any(|call| call.iter().any(|o| o == serial::OID_JNX_BOX_SERIAL))
    }

    /// The Virtual Chassis lists every member in member-id order, and ENTITY-MIB is not walked at
    /// all — a chassis row put there on purpose would otherwise have been a second answer.
    #[tokio::test]
    async fn a_virtual_chassis_lists_every_member_and_skips_entity_mib() {
        let mut job = snmp_job();
        job.probe_identity = true;
        let t = with_entity_rows(juniper_virtual_chassis(), &[(1, 3, "ENTITY-SN")]);
        let r = execute(&job, &t, 1_000).await;
        assert_eq!(
            r.serial_number.as_deref(),
            Some(
                "XR0123456789, XR0123456790, XR0123456791, XR0123456792, \
                 XR0123456793, XR0123456794, XR0123456795, XR0123456796"
            )
        );
        assert!(walked_juniper_columns(&t), "{:?}", t.asked());
        assert!(!walked_for_a_serial(&t), "{:?}", t.asked());
    }

    /// LibreNMS's `junos_vmx`, which `.210` replays as `sim-junos-vmx`.
    #[tokio::test]
    async fn a_juniper_box_outside_a_virtual_chassis_gives_its_box_serial() {
        let mut job = snmp_job();
        job.probe_identity = true;
        let t = juniper("1.3.6.1.4.1.2636.1.1.1.2.108", &[], Some("VM600B272BD3"));
        let r = execute(&job, &t, 1_000).await;
        assert_eq!(r.serial_number.as_deref(), Some("VM600B272BD3"));
    }

    /// 🚨 ADR-147 decision 12. Every row is in hand, and ENTITY-MIB has a chassis serial too: only
    /// the walk's own verdict stops a half-read Virtual Chassis from being replaced — by the box
    /// serial, or by an ENTITY-MIB fallback.
    #[tokio::test]
    async fn an_unfinished_juniper_walk_sends_nothing_and_does_not_fall_back() {
        let mut job = snmp_job();
        job.probe_identity = true;
        let unanswered = with_entity_rows(juniper_virtual_chassis(), &[(1, 3, "ENTITY-SN")])
            .with_unanswered_instance_columns();
        let r = execute(&job, &unanswered, 1_000).await;
        assert_eq!(r.serial_number, None);
        assert!(r.sys_descr.is_some(), "the device did answer");
        assert!(
            !walked_for_a_serial(&unanswered),
            "{:?}",
            unanswered.asked()
        );

        let silent = with_entity_rows(juniper_virtual_chassis(), &[(1, 3, "ENTITY-SN")])
            .with_silent_instance_walks();
        let r = execute(&job, &silent, 1_000).await;
        assert_eq!(r.serial_number, None, "a walk that failed outright");
        assert!(!walked_for_a_serial(&silent), "{:?}", silent.asked());
    }

    /// ADR-147 decision 13: a Juniper device whose own MIB has nothing is read exactly as before.
    #[tokio::test]
    async fn a_juniper_device_with_nothing_in_its_own_mib_falls_back_to_entity_rows() {
        let mut job = snmp_job();
        job.probe_identity = true;
        let t = with_entity_rows(
            juniper("1.3.6.1.4.1.2636.1.1.1.2.108", &[], None),
            &[(1, 3, "ENTITY-SN")],
        );
        let r = execute(&job, &t, 1_000).await;
        assert_eq!(r.serial_number.as_deref(), Some("ENTITY-SN"));
        assert!(walked_juniper_columns(&t), "{:?}", t.asked());
        assert!(walked_for_a_serial(&t), "{:?}", t.asked());
    }

    #[tokio::test]
    async fn a_non_juniper_device_is_not_asked_juniper_columns() {
        let mut job = snmp_job();
        job.probe_identity = true;
        let t = catalyst_stack();
        let r = execute(&job, &t, 1_000).await;
        assert_eq!(
            r.serial_number.as_deref(),
            Some("FCW1929B68S, FCW1931A06Z, FCW1929B6BP")
        );
        assert!(!walked_juniper_columns(&t), "{:?}", t.asked());
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
