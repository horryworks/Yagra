// SPDX-License-Identifier: AGPL-3.0-only
//! A node's VLANs, by name (ADR-257): `GET /api/v1/nodes/{node_id}/vlans`, and the same rows under
//! `vlans` in the MCP `get_node_status` tool.
//!
//! Three stores feed one list, and the joining is [`vlan_rows`], which is pure:
//!
//! - the port VLAN snapshot (ADR-201), whose `names` a Cisco or Huawei walk fills;
//! - the node's interfaces and their addresses, for the gateway — a switch virtual interface named
//!   `Vlan30` / `Vlanif30` routes VLAN 30 (`yagra_common::svi_vlan`);
//! - for a Meraki node, its network's MX VLAN list (ADR-164 decision 28), which names the VLANs
//!   and whose MX is their gateway.
//!
//! Port counts are deliberately not here (decision 7): the WebUI counts them from the interface
//! list with the predicate its VLAN filter uses, so a count and the list it opens cannot disagree.

use super::collection::{addresses_by_ifindex, InterfaceAddress};
use super::error::{ApiError, ApiResult};
use super::extract::{RequireView, Scoped, VisibleNode};
use super::ApiState;
use axum::{
    extract::{Path, State},
    routing::get,
    Json, Router,
};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;
use yagra_common::{svi_vlan, PortMode, VlanDialect, VlanSnapshot, VlanState};
use yagra_meraki::inventory::NetworkVlan;

/// This domain's slice of the OpenAPI document (ADR-035), merged by [`super::openapi::document`].
#[derive(utoipa::OpenApi)]
#[openapi(paths(list_node_vlans))]
pub(super) struct Doc;

/// The VLAN routes, merged into `/api/v1` by [`super::router`].
pub(super) fn routes() -> Router<ApiState> {
    Router::new().route("/api/v1/nodes/:node_id/vlans", get(list_node_vlans))
}

/// Whether a VLAN the device defines is forwarding. Only Cisco's VTP table says.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum NodeVlanState {
    Active,
    Suspended,
}

/// Where a VLAN is routed: a switch virtual interface on this node, or — for a Meraki switch — the
/// network's MX.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub(crate) struct VlanGateway {
    /// The interface's name (`Vl30`, `Vlanif30`). `null` for an MX, which has no interface row.
    pub(crate) if_name: Option<String>,
    /// The interface's `ifIndex`. `null` for an MX.
    pub(crate) ifindex: Option<u32>,
    /// The gateway's addresses on this VLAN. Empty when the address walk has not run yet.
    pub(crate) addresses: Vec<InterfaceAddress>,
    /// The node that routes it, when that is **another** node — a Meraki switch's MX. `null` when
    /// it is this node.
    pub(crate) node_id: Option<Uuid>,
}

/// One VLAN of a node (ADR-257).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub(crate) struct NodeVlan {
    /// The VLAN ID, 1-4094.
    pub(crate) vlan_id: u16,
    /// The name the device (or the Meraki Dashboard) gives it, verbatim. `null` when the names were
    /// not read; an empty string when the device gives this VLAN none.
    pub(crate) name: Option<String>,
    /// `active` or `suspended`, on a Cisco switch. `null` where the device does not say.
    pub(crate) state: Option<NodeVlanState>,
    /// `false`: a port uses this VLAN and the device's own VLAN table does not list it — a
    /// configuration gap. `null` when that cannot be judged (names not read, or a Meraki switch,
    /// whose own VLANs need no definition on the MX).
    pub(crate) defined: Option<bool>,
    /// The VLAN's subnet as the Meraki Dashboard writes it (`10.1.0.0/24`). `null` elsewhere.
    pub(crate) subnet: Option<String>,
    /// Where the VLAN is routed, when Yagra can tell.
    pub(crate) gateway: Option<VlanGateway>,
}

/// What a Meraki node's network contributes, already narrowed to what the caller may see.
pub(crate) struct MerakiVlans {
    /// The network's MX VLANs; `None` until a core that keeps them has read the network.
    pub(crate) vlans: Option<Vec<NetworkVlan>>,
    /// The node is the MX itself, so it is every VLAN's gateway.
    pub(crate) is_mx: bool,
    /// The network's MX, when one is monitored here **and visible to the caller**.
    pub(crate) mx_node: Option<Uuid>,
}

/// Everything [`vlan_rows`] joins.
pub(crate) struct VlanSources<'a> {
    pub(crate) snapshot: Option<&'a VlanSnapshot>,
    pub(crate) interfaces: &'a [yagra_base::repo::InterfaceMeta],
    pub(crate) addresses: &'a BTreeMap<u32, Vec<InterfaceAddress>>,
    pub(crate) meraki: Option<MerakiVlans>,
}

/// The VLANs a port uses by its own configuration: its access and voice VLAN, its native VLAN.
/// A trunk's allowed list is left out on purpose (decision 5) — `1-4094` is a setting, not 4,094
/// VLANs — and so is a member, which only repeats its aggregate.
fn used_by_ports(snapshot: Option<&VlanSnapshot>) -> BTreeSet<u16> {
    let mut used = BTreeSet::new();
    for p in snapshot.iter().flat_map(|s| &s.ports) {
        if p.lag_ifindex.is_some() {
            continue;
        }
        match p.mode {
            PortMode::Access => used.extend(p.access_vlan.into_iter().chain(p.voice_vlan)),
            PortMode::Trunk | PortMode::Hybrid => used.extend(p.native),
            PortMode::NotL2 | PortMode::Unknown => {}
        }
    }
    used
}

/// An MX's address on a VLAN, with the prefix length its subnet gives.
fn mx_address(v: &NetworkVlan) -> Vec<InterfaceAddress> {
    let prefix_len = v
        .subnet
        .as_deref()
        .and_then(|s| s.split_once('/'))
        .and_then(|(_, p)| p.trim().parse::<u8>().ok());
    v.appliance_ip
        .iter()
        .map(|ip| InterfaceAddress {
            ip: ip.to_string(),
            prefix_len,
        })
        .collect()
}

/// Join a node's VLAN sources into its VLAN list, ordered by ID (ADR-257 decisions 5 and 6). Pure.
pub(crate) fn vlan_rows(src: VlanSources<'_>) -> Vec<NodeVlan> {
    let used = used_by_ports(src.snapshot);
    // Each SVI by the VLAN it routes, the lowest ifIndex winning should two claim one.
    let mut svis: BTreeMap<u16, &yagra_base::repo::InterfaceMeta> = BTreeMap::new();
    for m in src.interfaces {
        if let Some(v) = m.if_name.as_deref().and_then(svi_vlan) {
            svis.entry(v)
                .and_modify(|cur| {
                    if m.ifindex < cur.ifindex {
                        *cur = m;
                    }
                })
                .or_insert(m);
        }
    }
    let snmp_gateway = |v: u16| {
        svis.get(&v).map(|m| {
            let ifindex = u32::try_from(m.ifindex).ok();
            VlanGateway {
                if_name: m.if_name.clone(),
                ifindex,
                addresses: ifindex
                    .and_then(|i| src.addresses.get(&i).cloned())
                    .unwrap_or_default(),
                node_id: None,
            }
        })
    };

    let mut out = Vec::new();
    match &src.meraki {
        None => {
            let names: Option<BTreeMap<u16, &yagra_common::VlanName>> = src
                .snapshot
                .and_then(|s| s.names.as_ref())
                .map(|n| n.iter().map(|v| (v.id, v)).collect());
            let ids: BTreeSet<u16> = used
                .iter()
                .copied()
                .chain(names.iter().flat_map(|n| n.keys().copied()))
                .collect();
            for id in ids {
                let named = names.as_ref().and_then(|n| n.get(&id));
                out.push(NodeVlan {
                    vlan_id: id,
                    name: named.map(|v| v.name.clone()),
                    state: named.and_then(|v| match v.state? {
                        VlanState::Active => Some(NodeVlanState::Active),
                        VlanState::Suspended => Some(NodeVlanState::Suspended),
                        VlanState::Unknown => None,
                    }),
                    defined: names.as_ref().map(|n| n.contains_key(&id)),
                    subnet: None,
                    gateway: snmp_gateway(id),
                });
            }
        }
        Some(m) => {
            let listed: BTreeMap<u16, &NetworkVlan> =
                m.vlans.iter().flatten().map(|v| (v.id, v)).collect();
            let ids: BTreeSet<u16> = used.iter().copied().chain(listed.keys().copied()).collect();
            for id in ids {
                let v = listed.get(&id);
                let gateway = v.and_then(|v| {
                    if m.is_mx {
                        Some(VlanGateway {
                            if_name: None,
                            ifindex: None,
                            addresses: mx_address(v),
                            node_id: None,
                        })
                    } else {
                        m.mx_node.map(|node| VlanGateway {
                            if_name: None,
                            ifindex: None,
                            addresses: mx_address(v),
                            node_id: Some(node),
                        })
                    }
                });
                out.push(NodeVlan {
                    vlan_id: id,
                    name: v.map(|v| v.name.clone()),
                    state: None,
                    // A switch-only VLAN needs no definition on the MX (decision 5).
                    defined: None,
                    subnet: v.and_then(|v| v.subnet.clone()),
                    gateway,
                });
            }
        }
    }
    out
}

/// Whether the VLANs tab is offered for a node (ADR-257 decision 8): a Cisco or Huawei device that
/// is polled over SNMP, or a Meraki switch or MX. The one place the rule lives; the node detail
/// carries it as `vlans_read`.
pub(crate) fn vlans_read_for(
    vendor: Option<&str>,
    snmp_configured: bool,
    meraki_product_type: Option<&str>,
) -> bool {
    match meraki_product_type {
        Some(p) => matches!(
            p.trim().to_ascii_lowercase().as_str(),
            "switch" | "appliance"
        ),
        None => snmp_configured && vendor.and_then(VlanDialect::for_vendor).is_some(),
    }
}

/// The MX a Meraki node's VLANs are routed by, as the caller may see it: the first of the
/// network's MXs (lowest node id first) that is another node and visible. In a warm-spare pair
/// whose first MX sits in a folder the caller cannot see, the second one still answers. Pure.
fn gateway_mx(mx_nodes: &[Uuid], node_id: Uuid, visible: impl Fn(Uuid) -> bool) -> Option<Uuid> {
    mx_nodes
        .iter()
        .copied()
        .find(|mx| *mx != node_id && visible(*mx))
}

/// Read and join one node's VLANs — the REST handler and `get_node_status` both call this, so the
/// two cannot answer differently. `visible` says whether the caller may see another node; it
/// decides whether a Meraki switch's MX may be named as its gateway.
pub(crate) async fn node_vlans(
    admin: &super::AdminState,
    node_id: Uuid,
    visible: impl Fn(Uuid) -> bool,
) -> anyhow::Result<Vec<NodeVlan>> {
    let interfaces = admin.repo.list_interfaces(node_id).await?;
    let l3 = admin.l3.current(node_id).await?;
    let addresses = addresses_by_ifindex(l3.as_ref());
    let snapshot = admin.vlans.current(node_id).await?;
    let meraki = admin
        .meraki_devices
        .vlan_context(node_id)
        .await?
        .map(|c| MerakiVlans {
            is_mx: c.product_type.trim().eq_ignore_ascii_case("appliance"),
            vlans: c.vlans,
            mx_node: gateway_mx(&c.mx_nodes, node_id, &visible),
        });
    Ok(vlan_rows(VlanSources {
        snapshot: snapshot.as_ref(),
        interfaces: &interfaces,
        addresses: &addresses,
        meraki,
    }))
}

/// A node's VLANs with their names, states and gateways (ADR-257).
///
/// `View`, like the interface list it sits beside. Skeleton mode answers an empty list: the VLANs
/// are PostgreSQL-only, so "none known" is the truthful answer.
#[utoipa::path(
    get, path = "/api/v1/nodes/{node_id}/vlans", tag = "collection",
    params(("node_id" = Uuid, Path, description = "Node id")),
    responses(
        (status = 200, description = "The node's VLANs by ID: the ones its VLAN table defines and the ones its ports use. Empty until the hourly VLAN walk (or, for Meraki, the network's LAN read) has run, and in skeleton mode", body = Vec<NodeVlan>),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks View", body = super::error::ErrorBody),
    ),
)]
async fn list_node_vlans(
    _guard: RequireView,
    _visible: VisibleNode,
    Scoped(scope): Scoped,
    State(st): State<ApiState>,
    Path(node_id): Path<Uuid>,
) -> ApiResult<Json<Vec<NodeVlan>>> {
    let Some(admin) = st.admin.as_ref() else {
        return Ok(Json(Vec::new()));
    };
    let rows = node_vlans(admin, node_id, |n| {
        scope.allows_node(&st, yagra_common::NodeId::from(n))
    })
    .await
    .map_err(|e| ApiError::from_internal(e.as_ref(), "read VLANs", "failed to read VLANs"))?;
    Ok(Json(rows))
}

#[cfg(test)]
mod tests {
    use super::*;
    use yagra_base::repo::InterfaceMeta;
    use yagra_common::{PortVlan, VlanName};

    fn meta(ifindex: i32, name: &str) -> InterfaceMeta {
        InterfaceMeta {
            ifindex,
            if_name: Some(name.to_owned()),
            ..InterfaceMeta::default()
        }
    }
    fn access(ifindex: u32, vlan: u16, voice: Option<u16>) -> PortVlan {
        PortVlan {
            access_vlan: Some(vlan),
            voice_vlan: voice,
            ..PortVlan::new(ifindex, PortMode::Access)
        }
    }
    fn trunk(ifindex: u32, native: u16) -> PortVlan {
        PortVlan {
            native: Some(native),
            allowed: vec![(1, 4094)],
            ..PortVlan::new(ifindex, PortMode::Trunk)
        }
    }
    fn name(id: u16, n: &str, state: Option<VlanState>) -> VlanName {
        VlanName {
            id,
            name: n.into(),
            state,
        }
    }
    fn addr(ip: &str, len: u8) -> InterfaceAddress {
        InterfaceAddress {
            ip: ip.into(),
            prefix_len: Some(len),
        }
    }
    fn ids(rows: &[NodeVlan]) -> Vec<u16> {
        rows.iter().map(|r| r.vlan_id).collect()
    }

    /// The Catalyst shape: names read, two SVIs (one spelt as ifName), an access port in a VLAN the
    /// table does not list, a trunk allowing every VLAN, and a member that only repeats its LAG.
    #[test]
    fn a_switch_lists_its_named_and_used_vlans_with_their_gateways() {
        let mut member = trunk(9, 999);
        member.lag_ifindex = Some(8);
        let snap = VlanSnapshot::new(vec![
            access(1, 20, Some(30)),
            access(2, 999, None),
            trunk(8, 1),
            member,
        ])
        .with_names(Some(vec![
            name(1, "default", Some(VlanState::Active)),
            name(20, "USERS", Some(VlanState::Active)),
            name(30, "VOICE", Some(VlanState::Active)),
            name(200, "GUEST", Some(VlanState::Suspended)),
        ]));
        let ifs = vec![meta(1, "Gi1/0/1"), meta(50, "Vl20"), meta(51, "Vlan30")];
        let mut addrs = BTreeMap::new();
        addrs.insert(50, vec![addr("10.0.20.1", 23)]);
        let rows = vlan_rows(VlanSources {
            snapshot: Some(&snap),
            interfaces: &ifs,
            addresses: &addrs,
            meraki: None,
        });
        // The trunk's 1-4094 adds nothing; the member's native 999 is not its own.
        assert_eq!(ids(&rows), vec![1, 20, 30, 200, 999]);
        let by = |id| rows.iter().find(|r| r.vlan_id == id).unwrap();
        assert_eq!(by(20).name.as_deref(), Some("USERS"));
        assert_eq!(by(20).defined, Some(true));
        let gw = by(20).gateway.as_ref().unwrap();
        assert_eq!(
            (gw.if_name.as_deref(), gw.ifindex),
            (Some("Vl20"), Some(50))
        );
        assert_eq!(gw.addresses, vec![addr("10.0.20.1", 23)]);
        // An SVI the address walk has not reached is still the gateway, with no address yet.
        assert_eq!(by(30).gateway.as_ref().unwrap().addresses, vec![]);
        assert_eq!(by(200).state, Some(NodeVlanState::Suspended));
        assert_eq!(by(200).gateway, None);
        // Used by a port, absent from the table the device answered: a configuration gap.
        assert_eq!(by(999).name, None);
        assert_eq!(by(999).defined, Some(false));
    }

    /// Names not read (an older poller, a name walk that did not finish): the used VLANs are
    /// listed and nothing is called undefined.
    #[test]
    fn without_names_nothing_is_judged_undefined() {
        let snap = VlanSnapshot::new(vec![access(1, 20, None), trunk(2, 1)]);
        let rows = vlan_rows(VlanSources {
            snapshot: Some(&snap),
            interfaces: &[],
            addresses: &BTreeMap::new(),
            meraki: None,
        });
        assert_eq!(ids(&rows), vec![1, 20]);
        assert!(rows.iter().all(|r| r.name.is_none() && r.defined.is_none()));
    }

    #[test]
    fn nothing_stored_is_an_empty_list() {
        let rows = vlan_rows(VlanSources {
            snapshot: None,
            interfaces: &[meta(50, "Vlan20")],
            addresses: &BTreeMap::new(),
            meraki: None,
        });
        assert!(rows.is_empty(), "an SVI alone does not make a VLAN row");
    }

    fn net_vlan(id: u16, n: &str, ip: Option<&str>) -> NetworkVlan {
        NetworkVlan {
            id,
            name: n.into(),
            subnet: Some(format!("10.{id}.0.0/24")),
            appliance_ip: ip.map(|i| i.parse().unwrap()),
        }
    }

    #[test]
    fn a_meraki_switch_takes_its_names_from_the_mx_and_judges_nothing_undefined() {
        let mx = Uuid::from_u128(7);
        let snap = VlanSnapshot::new(vec![access(1, 10, None), access(2, 77, None)]);
        let rows = vlan_rows(VlanSources {
            snapshot: Some(&snap),
            interfaces: &[],
            addresses: &BTreeMap::new(),
            meraki: Some(MerakiVlans {
                vlans: Some(vec![
                    net_vlan(10, "Data", Some("10.10.0.1")),
                    net_vlan(20, "Voice", None),
                ]),
                is_mx: false,
                mx_node: Some(mx),
            }),
        });
        assert_eq!(ids(&rows), vec![10, 20, 77]);
        assert!(rows.iter().all(|r| r.defined.is_none()));
        let gw = rows[0].gateway.as_ref().unwrap();
        assert_eq!(gw.node_id, Some(mx));
        assert_eq!(gw.addresses, vec![addr("10.10.0.1", 24)]);
        assert_eq!(rows[0].subnet.as_deref(), Some("10.10.0.0/24"));
        // A switch-only VLAN: no name, no gateway, no judgement.
        assert_eq!(
            (rows[2].name.as_ref(), rows[2].gateway.as_ref()),
            (None, None)
        );
    }

    #[test]
    fn a_meraki_switch_whose_mx_is_hidden_names_no_gateway() {
        let rows = vlan_rows(VlanSources {
            snapshot: None,
            interfaces: &[],
            addresses: &BTreeMap::new(),
            meraki: Some(MerakiVlans {
                vlans: Some(vec![net_vlan(10, "Data", Some("10.10.0.1"))]),
                is_mx: false,
                mx_node: None,
            }),
        });
        assert_eq!(rows[0].name.as_deref(), Some("Data"));
        assert_eq!(rows[0].gateway, None);
    }

    #[test]
    fn a_warm_spare_mx_answers_when_the_first_is_hidden() {
        let (a, b, me) = (Uuid::from_u128(1), Uuid::from_u128(2), Uuid::from_u128(9));
        // Both visible: the lowest id, every time.
        assert_eq!(gateway_mx(&[a, b], me, |_| true), Some(a));
        // The first hidden from this caller: the spare still names the gateway.
        assert_eq!(gateway_mx(&[a, b], me, |n| n != a), Some(b));
        // Neither visible, or none monitored: no gateway.
        assert_eq!(gateway_mx(&[a, b], me, |_| false), None);
        assert_eq!(gateway_mx(&[], me, |_| true), None);
        // An MX is never named as its own gateway.
        assert_eq!(gateway_mx(&[me, b], me, |_| true), Some(b));
    }

    #[test]
    fn an_mx_is_the_gateway_of_its_own_vlans() {
        let rows = vlan_rows(VlanSources {
            snapshot: None,
            interfaces: &[],
            addresses: &BTreeMap::new(),
            meraki: Some(MerakiVlans {
                vlans: Some(vec![net_vlan(10, "Data", Some("10.10.0.1"))]),
                is_mx: true,
                mx_node: None,
            }),
        });
        let gw = rows[0].gateway.as_ref().unwrap();
        assert_eq!((gw.node_id, gw.addresses.len()), (None, 1));
    }

    #[test]
    fn the_tab_is_offered_to_cisco_huawei_over_snmp_and_meraki_switches_and_mx() {
        assert!(vlans_read_for(Some("Cisco"), true, None));
        assert!(vlans_read_for(Some("Huawei"), true, None));
        assert!(!vlans_read_for(Some("Cisco"), false, None));
        assert!(!vlans_read_for(Some("Arista"), true, None));
        assert!(!vlans_read_for(None, true, None));
        assert!(vlans_read_for(None, false, Some("switch")));
        assert!(vlans_read_for(None, false, Some("appliance")));
        assert!(!vlans_read_for(Some("Cisco"), true, Some("wireless")));
    }
}
