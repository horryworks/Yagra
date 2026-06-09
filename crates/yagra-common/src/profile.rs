//! Device-class / profile: the polling-and-threshold template a node inherits.
//!
//! A profile groups the default thresholds (and, later, polling templates / OID sets)
//! for a class of device (e.g. "Cisco router"). Profiles may themselves nest via
//! `parent`. A node's effective thresholds resolve across profile → group → node
//! (ADR-013); this type carries only the profile-level contribution.

use crate::ids::ProfileId;
use crate::thresholds::ThresholdRule;
use serde::{Deserialize, Serialize};

/// A device-class / profile.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Profile {
    /// Stable identity.
    pub id: ProfileId,
    /// Human-readable name (e.g. "Cisco IOS router").
    pub name: String,
    /// Parent profile to inherit from, if any.
    pub parent: Option<ProfileId>,
    /// Default thresholds contributed at the profile scope.
    pub thresholds: Vec<ThresholdRule>,
}

impl Profile {
    /// A new profile with no parent and no thresholds.
    #[must_use]
    pub fn new(id: ProfileId, name: impl Into<String>) -> Self {
        Self {
            id,
            name: name.into(),
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
    }
}
