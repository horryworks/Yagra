// SPDX-License-Identifier: AGPL-3.0-only
//! Optical transceiver (DDM/DOM) reading: the vendor dialects and the pure maths (ADR-062).
//!
//! Everything here is a pure function over already-walked rows. The SNMP sessions live in
//! [`crate::worker`]; this module holds the parts worth testing, which is all of the parts that
//! can be wrong in a way nobody would notice:
//!
//! - **the scale**, because a wrong factor plots a smooth, believable, wrong line;
//! - **the transmit/receive split**, which for the vendor-neutral dialect can only be read out of
//!   a free-text description the device chose;
//! - **the index translation**, because most vendors report optical power against
//!   `entPhysicalIndex` (the transceiver is a *part*) and everything downstream of the bus is
//!   keyed by ifIndex.
//!
//! ⚠️ **None of this has been exercised against a real transceiver.** The lab has no optical
//! module, so the guards below are deliberately conservative: an unrecognised description, an
//! untranslatable index, or an implausible number is **dropped**, never guessed at. A gap in a
//! chart is a fact; a wrong dBm figure is a lie that looks like a measurement.

use std::collections::HashMap;
use yagra_common::{is_plausible_dbm, OpticalFlavor};
use yagra_transport::{SnmpInstanceRow, SnmpValue};

/// Which of the two readings a row carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OpticalReading {
    /// Receive power — what the far end's laser is delivering to this port.
    Rx,
    /// Transmit power — what this port's own laser is emitting.
    Tx,
}

// ── ENTITY-SENSOR-MIB / ENTITY-MIB column bases (the vendor-neutral dialect) ────────────

/// `entPhySensorType` — which quantity the row measures.
pub const ENT_SENSOR_TYPE: &str = "1.3.6.1.2.1.99.1.1.1.1";
/// `entPhySensorScale` — the power-of-ten prefix the raw value is expressed in.
pub const ENT_SENSOR_SCALE: &str = "1.3.6.1.2.1.99.1.1.1.2";
/// `entPhySensorPrecision` — how many decimal places the raw integer carries.
pub const ENT_SENSOR_PRECISION: &str = "1.3.6.1.2.1.99.1.1.1.3";
/// `entPhySensorValue` — the raw integer reading.
pub const ENT_SENSOR_VALUE: &str = "1.3.6.1.2.1.99.1.1.1.4";
/// `entPhysicalDescr` — the free text that says whether a sensor is the transmit or receive one.
pub const ENT_PHYSICAL_DESCR: &str = "1.3.6.1.2.1.47.1.1.1.1.2";
/// `entPhysicalName` — the other free text, checked when the description is unhelpful.
pub const ENT_PHYSICAL_NAME: &str = "1.3.6.1.2.1.47.1.1.1.1.7";
/// `entPhysicalContainedIn` — the parent entity, walked to climb from a sensor to its port.
pub const ENT_PHYSICAL_CONTAINED_IN: &str = "1.3.6.1.2.1.47.1.1.1.1.4";
/// `entAliasMappingIdentifier` — the standard `entPhysicalIndex` → ifIndex bridge. Indexed by
/// `(entPhysicalIndex, entAliasLogicalIndexOrZero)`; the value is an OID whose last sub-identifier
/// is the ifIndex.
pub const ENT_ALIAS_MAPPING: &str = "1.3.6.1.2.1.47.1.3.2.1.2";

/// `entPhySensorType` value for a reading already expressed in dBm.
///
/// ⚠️ **RFC 3433's own enumeration stops at `truthvalue(12)`** — `dBm(14)` comes from Cisco's
/// parallel `SensorDataType` and is what implementers converged on for optics. It is accepted
/// here because it is what devices send, not because the base RFC blesses it.
const SENSOR_TYPE_DBM: i64 = 14;
/// `entPhySensorType` value for a reading in watts, which some agents use for optical power
/// instead of dBm. Converted rather than rejected — see [`entity_sensor_dbm`].
const SENSOR_TYPE_WATTS: i64 = 6;
/// `entPhySensorScale` value meaning "no prefix" (`units(9)`), the zero point of the exponent.
const SENSOR_SCALE_UNITS: i64 = 9;

/// How many `entPhysicalContainedIn` hops to climb looking for an entity with an ifIndex.
///
/// A transceiver sensor is typically `sensor → transceiver → port`, so two hops reaches the port
/// that owns the alias mapping. The bound exists because `entPhysicalContainedIn` is
/// device-supplied and has been seen to contain cycles; without it a malformed chassis table
/// hangs the poller rather than losing one metric.
const MAX_CONTAINMENT_HOPS: usize = 4;

// ── The single-column dialects ─────────────────────────────────────────────────────────

/// A dialect that answers with two plain numeric columns and a fixed multiplier.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SimpleDialect {
    /// Column base carrying receive power.
    pub rx_oid: &'static str,
    /// Column base carrying transmit power.
    pub tx_oid: &'static str,
    /// Multiplier taking the raw integer to dBm.
    pub scale: f64,
    /// The module's own acceptable-power window, when the dialect publishes one.
    ///
    /// `None` for a dialect that does not. **The vendor-neutral spelling is the notable absence**:
    /// RFC 3433 defines no threshold objects at all, and Cisco's are in a separate
    /// `entSensorThresholdTable` with its own row shape — so the standards-based dialect draws a
    /// line and no band, which is a degradation the chart handles rather than a gap to work around.
    pub limits: Option<DialectLimits>,
}

/// The four threshold columns of a dialect that publishes its module's operating window.
///
/// Same `scale` as the readings they bound — every vendor that publishes both puts them in the
/// same table with the same units, and a mismatch here would draw a band in the wrong place, which
/// is worse than no band because it accuses a healthy link.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DialectLimits {
    /// Lowest acceptable receive power.
    pub rx_low_oid: &'static str,
    /// Highest acceptable receive power.
    pub rx_high_oid: &'static str,
    /// Lowest acceptable transmit power.
    pub tx_low_oid: &'static str,
    /// Highest acceptable transmit power.
    pub tx_high_oid: &'static str,
}

/// The column layout for a dialect that needs no correlation, or `None` for
/// [`OpticalFlavor::EntitySensor`], which does.
///
/// All three report hundredths of a dBm, which is the convention every vendor DOM MIB settled on;
/// the multiplier is spelled per dialect anyway so a vendor that differs is one number, not a
/// special case.
#[must_use]
pub fn simple_dialect(flavor: OpticalFlavor) -> Option<SimpleDialect> {
    match flavor {
        // Rows are keyed by entPhysicalIndex and need translating; the columns themselves are plain.
        // Thresholds verified against HUAWEI-ENTITY-EXTENT-MIB itself: same table, same ×100.
        OpticalFlavor::Huawei => Some(SimpleDialect {
            rx_oid: "1.3.6.1.4.1.2011.5.25.31.1.1.3.1.8",
            tx_oid: "1.3.6.1.4.1.2011.5.25.31.1.1.3.1.9",
            scale: 0.01,
            limits: Some(DialectLimits {
                rx_low_oid: "1.3.6.1.4.1.2011.5.25.31.1.1.3.1.13",
                rx_high_oid: "1.3.6.1.4.1.2011.5.25.31.1.1.3.1.14",
                tx_low_oid: "1.3.6.1.4.1.2011.5.25.31.1.1.3.1.15",
                tx_high_oid: "1.3.6.1.4.1.2011.5.25.31.1.1.3.1.16",
            }),
        }),
        // JUNIPER-DOM-MIB publishes both an alarm and a warning window; the **alarm** one is taken,
        // because the band is meant to say "outside this, the link is in trouble" rather than
        // "outside this, the module would like you to know".
        OpticalFlavor::Juniper => Some(SimpleDialect {
            rx_oid: "1.3.6.1.4.1.2636.3.18.1.1.1.5",
            tx_oid: "1.3.6.1.4.1.2636.3.18.1.1.1.7",
            scale: 0.01,
            limits: Some(DialectLimits {
                rx_low_oid: "1.3.6.1.4.1.2636.3.18.1.1.1.10",
                rx_high_oid: "1.3.6.1.4.1.2636.3.18.1.1.1.9",
                tx_low_oid: "1.3.6.1.4.1.2636.3.18.1.1.1.18",
                tx_high_oid: "1.3.6.1.4.1.2636.3.18.1.1.1.17",
            }),
        }),
        // ⚠️ No band: HH3C-TRANSCEIVER-INFO-MIB was not obtainable to confirm its threshold column
        // numbers, and a threshold OID pointing at the wrong column draws a window that accuses a
        // healthy link. The readings themselves are confirmed, so this dialect gets its two lines
        // and no shading — the degradation the chart is built to handle.
        OpticalFlavor::H3c => Some(SimpleDialect {
            rx_oid: "1.3.6.1.4.1.25506.2.70.1.1.1.12",
            tx_oid: "1.3.6.1.4.1.25506.2.70.1.1.1.11",
            scale: 0.01,
            limits: None,
        }),
        OpticalFlavor::EntitySensor => None,
    }
}

/// The module's acceptable window for one direction, after validation.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct OpticalWindow {
    /// Lowest acceptable receive power, dBm.
    pub rx_low: Option<f64>,
    /// Highest acceptable receive power, dBm.
    pub rx_high: Option<f64>,
    /// Lowest acceptable transmit power, dBm.
    pub tx_low: Option<f64>,
    /// Highest acceptable transmit power, dBm.
    pub tx_high: Option<f64>,
}

impl OpticalWindow {
    /// Whether anything survived validation and is worth sending.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rx_low.is_none()
            && self.rx_high.is_none()
            && self.tx_low.is_none()
            && self.tx_high.is_none()
    }
}

/// Accept a low/high pair only if it describes a window that could actually be a module's.
///
/// 🚨 **Vendors get this wrong in the field.** Huawei and Comware have been reported returning
/// nonsense limits (LibreNMS #15424), and a bogus window is worse than none: it paints a healthy
/// link as out of spec, which is the kind of false alarm that teaches an operator to ignore the
/// chart. Both ends must be plausible dBm *and* the low end must actually be below the high end —
/// a pair that fails either test is dropped as a pair, never half-kept, since half a window
/// implies a bound the module never claimed.
#[must_use]
pub fn validated_window(low: Option<f64>, high: Option<f64>) -> (Option<f64>, Option<f64>) {
    match (low, high) {
        (Some(l), Some(h)) if is_plausible_dbm(l) && is_plausible_dbm(h) && l < h => {
            (Some(l), Some(h))
        }
        // A module that publishes only one end is legitimate; it just has to be a real number.
        (Some(l), None) if is_plausible_dbm(l) => (Some(l), None),
        (None, Some(h)) if is_plausible_dbm(h) => (None, Some(h)),
        _ => (None, None),
    }
}

// ── The vendor-neutral dialect: value maths and the Rx/Tx split ────────────────────────

/// Combine an ENTITY-SENSOR row into dBm, or `None` when the row is not an optical power reading.
///
/// `entPhySensorValue` is a raw integer whose meaning needs two other columns: `scale` is a
/// power-of-ten prefix (`units(9)` is the zero point, each step is 10³) and `precision` is how
/// many decimal places the integer carries. A `watts` row is converted; a row of any other type
/// is not an optical power reading and is rejected.
///
/// ⚠️ **A watts reading of zero is `None`, not `-inf`.** No light is a gap in the chart, which is
/// what "the receiver sees nothing" honestly looks like — a floor value would be indistinguishable
/// from a very weak but real signal.
#[must_use]
pub fn entity_sensor_dbm(value: i64, sensor_type: i64, scale: i64, precision: i64) -> Option<f64> {
    // `precision` is documented as -8..=9; clamp rather than trust, since 10^-300 is a silent zero.
    let precision = precision.clamp(-8, 9);
    let exponent = (scale - SENSOR_SCALE_UNITS) * 3 - precision;
    let magnitude = powi10(exponent)?;
    let scaled = value as f64 * magnitude;
    match sensor_type {
        SENSOR_TYPE_DBM => Some(scaled),
        // 10·log₁₀(P in milliwatts). A non-positive reading carries no logarithm.
        SENSOR_TYPE_WATTS if scaled > 0.0 => Some(10.0 * (scaled * 1000.0).log10()),
        _ => None,
    }
}

/// `10^exp` for the exponent range the MIB can express, or `None` outside it.
fn powi10(exp: i64) -> Option<f64> {
    // yocto..yotta is (1-9)*3 = -24 .. (17-9)*3 = 24, and precision widens that by 9 either way.
    (-40..=40).contains(&exp).then(|| 10f64.powi(exp as i32))
}

/// Read a transmit/receive split out of an entity's free text, or `None` when it says neither.
///
/// This is the weakest link in the whole feature and it is deliberately strict: the text must
/// name a *power* reading and exactly one direction. A description naming both, or neither, or
/// naming a bias current or a temperature, is rejected rather than guessed at — being wrong here
/// swaps the two lines on the chart, which is the kind of defect that survives review because
/// both lines look plausible.
///
/// Real examples this must accept:
/// - `Te1/1/1 Transmit Power Sensor` (Cisco IOS-XE)
/// - `Ethernet1/1 Lane 1 Transceiver Receive Power Sensor` (Cisco NX-OS)
/// - `Ethernet1 Rx Power Sensor` (Arista)
#[must_use]
pub fn reading_from_text(text: &str) -> Option<OpticalReading> {
    let lower = text.to_ascii_lowercase();
    if !lower.contains("power") {
        return None;
    }
    // A bias current is measured in amperes but is sometimes described as "laser bias power".
    if lower.contains("bias") || lower.contains("temperature") || lower.contains("voltage") {
        return None;
    }
    // Whole-word `rx`/`tx` only: substring matching would fire on any word containing them.
    let has_word = |w: &str| {
        lower
            .split(|c: char| !c.is_ascii_alphanumeric())
            .any(|tok| tok == w)
    };
    let rx = lower.contains("receive") || has_word("rx");
    let tx = lower.contains("transmit") || has_word("tx");
    match (rx, tx) {
        (true, false) => Some(OpticalReading::Rx),
        (false, true) => Some(OpticalReading::Tx),
        // Both or neither: the text does not identify one direction, so it identifies none.
        _ => None,
    }
}

// ── entPhysicalIndex → ifIndex ─────────────────────────────────────────────────────────

/// The two ENTITY-MIB relations needed to attach a sensor to an interface.
#[derive(Debug, Default, Clone)]
pub struct EntityIndex {
    /// `entPhysicalIndex` → ifIndex, from `entAliasMappingIdentifier`.
    pub alias: HashMap<u32, u32>,
    /// `entPhysicalIndex` → parent `entPhysicalIndex`, from `entPhysicalContainedIn`.
    pub parent: HashMap<u32, u32>,
}

impl EntityIndex {
    /// The ifIndex an entity belongs to, climbing containment until one is found.
    ///
    /// Returns `None` when nothing in the chain maps to an interface — a sensor on a fan tray,
    /// say, or a device whose `entAliasMappingTable` is empty. **The caller must drop the row**;
    /// publishing it under its raw `entPhysicalIndex` would land on `MetricDimension::Entity`,
    /// costing a stored series that no chart can ever show (ADR-062 decision 3).
    #[must_use]
    pub fn ifindex_for(&self, mut ent: u32) -> Option<u32> {
        for _ in 0..MAX_CONTAINMENT_HOPS {
            if let Some(&ifidx) = self.alias.get(&ent) {
                return Some(ifidx);
            }
            match self.parent.get(&ent) {
                // `entPhysicalContainedIn` is 0 at the chassis root, and a self-reference is a
                // malformed table that would otherwise spin until the hop budget ran out.
                Some(&p) if p != 0 && p != ent => ent = p,
                _ => return None,
            }
        }
        None
    }

    /// Build the alias half from an `entAliasMappingIdentifier` walk.
    ///
    /// The table is indexed by `(entPhysicalIndex, entAliasLogicalIndexOrZero)` and its value is
    /// an OID pointing at an `ifIndex` instance — so the entity is the *first* sub-identifier of
    /// the instance and the interface is the *last* sub-identifier of the value.
    pub fn add_alias_rows(&mut self, rows: &[SnmpInstanceRow]) {
        for row in rows {
            let Some(&ent) = row.instance.first() else {
                continue;
            };
            let SnmpValue::Oid(target) = &row.value else {
                continue;
            };
            let Some(ifindex) = target
                .rsplit('.')
                .next()
                .and_then(|s| s.parse::<u32>().ok())
            else {
                continue;
            };
            // Keep the first mapping: a port with several logical rows names the same ifIndex,
            // and entry order is the agent's ascending index order.
            self.alias.entry(ent).or_insert(ifindex);
        }
    }

    /// Build the containment half from an `entPhysicalContainedIn` walk.
    pub fn add_parent_rows(&mut self, rows: &[SnmpInstanceRow]) {
        for row in rows {
            let (Some(&ent), SnmpValue::Int(parent)) = (row.instance.first(), &row.value) else {
                continue;
            };
            if let Ok(p) = u32::try_from(*parent) {
                self.parent.insert(ent, p);
            }
        }
    }
}

// ── Assembling the samples ─────────────────────────────────────────────────────────────

/// One accepted reading, ready to become a [`yagra_bus::Sample`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OpticalSample {
    /// The interface this reading belongs to.
    pub ifindex: u32,
    /// Which of the two readings it is.
    pub reading: OpticalReading,
    /// The value, in dBm.
    pub dbm: f64,
}

/// Collapse candidate readings into at most one per `(ifindex, reading)`.
///
/// Input is expected in ascending source-index order, which for a multi-lane module is lane
/// order. **The first row wins and the rest are dropped**: a QSFP reports one row per lane, and
/// there is no aggregate of four lanes that is both correct and unsurprising — summing needs
/// milliwatts, the minimum is dominated by an unused lane, and the maximum hides a failing one.
/// Reporting lane 1 is a stated, checkable rule; per-lane series are a named non-goal (ADR-062).
///
/// Implausible values are refused here rather than at the edges, so every dialect gets the guard:
/// a scale applied wrongly lands far outside a transceiver's operating window, and dropping it is
/// what stops a 100×-off number from being drawn as a credible line.
#[must_use]
pub fn dedupe_readings(candidates: impl IntoIterator<Item = OpticalSample>) -> Vec<OpticalSample> {
    let mut seen: Vec<OpticalSample> = Vec::new();
    for c in candidates {
        if !is_plausible_dbm(c.dbm) {
            continue;
        }
        if seen
            .iter()
            .any(|s| s.ifindex == c.ifindex && s.reading == c.reading)
        {
            continue;
        }
        seen.push(c);
    }
    seen
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_flavor_has_a_root_and_only_entity_sensor_needs_correlation() {
        for f in OpticalFlavor::ALL {
            assert!(!f.root_oid().is_empty(), "{f:?} has no root OID");
            let simple = simple_dialect(f);
            assert_eq!(
                simple.is_none(),
                f == OpticalFlavor::EntitySensor,
                "{f:?}: only the ENTITY-SENSOR dialect correlates columns"
            );
            // A simple dialect's columns must sit under the root that selects it, or the poller
            // would walk a table the operator did not ask for.
            if let Some(d) = simple {
                assert!(d.rx_oid.starts_with(f.root_oid()), "{f:?} rx outside root");
                assert!(d.tx_oid.starts_with(f.root_oid()), "{f:?} tx outside root");
                assert_ne!(d.rx_oid, d.tx_oid, "{f:?} rx and tx are the same column");
            }
        }
    }

    #[test]
    fn a_dialects_threshold_columns_sit_under_its_own_root_and_are_all_distinct() {
        for f in OpticalFlavor::ALL {
            let Some(d) = simple_dialect(f) else { continue };
            let Some(l) = d.limits else { continue };
            let cols = [l.rx_low_oid, l.rx_high_oid, l.tx_low_oid, l.tx_high_oid];
            for c in cols {
                assert!(
                    c.starts_with(f.root_oid()),
                    "{f:?}: {c} is outside the dialect's root"
                );
            }
            let mut sorted = cols.to_vec();
            sorted.sort_unstable();
            let before = sorted.len();
            sorted.dedup();
            assert_eq!(before, sorted.len(), "{f:?} reuses a threshold column");
            // A threshold column must not be one of the reading columns: pointing a bound at the
            // live value would draw a band that tracks the line and always looks satisfied.
            assert!(
                !cols.contains(&d.rx_oid),
                "{f:?}: a bound points at the rx reading"
            );
            assert!(
                !cols.contains(&d.tx_oid),
                "{f:?}: a bound points at the tx reading"
            );
        }
    }

    #[test]
    fn a_plausible_window_survives_validation() {
        // ⚠️ The accepting case first, and deliberately: a validator that refused everything would
        // satisfy every rejection test below and ship a feature that never draws a band.
        assert_eq!(
            validated_window(Some(-24.0), Some(-3.0)),
            (Some(-24.0), Some(-3.0))
        );
        // A module that publishes only one end is legitimate.
        assert_eq!(validated_window(Some(-24.0), None), (Some(-24.0), None));
        assert_eq!(validated_window(None, Some(-3.0)), (None, Some(-3.0)));
        assert_eq!(validated_window(None, None), (None, None));
    }

    #[test]
    fn a_nonsense_window_is_dropped_as_a_pair() {
        // 🚨 Reported for real on Huawei/Comware (LibreNMS #15424). Half-keeping a bad pair would
        // imply a bound the module never claimed, so both ends go.
        assert_eq!(
            validated_window(Some(-3.0), Some(-24.0)),
            (None, None),
            "inverted"
        );
        assert_eq!(
            validated_window(Some(-7.0), Some(-7.0)),
            (None, None),
            "zero width"
        );
        // A ×100 scale error, which is what forgetting the divisor produces.
        assert_eq!(
            validated_window(Some(-2400.0), Some(-300.0)),
            (None, None),
            "off scale"
        );
        assert_eq!(
            validated_window(Some(f64::NAN), Some(-3.0)),
            (None, None),
            "not a number"
        );
    }

    #[test]
    fn an_empty_window_is_recognised_so_nothing_pointless_is_sent() {
        assert!(OpticalWindow::default().is_empty());
        assert!(!OpticalWindow {
            rx_low: Some(-24.0),
            ..Default::default()
        }
        .is_empty());
    }

    #[test]
    fn root_oids_are_distinct_so_a_flavor_is_unambiguous() {
        let mut roots: Vec<&str> = OpticalFlavor::ALL.iter().map(|f| f.root_oid()).collect();
        roots.sort_unstable();
        let before = roots.len();
        roots.dedup();
        assert_eq!(before, roots.len(), "two flavors share a root OID");
        for f in OpticalFlavor::ALL {
            assert_eq!(OpticalFlavor::from_root(f.root_oid()), Some(f));
        }
        assert_eq!(OpticalFlavor::from_root("1.2.3"), None);
    }

    #[test]
    fn dbm_rows_pass_through_scale_and_precision() {
        // units(9), 2 decimal places: -742 → -7.42 dBm, the shape every vendor DOM MIB uses.
        assert_eq!(entity_sensor_dbm(-742, 14, 9, 2), Some(-7.42));
        // No precision at all.
        assert_eq!(entity_sensor_dbm(-7, 14, 9, 0), Some(-7.0));
        // A milli(8) scale shifts three decades down.
        let v = entity_sensor_dbm(-7420, 14, 8, 0).expect("milli scale");
        assert!((v - -7.42).abs() < 1e-9, "milli scale gave {v}");
    }

    #[test]
    fn watt_rows_become_dbm_and_darkness_becomes_a_gap() {
        // 1 mW = 0 dBm. micro(7) scale, no precision: 1000 µW.
        let v = entity_sensor_dbm(1000, 6, 7, 0).expect("1 mW");
        assert!(v.abs() < 1e-9, "1 mW should be 0 dBm, got {v}");
        // 0.1 mW = -10 dBm.
        let v = entity_sensor_dbm(100, 6, 7, 0).expect("0.1 mW");
        assert!(
            (v - -10.0).abs() < 1e-9,
            "0.1 mW should be -10 dBm, got {v}"
        );
        // No light: no logarithm, and therefore no sample.
        assert_eq!(entity_sensor_dbm(0, 6, 7, 0), None);
        assert_eq!(entity_sensor_dbm(-5, 6, 7, 0), None);
    }

    #[test]
    fn a_row_that_is_not_optical_power_is_refused() {
        // celsius(8) — a temperature sensor sitting in the same table.
        assert_eq!(entity_sensor_dbm(41, 8, 9, 0), None);
        // amperes(5) — laser bias current.
        assert_eq!(entity_sensor_dbm(30, 5, 3, 0), None);
    }

    #[test]
    fn real_vendor_descriptions_split_into_rx_and_tx() {
        // ⚠️ The accepting cases are the load-bearing half: a matcher that rejected *everything*
        // would satisfy the rejection tests below and ship a feature that never draws a line.
        for (text, want) in [
            ("Te1/1/1 Transmit Power Sensor", OpticalReading::Tx),
            ("Te1/1/1 Receive Power Sensor", OpticalReading::Rx),
            (
                "Ethernet1/1 Lane 1 Transceiver Receive Power Sensor",
                OpticalReading::Rx,
            ),
            ("Ethernet1 Rx Power Sensor", OpticalReading::Rx),
            ("Ethernet1 Tx Power Sensor", OpticalReading::Tx),
            ("GigabitEthernet0/1 TX Power", OpticalReading::Tx),
            ("optical rx power", OpticalReading::Rx),
        ] {
            assert_eq!(reading_from_text(text), Some(want), "text: {text}");
        }
    }

    #[test]
    fn ambiguous_or_unrelated_descriptions_are_refused() {
        for text in [
            "Te1/1/1 Module Temperature Sensor", // not a power reading
            "Te1/1/1 Laser Bias Current Sensor", // bias, not signal power
            "Te1/1/1 Supply Voltage Sensor",
            "Power Supply 1",          // power, but not optical and no direction
            "Tx/Rx Power Sensor",      // names both: identifies neither
            "Chassis Trxponder Power", // `trxponder` must not match `tx` or `rx`
            "Fan Tray 2",
        ] {
            assert_eq!(reading_from_text(text), None, "text: {text}");
        }
    }

    fn alias_row(ent: u32, logical: u32, target: &str) -> SnmpInstanceRow {
        SnmpInstanceRow {
            oid_base: ENT_ALIAS_MAPPING.to_owned(),
            instance: vec![ent, logical],
            value: SnmpValue::Oid(target.to_owned()),
        }
    }

    fn parent_row(ent: u32, parent: i64) -> SnmpInstanceRow {
        SnmpInstanceRow {
            oid_base: ENT_PHYSICAL_CONTAINED_IN.to_owned(),
            instance: vec![ent],
            value: SnmpValue::Int(parent),
        }
    }

    #[test]
    fn a_sensor_climbs_containment_to_reach_its_ports_ifindex() {
        let mut idx = EntityIndex::default();
        // sensor 1003 → transceiver 1002 → port 1001, and only the port has an ifIndex.
        idx.add_parent_rows(&[
            parent_row(1003, 1002),
            parent_row(1002, 1001),
            parent_row(1001, 1),
        ]);
        idx.add_alias_rows(&[alias_row(1001, 0, "1.3.6.1.2.1.2.2.1.1.42")]);
        assert_eq!(idx.ifindex_for(1003), Some(42));
        assert_eq!(idx.ifindex_for(1001), Some(42), "the port maps directly");
    }

    #[test]
    fn an_entity_with_no_interface_anywhere_above_it_is_unmapped() {
        let mut idx = EntityIndex::default();
        idx.add_parent_rows(&[parent_row(7, 6), parent_row(6, 0)]);
        idx.add_alias_rows(&[alias_row(1001, 0, "1.3.6.1.2.1.2.2.1.1.42")]);
        // A fan-tray sensor: the chain reaches the chassis and stops.
        assert_eq!(idx.ifindex_for(7), None);
        // An index nothing mentions at all.
        assert_eq!(idx.ifindex_for(999), None);
    }

    #[test]
    fn a_containment_cycle_terminates_instead_of_hanging() {
        let mut idx = EntityIndex::default();
        idx.add_parent_rows(&[parent_row(1, 2), parent_row(2, 1)]);
        assert_eq!(idx.ifindex_for(1), None);
        // A self-referencing row is the degenerate case of the same bug.
        let mut selfref = EntityIndex::default();
        selfref.add_parent_rows(&[parent_row(5, 5)]);
        assert_eq!(selfref.ifindex_for(5), None);
    }

    #[test]
    fn alias_rows_that_are_not_oids_are_skipped_not_fatal() {
        let mut idx = EntityIndex::default();
        idx.add_alias_rows(&[
            SnmpInstanceRow {
                oid_base: ENT_ALIAS_MAPPING.to_owned(),
                instance: vec![10, 0],
                value: SnmpValue::Int(3),
            },
            alias_row(11, 0, "1.3.6.1.2.1.2.2.1.1.7"),
        ]);
        assert_eq!(idx.alias.get(&10), None);
        assert_eq!(idx.alias.get(&11), Some(&7));
    }

    fn sample(ifindex: u32, reading: OpticalReading, dbm: f64) -> OpticalSample {
        OpticalSample {
            ifindex,
            reading,
            dbm,
        }
    }

    #[test]
    fn the_first_lane_wins_and_later_lanes_are_dropped() {
        let out = dedupe_readings([
            sample(42, OpticalReading::Rx, -7.4),
            sample(42, OpticalReading::Rx, -7.9), // lane 2 of the same module
            sample(42, OpticalReading::Tx, -2.1),
            sample(43, OpticalReading::Rx, -12.0),
        ]);
        assert_eq!(out.len(), 3);
        assert!((out[0].dbm - -7.4).abs() < 1e-9, "lane 1 must win");
        assert!(out.iter().any(|s| s.ifindex == 43));
    }

    #[test]
    fn an_implausible_reading_is_dropped_so_a_bad_scale_cannot_draw_a_line() {
        // A ×100 scale error on -7.42 dBm, which is exactly what forgetting the divisor produces.
        let out = dedupe_readings([
            sample(1, OpticalReading::Rx, -742.0),
            sample(2, OpticalReading::Rx, -7.42),
            sample(3, OpticalReading::Tx, f64::NAN),
            sample(4, OpticalReading::Tx, 999.0),
        ]);
        assert_eq!(out.len(), 1, "only the plausible reading survives: {out:?}");
        assert_eq!(out[0].ifindex, 2);
    }
}
