// SPDX-License-Identifier: AGPL-3.0-only
//! Stable identifiers shared across components.
//!
//! These are the *stable* keys the whole system agrees on: `NodeId`/`IfIndex` are
//! used as TSDB series labels (thin-label model, ADR-011) and as PostgreSQL keys.
//! Human-readable names (sysName, ifDescr, …) are metadata, never identifiers —
//! they live in PostgreSQL and are joined at query time, so they can change without
//! orphaning a time series.

use serde::{Deserialize, Serialize};
use std::fmt;
use uuid::Uuid;

/// Stable identifier for a monitored node.
///
/// A UUID, not a name or address — both of which can change over a node's life.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    PartialOrd,
    Ord,
    Serialize,
    Deserialize,
    utoipa::ToSchema,
)]
#[serde(transparent)]
pub struct NodeId(pub Uuid);

impl NodeId {
    /// Generate a fresh, random node id.
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    /// The underlying UUID.
    #[must_use]
    pub const fn as_uuid(&self) -> Uuid {
        self.0
    }
}

impl Default for NodeId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl From<Uuid> for NodeId {
    fn from(id: Uuid) -> Self {
        Self(id)
    }
}

/// Stable identifier for a device profile / device-class (e.g. "Cisco router").
///
/// Profiles carry the polling templates and default thresholds a node inherits.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    PartialOrd,
    Ord,
    Serialize,
    Deserialize,
    utoipa::ToSchema,
)]
#[serde(transparent)]
pub struct ProfileId(pub Uuid);

impl ProfileId {
    /// Generate a fresh, random profile id.
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    /// The underlying UUID.
    #[must_use]
    pub const fn as_uuid(&self) -> Uuid {
        self.0
    }
}

impl Default for ProfileId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for ProfileId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl From<Uuid> for ProfileId {
    fn from(id: Uuid) -> Self {
        Self(id)
    }
}

/// Stable identifier for a single check on a node (e.g. "icmp liveness", a specific
/// threshold). Part of the alert dedup key `(node, check, severity)`.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    PartialOrd,
    Ord,
    Serialize,
    Deserialize,
    utoipa::ToSchema,
)]
#[serde(transparent)]
pub struct CheckId(pub Uuid);

impl CheckId {
    /// Generate a fresh, random check id.
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    /// The underlying UUID.
    #[must_use]
    pub const fn as_uuid(&self) -> Uuid {
        self.0
    }
}

impl Default for CheckId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for CheckId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl From<Uuid> for CheckId {
    fn from(id: Uuid) -> Self {
        Self(id)
    }
}

/// Stable identifier for a stored monitoring credential (SNMP community / v3 / token).
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    PartialOrd,
    Ord,
    Serialize,
    Deserialize,
    utoipa::ToSchema,
)]
#[serde(transparent)]
pub struct CredentialId(pub Uuid);

impl CredentialId {
    /// Generate a fresh, random credential id.
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    /// The underlying UUID.
    #[must_use]
    pub const fn as_uuid(&self) -> Uuid {
        self.0
    }
}

impl Default for CredentialId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for CredentialId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl From<Uuid> for CredentialId {
    fn from(id: Uuid) -> Self {
        Self(id)
    }
}

/// Stable identifier for a node group (a folder in the hierarchical inventory tree).
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    PartialOrd,
    Ord,
    Serialize,
    Deserialize,
    utoipa::ToSchema,
)]
#[serde(transparent)]
pub struct GroupId(pub Uuid);

impl GroupId {
    /// Generate a fresh, random group id.
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    /// The underlying UUID.
    #[must_use]
    pub const fn as_uuid(&self) -> Uuid {
        self.0
    }
}

impl Default for GroupId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for GroupId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl From<Uuid> for GroupId {
    fn from(id: Uuid) -> Self {
        Self(id)
    }
}

/// SNMP interface index (`ifIndex`).
///
/// Identifies an interface *within a node*. `ifIndex` can be re-numbered on reboot,
/// so the `(NodeId, IfIndex)` pair is re-resolved against PostgreSQL interface
/// metadata on each discovery (ADR-011).
///
/// ⚠️ **A renumbering is NOT recorded anywhere, and this comment used to claim it was.**
/// There is no remap table and no code that writes one — `DELETE FROM interfaces` does not appear
/// in the workspace, and nothing keys an interface by `if_name`. So when a device renumbers, the
/// TSDB series silently becomes a different interface. ADR-011 carries this as remaining work; do
/// not read this doc as a description of behaviour that exists.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    PartialOrd,
    Ord,
    Serialize,
    Deserialize,
    utoipa::ToSchema,
)]
#[serde(transparent)]
pub struct IfIndex(pub u32);

impl fmt::Display for IfIndex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl From<u32> for IfIndex {
    fn from(idx: u32) -> Self {
        Self(idx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_ids_are_unique() {
        assert_ne!(NodeId::new(), NodeId::new());
    }

    #[test]
    fn node_id_is_transparent_in_json() {
        let id = NodeId::from(Uuid::nil());
        let json = serde_json::to_string(&id).unwrap();
        // Serializes as the bare UUID string, not a wrapper object.
        assert_eq!(json, "\"00000000-0000-0000-0000-000000000000\"");
        let back: NodeId = serde_json::from_str(&json).unwrap();
        assert_eq!(id, back);
    }

    #[test]
    fn ifindex_displays_as_number() {
        assert_eq!(IfIndex(3).to_string(), "3");
    }
}
