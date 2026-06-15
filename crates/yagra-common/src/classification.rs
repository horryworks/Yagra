//! Device classification rules: map a discovered device's SNMP signature to a device
//! profile.
//!
//! Discovery probes each device for `sysObjectID` (the vendor-assigned enterprise OID that
//! authoritatively identifies device type) and the free-form `sysDescr`. A
//! [`ClassificationRule`] maps such a signature to the [`ProfileId`](crate::ids::ProfileId)
//! to suggest on import. Rules are persisted (operator-editable) so new device types are
//! added as data, not code; the [`builtin_classification_rules`] below are only the seed.
//!
//! Rules are evaluated in ascending `priority` (most specific first); the first match wins.
//! A `sysObjectID` prefix is authoritative and should outrank a `sysDescr` keyword fallback.
//! Device-supplied strings are untrusted — matching is read-only and never executes them.

use crate::ids::ProfileId;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A persisted rule mapping a device signature to a profile. At least one of
/// `sysobjectid_prefix` / `sysdescr_regex` is set (the DB enforces this with a CHECK).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
/// One rule per vendor, carrying **both** an authoritative `sysObjectID` enterprise-prefix
/// (matched first — see [`Classifier`](crate::classification)) and a `sysDescr` keyword regex
/// fallback. Every `profile_name` must be one of [`builtin_profiles`]. Devices that match no
/// rule but answered SNMP fall back to "Generic SNMP" at the call site, so a vendor without a
/// dedicated profile is never worse off than before. Operators extend this set via the
/// persisted `classification_rules` table without a code change.
///
/// [`builtin_profiles`]: crate::collection::builtin_profiles
#[must_use]
pub fn builtin_classification_rules() -> Vec<BuiltinClassificationRule> {
    // (enterprise-OID prefix, sysDescr keyword regex, profile name, vendor label).
    // Enterprise numbers are IANA-assigned (1.3.6.1.4.1.<n>); a prefix ends in '.' so it can't
    // partial-match a longer enterprise number.
    const RULES: &[(&str, &str, &str, &str)] = &[
        (
            "1.3.6.1.4.1.9.",
            "(?i)cisco|ios-?xe|ios",
            "Cisco IOS router",
            "Cisco",
        ),
        (
            "1.3.6.1.4.1.2011.",
            "(?i)huawei|vrp|usg",
            "Huawei USG firewall",
            "Huawei",
        ),
        (
            "1.3.6.1.4.1.2636.",
            "(?i)juniper|junos",
            "Juniper router/switch",
            "Juniper",
        ),
        (
            "1.3.6.1.4.1.12356.",
            "(?i)fortinet|fortigate|fortios",
            "Fortinet FortiGate",
            "Fortinet",
        ),
        (
            "1.3.6.1.4.1.30065.",
            "(?i)arista",
            "Arista switch",
            "Arista",
        ),
        (
            "1.3.6.1.4.1.14988.",
            "(?i)mikrotik|routeros",
            "MikroTik RouterOS",
            "MikroTik",
        ),
        (
            "1.3.6.1.4.1.41112.",
            "(?i)ubiquiti|edgeos|edgeswitch|unifi|airos",
            "Ubiquiti device",
            "Ubiquiti",
        ),
        (
            "1.3.6.1.4.1.25461.",
            r"(?i)palo\s?alto|pan-os",
            "Palo Alto firewall",
            "Palo Alto",
        ),
        (
            "1.3.6.1.4.1.14823.",
            "(?i)aruba",
            "Aruba / HPE switch",
            "Aruba",
        ),
        (
            "1.3.6.1.4.1.25506.",
            "(?i)h3c|comware",
            "Aruba / HPE switch",
            "HPE",
        ),
        (
            "1.3.6.1.4.1.674.",
            "(?i)dell|force10",
            "Dell switch",
            "Dell",
        ),
        (
            "1.3.6.1.4.1.1916.",
            "(?i)extreme\\s?networks|exos",
            "Extreme Networks switch",
            "Extreme Networks",
        ),
        (
            "1.3.6.1.4.1.1991.",
            "(?i)foundry|brocade|ruckus",
            "Brocade / Ruckus device",
            "Brocade",
        ),
        (
            "1.3.6.1.4.1.25053.",
            "(?i)ruckus",
            "Brocade / Ruckus device",
            "Ruckus",
        ),
        (
            "1.3.6.1.4.1.2620.",
            r"(?i)check\s?point|gaia",
            "Check Point firewall",
            "Check Point",
        ),
        (
            "1.3.6.1.4.1.3375.",
            r"(?i)big-?ip|f5\s?networks",
            "F5 BIG-IP",
            "F5",
        ),
        (
            "1.3.6.1.4.1.5951.",
            "(?i)netscaler|citrix",
            "Citrix ADC (NetScaler)",
            "Citrix",
        ),
        (
            "1.3.6.1.4.1.6527.",
            "(?i)nokia|alcatel|timos|tmos|7750|sr ?os",
            "Nokia / Alcatel-Lucent router",
            "Nokia",
        ),
        (
            "1.3.6.1.4.1.6486.",
            "(?i)omniswitch|alcatel-lucent",
            "Nokia / Alcatel-Lucent router",
            "Alcatel-Lucent",
        ),
        ("1.3.6.1.4.1.890.", "(?i)zyxel", "Zyxel device", "Zyxel"),
        (
            "1.3.6.1.4.1.4526.",
            "(?i)netgear",
            "NETGEAR switch",
            "NETGEAR",
        ),
        ("1.3.6.1.4.1.171.", "(?i)d-?link", "D-Link switch", "D-Link"),
        (
            "1.3.6.1.4.1.11863.",
            "(?i)tp-?link",
            "TP-Link device",
            "TP-Link",
        ),
        (
            "1.3.6.1.4.1.6876.",
            "(?i)vmware|esxi",
            "VMware ESXi host",
            "VMware",
        ),
        (
            "1.3.6.1.4.1.6574.",
            "(?i)synology",
            "Synology NAS",
            "Synology",
        ),
        (
            "1.3.6.1.4.1.318.",
            "(?i)\\bapc\\b|american power",
            "APC UPS",
            "APC",
        ),
        (
            "1.3.6.1.4.1.311.",
            "(?i)windows|microsoft",
            "Windows server",
            "Microsoft",
        ),
        // Net-SNMP last: a Linux appliance with a vendor sysObjectID is classified by that
        // vendor's prefix first (sysObjectID beats sysDescr); only a plain Net-SNMP host (whose
        // sysObjectID is under 8072, or whose sysDescr just says "Linux") lands here.
        (
            "1.3.6.1.4.1.8072.",
            "(?i)net-snmp|\\blinux\\b",
            "Linux server (Net-SNMP)",
            "Linux",
        ),
    ];
    RULES
        .iter()
        .enumerate()
        .map(
            |(i, (prefix, regex, profile_name, vendor))| BuiltinClassificationRule {
                // Stable, spread priorities (most-specific vendors first by list order). The
                // two-pass classifier already makes any sysObjectID match beat any sysDescr match,
                // so priority only orders within a pass.
                priority: 100 + i as i32,
                sysobjectid_prefix: Some(prefix),
                sysdescr_regex: Some(regex),
                profile_name,
                vendor: Some(vendor),
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
        // A prefix must be dotted-numeric and end in '.', so it can't partial-match a longer
        // enterprise number (e.g. `...9.` must not also match `...91`).
        for rule in builtin_classification_rules() {
            let p = rule
                .sysobjectid_prefix
                .expect("builtin rules carry an OID prefix");
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
        // Guard against an accidental deletion: the common vendors must stay mapped.
        let profiles: Vec<&str> = builtin_classification_rules()
            .into_iter()
            .map(|r| r.profile_name)
            .collect();
        for expected in [
            "Cisco IOS router",
            "Huawei USG firewall",
            "Juniper router/switch",
            "Fortinet FortiGate",
            "Arista switch",
            "Palo Alto firewall",
            "Linux server (Net-SNMP)",
        ] {
            assert!(profiles.contains(&expected), "missing rule for {expected}");
        }
    }
}
