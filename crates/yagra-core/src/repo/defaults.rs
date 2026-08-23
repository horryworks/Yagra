// SPDX-License-Identifier: AGPL-3.0-only
//! The seeded default alert rules: one `const` table, and the tests that keep it honest.
//!
//! Separated from [`super::seed`], which writes it, on the line that separates a `const` from the
//! `async fn` that inserts it. The three tests below are about **this table's agreement with the
//! built-in catalogue** and need no database, which is why they can sit beside the data rather than
//! beside the writer.
//!
//! ⚠️ Each row's offset is its PRIMARY KEY, so the table is append-only in normal use — see the
//! note on [`DefaultThreshold`].

// Only the settings struct: `retention::Row` would collide with `sqlx::Row` above.

// (offset, profiles, metric, direction, warning, critical, dwell_samples)
//
// `profiles` names the built-in profiles the rule targets. **An empty slice means
// `global`** — every node, whatever it is.
//
// 🚨 ADR-078 決定 1: a vendor metric belongs on that vendor’s profiles, not on every
// node. ADR-077 put all 21 of them at `global` and argued that it changed nothing about
// *who fires* — which is true, since a node that does not collect `cisco_cpu_5min` never
// evaluates the rule. What it missed is that the screen then tells the operator a
// Cisco-only rule applies to the whole fleet, which is false and makes a 30-row list
// unreadable. ADR-075 決定 1 (a profile rule cannot reach a node with no profile) is
// still true; it is the argument for the four `global` rows below, not for these.
//
// ⚠️ The profile names are literals, and `builtin_profiles()` is where they really live.
// `the_seeded_vendor_defaults_target_exactly_the_profiles_that_collect_them` pins the
// two together, so a profile that later gains one of these templates fails the build
// instead of quietly falling outside its own default rule.
const CISCO_IOS: &[&str] = &[
    "Cisco IOS/IOS-XE router",
    "Cisco Catalyst switch (IOS/IOS-XE)",
];

// `cpmCPUTotal5minRev` is the CPU OID every Cisco health template reuses, so `cisco_cpu_5min`
// reaches FIVE profiles rather than the two `CISCO_IOS` does. Counting by hand got this wrong;
// `the_seeded_vendor_defaults_target_exactly_the_profiles_that_collect_them` is what said so,
// which is the whole reason that test exists rather than a careful reading of the catalogue.
const CISCO_CPU: &[&str] = &[
    "Cisco IOS/IOS-XE router",
    "Cisco IOS-XR router",
    "Cisco Catalyst switch (IOS/IOS-XE)",
    "Cisco Nexus switch (NX-OS)",
    "Cisco ASA firewall",
];

const CISCO_SENSORS: &[&str] = &["Cisco IOS-XR router", "Cisco Nexus switch (NX-OS)"];

const NXOS: &[&str] = &["Cisco Nexus switch (NX-OS)"];

const JUNIPER: &[&str] = &[
    "Juniper MX router",
    "Juniper EX/QFX switch",
    "Juniper SRX firewall",
];

const HUAWEI: &[&str] = &[
    "Huawei NE/AR router",
    "Huawei CloudEngine switch",
    "Huawei USG firewall",
    "Huawei wireless controller",
];

const FORTINET: &[&str] = &["Fortinet FortiGate"];

const PANOS: &[&str] = &["Palo Alto PAN-OS firewall"];

const NETSNMP: &[&str] = &["Linux server (Net-SNMP)"];

const UPS: &[&str] = &["APC UPS", "Generic UPS (RFC1628)"];

const FLEET: &[&str] = &[];

/// One seeded default: (offset, target profiles, metric, direction, warning, critical, dwell).
///
/// ⚠️ The offset is the row's PRIMARY KEY (`SeedRange::DefaultThresholds`), so this table is
/// append-only in normal use. Offsets 4-23 were re-pointed exactly once, by migration 0097's range
/// delete — the mechanism that exists precisely because `ON CONFLICT DO NOTHING` cannot update a
/// row it has already written.
type DefaultThreshold = (
    usize,
    &'static [&'static str],
    &'static str,
    &'static str,
    Option<f64>,
    Option<f64>,
    i32,
);

pub(super) const DEFAULT_THRESHOLDS: [DefaultThreshold; 24] = [
    // ── Fleet-wide (ADR-075 + `icmp_rtt_ms`) ───────────────────────────────────
    // These four really do apply to every node, which is why the ADR-075 argument for
    // `global` holds for them and not for the vendor rows below.
    //
    // The node stopped answering. Dwell 3 is what the removed hard-coded constant was,
    // so an upgrade does not change how quickly an existing fleet pages.
    (
        0,
        FLEET,
        crate::alerts::LIVENESS,
        "below",
        None,
        None,
        crate::alerts::DEFAULT_LIVENESS_DWELL as i32,
    ),
    // The SNMP agent stopped answering while the device itself is fine. Two polls
    // rather than three: SNMP intervals are longer than ICMP, so three would be a long
    // wait, and a scalar GET is not lossy the way a single ping is.
    (
        1,
        FLEET,
        yagra_common::METRIC_SNMP_UP,
        "below",
        None,
        Some(0.5),
        2,
    ),
    // Degradation before the node is gone. Warning only — 100% loss is the reachability
    // rule’s job, and two criticals for one outage is a notification storm, which this
    // project treats as a bug rather than a feature. ⚠️ At 100% loss BOTH fire today
    // (this warning and that critical); removing the overlap needs a range bound
    // ("20% to 99%"), which the model cannot express yet — ADR-078 決定 5.
    (2, FLEET, "icmp_loss_pct", "above", Some(20.0), None, 3),
    // Slow, not gone. Warning only, same argument.
    (3, FLEET, "icmp_rtt_ms", "above", Some(500.0), None, 3),
    // ── Per-profile (ADR-077 bounds, ADR-078 scoping) ──────────────────────────
    // ⚠️ Bounds were chosen against measured fleet values (ADR-077 決定 5), not from
    // the MIB alone. Where a metric could not be measured the ADR says so.
    //
    // Linux CPU. ⚠️ This is the IDLE percentage, so it reads `below` — the one metric
    // in this table where up is healthy.
    (
        4,
        NETSNMP,
        "ucd_cpu_idle_pct",
        "below",
        Some(20.0),
        Some(10.0),
        3,
    ),
    (
        5,
        NETSNMP,
        "ucd_disk_used_pct",
        "above",
        Some(85.0),
        Some(95.0),
        2,
    ),
    // Cisco. Temperature bounds sit at 70/80 because the measured fleet maximum was
    // 54°C; 65 would have left six degrees of headroom on a device already at 59.
    (
        6,
        CISCO_CPU,
        "cisco_cpu_5min",
        "above",
        Some(80.0),
        Some(90.0),
        3,
    ),
    (
        7,
        CISCO_IOS,
        "cisco_env_temp",
        "above",
        Some(70.0),
        Some(80.0),
        2,
    ),
    (
        8,
        CISCO_SENSORS,
        "cisco_temp_c",
        "above",
        Some(70.0),
        Some(80.0),
        2,
    ),
    (9, NXOS, "nxos_cpu_util", "above", Some(80.0), Some(90.0), 3),
    (
        10,
        NXOS,
        "nxos_mem_util",
        "above",
        Some(80.0),
        Some(90.0),
        3,
    ),
    // Juniper. Buffer utilisation runs high in normal operation, so it starts at 85.
    (
        11,
        JUNIPER,
        "juniper_cpu_1min",
        "above",
        Some(80.0),
        Some(90.0),
        3,
    ),
    (
        12,
        JUNIPER,
        "juniper_buffer_util",
        "above",
        Some(85.0),
        Some(95.0),
        3,
    ),
    (
        13,
        JUNIPER,
        "juniper_temp",
        "above",
        Some(70.0),
        Some(80.0),
        2,
    ),
    // Huawei VRP. 🚨 Memory is 85/95, not the 80/90 ADR-077 shipped: the real firewall
    // on the test deployment measured EXACTLY 80.0, and `above` is inclusive, so that
    // rule alerted continuously from the moment it shipped. CPU and temperature were
    // measured before choosing their bounds; memory was the one that was not.
    (
        14,
        HUAWEI,
        "huawei_cpu_usage",
        "above",
        Some(80.0),
        Some(90.0),
        3,
    ),
    (
        15,
        HUAWEI,
        "huawei_mem_usage",
        "above",
        Some(85.0),
        Some(95.0),
        3,
    ),
    (
        16,
        HUAWEI,
        "huawei_temp",
        "above",
        Some(70.0),
        Some(80.0),
        2,
    ),
    // FortiOS enters conserve mode around 80% memory, so warn before that.
    (
        17,
        FORTINET,
        "fortinet_cpu_usage",
        "above",
        Some(80.0),
        Some(90.0),
        3,
    ),
    (
        18,
        FORTINET,
        "fortinet_mem_usage",
        "above",
        Some(75.0),
        Some(85.0),
        3,
    ),
    (
        19,
        PANOS,
        "panos_session_util_pct",
        "above",
        Some(80.0),
        Some(90.0),
        3,
    ),
    // UPS. Charge and runtime read `below` — falling is the fault.
    (
        20,
        UPS,
        "ups_charge_remaining_pct",
        "below",
        Some(50.0),
        Some(20.0),
        2,
    ),
    (
        21,
        UPS,
        "ups_minutes_remaining",
        "below",
        Some(30.0),
        Some(10.0),
        2,
    ),
    // `3 = batteryLow`, `4 = batteryDepleted` (RFC 1628), so the bound sits between
    // `2 = batteryNormal` and `3`. Critical only, and one breach: a depleted battery
    // is not a condition to wait three polls on.
    (22, UPS, "ups_battery_status", "above", None, Some(2.5), 1),
    (
        23,
        UPS,
        "ups_output_load_pct",
        "above",
        Some(80.0),
        Some(95.0),
        2,
    ),
];

#[cfg(test)]
mod tests {
    use super::*;

    /// Every profile-scoped default targets exactly the profiles that collect its metric.
    ///
    /// The target lists in `DEFAULT_THRESHOLDS` are literals; the real answer lives in
    /// `builtin_profiles()` × `builtin_templates()`. That is a mirror, so it gets a test rather
    /// than care (`extensibility.md` §2) — and it drifts silently in both directions. A profile
    /// that later gains `Cisco IOS/IOS-XE health` would collect `cisco_cpu_5min` and have no
    /// default rule for it; a name that is renamed here is dropped at seed time with nothing but
    /// a log line to say so. Both look like "the alert just never fired".
    #[test]
    fn the_seeded_vendor_defaults_target_exactly_the_profiles_that_collect_them() {
        use std::collections::BTreeSet;
        let templates = yagra_common::builtin_templates();
        let profiles = yagra_common::builtin_profiles();
        let mut checked = 0usize;
        let mut wrong: Vec<String> = Vec::new();
        for (offset, targets, metric, ..) in DEFAULT_THRESHOLDS {
            if targets.is_empty() {
                continue; // a fleet-wide row — there is nothing to derive it from
            }
            let carrying: BTreeSet<&str> = templates
                .iter()
                .filter(|t| t.items.iter().any(|i| i.metric_name == metric))
                .map(|t| t.name)
                .collect();
            assert!(
                !carrying.is_empty(),
                "offset {offset}: no built-in template publishes {metric}, so this rule is inert"
            );
            let want: BTreeSet<&str> = profiles
                .iter()
                .filter(|p| p.templates.iter().any(|t| carrying.contains(t)))
                .map(|p| p.name)
                .collect();
            let got: BTreeSet<&str> = targets.iter().copied().collect();
            if got != want {
                // Report EVERY mismatch, not the first. A metric can be published by several
                // vendor templates (`cisco_cpu_5min` is on IOS, IOS-XR, NX-OS and ASA), so a
                // hand-written list is usually wrong in more than one place at a time and one
                // failure per run would mean one round trip per list.
                wrong.push(format!(
                    "offset {offset} ({metric}):\n     have {got:?}\n     want {want:?}"
                ));
            }
            checked += 1;
        }
        assert!(wrong.is_empty(), "{}", wrong.join("\n"));
        // Load-bearing: without it, a loop that stopped matching would skip every assertion above
        // and report success about nothing.
        assert_eq!(checked, 20, "twenty of the defaults are profile-scoped");
    }

    /// Exactly four seeded defaults are fleet-wide, and they are the four that really are.
    ///
    /// ADR-078 決定 1. The ADR-075 argument for `global` — a profile-scoped rule cannot reach a
    /// node with no profile — applies to a rule about whether the node answered at all, and not to
    /// one about a vendor’s CPU register. This pins which side each row is on, because the cheap
    /// mistake when adding the next default is to copy the row above it.
    #[test]
    fn only_the_four_genuinely_fleet_wide_defaults_are_global() {
        let fleet: Vec<&str> = DEFAULT_THRESHOLDS
            .iter()
            .filter(|(_, targets, ..)| targets.is_empty())
            .map(|(_, _, metric, ..)| *metric)
            .collect();
        assert_eq!(
            fleet,
            vec![
                crate::alerts::LIVENESS,
                yagra_common::METRIC_SNMP_UP,
                "icmp_loss_pct",
                "icmp_rtt_ms",
            ]
        );
    }

    /// The seeded offsets are dense and start at zero — the id IS the offset.
    ///
    /// A gap would not fail anything at runtime; it would just waste an id. A DUPLICATE would
    /// silently drop a rule, because the second insert hits `ON CONFLICT (id) DO NOTHING` and the
    /// deployment ends up with one fewer default than the table lists.
    #[test]
    fn every_seeded_default_has_its_own_offset_and_they_are_dense() {
        let offsets: Vec<usize> = DEFAULT_THRESHOLDS.iter().map(|(o, ..)| *o).collect();
        assert_eq!(offsets, (0..DEFAULT_THRESHOLDS.len()).collect::<Vec<_>>());
    }
}
