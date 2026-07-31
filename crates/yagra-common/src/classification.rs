// SPDX-License-Identifier: AGPL-3.0-only
//! Device classification rules: map a discovered device's SNMP signature to a device
//! profile.
//!
//! Discovery probes each device for `sysObjectID` (the vendor-assigned enterprise OID that
//! authoritatively identifies device type) and the free-form `sysDescr`. A
//! [`ClassificationRule`] maps such a signature to the [`ProfileId`](crate::ids::ProfileId)
//! to suggest on import. Rules are persisted (operator-editable) so new device types are
//! added as data, not code; the [`builtin_classification_rules`] below are only the seed.
//!
//! A rule matches when *all* its present matchers match (a `sysObjectID` prefix AND/or a
//! `sysDescr` regex), so one rule can mean "this vendor AND this NOS". Prefix-bearing rules
//! outrank `sysDescr`-only rules; within that, ascending `priority` wins (the first match).
//! Device-supplied strings are untrusted — matching is read-only and never executes them.

use crate::ids::ProfileId;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A persisted rule mapping a device signature to a profile. At least one of
/// `sysobjectid_prefix` / `sysdescr_regex` is set (the DB enforces this with a CHECK).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ClassificationRule {
    /// Stable identity.
    pub id: Uuid,
    /// Evaluation order: ascending, lowest first. Most-specific rules get lower numbers.
    pub priority: i32,
    /// Match when the device's `sysObjectID` starts with this dotted-OID prefix
    /// (e.g. `1.3.6.1.4.1.9.` for Cisco). Authoritative — preferred over `sysDescr`.
    pub sysobjectid_prefix: Option<String>,
    /// Match when the device's `sysDescr` matches this regular expression — a fallback for
    /// devices whose `sysObjectID` isn't covered by a prefix rule.
    pub sysdescr_regex: Option<String>,
    /// The profile to suggest when this rule matches.
    pub profile_id: ProfileId,
    /// Optional vendor/model to pre-fill on the import row (overrides the `sysDescr`
    /// heuristic when set). Descriptive metadata only — never a TSDB label.
    pub vendor: Option<String>,
    /// See `vendor`.
    pub model: Option<String>,
    /// Disabled rules are skipped during classification.
    pub enabled: bool,
}

/// A built-in classification rule, keyed to a built-in profile by name (its stable id is
/// resolved at seed time). Ships the vendor knowledge as seed data; operators extend the
/// set via the persisted [`ClassificationRule`] table without a code change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuiltinClassificationRule {
    pub priority: i32,
    pub sysobjectid_prefix: Option<&'static str>,
    pub sysdescr_regex: Option<&'static str>,
    /// Name of the built-in profile this rule suggests (see [`builtin_profiles`]).
    ///
    /// [`builtin_profiles`]: crate::collection::builtin_profiles
    pub profile_name: &'static str,
    pub vendor: Option<&'static str>,
    pub model: Option<&'static str>,
}

/// The built-in classification rules shipped with Yagra (seeded on startup).
///
/// Rules split a vendor into its NOS/role profiles by combining matchers: a `sysObjectID`
/// **prefix** guards the vendor and a `sysDescr` **regex** AND-ed with it discriminates the
/// NOS/role (the [`Classifier`](crate::classification) requires *all* present matchers to
/// match). Prefix-bearing rules always outrank `sysDescr`-only rules; within that, lower
/// `priority` wins, so Cisco's NX-OS/ASA/IOS-XR/router rules precede the prefix-only Catalyst
/// catch-all. Every `profile_name` must be one of [`builtin_profiles`]. A device that answered
/// SNMP but matched nothing falls back to "Generic SNMP" at the call site. Operators extend
/// this set via the persisted `classification_rules` table without a code change.
///
/// [`builtin_profiles`]: crate::collection::builtin_profiles
#[must_use]
pub fn builtin_classification_rules() -> Vec<BuiltinClassificationRule> {
    // (priority, sysObjectID prefix, sysDescr regex, profile name, vendor). Either matcher may
    // be None, but not both. Enterprise prefixes are IANA-assigned (1.3.6.1.4.1.<n>) and end in
    // '.' so they can't partial-match a longer enterprise number.
    type Row = (
        i32,
        Option<&'static str>,
        Option<&'static str>,
        &'static str,
        Option<&'static str>,
    );
    const RULES: &[Row] = &[
        // ── Cisco: one vendor prefix (9.), NOS/role discriminated by sysDescr (AND), catch-all last ──
        (
            10,
            Some("1.3.6.1.4.1.9.12.3."),
            None,
            "Cisco Nexus switch (NX-OS)",
            Some("Cisco"),
        ),
        (
            20,
            Some("1.3.6.1.4.1.9."),
            Some("(?i)nx-?os|nexus"),
            "Cisco Nexus switch (NX-OS)",
            Some("Cisco"),
        ),
        (
            30,
            Some("1.3.6.1.4.1.9."),
            Some(r"(?i)adaptive security|\bASA\b"),
            "Cisco ASA firewall",
            Some("Cisco"),
        ),
        (
            40,
            Some("1.3.6.1.4.1.9."),
            Some("(?i)ios[ -]?xr"),
            "Cisco IOS-XR router",
            Some("Cisco"),
        ),
        (
            50,
            Some("1.3.6.1.4.1.9."),
            Some(r"(?i)firepower|\bFTD\b"),
            "Cisco Firepower (FTD)",
            Some("Cisco"),
        ),
        (
            60,
            Some("1.3.6.1.4.1.9."),
            Some(r"(?i)wireless lan controller|\bWLC\b|air-ct"),
            "Cisco wireless controller",
            Some("Cisco"),
        ),
        (
            70,
            Some("1.3.6.1.4.1.9."),
            Some(r"(?i)\bISR\b|\bASR\b|ios[ -]?xe.*router"),
            "Cisco IOS/IOS-XE router",
            Some("Cisco"),
        ),
        (
            80,
            Some("1.3.6.1.4.1.9."),
            None,
            "Cisco Catalyst switch (IOS/IOS-XE)",
            Some("Cisco"),
        ),
        // ── Juniper: role split on sysDescr, EX/QFX catch-all ──
        (
            100,
            Some("1.3.6.1.4.1.2636."),
            Some(r"(?i)\bsrx\b"),
            "Juniper SRX firewall",
            Some("Juniper"),
        ),
        (
            110,
            Some("1.3.6.1.4.1.2636."),
            Some(r"(?i)\bmx\b|\bptx\b"),
            "Juniper MX router",
            Some("Juniper"),
        ),
        (
            120,
            Some("1.3.6.1.4.1.2636."),
            None,
            "Juniper EX/QFX switch",
            Some("Juniper"),
        ),
        // ── Huawei: role split, CloudEngine switch catch-all ──
        (
            140,
            Some("1.3.6.1.4.1.2011."),
            Some("(?i)usg|firewall"),
            "Huawei USG firewall",
            Some("Huawei"),
        ),
        (
            150,
            Some("1.3.6.1.4.1.2011."),
            Some(r"(?i)\bne\d|\bar\d|router"),
            "Huawei NE/AR router",
            Some("Huawei"),
        ),
        (
            160,
            Some("1.3.6.1.4.1.2011."),
            None,
            "Huawei CloudEngine switch",
            Some("Huawei"),
        ),
        // ── Single-line vendors (prefix only) ──
        (
            200,
            Some("1.3.6.1.4.1.12356."),
            None,
            "Fortinet FortiGate",
            Some("Fortinet"),
        ),
        (
            210,
            Some("1.3.6.1.4.1.30065."),
            None,
            "Arista EOS switch",
            Some("Arista"),
        ),
        (
            220,
            Some("1.3.6.1.4.1.14988."),
            None,
            "MikroTik RouterOS",
            Some("MikroTik"),
        ),
        (
            230,
            Some("1.3.6.1.4.1.25461."),
            None,
            "Palo Alto PAN-OS firewall",
            Some("Palo Alto"),
        ),
        (
            240,
            Some("1.3.6.1.4.1.2620."),
            None,
            "Check Point firewall",
            Some("Check Point"),
        ),
        (
            250,
            Some("1.3.6.1.4.1.3375."),
            None,
            "F5 BIG-IP",
            Some("F5"),
        ),
        (
            260,
            Some("1.3.6.1.4.1.5951."),
            None,
            "Citrix ADC (NetScaler)",
            Some("Citrix"),
        ),
        (
            270,
            Some("1.3.6.1.4.1.14823."),
            None,
            "Aruba/HPE switch",
            Some("Aruba"),
        ),
        (
            280,
            Some("1.3.6.1.4.1.25506."),
            None,
            "Aruba/HPE switch",
            Some("HPE"),
        ),
        (
            290,
            Some("1.3.6.1.4.1.674."),
            None,
            "Dell switch",
            Some("Dell"),
        ),
        (
            300,
            Some("1.3.6.1.4.1.1916."),
            None,
            "Extreme EXOS switch",
            Some("Extreme Networks"),
        ),
        (
            310,
            Some("1.3.6.1.4.1.1991."),
            None,
            "Brocade / Ruckus switch",
            Some("Brocade"),
        ),
        (
            320,
            Some("1.3.6.1.4.1.25053."),
            None,
            "Brocade / Ruckus switch",
            Some("Ruckus"),
        ),
        (
            330,
            Some("1.3.6.1.4.1.6527."),
            None,
            "Nokia SR router",
            Some("Nokia"),
        ),
        (
            340,
            Some("1.3.6.1.4.1.6486."),
            None,
            "Nokia SR router",
            Some("Alcatel-Lucent"),
        ),
        (
            350,
            Some("1.3.6.1.4.1.41112."),
            None,
            "Ubiquiti switch/AP",
            Some("Ubiquiti"),
        ),
        (
            360,
            Some("1.3.6.1.4.1.890."),
            None,
            "Zyxel switch",
            Some("Zyxel"),
        ),
        (
            370,
            Some("1.3.6.1.4.1.4526."),
            None,
            "NETGEAR switch",
            Some("NETGEAR"),
        ),
        (
            380,
            Some("1.3.6.1.4.1.171."),
            None,
            "D-Link switch",
            Some("D-Link"),
        ),
        (
            390,
            Some("1.3.6.1.4.1.11863."),
            None,
            "TP-Link switch",
            Some("TP-Link"),
        ),
        (
            400,
            Some("1.3.6.1.4.1.6876."),
            None,
            "VMware ESXi host",
            Some("VMware"),
        ),
        (
            410,
            Some("1.3.6.1.4.1.6574."),
            None,
            "Synology NAS",
            Some("Synology"),
        ),
        (
            420,
            Some("1.3.6.1.4.1.24681."),
            None,
            "QNAP NAS",
            Some("QNAP"),
        ),
        (
            430,
            Some("1.3.6.1.4.1.789."),
            None,
            "NetApp / generic storage",
            Some("NetApp"),
        ),
        (440, Some("1.3.6.1.4.1.318."), None, "APC UPS", Some("APC")),
        (
            450,
            Some("1.3.6.1.4.1.311."),
            None,
            "Windows server",
            Some("Microsoft"),
        ),
        (
            460,
            Some("1.3.6.1.4.1.8072."),
            None,
            "Linux server (Net-SNMP)",
            Some("Linux"),
        ),
        (
            470,
            Some("1.3.6.1.4.1.2021."),
            None,
            "Linux server (Net-SNMP)",
            Some("Linux"),
        ),
        // ── sysDescr-only fallbacks (evaluated after every prefix rule) ──
        (
            600,
            None,
            Some(r"(?i)\bups\b|battery system"),
            "Generic UPS (RFC1628)",
            None,
        ),
        (
            610,
            None,
            Some("(?i)printer|jetdirect|laserjet"),
            "Network printer",
            None,
        ),
        (
            620,
            None,
            Some("(?i)vmware|esxi"),
            "VMware ESXi host",
            Some("VMware"),
        ),
        (
            630,
            None,
            Some("(?i)windows|microsoft"),
            "Windows server",
            Some("Microsoft"),
        ),
        (
            640,
            None,
            Some(r"(?i)net-snmp|\blinux\b"),
            "Linux server (Net-SNMP)",
            Some("Linux"),
        ),
        (
            650,
            None,
            Some("(?i)juniper|junos"),
            "Juniper EX/QFX switch",
            Some("Juniper"),
        ),
        (
            660,
            None,
            Some("(?i)cisco"),
            "Cisco Catalyst switch (IOS/IOS-XE)",
            Some("Cisco"),
        ),
        // ── Appended post-redesign — kept at array end on purpose ──
        // Seed ids are Uuid::from_u128(BASE + index); inserting mid-array would shift later rule
        // ids and duplicate their rows on reseed. The classifier sorts by `priority`, so array
        // position is irrelevant to evaluation — these slot in by their priority numbers below.
        //
        // Huawei wireless controller (AC/AirEngine): vendor prefix 2011. AND an AC sysDescr, at
        // priority 145 — between USG (140) and the NE/AR router (150) / CloudEngine catch-all (160).
        (
            145,
            Some("1.3.6.1.4.1.2011."),
            Some(r"(?i)airengine|wlan ac|\bac6\d{3}\b|\bwac\b"),
            "Huawei wireless controller",
            Some("Huawei"),
        ),
        // A10 Networks Thunder/AX ADC — single enterprise prefix (PEN 22610), priority 265
        // (alongside the other load balancers F5 250 / Citrix 260).
        (
            265,
            Some("1.3.6.1.4.1.22610."),
            None,
            "A10 Thunder ADC",
            Some("A10 Networks"),
        ),
        // Cisco Meraki (cloud-managed): one enterprise prefix (29671), split MX (security
        // appliance → firewall) vs MS (switch) on the model token in sysDescr (e.g. "Meraki
        // MX67", "Meraki MS220-8P"). No prefix-only catch-all on purpose: other Meraki lines
        // (MR APs, MV cameras) shouldn't fall into MX/MS — they drop to "Generic SNMP" instead.
        (
            480,
            Some("1.3.6.1.4.1.29671."),
            Some(r"(?i)\bMX\d"),
            "Cisco Meraki MX",
            Some("Cisco Meraki"),
        ),
        (
            485,
            Some("1.3.6.1.4.1.29671."),
            Some(r"(?i)\bMS\d"),
            "Cisco Meraki MS switch",
            Some("Cisco Meraki"),
        ),
    ];
    RULES
        .iter()
        .map(
            |(priority, prefix, regex, profile_name, vendor)| BuiltinClassificationRule {
                priority: *priority,
                sysobjectid_prefix: *prefix,
                sysdescr_regex: *regex,
                profile_name,
                vendor: *vendor,
                model: None,
            },
        )
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collection::builtin_profiles;

    #[test]
    fn every_builtin_rule_references_an_existing_profile() {
        let profile_names: Vec<&str> = builtin_profiles().into_iter().map(|p| p.name).collect();
        for rule in builtin_classification_rules() {
            assert!(
                profile_names.contains(&rule.profile_name),
                "rule references unknown profile {:?}",
                rule.profile_name
            );
        }
    }

    #[test]
    fn every_builtin_rule_has_at_least_one_matcher() {
        for rule in builtin_classification_rules() {
            assert!(
                rule.sysobjectid_prefix.is_some() || rule.sysdescr_regex.is_some(),
                "rule for {:?} has no matcher",
                rule.profile_name
            );
        }
    }

    #[test]
    fn builtin_oid_prefixes_are_dotted_and_dot_terminated() {
        // A prefix (where present) must be dotted-numeric and end in '.', so it can't
        // partial-match a longer enterprise number (e.g. `...9.` must not also match `...91`).
        for rule in builtin_classification_rules() {
            let Some(p) = rule.sysobjectid_prefix else {
                continue; // sysDescr-only fallback rule
            };
            assert!(p.ends_with('.'), "prefix {p:?} must end with '.'");
            let core = &p[..p.len() - 1];
            assert!(
                !core.is_empty()
                    && core
                        .split('.')
                        .all(|arc| !arc.is_empty() && arc.bytes().all(|b| b.is_ascii_digit())),
                "prefix {p:?} must be dotted-numeric"
            );
        }
    }

    #[test]
    fn builtin_rules_cover_the_major_vendors() {
        // Guard against an accidental deletion: the common vendor classes must stay mapped.
        let profiles: Vec<&str> = builtin_classification_rules()
            .into_iter()
            .map(|r| r.profile_name)
            .collect();
        for expected in [
            "Cisco Catalyst switch (IOS/IOS-XE)",
            "Cisco Nexus switch (NX-OS)",
            "Cisco ASA firewall",
            "Huawei USG firewall",
            "Juniper EX/QFX switch",
            "Fortinet FortiGate",
            "Arista EOS switch",
            "Palo Alto PAN-OS firewall",
            "Linux server (Net-SNMP)",
        ] {
            assert!(profiles.contains(&expected), "missing rule for {expected}");
        }
    }

    #[test]
    fn cisco_rules_share_one_prefix_but_split_by_sysdescr() {
        // The same-vendor split: several Cisco rules share the 9. prefix and differ only by
        // their sysDescr discriminator, with a prefix-only catch-all.
        let cisco: Vec<_> = builtin_classification_rules()
            .into_iter()
            .filter(|r| r.vendor == Some("Cisco") && r.sysobjectid_prefix == Some("1.3.6.1.4.1.9."))
            .collect();
        assert!(
            cisco.iter().filter(|r| r.sysdescr_regex.is_some()).count() >= 4,
            "expected several sysDescr-discriminated Cisco rules"
        );
        assert!(
            cisco.iter().any(|r| r.sysdescr_regex.is_none()),
            "expected a prefix-only Cisco catch-all"
        );
    }

    #[test]
    fn huawei_wac_rule_guards_vendor_and_discriminates_on_sysdescr() {
        // The Huawei WAC rule must AND the 2011. vendor prefix with an AC sysDescr, and sort
        // (by priority) ahead of the Huawei NE/AR router and CloudEngine catch-all rules.
        let rules = builtin_classification_rules();
        let wac = rules
            .iter()
            .find(|r| r.profile_name == "Huawei wireless controller")
            .expect("Huawei WAC rule present");
        assert_eq!(wac.sysobjectid_prefix, Some("1.3.6.1.4.1.2011."));
        assert!(
            wac.sysdescr_regex.is_some(),
            "WAC rule needs a discriminator"
        );
        let cloudengine_prio = rules
            .iter()
            .find(|r| r.profile_name == "Huawei CloudEngine switch")
            .expect("CloudEngine catch-all present")
            .priority;
        assert!(
            wac.priority < cloudengine_prio,
            "WAC must out-prioritise the CloudEngine catch-all"
        );
    }

    #[test]
    fn a10_rule_maps_its_enterprise_prefix() {
        let rules = builtin_classification_rules();
        let a10 = rules
            .iter()
            .find(|r| r.profile_name == "A10 Thunder ADC")
            .expect("A10 rule present");
        assert_eq!(a10.sysobjectid_prefix, Some("1.3.6.1.4.1.22610."));
        assert_eq!(a10.vendor, Some("A10 Networks"));
    }

    #[test]
    fn meraki_rules_split_mx_and_ms_on_sysdescr() {
        // Both Meraki rules share the 29671 prefix and discriminate MX vs MS on the model token
        // in sysDescr — so the same cloud vendor maps to firewall vs switch profiles.
        let rules = builtin_classification_rules();
        let mx = rules
            .iter()
            .find(|r| r.profile_name == "Cisco Meraki MX")
            .expect("Meraki MX rule present");
        let ms = rules
            .iter()
            .find(|r| r.profile_name == "Cisco Meraki MS switch")
            .expect("Meraki MS rule present");
        assert_eq!(mx.sysobjectid_prefix, Some("1.3.6.1.4.1.29671."));
        assert_eq!(ms.sysobjectid_prefix, Some("1.3.6.1.4.1.29671."));
        assert!(
            mx.sysdescr_regex.is_some() && ms.sysdescr_regex.is_some(),
            "both Meraki rules need a model discriminator"
        );
        assert_eq!(mx.vendor, Some("Cisco Meraki"));
    }
}
