//! Yagra-discovery — device discovery and classification.
//!
//! IP-range / SNMP sweep / LLDP-CDP based discovery, classification into profiles, and
//! the built-in **Credential Finder** that probes candidate credentials to find the one a
//! device accepts. The per-device probe rate limiter ([`credential_finder`]) is in place;
//! the sweep itself runs on the poller (raw-socket ICMP). Profile suggestion is owned by the
//! core `Classifier` (authoritative `sysObjectID` rules); [`identify`] here only extracts a
//! best-effort vendor/model from the free-form `sysDescr` to pre-fill the import form.

pub mod credential_finder;

pub use credential_finder::{AttemptDecision, CredentialProbeLimiter, LimiterConfig};

/// Vendor/model extracted from a device's SNMP `sysDescr` (best-effort, case-insensitive). Both
/// fields are optional — `sysDescr` is free-form, untrusted device text, so this is a heuristic to
/// pre-fill the operator's import form, not an authority. `vendor` and `model` are stored as
/// descriptive node metadata (never TSDB labels). Profile suggestion is *not* done here — the core
/// `Classifier` resolves it authoritatively from `sysObjectID` (and `sysDescr` rules).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeviceIdentity {
    /// Maker / manufacturer (e.g. "Huawei", "Cisco"), if recognised.
    pub vendor: Option<String>,
    /// Model / product token (e.g. "USG6000", "C2960"), if one is confidently extractable.
    pub model: Option<String>,
}

/// Identify a device from its SNMP `sysDescr`: maker + a best-effort model token (vendor/model
/// only — profile suggestion is the core `Classifier`'s job). Vendor detection is a conservative
/// keyword match; model extraction only fires for a token that begins with a known vendor prefix
/// immediately followed by a digit (so a wrong model is never invented from arbitrary words).
/// Everything here is editable by the operator on import.
#[must_use]
pub fn identify(sysdescr: &str) -> DeviceIdentity {
    let d = sysdescr.to_ascii_lowercase();
    // (vendor, model-token prefixes to look for).
    let (vendor, prefixes): (Option<&str>, &[&str]) =
        if d.contains("huawei") || d.contains("vrp") || d.contains("usg") {
            (Some("Huawei"), &["usg", "ar", "ne", "ce", "s"])
        } else if d.contains("cisco") || d.contains("ios") {
            (Some("Cisco"), &["ws-c", "isr", "asr", "nexus", "c"])
        } else if d.contains("juniper") || d.contains("junos") {
            (Some("Juniper"), &["mx", "ex", "srx", "qfx"])
        } else if d.contains("arista") {
            (Some("Arista"), &["dcs", "ccs"])
        } else if d.contains("mikrotik") || d.contains("routeros") {
            (Some("MikroTik"), &[])
        } else if d.contains("ubiquiti") || d.contains("edgeos") || d.contains("unifi") {
            (Some("Ubiquiti"), &[])
        } else if d.contains("fortinet") || d.contains("fortigate") {
            (Some("Fortinet"), &["fg", "fgt"])
        } else if d.contains("paloalto") || d.contains("pan-os") {
            (Some("Palo Alto"), &["pa"])
        } else {
            (None, &[])
        };
    DeviceIdentity {
        vendor: vendor.map(str::to_owned),
        model: extract_model(sysdescr, prefixes),
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

#[cfg(test)]
mod tests {
    use super::identify;

    #[test]
    fn identify_extracts_vendor_and_model() {
        let huawei = identify("Huawei Versatile Routing Platform Software VRP USG6000");
        assert_eq!(huawei.vendor.as_deref(), Some("Huawei"));
        assert_eq!(huawei.model.as_deref(), Some("USG6000"));

        let cisco = identify("Cisco IOS Software, C2960 Software, Version 15.0");
        assert_eq!(cisco.vendor.as_deref(), Some("Cisco"));
        assert_eq!(cisco.model.as_deref(), Some("C2960"));

        // A device with no recognised vendor keyword yields nothing.
        assert_eq!(identify("Linux server 5.10 net-snmp").vendor, None);
    }

    #[test]
    fn identify_yields_vendor_without_a_confident_model() {
        // RouterOS has no model-token prefix list → vendor only, no invented model.
        let mt = identify("RouterOS RB750 MikroTik");
        assert_eq!(mt.vendor.as_deref(), Some("MikroTik"));
        assert_eq!(mt.model, None);
    }

    #[test]
    fn identify_is_empty_for_unknown_devices() {
        let unknown = identify("Linux server 5.10 net-snmp");
        assert_eq!(unknown, super::DeviceIdentity::default());
    }
}
