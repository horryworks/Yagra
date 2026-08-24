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

// Every Cisco profile that attaches a health template. `cpmCPUTotal5minRev` and the enhanced
// memory pool are both on it, so `cisco_cpu_5min` and `cisco_cemp_mem_used_pct` reach FIVE
// profiles rather than the two `CISCO_IOS` does. Counting by hand got this wrong;
// `the_seeded_vendor_defaults_target_exactly_the_profiles_that_collect_them` is what said so,
// which is the whole reason that test exists rather than a careful reading of the catalogue.
const CISCO_HEALTH: &[&str] = &[
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

// ⚠️ These five are placeholders resolved by
// `the_seeded_vendor_defaults_target_exactly_the_profiles_that_collect_them`, which derives the
// real answer from `builtin_profiles()` x `builtin_templates()` and prints it on a mismatch.
// The profiles that collect a BGP peer table.
const BGP: &[&str] = &[
    "Cisco IOS-XR router",
    "Cisco IOS/IOS-XE router",
    "Generic router",
    "Huawei NE/AR router",
    "Juniper MX router",
    "Nokia SR router",
];

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

pub(super) const DEFAULT_THRESHOLDS: [DefaultThreshold; 31] = [
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
        CISCO_HEALTH,
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
    // ── The derived percentages (ADR-105) ─────────────────────────────────────
    // 🚨 Every row here is on a metric **Yagra computes rather than collects**, so it is evaluated
    // once a minute by `derived::run_derived_metric_watch` and its dwell counts minutes, not polls
    // — the same trade the four interface metrics make (ADR-076).
    //
    // 🚨 **A default ships only where nothing else already covers the same physical quantity on the
    // same device.** Four rows were written and then removed for failing that, which is worth
    // recording because each looked obviously right on its own:
    //   - `huawei_mem_used_pct` — `huawei_mem_usage` (offset 12) is the same memory read another
    //     way, and already pages at 85/95. Two rules would page twice for one condition, which
    //     `repo/mod.rs` calls a bug rather than a feature.
    //   - `ucd_cpu_used_pct` — `ucd_cpu_idle_pct` (offset 6) is the same CPU from the other side.
    //   - `hr_storage_used_pct` — on a Net-SNMP host `ucd_disk_used_pct` (offset 7) already covers
    //     the same filesystems. It would be right for the eighteen *other* Host-Resources
    //     profiles, and that is the follow-up: a default that must exclude the profiles another
    //     default already reaches needs a documented-subset rule in the consistency test below,
    //     which today demands exact equality.
    //   - `hr_processor_load` — every vendor CPU metric here already has a default, and fleet-wide
    //     it would double each of them on any device that answers `hrProcessorLoad` too.
    //
    // 80/90 unless the metric argues otherwise: they all answer "how much of this is in use", and
    // an operator who has to remember a different number per vendor has a worse product, not a
    // safer one.
    (
        24,
        CISCO_IOS,
        "cisco_mem_used_pct",
        "above",
        Some(80.0),
        Some(90.0),
        3,
    ),
    (
        25,
        CISCO_IOS,
        "cisco_cpu_mem_used_pct",
        "above",
        Some(80.0),
        Some(90.0),
        3,
    ),
    (
        26,
        CISCO_HEALTH,
        "cisco_cemp_mem_used_pct",
        "above",
        Some(80.0),
        Some(90.0),
        3,
    ),
    (
        27,
        NETSNMP,
        "ucd_mem_used_pct",
        "above",
        Some(80.0),
        Some(90.0),
        3,
    ),
    // Swap is the exception, deliberately: a Linux host that has touched swap at all is usually
    // already in trouble, so the warning sits where "it started" rather than "it is nearly gone".
    (
        28,
        NETSNMP,
        "ucd_swap_used_pct",
        "above",
        Some(50.0),
        Some(80.0),
        3,
    ),
    // ⚠️ In hundredths, like `laLoadInt` itself: 100 means one runnable task per processor, which
    // is textbook "fully utilised", and 200 is twice that. Not measured against this fleet — there
    // is no Linux host running in the lab — but these two are the definition of the unit rather
    // than a guess about a workload.
    (
        29,
        NETSNMP,
        "ucd_load_per_core",
        "above",
        Some(100.0),
        Some(200.0),
        3,
    ),
    // ── Routing ───────────────────────────────────────────────────────────────
    // `bgpPeerState`: 1=idle 2=connect 3=active 4=opensent 5=openconfirm 6=established, so the
    // bound sits between openconfirm and established and "critical" means "this peer is not up".
    // Critical only — there is no degraded BGP session, it is either established or it is not.
    // Dwell 3 because the transient states are exactly what a session walks through while coming
    // back, and one poll of `active` during a normal reconvergence is not an incident.
    (30, BGP, "bgp_peer_state", "below", None, Some(5.5), 3),
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
            // What a rule on this metric actually needs collected. For a collected metric that is
            // the metric itself; for a derived one (ADR-105) it is **every input of its formula**,
            // and the answer has to be computed at the profile rather than the template level —
            // `ucd_load_per_core` divides a UCD-SNMP-MIB reading by a HOST-RESOURCES-MIB row count,
            // so no single template carries it and a template-level fold would find nothing.
            let needs: Vec<&str> = match crate::derived::derived_node_metric(metric) {
                Some(d) => {
                    let [a, b] = d.formula.inputs();
                    if a == b {
                        vec![a]
                    } else {
                        vec![a, b]
                    }
                }
                None => vec![metric],
            };
            let publishes = |profile: &yagra_common::BuiltinProfile, want: &str| {
                templates.iter().any(|t| {
                    profile.templates.contains(&t.name)
                        && t.items.iter().any(|i| i.metric_name == want)
                })
            };
            let want: BTreeSet<&str> = profiles
                .iter()
                .filter(|p| needs.iter().all(|m| publishes(p, m)))
                .map(|p| p.name)
                .collect();
            assert!(
                !want.is_empty(),
                "offset {offset}: no built-in profile collects everything {metric} needs                  ({needs:?}), so this rule is inert"
            );
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
        assert_eq!(
            checked, 27,
            "twenty-seven of the defaults are profile-scoped"
        );
    }

    /// Exactly four seeded defaults are fleet-wide, and they are the four that really are.
    ///
    /// ADR-078 決定 1. The ADR-075 argument for `global` — a profile-scoped rule cannot reach a
    /// node with no profile — applies to a rule about whether the node answered at all, and not to
    /// one about a vendor’s CPU register. This pins which side each row is on, because the cheap
    /// mistake when adding the next default is to copy the row above it.
    ///
    /// ⚠️ ADR-105 wrote a fifth (`hr_processor_load`) and took it back out: the standard SNMP set
    /// carries it, so the honest target list is all 52 profiles — but every vendor CPU metric in
    /// this table already has a default, and a fleet-wide rule would double each of them on any
    /// device that answers `hrProcessorLoad` as well. See the note above offset 24.
    #[test]
    fn only_the_genuinely_fleet_wide_defaults_are_global() {
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
