//! Yagra-discovery — device discovery and classification.
//!
//! IP-range / SNMP sweep / LLDP-CDP based discovery, classification into profiles, and
//! the built-in **Credential Finder** that probes candidate credentials to find the one a
//! device accepts. The per-device probe rate limiter ([`credential_finder`]) is in place;
//! the sweep itself runs on the poller (raw-socket ICMP), and [`classify`] maps a device's
//! SNMP `sysDescr` to a suggested built-in device profile.

pub mod credential_finder;

pub use credential_finder::{AttemptDecision, CredentialProbeLimiter, LimiterConfig};

/// Suggest a built-in device-profile name for a device from its SNMP `sysDescr` (best-effort,
/// case-insensitive substring match). `None` when nothing matches — the caller defaults to
/// "Generic SNMP" if the device answered SNMP, else "Generic ping". The returned names match
/// the built-in profiles seeded by core (`yagra_common::builtin_profiles`).
#[must_use]
pub fn classify(sysdescr: &str) -> Option<&'static str> {
    let d = sysdescr.to_ascii_lowercase();
    if d.contains("huawei") || d.contains("usg") || d.contains("vrp") {
        Some("Huawei USG firewall")
    } else if d.contains("cisco") || d.contains("ios") {
        Some("Cisco IOS router")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::classify;

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
}
