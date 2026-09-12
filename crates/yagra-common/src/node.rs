// SPDX-License-Identifier: AGPL-3.0-only
//! The core inventory entity: a monitored node.
//!
//! Nodes form a **multi-tier nested** inventory (a node may have a parent), which is
//! what dependency suppression and root-cause roll-up build on. Identity is the
//! stable [`NodeId`]; `name`/`tags` are mutable metadata. Poller assignment uses the
//! `pool` attribute (location-affinity pools, ADR-009).

use crate::ids::{CredentialId, GroupId, NodeId, ProfileId};
use serde::{Deserialize, Serialize};
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
    /// Labels an operator hung on this node — free-form single words or phrases
    /// (`JAPAN`, `core`, `松山本社`), not key=value pairs (ADR-135 inc. 2). Sorted and
    /// de-duplicated, for deterministic output.
    ///
    /// ⚠️ **This is the node's OWN set, not what it effectively carries.** Since ADR-135
    /// inc. 2 a folder carries labels too, and every folder and node beneath it inherits
    /// them; the union is resolved at read time by `TagResolver` in core and is
    /// deliberately never stored here — a written-down copy goes stale the moment a
    /// parent is edited or a folder is moved (the same argument pool and geo inheritance
    /// record). Anything deciding behaviour from a label wants the resolved set.
    pub tags: Vec<String>,
    /// Labels this node refuses to inherit from its folder chain (ADR-135 inc. 2). Almost always
    /// empty.
    ///
    /// ⚠️ **On `Node` for a reason `notes` is not.** The argument that kept a note out of this
    /// type was its size — up to 2,000 characters carried fleet-wide so one page could show it.
    /// An empty `Vec` is 24 bytes, and unlike a note this is read by the same fleet-wide passes
    /// that read `tags`: resolving what a node effectively carries is exactly
    /// `(inherited − this) ∪ tags`, so a resolver that could not see it would have to go back to
    /// the database once per node.
    ///
    /// ⚠️ Only *inherited* labels can be excluded. A label in `tags` is removed by dropping it
    /// from `tags`; naming it here does nothing, because the exclusion is applied before a node's
    /// own labels are added.
    #[serde(default)]
    pub tags_excluded: Vec<String>,
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
            tags: Vec::new(),
            tags_excluded: Vec::new(),
        }
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
        assert_eq!(n.parent, None);
        assert_eq!(n.profile, None);
        assert_eq!(n.pool, None);
        assert!(n.tags.is_empty());
    }

    #[test]
    fn nested_node_reports_parent() {
        let parent = v4("parent");
        let mut child = v4("child");
        child.parent = Some(parent.id);
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

    // `tags_are_looked_up_by_key` lived here and was deleted with the key (ADR-135 inc. 2).
    // `Node::tag(key)` had exactly one caller in the whole workspace — that test — because both
    // readers of this field (`ScopeLevel::Group` thresholds, `WindowScope::Group` maintenance)
    // matched on `tags.values()` and threw the key away. A set of labels is what they were both
    // already treating it as.
}
