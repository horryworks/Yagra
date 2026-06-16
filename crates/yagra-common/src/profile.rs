//! Device-class / profile: the polling-and-threshold template a node inherits.
//!
//! A profile groups the default thresholds (and, later, polling templates / OID sets)
//! for a class of device (e.g. "Cisco IOS/IOS-XE router"). A profile is split along the
//! axis that actually changes what Yagra collects: its functional [`ProfileCategory`]
//! (role — router/switch/firewall/…) crossed with its `vendor`-NOS family. Profiles may
//! themselves nest via `parent`. A node's effective thresholds resolve across
//! profile → group → node (ADR-013); this type carries only the profile-level contribution.

use crate::ids::ProfileId;
use crate::thresholds::ThresholdRule;
use serde::{Deserialize, Serialize};

/// The functional role of a device class — the primary split axis for profiles and the
/// grouping the Device-profiles UI sections by. Serialized to a kebab-case TEXT column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum ProfileCategory {
    Router,
    L3Switch,
    L2Switch,
    Firewall,
    WirelessController,
    WirelessAp,
    LoadBalancer,
    Server,
    Hypervisor,
    Storage,
    Power,
    Printer,
    /// Operator-created profiles default here.
    #[default]
    GenericSnmp,
    PingOnly,
}

impl ProfileCategory {
    /// Every category, in display order (router → switches → … → generic).
    pub const ALL: [ProfileCategory; 14] = [
        ProfileCategory::Router,
        ProfileCategory::L3Switch,
        ProfileCategory::L2Switch,
        ProfileCategory::Firewall,
        ProfileCategory::WirelessController,
        ProfileCategory::WirelessAp,
        ProfileCategory::LoadBalancer,
        ProfileCategory::Server,
        ProfileCategory::Hypervisor,
        ProfileCategory::Storage,
        ProfileCategory::Power,
        ProfileCategory::Printer,
        ProfileCategory::GenericSnmp,
        ProfileCategory::PingOnly,
    ];

    /// The stable kebab-case token stored in the DB / sent over the API.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            ProfileCategory::Router => "router",
            ProfileCategory::L3Switch => "l3-switch",
            ProfileCategory::L2Switch => "l2-switch",
            ProfileCategory::Firewall => "firewall",
            ProfileCategory::WirelessController => "wireless-controller",
            ProfileCategory::WirelessAp => "wireless-ap",
            ProfileCategory::LoadBalancer => "load-balancer",
            ProfileCategory::Server => "server",
            ProfileCategory::Hypervisor => "hypervisor",
            ProfileCategory::Storage => "storage",
            ProfileCategory::Power => "power",
            ProfileCategory::Printer => "printer",
            ProfileCategory::GenericSnmp => "generic-snmp",
            ProfileCategory::PingOnly => "ping-only",
        }
    }

    /// Parse the DB/API token back into a category (for validating operator input).
    #[must_use]
    pub fn from_token(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.as_str() == s)
    }
}

/// A device-class / profile.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Profile {
    /// Stable identity.
    pub id: ProfileId,
    /// Human-readable name (e.g. "Cisco IOS/IOS-XE router").
    pub name: String,
    /// Functional role — the primary split axis / UI grouping.
    pub category: ProfileCategory,
    /// Vendor label (e.g. "Cisco"). Descriptive metadata only — never a TSDB label.
    pub vendor: Option<String>,
    /// Parent profile to inherit from, if any.
    pub parent: Option<ProfileId>,
    /// Default thresholds contributed at the profile scope.
    pub thresholds: Vec<ThresholdRule>,
}

impl Profile {
    /// A new generic profile with no parent and no thresholds.
    #[must_use]
    pub fn new(id: ProfileId, name: impl Into<String>) -> Self {
        Self {
            id,
            name: name.into(),
            category: ProfileCategory::default(),
            vendor: None,
            parent: None,
            thresholds: Vec::new(),
        }
    }

    /// Whether this profile inherits from another.
    #[must_use]
    pub const fn is_root(&self) -> bool {
        self.parent.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_profile_is_root_without_thresholds() {
        let p = Profile::new(ProfileId::new(), "Cisco router");
        assert!(p.is_root());
        assert!(p.thresholds.is_empty());
        assert_eq!(p.category, ProfileCategory::GenericSnmp);
    }

    #[test]
    fn category_token_roundtrips() {
        for c in ProfileCategory::ALL {
            assert_eq!(ProfileCategory::from_token(c.as_str()), Some(c));
        }
        assert_eq!(ProfileCategory::from_token("nonsense"), None);
    }

    #[test]
    fn category_serializes_kebab_case() {
        assert_eq!(
            serde_json::to_string(&ProfileCategory::L3Switch).unwrap(),
            "\"l3-switch\""
        );
    }
}
