// SPDX-License-Identifier: AGPL-3.0-only
//! Yagra-discovery — the *pure* half of device discovery: `sysDescr` identification and the
//! Credential Finder's probe rate limiter.
//!
//! Despite the name, the discovery **sweep** is not here — it lives in `yagra-poller`
//! (`discovery.rs`), because it needs sockets. Nor is the classifier: profile suggestion is owned
//! authoritatively by `yagra-core`'s `Classifier` (`sysObjectID`/`sysDescr` rules, operator-
//! editable), over the rule table seeded from `yagra-common`. What this crate holds is the logic
//! that must stay I/O-free and unit-testable:
//!
//!  - [`identify`] — best-effort vendor/model extraction from free-form, untrusted `sysDescr`,
//!    used only to pre-fill the operator's import form.
//!  - `credential_finder` — the per-device rate limiter for the **Credential Finder**, which
//!    probes candidate credentials to find the one a device accepts. Enforced by the poller's
//!    sweep; rate limiting is what keeps probing from tripping device account lockout.
//!  - [`os_version`] — where each OS family keeps its version and how to read it out of what an
//!    identity probe returned (ADR-138). The poller asks it which OIDs to read and what they mean;
//!    core asks it only to [`os_version::sanitize`] what arrived.
//!  - [`serial`] — which of ENTITY-MIB's rows hold the device's serial number, and the cap on it
//!    (ADR-147). The poller walks the two columns it names; core asks it only to [`serial::sanitize`].
//!  - [`normalize_sys_object_id`] / [`sanitize_sys_descr`] — what a node keeps of the two values the
//!    classification rules match on, so they can be re-run on a node that already exists (ADR-140).
//!    Applied by the poller and again by core, which cannot assume which poller sent them.

mod credential_finder;
pub mod os_version;
pub mod serial;

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

/// The longest `sysDescr` a node keeps (ADR-140). RFC 1213 caps it at 255 octets; the margin is for
/// devices that ignore that, and the cap is what stops one from filling a row.
pub const SYS_DESCR_MAX_CHARS: usize = 1024;

/// A `sysObjectID` as the classification rules compare it — dotted decimal such as
/// `1.3.6.1.4.1.9.1.516` — or `None` if what the device sent is not one (ADR-140).
///
/// Surrounding whitespace and net-snmp's leading-dot spelling (`.1.3.6…`) are dropped. Anything else
/// that is not digits and single dots is refused rather than stored, because a rule matches it as a
/// prefix and a mangled value would quietly match nothing. `0.0.0` is kept: some Ruckus APs really
/// answer that, and only their `sysDescr` then identifies them.
#[must_use]
pub fn normalize_sys_object_id(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    let oid = trimmed.strip_prefix('.').unwrap_or(trimmed);
    let valid = !oid.is_empty()
        && oid.len() <= SYS_DESCR_MAX_CHARS
        && oid
            .split('.')
            .all(|arc| !arc.is_empty() && arc.bytes().all(|b| b.is_ascii_digit()));
    valid.then(|| oid.to_owned())
}

/// A `sysDescr` as a node keeps it for the classification rules (ADR-140).
///
/// Control characters are dropped **except** `\r`, `\n` and `\t`: a VRP or YunShan description spans
/// several lines, and a rule may match across them, so flattening it the way
/// [`os_version::sanitize`] does would change what a rule sees. The result is cut at
/// [`SYS_DESCR_MAX_CHARS`] and trimmed; `None` when nothing is left.
#[must_use]
pub fn sanitize_sys_descr(raw: &str) -> Option<String> {
    let kept: String = raw
        .chars()
        .filter(|c| !c.is_control() || matches!(c, '\r' | '\n' | '\t'))
        .take(SYS_DESCR_MAX_CHARS)
        .collect();
    let trimmed = kept.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

#[cfg(test)]
mod tests {
    use super::{identify, normalize_sys_object_id, sanitize_sys_descr, SYS_DESCR_MAX_CHARS};

    #[test]
    fn a_sys_object_id_is_kept_only_as_dotted_decimal() {
        assert_eq!(
            normalize_sys_object_id(" .1.3.6.1.4.1.9.1.516 ").as_deref(),
            Some("1.3.6.1.4.1.9.1.516")
        );
        assert_eq!(normalize_sys_object_id("0.0.0").as_deref(), Some("0.0.0"));
        for bad in [
            "",
            "   ",
            "1.3..6",
            "1.3.6.",
            "SNMPv2-SMI::enterprises.9.1.516",
            "1.3.6\n1",
        ] {
            assert_eq!(normalize_sys_object_id(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn a_sys_descr_keeps_its_line_breaks_and_loses_other_controls() {
        let vrp = "Huawei YunShan OS \r\nVersion 1.24.0.1 (USG V600R024C00SPC100)\u{0}\u{7} \r\n";
        assert_eq!(
            sanitize_sys_descr(vrp).as_deref(),
            Some("Huawei YunShan OS \r\nVersion 1.24.0.1 (USG V600R024C00SPC100)")
        );
        assert_eq!(sanitize_sys_descr(" \u{1b}\t\r\n"), None);
        let long = "x".repeat(SYS_DESCR_MAX_CHARS + 10);
        assert_eq!(
            sanitize_sys_descr(&long).map(|s| s.chars().count()),
            Some(SYS_DESCR_MAX_CHARS)
        );
    }

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
