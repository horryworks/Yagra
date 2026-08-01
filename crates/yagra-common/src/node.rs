// SPDX-License-Identifier: AGPL-3.0-only
//! The core inventory entity: a monitored node.
//!
//! Nodes form a **multi-tier nested** inventory (a node may have a parent), which is
//! what dependency suppression and root-cause roll-up build on. Identity is the
//! stable [`NodeId`]; `name`/`tags` are mutable metadata. Poller assignment uses the
//! `pool` attribute (location-affinity pools, ADR-009).

use crate::ids::{CredentialId, GroupId, NodeId, ProfileId};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::net::IpAddr;

/// A monitored node (device or server).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Node {
    /// Stable identity. Never derived from name/address (both can change).
    pub id: NodeId,
    /// Human-readable name (sysName, hostname, …) — metadata, not an identifier.
    pub name: String,
    /// Parent in the nested inventory, if any. `None` means a top-level node.
    pub parent: Option<NodeId>,
    /// Primary management address. IPv4 or IPv6 — both are first-class.
    pub address: IpAddr,
    /// Device-class / profile this node inherits polling templates and default
    /// thresholds from.
    pub profile: Option<ProfileId>,
    /// Poller-assignment attribute (ADR-009). Maps the node to a poller pool;
    /// conventionally the site, but free to mirror region or any topology.
    pub pool: Option<String>,
    /// Monitoring credential bound to this node (SNMP community / v3 / token), if any.
    /// The poller never reads the store; core resolves/inlines the secret (ADR-018/020).
    pub credential: Option<CredentialId>,
    /// Maker / manufacturer, descriptive metadata (best-effort from discovery's sysDescr
    /// classification, editable). Shown in the node display; never a TSDB label.
    pub vendor: Option<String>,
    /// Model / product designation, descriptive metadata (same source as `vendor`).
    pub model: Option<String>,
    /// The hierarchical group (folder) this node belongs to, if any. `None` ⇒ ungrouped
    /// (shown at the tree root). A node belongs to at most one group.
    pub group: Option<GroupId>,
    /// Arbitrary grouping attributes (site, region, role, …) used for filtering
    /// and group-scoped thresholds/visibility. Sorted for deterministic output.
    pub tags: BTreeMap<String, String>,
}

impl Node {
    /// A new top-level node with the required identity, name, and address.
    #[must_use]
    pub fn new(id: NodeId, name: impl Into<String>, address: IpAddr) -> Self {
        Self {
            id,
            name: name.into(),
            parent: None,
            address,
            profile: None,
            pool: None,
            credential: None,
            vendor: None,
            model: None,
            group: None,
            tags: BTreeMap::new(),
        }
    }

    /// Whether this node is at the top of the inventory (has no parent).
    #[must_use]
    pub const fn is_root(&self) -> bool {
        self.parent.is_none()
    }

    /// Look up a grouping tag value.
    #[must_use]
    pub fn tag(&self, key: &str) -> Option<&str> {
        self.tags.get(key).map(String::as_str)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    fn v4(node_name: &str) -> Node {
        Node::new(
            NodeId::new(),
            node_name,
            IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
        )
    }

    #[test]
    fn new_node_is_root_with_no_profile_or_pool() {
        let n = v4("core-router");
        assert!(n.is_root());
        assert_eq!(n.profile, None);
        assert_eq!(n.pool, None);
        assert!(n.tags.is_empty());
    }

    #[test]
    fn nested_node_reports_parent() {
        let parent = v4("parent");
        let mut child = v4("child");
        child.parent = Some(parent.id);
        assert!(!child.is_root());
        assert_eq!(child.parent, Some(parent.id));
    }

    #[test]
    fn a_node_carries_either_address_family() {
        // IPv4 and IPv6 are both in scope from the start (coding-conventions / NFR): the address
        // is an `IpAddr` throughout, never a v4-shaped field.
        let n4 = v4("a");
        assert!(n4.address.is_ipv4());

        let n6 = Node::new(NodeId::new(), "b", IpAddr::V6(Ipv6Addr::LOCALHOST));
        assert!(n6.address.is_ipv6());
    }

    #[test]
    fn tags_are_looked_up_by_key() {
        let mut n = v4("a");
        n.tags.insert("site".into(), "tokyo".into());
        assert_eq!(n.tag("site"), Some("tokyo"));
        assert_eq!(n.tag("region"), None);
    }
}
