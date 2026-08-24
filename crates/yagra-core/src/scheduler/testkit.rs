// SPDX-License-Identifier: AGPL-3.0-only
//! Fixtures more than one of the split files needs.
//!
//! `item`, `optical_item`, `v3_secret` and `node` are used by both the [`checks`](super::checks)
//! and [`assemble`](super::assemble) tests. Same shape as `events/testkit.rs` and
//! `mcp/tools/testkit.rs`.

use crate::secrets::SnmpV3Secret;
use std::net::{IpAddr, Ipv4Addr};
use yagra_common::{CollectionItem, CollectionKind, Node, NodeId, OpticalFlavor};

pub(super) fn item(metric: &str, oid: &str, kind: CollectionKind) -> CollectionItem {
    CollectionItem {
        metric_name: metric.to_owned(),
        oid: oid.to_owned(),
        kind,
        metric_kind: yagra_common::MetricKind::Gauge,
    }
}

/// An optical item for `flavor`, publishing under `metric`.
pub(super) fn optical_item(metric: &str, flavor: OpticalFlavor) -> CollectionItem {
    item(metric, flavor.root_oid(), CollectionKind::Optical)
}

pub(super) fn v3_secret() -> SnmpV3Secret {
    SnmpV3Secret::parse(
        br#"{"user":"monitor","security_level":"authpriv","auth_protocol":"sha256",
             "auth_key":"a-pass","priv_protocol":"aes128","priv_key":"p-pass"}"#,
    )
    .expect("valid v3 secret")
}

pub(super) fn node(name: &str) -> Node {
    Node::new(NodeId::new(), name, IpAddr::V4(Ipv4Addr::new(10, 0, 0, 20)))
}
