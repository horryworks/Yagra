//! Yagra-discovery — device discovery and classification.
//!
//! IP-range / SNMP sweep / LLDP-CDP based discovery, classification into profiles, and
//! the built-in **Credential Finder** that probes candidate credentials to find the one a
//! device accepts. The per-device probe rate limiter ([`credential_finder`]) is in place;
//! the sweep itself runs on the poller (raw-socket ICMP), and [`classify`] maps a device's
//! SNMP `sysDescr` to a suggested built-in device profile.

pub mod credential_finder;

pub use credential_finder::{AttemptDecision, CredentialProbeLimiter, LimiterConfig};

/// Vendor/model/profile extracted from a device's SNMP `sysDescr` (best-effort,
/// case-insensitive). All fields are optional — `sysDescr` is free-form, untrusted device text,
/// so this is a heuristic to pre-fill the operator's import form, not an authority. `vendor` and
/// `model` are stored as descriptive node metadata (never TSDB labels); `profile` is a suggested
/// built-in device-profile name matching the profiles core seeds (`yagra_common::builtin_profiles`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeviceIdentity {
    /// Maker / manufacturer (e.g. "Huawei", "Cisco"), if recognised.
    pub vendor: Option<String>,
    /// Model / product token (e.g. "USG6000", "C2960"), if one is confidently extractable.
    pub model: Option<String>,
    /// Suggested built-in device-profile name, if the vendor maps to one.
    pub profile: Option<&'static str>,
}

/// Identify a device from its SNMP `sysDescr`: maker, a best-effort model token, and a suggested
/// profile. Vendor detection is a conservative keyword match; model extraction only fires for a
/// token that begins with a known vendor prefix immediately followed by a digit (so a wrong model
/// is never invented from arbitrary words). Everything here is editable by the operator on import.
#[must_use]
pub fn identify(sysdescr: &str) -> DeviceIdentity {
    let d = sysdescr.to_ascii_lowercase();
    // (vendor, suggested profile, model-token prefixes to look for).
    let (vendor, profile, prefixes): (Option<&str>, Option<&str>, &[&str]) =
        if d.contains("huawei") || d.contains("vrp") || d.contains("usg") {
            (
                Some("Huawei"),
                Some("Huawei USG firewall"),
                &["usg", "ar", "ne", "ce", "s"],
            )
        } else if d.contains("cisco") || d.contains("ios") {
            (
                Some("Cisco"),
                Some("Cisco IOS router"),
                &["ws-c", "isr", "asr", "nexus", "c"],
            )
        } else if d.contains("juniper") || d.contains("junos") {
            (Some("Juniper"), None, &["mx", "ex", "srx", "qfx"])
        } else if d.contains("arista") {
            (Some("Arista"), None, &["dcs", "ccs"])
        } else if d.contains("mikrotik") || d.contains("routeros") {
            (Some("MikroTik"), None, &[])
        } else if d.contains("ubiquiti") || d.contains("edgeos") || d.contains("unifi") {
            (Some("Ubiquiti"), None, &[])
        } else if d.contains("fortinet") || d.contains("fortigate") {
            (Some("Fortinet"), None, &["fg", "fgt"])
        } else if d.contains("paloalto") || d.contains("pan-os") {
            (Some("Palo Alto"), None, &["pa"])
        } else {
            (None, None, &[])
        };
    DeviceIdentity {
        vendor: vendor.map(str::to_owned),
        model: extract_model(sysdescr, prefixes),
        profile,
    }
}

/// Pull a model token out of `sysDescr`: the first whitespace/punctuation-delimited token whose
/// lowercase form starts with one of `prefixes` and has a digit right after the prefix (e.g.
/// "USG6000", "C2960"). Returns it upper-cased. Conservative by design — no match ⇒ `None`.
fn extract_model(sysdescr: &str, prefixes: &[&str]) -> Option<String> {
    if prefixes.is_empty() {
        return None;
    }
    for raw in sysdescr.split(|c: char| !(c.is_ascii_alphanumeric() || c == '-')) {
        if raw.len() < 2 {
            continue;
        }
        let lower = raw.to_ascii_lowercase();
        for p in prefixes {
            if let Some(rest) = lower.strip_prefix(p) {
                if rest.chars().next().is_some_and(|c| c.is_ascii_digit()) {
                    return Some(raw.to_ascii_uppercase());
                }
            }
        }
    }
    None
}

/// Suggest a built-in device-profile name from a device's SNMP `sysDescr` (best-effort). `None`
/// when nothing matches — the caller defaults to "Generic SNMP" if the device answered SNMP, else
/// "Generic ping". Thin wrapper over [`identify`] for callers that only want the profile.
#[must_use]
pub fn classify(sysdescr: &str) -> Option<&'static str> {
    identify(sysdescr).profile
}

#[cfg(test)]
mod tests {
    use super::{classify, identify};

    #[test]
    fn classifies_huawei_and_cisco_from_sysdescr() {
        assert_eq!(
            classify("Huawei Versatile Routing Platform Software VRP USG6000"),
            Some("Huawei USG firewall")
        );
        assert_eq!(
            classify("Cisco IOS Software, C2960 Software"),
            Some("Cisco IOS router")
        );
        assert_eq!(classify("Linux server 5.10 net-snmp"), None);
    }

    #[test]
    fn identify_extracts_vendor_model_and_profile() {
        let huawei = identify("Huawei Versatile Routing Platform Software VRP USG6000");
        assert_eq!(huawei.vendor.as_deref(), Some("Huawei"));
        assert_eq!(huawei.model.as_deref(), Some("USG6000"));
        assert_eq!(huawei.profile, Some("Huawei USG firewall"));

        let cisco = identify("Cisco IOS Software, C2960 Software, Version 15.0");
        assert_eq!(cisco.vendor.as_deref(), Some("Cisco"));
        assert_eq!(cisco.model.as_deref(), Some("C2960"));
        assert_eq!(cisco.profile, Some("Cisco IOS router"));
    }

    #[test]
    fn identify_yields_vendor_without_a_confident_model() {
        // RouterOS has no model-token prefix list → vendor only, no invented model.
        let mt = identify("RouterOS RB750 MikroTik");
        assert_eq!(mt.vendor.as_deref(), Some("MikroTik"));
        assert_eq!(mt.model, None);
        assert_eq!(mt.profile, None);
    }

    #[test]
    fn identify_is_empty_for_unknown_devices() {
        let unknown = identify("Linux server 5.10 net-snmp");
        assert_eq!(unknown, super::DeviceIdentity::default());
    }
}
