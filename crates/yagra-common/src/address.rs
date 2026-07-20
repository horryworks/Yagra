// SPDX-License-Identifier: AGPL-3.0-only
//! Address-family helpers.
//!
//! IPv4 and IPv6 are both in scope from the start — never assume v4
//! (coding-conventions / NFR). Addresses are carried as [`std::net::IpAddr`];
//! this enum names the family for metadata, filtering, and labels where the full
//! address is not needed.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::net::IpAddr;

/// Whether an address is IPv4 or IPv6.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AddressFamily {
    /// IPv4.
    V4,
    /// IPv6.
    V6,
}

impl AddressFamily {
    /// The family of a concrete address.
    #[must_use]
    pub const fn of(addr: &IpAddr) -> Self {
        match addr {
            IpAddr::V4(_) => AddressFamily::V4,
            IpAddr::V6(_) => AddressFamily::V6,
        }
    }

    /// Stable lowercase string for labels and API payloads.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            AddressFamily::V4 => "v4",
            AddressFamily::V6 => "v6",
        }
    }
}

impl From<&IpAddr> for AddressFamily {
    fn from(addr: &IpAddr) -> Self {
        Self::of(addr)
    }
}

impl fmt::Display for AddressFamily {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    #[test]
    fn family_of_v4_and_v6() {
        let v4 = IpAddr::V4(Ipv4Addr::LOCALHOST);
        let v6 = IpAddr::V6(Ipv6Addr::LOCALHOST);
        assert_eq!(AddressFamily::of(&v4), AddressFamily::V4);
        assert_eq!(AddressFamily::of(&v6), AddressFamily::V6);
    }

    #[test]
    fn family_serializes_lowercase() {
        assert_eq!(serde_json::to_string(&AddressFamily::V6).unwrap(), "\"v6\"");
    }
}
