// SPDX-License-Identifier: AGPL-3.0-only
//! What is connected to what: stored links and their projection, overrides, L3 addressing and
//! routing, VLANs and wireless (ADR-043/064).

pub(crate) mod l3;
pub(crate) mod l3_routing;
pub(crate) mod link_overrides;
pub(crate) mod topology_links;
pub(crate) mod topology_projection;
pub(crate) mod vlans;
pub(crate) mod wireless;
pub(crate) mod wireless_fanout;
