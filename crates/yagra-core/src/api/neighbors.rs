// SPDX-License-Identifier: AGPL-3.0-only
//! CDP/LLDP adjacency: what a node currently sees on its ports, how that changed over time, and
//! whether this deployment collects it at all (ADR-038).
//!
//! The two reads are per node and node-scoped; the settings are deployment-wide and admin-only.
//! They are separate endpoints rather than more fields on `/api/v1/settings/retention` because they
//! answer a different question and carry a different justification — retention is "how long is
//! anything kept", this is "is a walk being issued, and how often".
//!
//! ⚠️ **`NeighborConfig` now carries three walks, not one**: CDP/LLDP adjacency (ADR-038), interface
//! addresses (ADR-043) and the ARP/ND cache (ADR-043 Increment 3). The name and its first two field
//! names describe only the first, and are kept verbatim because renaming them would break every
//! existing client for no gain. Each later pair is `Option`, where absent means "leave that setting
//! as it is" — the difference between an additive field and a silent regression for a client that
//! predates it.
//!
//! Nothing here writes to a device, and no response carries a credential: adjacency is descriptive
//! device data (a chassis id, a port name, a peer's sysDescr), untrusted and rendered as such.

use super::error::{ApiError, ApiResult};
use super::extract::{Admin, RequireManageConfig, RequireView, Scoped, VisibleNode};
use super::scope::NodeScope;
use super::ApiState;
use crate::neighbors::{self, AdjacencySettings};
use crate::repo::AddressClaim;
use crate::store::MetricStore;
use axum::extract::{Path, Query, State};
use axum::{http::StatusCode, routing::get, Json, Router};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::net::IpAddr;
use uuid::Uuid;
use yagra_common::{Neighbor, NeighborCapability, NeighborIdKind, NeighborSet};

/// Default page size for the change history.
const HISTORY_DEFAULT_LIMIT: i64 = 50;
/// Hard cap on the page size — an unbounded page is a DoS vector (api-conventions).
const HISTORY_MAX_LIMIT: i64 = 200;

/// This domain's slice of the OpenAPI document (ADR-035), merged by [`super::openapi::document`].
#[derive(utoipa::OpenApi)]
#[openapi(paths(
    get_neighbors,
    list_neighbor_history,
    get_adjacency_settings,
    update_neighbor_settings
))]
pub(super) struct Doc;

/// The neighbour routes, merged into `/api/v1` by [`super::router`].
pub(super) fn routes() -> Router<ApiState> {
    Router::new()
        .route("/api/v1/nodes/:node_id/neighbors", get(get_neighbors))
        .route(
            "/api/v1/nodes/:node_id/neighbors/history",
            get(list_neighbor_history),
        )
        .route(
            "/api/v1/settings/neighbors",
            get(get_adjacency_settings).put(update_neighbor_settings),
        )
}

// ── Per-node reads ───────────────────────────────────────────────────────────

/// A node's current adjacency and how long it has held.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct CurrentNeighbors {
    /// The adjacencies the node last reported.
    neighbors: NeighborSet,
    /// When this exact set was first seen (RFC 3339).
    first_seen: String,
    /// When it was last confirmed unchanged (RFC 3339).
    last_seen: String,
    /// For each distinct management address the neighbours advertise, which monitored node it
    /// belongs to. Matched on the address — an inventory address or any address one of a node's
    /// interfaces carries. Only when several nodes claim it is the name the neighbour sent used,
    /// and only to choose among those nodes (ADR-180 Inc.4); a name alone never matches.
    peers: Vec<NeighborPeer>,
    /// For each distinct MAC-address chassis id on a row that advertises **no** usable management
    /// address, the Meraki device a Meraki organization lists under that MAC, if any (ADR-180
    /// Inc.3). The Dashboard reports no management address for an MR or an MX, so this is how
    /// those rows say whether the device is monitored. Only MACs a Meraki device listing states are
    /// matched; any other chassis id is absent here, as is a row that has an address — that one is
    /// answered in `peers`.
    chassis_peers: Vec<NeighborChassisPeer>,
    /// The maker the IEEE registered each MAC-address chassis or port id to. Only ids the device
    /// labelled as MAC addresses are looked up — except on a Meraki switch, MX or MR, whose Dashboard
    /// reports no label, where an id shaped like a MAC address is (six octets, or twelve bare hex digits
    /// for a CDP device id). This names who made the network
    /// interface, which is not necessarily who made the device or its software.
    mac_vendors: Vec<MacVendor>,
}

/// What a neighbour's management address is to this deployment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum NeighborPeerState {
    /// Exactly one monitored node claims the address — or, of several, exactly one bears the name
    /// the neighbour sent (`matched_by_name`) — and the caller may see it.
    Node,
    /// Exactly one monitored node claims the address, and it is outside the caller's folders.
    /// Never the answer for a node picked by name (ADR-180 Inc.4 decision 6).
    OutsideScope,
    /// More than one node claims the address (a shared virtual address, or a duplicate) and the
    /// name the neighbour sent does not pick out exactly one of them the caller may see — a name
    /// that picks a node outside the caller's folders is answered as picking none, so this state
    /// says nothing about which hidden node bears which name.
    Ambiguous,
    /// No monitored node claims the address.
    Unregistered,
}

/// One neighbour management address and the node it belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub(crate) struct NeighborPeer {
    /// The address exactly as the neighbour row carries it in `remote_mgmt_addr`.
    address: String,
    state: NeighborPeerState,
    /// Present only when `state` is `node`.
    node_id: Option<Uuid>,
    /// Present only when `state` is `node`.
    node_name: Option<String>,
    /// Whether the address is on the caller's Discovery ▸ Unregistered list.
    discovery_listed: bool,
    /// That list's row for the address, when `discovery_listed` — the id the endpoint probe and
    /// import act on (ADR-179 Inc.3).
    discovery_id: Option<Uuid>,
    /// Who adds this device instead of a hand registration: a wireless controller or a Meraki
    /// organization that already lists it. Present only when `state` is `unregistered`.
    managed_by: Option<NeighborManagedBy>,
    /// Why nothing adds this device from here (ADR-179 Inc.9): present only when `state` is
    /// `unregistered`, it is not on the caller's Unregistered list, and nothing manages it.
    setup_blocked: Option<SetupBlocked>,
    /// Several nodes claim the address and the node answered was chosen among them by the name the
    /// neighbour sent (ADR-180 Inc.4). The others are in `also_claimed_by`.
    matched_by_name: bool,
    /// The other nodes that claim the address, when more than one does — the ones the caller may
    /// see, by name, at most ten. Empty when one node or none claims it. A duplicate address stays
    /// visible even when the name picked the peer out.
    also_claimed_by: Vec<AlsoClaimedBy>,
    /// How many other nodes claim the address, including those outside the caller's folders and
    /// those past the first ten. `0` when one node or none claims it.
    also_claimed_total: u32,
}

/// At most this many other claimants are named per address; `also_claimed_total` says how many
/// there are. A private range reused at every site would otherwise name one node per site.
const ALSO_CLAIMED_MAX: usize = 10;

/// Another node that claims a neighbour's management address (ADR-180 Inc.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub(crate) struct AlsoClaimedBy {
    node_id: Uuid,
    node_name: String,
    /// Whether the ports carrying the address on that node have link.
    port_state: ClaimPortState,
}

/// Whether a claimant's ports carrying the address have link, from `if_oper_status` (ADR-180
/// Inc.4 decision 4). A claimant that is down is still counted: a standby line configured with the
/// same address is part of the duplicate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ClaimPortState {
    /// At least one port carrying the address is up.
    Up,
    /// No port carrying the address is up, and at least one reads down (2) or lowerLayerDown (7).
    /// A port reading anything else (testing, dormant, notPresent) is passed over.
    LinkDown,
    /// Neither: the node claims it only as its inventory address, which names no port; no status
    /// was collected for its ports lately; or every reading is one of the others above.
    Unknown,
}

/// Why an unregistered neighbour address offers no way to add it (ADR-179 Inc.9). The rules are
/// the Unregistered list's own (`arp::identifies_a_device`, `arp::only_an_end_station`), so what
/// this says cannot drift from what the list does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SetupBlocked {
    /// Loopback, link-local, multicast… — an address that names no device of its own.
    NotADeviceAddress,
    /// Every row advertising it says it is only an end station (a phone, a host).
    EndStation,
    /// On the list, but seen by nodes outside the caller's folders only.
    FoundOutsideYourFolders,
    /// Not on the list yet. The list is rebuilt every five minutes, and holds at most 10,000 rows;
    /// which of the two this is cannot be told, and the words say neither is ruled out.
    NotListedYet,
}

/// Which reason applies, in the order the list's rules are applied.
fn setup_blocked(ip: IpAddr, only_end_stations: bool, listed_elsewhere: bool) -> SetupBlocked {
    if !crate::arp::identifies_a_device(ip) {
        SetupBlocked::NotADeviceAddress
    } else if only_end_stations {
        SetupBlocked::EndStation
    } else if listed_elsewhere {
        SetupBlocked::FoundOutsideYourFolders
    } else {
        SetupBlocked::NotListedYet
    }
}

/// One neighbour chassis MAC and the Meraki device listed under it (ADR-180 Inc.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub(crate) struct NeighborChassisPeer {
    /// The chassis id exactly as the neighbour row carries it (`aa:bb:cc:dd:ee:ff`).
    chassis: String,
    /// `node`, `outside_scope` or `unregistered` — never `ambiguous`. When two organizations list
    /// the same MAC (a device moving between them), the first by organization name answers.
    state: NeighborPeerState,
    /// Present only when `state` is `node`.
    node_id: Option<Uuid>,
    /// Present only when `state` is `node`.
    node_name: Option<String>,
    /// The organization that lists the device, when it has not been imported (`unregistered`):
    /// always `kind: meraki`.
    managed_by: Option<NeighborManagedBy>,
    /// What the device is, from the kind of product the organization lists it as — `switch` for
    /// an MS, `wlan_ap` for an MR, `router` for an MX (ADR-181 Inc.4 decision 2). For a row whose own
    /// capabilities are blank; empty for a product with no such role.
    capabilities: Vec<NeighborCapability>,
}

/// Who manages the device at an unregistered neighbour address (ADR-179 Inc.3). Registering such
/// a device by hand would leave a second node for it once its controller or organization imports
/// it, so the Neighbors tab sends the operator there instead.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum NeighborManagedBy {
    /// A wireless controller the caller can see reports an access point at this address.
    Controller {
        /// The access point, for `POST /api/v1/wireless/aps/{ap_id}/import`.
        ap_id: Uuid,
        controller_node_id: Uuid,
        controller_name: String,
        /// The access point is already a node, at another address.
        imported: bool,
    },
    /// Only controllers outside the caller's folders report an access point at this address.
    ControllerHidden,
    /// A Meraki organization's inventory lists a device at this address.
    Meraki { org_id: Uuid, org_name: String },
}

/// One address's manager, from what the two inventories answered: a controller the caller can
/// see first (one click imports it), then a Meraki organization (a page to go to), then a
/// controller the caller cannot see (nothing to offer, but still not the caller's to add by hand).
fn managed_by(
    ap: Option<&crate::wireless::ApAtAddress>,
    meraki: Option<&(Uuid, String)>,
) -> Option<NeighborManagedBy> {
    match (ap, meraki) {
        (Some(a), _) if a.controller.is_some() => {
            let (id, name) = a.controller.clone()?;
            Some(NeighborManagedBy::Controller {
                ap_id: a.ap_id,
                controller_node_id: id,
                controller_name: name,
                imported: a.imported,
            })
        }
        (_, Some((org_id, org_name))) => Some(NeighborManagedBy::Meraki {
            org_id: *org_id,
            org_name: org_name.clone(),
        }),
        (Some(_), None) => Some(NeighborManagedBy::ControllerHidden),
        (None, None) => None,
    }
}

/// The registered maker of one MAC address.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub(crate) struct MacVendor {
    /// The MAC exactly as the neighbour row carries it.
    mac: String,
    vendor: String,
}

/// One append-on-change history row.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct NeighborChange {
    id: i64,
    /// When the change was recorded (RFC 3339).
    at: String,
    /// The adjacency as of this change.
    neighbors: NeighborSet,
    /// The content key this replaced; `null` marks the first observation ever recorded for the node.
    prev_neighbor_key: Option<String>,
    /// `true` when the collector changed how it writes these rows (after an upgrade, ADR-182) —
    /// a port read `7` and now reads `Port 7`, say. The row is a change of spelling, not of
    /// cabling; a real change read at the same moment is still in it.
    format_changed: bool,
}

/// Keyset cursor for the next page (ADR-019 — never OFFSET).
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct NeighborHistoryCursor {
    at: String,
    id: i64,
}

/// One page of adjacency changes, newest first.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct NeighborHistory {
    changes: Vec<NeighborChange>,
    /// Pass back as `before_at`+`before_id` for the next page; `null` ⇒ this was the last one.
    next: Option<NeighborHistoryCursor>,
}

#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub(super) struct HistoryQuery {
    #[serde(default)]
    limit: Option<i64>,
    #[serde(default)]
    before_at: Option<String>,
    #[serde(default)]
    before_id: Option<i64>,
}

impl HistoryQuery {
    /// The keyset cursor this query names, if any.
    fn cursor(&self) -> Result<Option<(chrono::DateTime<chrono::Utc>, i64)>, ApiError> {
        parse_history_cursor(self.before_at.as_deref(), self.before_id)
    }
}

/// Parse a keyset cursor: both halves or neither.
///
/// A half-specified cursor is **rejected rather than ignored**, and that is the whole reason this is
/// a named function rather than an inline `if let`: silently dropping it restarts paging from the
/// top, so a client walking the history loops over the first page forever while looking like it is
/// making progress. Any second surface that pages this history has to get the same answer.
pub(crate) fn parse_history_cursor(
    before_at: Option<&str>,
    before_id: Option<i64>,
) -> Result<Option<(chrono::DateTime<chrono::Utc>, i64)>, ApiError> {
    super::util::keyset_cursor(before_at, before_id, "before_at")
}

/// The node's current CDP/LLDP neighbours.
///
/// `404` means nothing has recorded this node's neighbours yet — the node may be neither an SNMP
/// device nor a Meraki switch, MX or MR (whose neighbours are read from the Meraki Dashboard), may not speak
/// either protocol, or may simply not have been read since collection was enabled. It is distinct from a recorded **empty** set, which is a real answer meaning the device
/// reports no neighbours.
#[utoipa::path(
    get, path = "/api/v1/nodes/{node_id}/neighbors", tag = "neighbors",
    params(("node_id" = Uuid, Path, description = "Node id")),
    responses(
        (status = 200, description = "The node's current adjacency and how long it has held", body = CurrentNeighbors),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks View", body = super::error::ErrorBody),
        (status = 404, description = "No adjacency has been recorded for the node", body = super::error::ErrorBody),
        (status = 503, description = "Inventory storage is unavailable (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn get_neighbors(
    _perm: RequireView,
    _visible: VisibleNode,
    Scoped(scope): Scoped,
    admin: Admin,
    State(st): State<ApiState>,
    Path(node_id): Path<Uuid>,
) -> ApiResult<Json<CurrentNeighbors>> {
    Ok(Json(
        current_neighbors(&admin, st.store.as_ref(), &scope, node_id).await?,
    ))
}

/// One node's current adjacency, with the "nothing recorded" rule.
///
/// The `404` is the load-bearing part and the reason this is a function rather than three lines
/// repeated: **no walk recorded is not an empty set**. A device that genuinely reports no neighbours
/// has a recorded empty set, which is a real answer; a node that was never walked has nothing, and
/// answering `[]` for it would tell an operator — or a model — that the device has no adjacency
/// when what is true is that nobody has looked.
///
/// `scope` decides what `peers` may name: a claimant outside it is reported as `outside_scope`
/// with no id and no name (ADR-180 decision 3, the disclosure ADR-139 already accepted).
///
/// `store` is asked once, and only when some address has several claimants, for the link state
/// of the ports carrying it on the claimants the answer lists (ADR-180 Inc.4 decision 7). It is asked
/// alongside the inventory reads and given [`LINK_STATE_BUDGET`]: a store that does not answer
/// leaves every such port `unknown` rather than failing the tab — or holding it up (decision 8).
pub(crate) async fn current_neighbors(
    admin: &super::AdminState,
    store: &dyn MetricStore,
    scope: &NodeScope,
    node_id: Uuid,
) -> ApiResult<CurrentNeighbors> {
    let current = admin
        .neighbors
        .current(node_id)
        .await
        .map_err(|e| {
            ApiError::from_internal(e.as_ref(), "get neighbors", "failed to load neighbours")
        })?
        .ok_or_else(|| {
            ApiError::not_found(
                "neighbors_not_found",
                format!("no adjacency recorded for node {node_id}"),
            )
        })?;
    let advertised = advertised_addresses(&current.set);
    let addresses: Vec<IpAddr> = advertised.values().copied().collect();
    let claims = admin
        .repo
        .address_claims(&addresses, scope.group_filter())
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "match neighbours to nodes",
                "failed to read the inventory",
            )
        })?;
    let names = advertised_names(&current.set);
    let unaddressed = unaddressed_mac_chassis(&current.set);
    let unaddressed_list: Vec<String> = unaddressed.iter().cloned().collect();
    let ports = ports_to_read(&advertised, &claims, &names);
    // Everything below depends on the addresses or the claims and on nothing else, so it is asked
    // at once — the link-state read in particular must not add its wait to the tab's (decision 8).
    let (listed, aps, meraki, by_mac, oper) = tokio::join!(
        admin
            .discovered
            .listed_among(&addresses, scope.group_filter()),
        admin.wireless.aps_at(&addresses, scope.group_filter()),
        admin.meraki_inventory.devices_at(&addresses),
        admin.meraki_inventory.devices_with_mac(&unaddressed_list),
        link_states(store, &ports),
    );
    let listed = listed.map_err(|e| {
        ApiError::from_internal(
            e.as_ref(),
            "match neighbours to discovered endpoints",
            "failed to read discovered endpoints",
        )
    })?;
    let aps = aps.map_err(|e| {
        ApiError::from_internal(
            e.as_ref(),
            "match neighbours to access points",
            "failed to read the access point inventory",
        )
    })?;
    let meraki = meraki.map_err(|e| {
        ApiError::from_internal(
            e.as_ref(),
            "match neighbours to Meraki devices",
            "failed to read the Meraki inventory",
        )
    })?;
    let by_mac = by_mac.map_err(|e| {
        ApiError::from_internal(
            e.as_ref(),
            "match neighbour chassis to Meraki devices",
            "failed to read the Meraki inventory",
        )
    })?;
    let managed: HashMap<IpAddr, NeighborManagedBy> = addresses
        .iter()
        .filter_map(|ip| managed_by(aps.get(ip), meraki.get(ip)).map(|m| (*ip, m)))
        .collect();
    // Rows on the list the caller cannot see (ADR-179 Inc.9): asked only for a scoped caller, and
    // only about addresses nothing else explains — an unrestricted caller already sees every row.
    // An address a node claims (visible or not) is never Unregistered, so no blocker is read for it.
    let claimed: HashSet<IpAddr> = claims.iter().map(|c| c.address).collect();
    let unexplained: Vec<IpAddr> = addresses
        .iter()
        .filter(|ip| !listed.contains_key(ip) && !managed.contains_key(ip) && !claimed.contains(ip))
        .copied()
        .collect();
    let listed_elsewhere: BTreeSet<IpAddr> = if scope.is_all() || unexplained.is_empty() {
        BTreeSet::new()
    } else {
        admin
            .discovered
            .listed_among(&unexplained, None)
            .await
            .map_err(|e| {
                ApiError::from_internal(
                    e.as_ref(),
                    "match neighbours to hidden discovered endpoints",
                    "failed to read discovered endpoints",
                )
            })?
            .into_keys()
            .collect()
    };
    let blockers = Blockers {
        end_station_only: end_station_only(&current.set),
        listed_elsewhere,
    };
    Ok(CurrentNeighbors {
        chassis_peers: classify_chassis(&unaddressed, &by_mac, scope),
        peers: classify_peers(
            &advertised,
            &claims,
            &Evidence {
                listed: &listed,
                managed: &managed,
                blockers: &blockers,
                names: &names,
                oper: &oper,
            },
        ),
        mac_vendors: mac_vendors(&current.set),
        neighbors: current.set,
        first_seen: current.first_seen.to_rfc3339(),
        last_seen: current.last_seen.to_rfc3339(),
    })
}

/// What the Unregistered list's rules say about the set's addresses, for [`SetupBlocked`].
#[derive(Debug, Default)]
struct Blockers {
    /// Addresses every row advertising which says it is only an end station.
    end_station_only: BTreeSet<IpAddr>,
    /// Addresses on the list, whoever may see them.
    listed_elsewhere: BTreeSet<IpAddr>,
}

/// The addresses every row advertising which is only an end station — the list skips a row, so an
/// address it hears about from one row that is not is still admitted.
fn end_station_only(set: &NeighborSet) -> BTreeSet<IpAddr> {
    let mut verdict: BTreeMap<IpAddr, bool> = BTreeMap::new();
    for n in &set.neighbors {
        let Some(ip) = usable_mgmt_addr(n) else {
            continue;
        };
        let only = crate::arp::only_an_end_station(&n.capabilities);
        verdict.entry(ip).and_modify(|v| *v &= only).or_insert(only);
    }
    verdict
        .into_iter()
        .filter_map(|(ip, only)| only.then_some(ip))
        .collect()
}

/// A row's management address, when it has one. Text that does not parse is no address, and neither
/// is the unspecified address (ADR-180 Inc.4 decision 10): a Meraki switch sends `0.0.0.0` over CDP
/// when it has none, and a Meraki node with no LAN address is stored at `0.0.0.0`, so matching on it
/// made every such node a claimant. Such a row is matched on its chassis MAC instead (Inc.3).
///
/// Loopback, link-local and the like are still addresses here — they name no device, and the row says
/// so through [`SetupBlocked::NotADeviceAddress`] rather than going quiet.
fn usable_mgmt_addr(n: &Neighbor) -> Option<IpAddr> {
    n.remote_mgmt_addr
        .as_deref()
        .and_then(|a| a.parse::<IpAddr>().ok())
        .filter(|ip| !ip.is_unspecified())
}

/// Every distinct management address the set advertises, keyed by the text the row carries. A row
/// with no usable one ([`usable_mgmt_addr`]) is left out — it cannot be matched to anything by address.
fn advertised_addresses(set: &NeighborSet) -> BTreeMap<String, IpAddr> {
    set.neighbors
        .iter()
        .filter_map(|n| Some((n.remote_mgmt_addr.clone()?, usable_mgmt_addr(n)?)))
        .collect()
}

/// The name each row advertising an address sent, per address (ADR-180 Inc.4 decision 2): the LLDP
/// system name, else the chassis id when it is text — CDP's device id serves as both. An id the
/// poller labelled as a MAC, an address or raw octets is not a name.
fn advertised_names(set: &NeighborSet) -> BTreeMap<IpAddr, Vec<String>> {
    let mut out: BTreeMap<IpAddr, Vec<String>> = BTreeMap::new();
    for n in &set.neighbors {
        let Some(ip) = usable_mgmt_addr(n) else {
            continue;
        };
        if let Some(name) = advertised_name(n) {
            out.entry(ip).or_default().push(name.to_owned());
        }
    }
    out
}

/// One row's name for itself, if it sent one.
fn advertised_name(n: &Neighbor) -> Option<&str> {
    if let Some(sys) = n
        .remote_sys_name
        .as_deref()
        .filter(|s| !s.trim().is_empty())
    {
        return Some(sys);
    }
    match n.remote_chassis_kind {
        Some(NeighborIdKind::Text) => Some(n.remote_chassis.as_str()),
        // A record older than the kind field says nothing about its chassis. It is a name only if
        // it does not read as an address or a MAC: a node added by address and never renamed is
        // *named* that address, so an address-shaped id would match it though no name was sent
        // (ADR-180 Inc.4 decision 9).
        None => {
            let id = n.remote_chassis.trim();
            let shaped = id.parse::<IpAddr>().is_ok() || yagra_common::mac::parse_mac(id).is_some();
            (!shaped).then_some(n.remote_chassis.as_str())
        }
        Some(
            NeighborIdKind::Mac
            | NeighborIdKind::NetworkAddress
            | NeighborIdKind::Hex
            | NeighborIdKind::Unknown,
        ) => None,
    }
}

/// A name as it is compared (ADR-180 Inc.4 decision 2): the shared name key, less a trailing `(…)` —
/// CDP appends a serial as `name(FOC1234X0YZ)`.
fn peer_name_key(raw: &str) -> Option<String> {
    let key = crate::duplicates::name_key(raw)?;
    let bare = match key
        .strip_suffix(')')
        .and_then(|s| s.rfind('(').map(|i| &s[..i]))
    {
        Some(stem) => stem.trim_end(),
        None => key.as_str(),
    };
    (!bare.is_empty()).then(|| bare.to_owned())
}

/// The part before the first dot, so `sw-01.example.com` and `sw-01` compare equal. `None` for a
/// key that is an address, whose first dot separates octets rather than a domain.
fn host_label(key: &str) -> Option<&str> {
    if key.parse::<IpAddr>().is_ok() {
        return None;
    }
    key.split('.').next().filter(|h| !h.is_empty())
}

/// Which of `candidates` the names the neighbour sent pick out (ADR-180 Inc.4 decision 2), or `None`.
///
/// Each name is compared whole first and by its host label second, and counts only when exactly one
/// candidate matches. The names must agree: two rows naming two different candidates pick neither.
/// A name that matches no candidate is simply not evidence.
fn claimant_named(names: &[String], candidates: &[(Uuid, &str)]) -> Option<Uuid> {
    let keyed: Vec<(Uuid, String)> = candidates
        .iter()
        .filter_map(|(id, name)| peer_name_key(name).map(|k| (*id, k)))
        .collect();
    let one = |matching: Vec<Uuid>| match matching.as_slice() {
        [id] => Some(*id),
        _ => None,
    };
    let mut picked: BTreeSet<Uuid> = BTreeSet::new();
    for raw in names {
        let Some(key) = peer_name_key(raw) else {
            continue;
        };
        let whole: Vec<Uuid> = keyed
            .iter()
            .filter(|(_, k)| *k == key)
            .map(|(id, _)| *id)
            .collect();
        let hit = if whole.is_empty() {
            host_label(&key).and_then(|host| {
                one(keyed
                    .iter()
                    .filter(|(_, k)| host_label(k) == Some(host))
                    .map(|(id, _)| *id)
                    .collect())
            })
        } else {
            one(whole)
        };
        if let Some(id) = hit {
            picked.insert(id);
        }
    }
    yagra_topology::derive::sole_claimant(&picked)
}

/// A claimant's link state over the ports carrying the address (ADR-180 Inc.4 decision 4):
/// `if_oper_status` 1 is up; 2 (down) and 7 (lowerLayerDown) have no link; anything else, or no
/// reading at all, says nothing.
fn claim_port_state(
    node: Uuid,
    ports: impl Iterator<Item = u32>,
    oper: &HashMap<(Uuid, i64), f64>,
) -> ClaimPortState {
    let mut down = false;
    for p in ports {
        match oper.get(&(node, i64::from(p))).copied() {
            Some(v) if (v - 1.0).abs() < f64::EPSILON => return ClaimPortState::Up,
            Some(v) if (v - 2.0).abs() < f64::EPSILON || (v - 7.0).abs() < f64::EPSILON => {
                down = true;
            }
            _ => {}
        }
    }
    if down {
        ClaimPortState::LinkDown
    } else {
        ClaimPortState::Unknown
    }
}

/// Address → node → that node's claims (one per port, plus its inventory address).
type ClaimsByAddress<'c> = BTreeMap<IpAddr, BTreeMap<Uuid, Vec<&'c AddressClaim>>>;

fn claims_by_address(claims: &[AddressClaim]) -> ClaimsByAddress<'_> {
    let mut by_address: ClaimsByAddress<'_> = BTreeMap::new();
    for c in claims {
        by_address
            .entry(c.address)
            .or_default()
            .entry(c.id)
            .or_default()
            .push(c);
    }
    by_address
}

/// One address's answer before any link state is read: which claimant it names, and which of the
/// others it lists. The only place those two rules are written — both the store query
/// ([`ports_to_read`]) and the answer ([`classify_peers`]) ask it, so the ports read are exactly
/// the ports shown (ADR-180 Inc.4 decision 7).
struct Pick<'m, 'c> {
    /// How many nodes claim the address.
    claimants: usize,
    /// The claimant answered with: the sole one (seen or not), or the one a name picked — which
    /// is never a node the caller may not see (decision 6).
    chosen: Option<&'c AddressClaim>,
    matched_by_name: bool,
    /// Every claimant but `chosen`, when more than one claims the address.
    others_total: usize,
    /// Those of them the caller may see, in name order, at most [`ALSO_CLAIMED_MAX`].
    listed: Vec<(Uuid, &'m [&'c AddressClaim])>,
}

fn pick_peer<'m, 'c>(
    owners: Option<&'m BTreeMap<Uuid, Vec<&'c AddressClaim>>>,
    names: &[String],
) -> Pick<'m, 'c> {
    let ids: BTreeSet<Uuid> = owners
        .map(|o| o.keys().copied().collect())
        .unwrap_or_default();
    let first = |id: Uuid| {
        owners
            .and_then(|o| o.get(&id))
            .and_then(|cs| cs.first().copied())
    };
    let (chosen, matched_by_name) = match yagra_topology::derive::sole_claimant(&ids) {
        Some(id) => (first(id), false),
        None if ids.len() > 1 => {
            let candidates: Vec<(Uuid, &str)> = owners
                .into_iter()
                .flatten()
                .filter_map(|(id, cs)| cs.first().map(|c| (*id, c.name.as_str())))
                .collect();
            // decision 6: a name that picks a node the caller may not see picks nothing. Answering
            // `outside_scope` "by name" would say the hidden node's name is the one on the row.
            match claimant_named(names, &candidates).and_then(first) {
                Some(c) if c.visible => (Some(c), true),
                Some(_) | None => (None, false),
            }
        }
        None => (None, false),
    };
    let chosen_id = chosen.map(|c| c.id);
    let others: Vec<(Uuid, &'m [&'c AddressClaim])> = if ids.len() > 1 {
        owners
            .into_iter()
            .flatten()
            .filter(|(id, _)| Some(**id) != chosen_id)
            .map(|(id, cs)| (*id, cs.as_slice()))
            .collect()
    } else {
        Vec::new()
    };
    let mut listed: Vec<(Uuid, &'m [&'c AddressClaim])> = others
        .iter()
        .filter(|(_, cs)| cs.first().is_some_and(|c| c.visible))
        .copied()
        .collect();
    listed.sort_by(|a, b| a.1[0].name.cmp(&b.1[0].name).then(a.0.cmp(&b.0)));
    listed.truncate(ALSO_CLAIMED_MAX);
    Pick {
        claimants: ids.len(),
        chosen,
        matched_by_name,
        others_total: others.len(),
        listed,
    }
}

/// The `(node, ifindex)` pairs whose link state the answer shows: the ports carrying each address
/// on the claimants it lists. Never a hidden claimant, never the one answered with, never a port
/// that does not carry the address (decision 7).
fn ports_to_read(
    advertised: &BTreeMap<String, IpAddr>,
    claims: &[AddressClaim],
    names: &BTreeMap<IpAddr, Vec<String>>,
) -> Vec<(Uuid, i64)> {
    let by_address = claims_by_address(claims);
    let mut out: BTreeSet<(Uuid, i64)> = BTreeSet::new();
    for ip in advertised.values() {
        let pick = pick_peer(
            by_address.get(ip),
            names.get(ip).map_or(&[][..], Vec::as_slice),
        );
        for (id, cs) in pick.listed {
            out.extend(
                cs.iter()
                    .filter_map(|c| c.ifindex)
                    .map(|i| (id, i64::from(i))),
            );
        }
    }
    out.into_iter().collect()
}

/// How long the tab waits for link state before answering without it (decision 8). The store's own
/// client allows ten seconds, which is a fleet-ingest budget, not a tab's.
const LINK_STATE_BUDGET: std::time::Duration = std::time::Duration::from_secs(2);

/// `if_oper_status` for `ports`, or nothing — a store that fails or is slow leaves every port
/// `unknown`.
async fn link_states(store: &dyn MetricStore, ports: &[(Uuid, i64)]) -> HashMap<(Uuid, i64), f64> {
    if ports.is_empty() {
        return HashMap::new();
    }
    within_budget(
        store.series_rows_at("if_oper_status", ports, crate::store::INSTANT_LOOKBACK_SECS),
        ports.len(),
    )
    .await
}

/// `read`, or nothing once [`LINK_STATE_BUDGET`] has passed.
async fn within_budget(
    read: impl std::future::Future<Output = HashMap<(Uuid, i64), f64>>,
    ports: usize,
) -> HashMap<(Uuid, i64), f64> {
    tokio::time::timeout(LINK_STATE_BUDGET, read)
        .await
        .unwrap_or_else(|_| {
            tracing::debug!(
                ports,
                "link state for contested neighbour addresses timed out; answered as unknown"
            );
            HashMap::new()
        })
}

/// Everything besides the claims that decides a peer, read once per request.
struct Evidence<'a> {
    listed: &'a BTreeMap<IpAddr, Uuid>,
    managed: &'a HashMap<IpAddr, NeighborManagedBy>,
    blockers: &'a Blockers,
    names: &'a BTreeMap<IpAddr, Vec<String>>,
    oper: &'a HashMap<(Uuid, i64), f64>,
}

/// Each advertised address against the nodes that claim it (ADR-180 decision 2/3, Inc.4).
///
/// Pure, so the rules a mistake here would break — one claimant or none, a name choosing only among
/// claimants, the others staying listed, and a hidden claimant's name never leaving — are tested
/// without a database.
fn classify_peers(
    advertised: &BTreeMap<String, IpAddr>,
    claims: &[AddressClaim],
    ev: &Evidence<'_>,
) -> Vec<NeighborPeer> {
    let by_address = claims_by_address(claims);
    advertised
        .iter()
        .map(|(text, ip)| {
            let pick = pick_peer(
                by_address.get(ip),
                ev.names.get(ip).map_or(&[][..], Vec::as_slice),
            );
            let matched_by_name = pick.matched_by_name;
            let (state, node_id, node_name) = match pick.chosen {
                Some(m) if m.visible => (NeighborPeerState::Node, Some(m.id), Some(m.name.clone())),
                Some(_) => (NeighborPeerState::OutsideScope, None, None),
                None if pick.claimants == 0 => (NeighborPeerState::Unregistered, None, None),
                None => (NeighborPeerState::Ambiguous, None, None),
            };
            // The rest of the claimants, when there is more than one: listed by name if the
            // caller may see them, counted either way.
            let also_claimed_by: Vec<AlsoClaimedBy> = pick
                .listed
                .iter()
                .map(|(id, cs)| AlsoClaimedBy {
                    node_id: *id,
                    node_name: cs[0].name.clone(),
                    port_state: claim_port_state(*id, cs.iter().filter_map(|c| c.ifindex), ev.oper),
                })
                .collect();
            let unregistered = state == NeighborPeerState::Unregistered;
            let managed_by = ev.managed.get(ip).filter(|_| unregistered).cloned();
            let stuck = unregistered && managed_by.is_none() && !ev.listed.contains_key(ip);
            NeighborPeer {
                address: text.clone(),
                state,
                node_id,
                node_name,
                discovery_listed: ev.listed.contains_key(ip),
                discovery_id: ev.listed.get(ip).copied(),
                managed_by,
                setup_blocked: stuck.then(|| {
                    setup_blocked(
                        *ip,
                        ev.blockers.end_station_only.contains(ip),
                        ev.blockers.listed_elsewhere.contains(ip),
                    )
                }),
                matched_by_name,
                also_claimed_by,
                also_claimed_total: u32::try_from(pick.others_total).unwrap_or(u32::MAX),
            }
        })
        .collect()
}

/// Every distinct MAC-address chassis id on a row with no usable management address (ADR-180
/// Inc.3 decision 2): a row that has one is decided by its address alone, so the two rules never both
/// answer for one row.
fn unaddressed_mac_chassis(set: &NeighborSet) -> BTreeSet<String> {
    set.neighbors
        .iter()
        .filter(|n| n.remote_chassis_kind == Some(NeighborIdKind::Mac))
        .filter(|n| usable_mgmt_addr(n).is_none())
        .map(|n| n.remote_chassis.clone())
        .collect()
}

/// Each such chassis against the Meraki device listed under it. Pure, for the same reason as
/// [`classify_peers`]: a node outside the caller's folders is `outside_scope` with no id and no
/// name, and a chassis no organization lists is left out.
fn classify_chassis(
    chassis: &BTreeSet<String>,
    by_mac: &HashMap<String, crate::meraki_inventory::DeviceWithMac>,
    scope: &NodeScope,
) -> Vec<NeighborChassisPeer> {
    chassis
        .iter()
        .filter_map(|mac| {
            let d = by_mac.get(mac)?;
            let (state, node_id, node_name, managed_by) = match &d.node {
                Some(n) if scope.allows_group(n.group_id) => (
                    NeighborPeerState::Node,
                    Some(n.id),
                    Some(n.name.clone()),
                    None,
                ),
                Some(_) => (NeighborPeerState::OutsideScope, None, None, None),
                None => (
                    NeighborPeerState::Unregistered,
                    None,
                    None,
                    Some(NeighborManagedBy::Meraki {
                        org_id: d.org_id,
                        org_name: d.org_name.clone(),
                    }),
                ),
            };
            Some(NeighborChassisPeer {
                chassis: mac.clone(),
                state,
                node_id,
                node_name,
                managed_by,
                capabilities: product_capabilities(&d.product_type),
            })
        })
        .collect()
}

/// The role a Meraki product plays, as a neighbour row's capabilities name it (ADR-181 Inc.4
/// decision 2). A product with no such role — a camera, a sensor — has none, never a guess.
fn product_capabilities(product_type: &str) -> Vec<NeighborCapability> {
    match product_type.trim().to_ascii_lowercase().as_str() {
        "switch" => vec![NeighborCapability::Switch],
        "wireless" => vec![NeighborCapability::WlanAp],
        "appliance" | "cellulargateway" => vec![NeighborCapability::Router],
        _ => Vec::new(),
    }
}

/// The registered maker of every chassis or port id the device labelled a MAC address (ADR-180
/// decision 4/5). An id rendered any other way is not looked up, however much it looks like a MAC.
fn mac_vendors(set: &NeighborSet) -> Vec<MacVendor> {
    let mut out: BTreeMap<String, String> = BTreeMap::new();
    for n in &set.neighbors {
        for (id, kind) in [
            (&n.remote_chassis, n.remote_chassis_kind),
            (&n.remote_port, n.remote_port_kind),
        ] {
            if kind != Some(NeighborIdKind::Mac) || out.contains_key(id) {
                continue;
            }
            if let Some(vendor) = yagra_common::parse_mac(id).and_then(yagra_oui::vendor) {
                out.insert(id.clone(), vendor.to_owned());
            }
        }
    }
    out.into_iter()
        .map(|(mac, vendor)| MacVendor { mac, vendor })
        .collect()
}

/// The node's adjacency change history, newest first.
///
/// A row is written only when the adjacency actually changed, so a quiet rack produces none. The
/// content key deliberately excludes the agent's own churn (LLDP's `TimeMark` and remote index), so
/// a row here means a port genuinely started or stopped facing something, or the peer on it changed.
#[utoipa::path(
    get, path = "/api/v1/nodes/{node_id}/neighbors/history", tag = "neighbors",
    params(("node_id" = Uuid, Path, description = "Node id"), HistoryQuery),
    responses(
        (status = 200, description = "One page of adjacency changes, newest first", body = NeighborHistory),
        (status = 400, description = "before_at and before_id must be given together, and before_at must be RFC 3339", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks View", body = super::error::ErrorBody),
        (status = 503, description = "Inventory storage is unavailable (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn list_neighbor_history(
    _perm: RequireView,
    _visible: VisibleNode,
    admin: Admin,
    Path(node_id): Path<Uuid>,
    Query(q): Query<HistoryQuery>,
) -> ApiResult<Json<NeighborHistory>> {
    Ok(Json(
        neighbor_history(&admin, node_id, q.cursor()?, q.limit).await?,
    ))
}

/// One page of a node's adjacency changes, newest first.
///
/// Carries the limit clamp and the cursor rule, both of which a second surface would otherwise have
/// to reproduce: a cursor is returned **only when the page came back full**, because handing one
/// back on a short page makes a client fetch an empty page to discover the history ended.
pub(crate) async fn neighbor_history(
    admin: &super::AdminState,
    node_id: Uuid,
    before: Option<(chrono::DateTime<chrono::Utc>, i64)>,
    limit: Option<i64>,
) -> ApiResult<NeighborHistory> {
    let limit = limit
        .unwrap_or(HISTORY_DEFAULT_LIMIT)
        .clamp(1, HISTORY_MAX_LIMIT);
    let rows = admin
        .neighbors
        .list_changes(node_id, before, limit)
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "list neighbor history",
                "failed to load neighbour history",
            )
        })?;
    // A cursor only when the page came back full — a short page is the end of the history, and
    // handing back a cursor there makes a client fetch an empty page to discover that.
    let next = super::util::cursor_if_full(&rows, limit, |r| NeighborHistoryCursor {
        at: r.at.to_rfc3339(),
        id: r.id,
    });
    Ok(NeighborHistory {
        changes: rows
            .into_iter()
            .map(|r| NeighborChange {
                id: r.id,
                at: r.at.to_rfc3339(),
                neighbors: r.set,
                prev_neighbor_key: r.prev_neighbor_key,
                format_changed: r.format_changed,
            })
            .collect(),
        next,
    })
}

// ── Deployment-wide settings ─────────────────────────────────────────────────

/// How this deployment discovers connectivity: CDP/LLDP neighbours and interface addresses.
//
// The `enabled` / `interval_secs` field names describe the neighbour walk and are kept verbatim:
// renaming them would break every existing client for no gain. The L3 pair is added alongside as
// `Option`, so a client that predates it can still `PUT` this body — absent means "leave that
// setting as it is" rather than "turn it off", which is the difference between an additive field
// and a silent regression.
#[derive(Debug, Serialize, Deserialize, utoipa::ToSchema)]
pub(crate) struct NeighborConfig {
    /// Whether CDP/LLDP neighbours are collected at all — the SNMP walks and the Meraki Dashboard
    /// reads (a switch's, an MX's and an MR's) alike.
    pub enabled: bool,
    /// How often each SNMP node's neighbour tables are walked, and each Meraki switch's, MX's and
    /// MR's neighbours are read, in seconds. An MX or MR is read a share per inventory sync within
    /// the organization's API rate, so across a large organization a round can take longer than
    /// this interval.
    pub interval_secs: u32,
    /// Whether interface-address walks are issued at all. Omitted on update leaves it unchanged.
    #[serde(default)]
    pub l3_enabled: Option<bool>,
    /// How often each SNMP node's interface-address tables are walked, in seconds. Omitted on
    /// update leaves it unchanged.
    #[serde(default)]
    pub l3_interval_secs: Option<u32>,
    /// Whether ARP / IPv6-neighbour walks are issued at all. Omitted on update leaves it unchanged.
    ///
    /// Off unless an operator turns it on: this walk reads a table sized by the network rather than
    /// by the device, and it is the only discovery walk here that costs a busy switch measurable
    /// work. What it buys is the only answer nothing else can give — which hosts are on your
    /// segments that Yagra is not monitoring.
    #[serde(default)]
    pub arp_enabled: Option<bool>,
    /// How often each SNMP node's ARP / IPv6-neighbour tables are walked, in seconds. Omitted on
    /// update leaves it unchanged.
    #[serde(default)]
    pub arp_interval_secs: Option<u32>,
    /// Whether routing-adjacency collection is issued at all. Omitted on update leaves it unchanged.
    ///
    /// On unless an operator turns it off. This is what finds the links that share no subnet — a
    /// point-to-point `/32`, an unnumbered OSPF link, an eBGP session across a segment — which the
    /// shared-subnet rule structurally cannot see. The tables it reads are sized by the device's own
    /// peering mesh, and the routing table itself is never walked: it is probed one destination at a
    /// time.
    #[serde(default)]
    pub routing_enabled: Option<bool>,
    /// How often each SNMP node's routing adjacency is collected, in seconds. Omitted on update
    /// leaves it unchanged.
    #[serde(default)]
    pub routing_interval_secs: Option<u32>,
    /// Whether media-type collection is issued at all. Omitted on update leaves it unchanged.
    ///
    /// On unless an operator turns it off. It reads `ifMauTable` — one row per Ethernet port on the
    /// device itself, once an hour — which is what fills the Interfaces tab's Media column.
    /// ⚠️ Many devices do not implement MAU-MIB at all; on those the walk costs one query and
    /// returns nothing, and the column stays empty. Turning this off changes nothing for them.
    #[serde(default)]
    pub media_enabled: Option<bool>,
    /// How often each SNMP node's media type is collected, in seconds. Omitted on update leaves it
    /// unchanged.
    #[serde(default)]
    pub media_interval_secs: Option<u32>,
    /// Smallest cadence this deployment accepts, in seconds.
    #[serde(default)]
    pub min_interval_secs: u32,
    /// Largest cadence this deployment accepts, in seconds.
    #[serde(default)]
    pub max_interval_secs: u32,
}

/// The deployment's adjacency-collection settings, with the accepted cadence range.
#[utoipa::path(
    get, path = "/api/v1/settings/neighbors", tag = "settings",
    responses(
        (status = 200, description = "Whether adjacency is collected, how often, and the accepted range", body = NeighborConfig),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks View", body = super::error::ErrorBody),
        (status = 503, description = "Inventory storage is unavailable (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn get_adjacency_settings(
    _guard: RequireView,
    admin: Admin,
) -> ApiResult<Json<NeighborConfig>> {
    Ok(Json(adjacency_config(&admin).await))
}

/// The adjacency-collection settings with the accepted cadence range — the seam both edges call.
///
/// The range travels with the values on purpose: a caller that knows the current interval but not
/// the bounds cannot tell a rejected write from a broken one.
pub(crate) async fn adjacency_config(admin: &super::AdminState) -> NeighborConfig {
    let s = admin.repo.get_adjacency_settings().await;
    NeighborConfig {
        enabled: s.neighbors_enabled,
        interval_secs: s.neighbors_interval_secs,
        l3_enabled: Some(s.l3_enabled),
        l3_interval_secs: Some(s.l3_interval_secs),
        arp_enabled: Some(s.arp_enabled),
        arp_interval_secs: Some(s.arp_interval_secs),
        routing_enabled: Some(s.routing_enabled),
        routing_interval_secs: Some(s.routing_interval_secs),
        media_enabled: Some(s.media_enabled),
        media_interval_secs: Some(s.media_interval_secs),
        min_interval_secs: neighbors::MIN_NEIGHBOR_INTERVAL_SECS,
        max_interval_secs: neighbors::MAX_NEIGHBOR_INTERVAL_SECS,
    }
}

/// Change whether and how often adjacency is collected.
///
/// Applies from the next scheduler sweep. Turning collection off stops issuing walks but keeps
/// everything already recorded — the current set and the change history are unaffected.
#[utoipa::path(
    put, path = "/api/v1/settings/neighbors", tag = "settings",
    request_body = NeighborConfig,
    responses(
        (status = 204, description = "Settings updated; the change applies from the next scheduler sweep"),
        (status = 400, description = "The cadence is outside the allowed range", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig", body = super::error::ErrorBody),
        (status = 503, description = "Inventory storage is unavailable (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn update_neighbor_settings(
    _guard: RequireManageConfig,
    admin: Admin,
    Json(body): Json<NeighborConfig>,
) -> ApiResult<StatusCode> {
    // The bounds in the request body are informational (they let one shape serve both directions);
    // what the server accepts is decided here, never by what the client sent back.
    //
    // The L3 pair falls back to what is already stored, so a client that predates those fields
    // updates the neighbour settings without silently switching L3 discovery off.
    let current = admin.repo.get_adjacency_settings().await;
    let next = AdjacencySettings {
        neighbors_enabled: body.enabled,
        neighbors_interval_secs: body.interval_secs,
        l3_enabled: body.l3_enabled.unwrap_or(current.l3_enabled),
        l3_interval_secs: body.l3_interval_secs.unwrap_or(current.l3_interval_secs),
        arp_enabled: body.arp_enabled.unwrap_or(current.arp_enabled),
        arp_interval_secs: body.arp_interval_secs.unwrap_or(current.arp_interval_secs),
        routing_enabled: body.routing_enabled.unwrap_or(current.routing_enabled),
        routing_interval_secs: body
            .routing_interval_secs
            .unwrap_or(current.routing_interval_secs),
        media_enabled: body.media_enabled.unwrap_or(current.media_enabled),
        media_interval_secs: body
            .media_interval_secs
            .unwrap_or(current.media_interval_secs),
    };
    if !next.in_bounds() {
        return Err(ApiError::bad_request(
            "invalid_neighbor_interval",
            format!(
                "the neighbour and interface-address intervals must be between {} and {} seconds",
                neighbors::MIN_NEIGHBOR_INTERVAL_SECS,
                neighbors::MAX_NEIGHBOR_INTERVAL_SECS,
            ),
        ));
    }
    admin
        .repo
        .set_adjacency_settings(&next)
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "set neighbor settings",
                "failed to update neighbour settings",
            )
        })?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::tests_support::{private_state, public_state};
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    async fn status(state: ApiState, method: &str, path: &str, body: &str) -> StatusCode {
        let req = Request::builder()
            .method(method)
            .uri(path)
            .header("content-type", "application/json")
            .body(Body::from(body.to_owned()))
            .expect("request builds");
        super::super::router(state)
            .oneshot(req)
            .await
            .expect("router responds")
            .status()
    }

    /// Every route authenticates before it reports anything about this deployment — including
    /// whether it has inventory storage at all. 401, never 503 (api-conventions).
    #[tokio::test]
    async fn anonymous_is_refused_before_the_subsystem_is_consulted() {
        for (method, path, body) in [
            (
                "GET",
                "/api/v1/nodes/00000000-0000-0000-0000-000000000000/neighbors",
                "",
            ),
            (
                "GET",
                "/api/v1/nodes/00000000-0000-0000-0000-000000000000/neighbors/history",
                "",
            ),
            ("GET", "/api/v1/settings/neighbors", ""),
            (
                "PUT",
                "/api/v1/settings/neighbors",
                r#"{"enabled":true,"interval_secs":3600}"#,
            ),
        ] {
            assert_eq!(
                status(private_state(), method, path, body).await,
                StatusCode::UNAUTHORIZED,
                "{method} {path}"
            );
        }
    }

    /// A public-dashboard deployment opens reads, not writes. Adjacency is a read, so the two node
    /// endpoints answer; changing collection stays closed.
    #[tokio::test]
    async fn public_dashboard_opens_the_reads_but_not_the_write() {
        assert_ne!(
            status(public_state(), "GET", "/api/v1/settings/neighbors", "").await,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            status(
                public_state(),
                "PUT",
                "/api/v1/settings/neighbors",
                r#"{"enabled":false,"interval_secs":3600}"#
            )
            .await,
            StatusCode::UNAUTHORIZED,
            "a write guard must stay closed on a public deployment"
        );
    }

    /// The cadence band is enforced at the edge, not left to the table CHECK — the CHECK would
    /// surface as a 500 with no usable message.
    #[test]
    fn the_cadence_band_is_the_one_the_module_declares() {
        for bad in [0, 1, neighbors::MIN_NEIGHBOR_INTERVAL_SECS - 1, 999_999] {
            assert!(
                !AdjacencySettings {
                    neighbors_interval_secs: bad,
                    ..AdjacencySettings::default()
                }
                .in_bounds(),
                "{bad} should be rejected as a neighbour cadence"
            );
            // Every cadence is bounded. Checking only the first would let the others through.
            assert!(
                !AdjacencySettings {
                    l3_interval_secs: bad,
                    ..AdjacencySettings::default()
                }
                .in_bounds(),
                "{bad} should be rejected as an interface-address cadence"
            );
            assert!(
                !AdjacencySettings {
                    arp_interval_secs: bad,
                    ..AdjacencySettings::default()
                }
                .in_bounds(),
                "{bad} should be rejected as an ARP cadence"
            );
        }
        for ok in [
            neighbors::MIN_NEIGHBOR_INTERVAL_SECS,
            3600,
            neighbors::MAX_NEIGHBOR_INTERVAL_SECS,
        ] {
            assert!(AdjacencySettings {
                neighbors_interval_secs: ok,
                l3_interval_secs: ok,
                arp_interval_secs: ok,
                ..AdjacencySettings::default()
            }
            .in_bounds());
        }
    }

    /// A client that predates the ARP fields must not switch ARP discovery off — nor, more subtly,
    /// switch it *on*: the body's `None` means "leave it", and both directions are silent failures
    /// if that is got wrong.
    #[test]
    fn an_absent_field_leaves_the_stored_setting_alone() {
        let body: NeighborConfig =
            serde_json::from_str(r#"{"enabled":true,"interval_secs":3600}"#).unwrap();
        assert_eq!(body.arp_enabled, None);
        assert_eq!(body.arp_interval_secs, None);
        assert_eq!(body.l3_enabled, None);

        let current = AdjacencySettings {
            arp_enabled: true,
            arp_interval_secs: 7200,
            ..AdjacencySettings::default()
        };
        assert!(body.arp_enabled.unwrap_or(current.arp_enabled));
        assert_eq!(
            body.arp_interval_secs.unwrap_or(current.arp_interval_secs),
            7200
        );
    }

    /// The response advertises the range the server actually enforces, so a UI cannot render a
    /// control whose values the server will refuse.
    #[test]
    fn the_advertised_range_is_the_enforced_range() {
        let cfg = NeighborConfig {
            enabled: true,
            interval_secs: 3600,
            l3_enabled: Some(true),
            l3_interval_secs: Some(3600),
            arp_enabled: Some(false),
            arp_interval_secs: Some(21_600),
            routing_enabled: Some(true),
            routing_interval_secs: Some(3600),
            media_enabled: Some(true),
            media_interval_secs: Some(3600),
            min_interval_secs: neighbors::MIN_NEIGHBOR_INTERVAL_SECS,
            max_interval_secs: neighbors::MAX_NEIGHBOR_INTERVAL_SECS,
        };
        assert!(neighbors::interval_in_bounds(cfg.min_interval_secs));
        assert!(neighbors::interval_in_bounds(cfg.max_interval_secs));
        assert!(!neighbors::interval_in_bounds(cfg.min_interval_secs - 1));
        assert!(!neighbors::interval_in_bounds(cfg.max_interval_secs + 1));
    }

    /// ADR-179 Inc.9: a neighbour on the list only through a node outside the caller's folders
    /// says so to that caller, and is simply listed for one who sees everything; one on no list
    /// says it is not listed yet.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_neighbour_listed_only_outside_the_callers_folders_says_so(pool: sqlx::PgPool) {
        use crate::api::tests_support::{live_state, scoped_token, send, token};
        let mine = crate::pgtest::group(&pool, "mine").await;
        let theirs = crate::pgtest::group(&pool, "theirs").await;
        let here = crate::pgtest::node(&pool, "sw-01", 1, Some(mine)).await;
        let there = crate::pgtest::node(&pool, "sw-02", 2, Some(theirs)).await;
        sqlx::query("INSERT INTO l3_discovered (ip, via_node) VALUES ('198.51.100.20', $1)")
            .bind(there)
            .execute(&pool)
            .await
            .expect("listed row");
        let heard = |chassis: &str, addr: &str| {
            let mut n = yagra_common::Neighbor::new(
                yagra_common::NeighborProto::Lldp,
                "Gi0/1",
                chassis,
                "Gi0/2",
            );
            n.remote_mgmt_addr = Some(addr.to_owned());
            n
        };
        crate::neighbors::NeighborRepo::new(pool.clone())
            .record_observation(
                here,
                &NeighborSet::new(
                    vec![
                        heard("far-01", "198.51.100.20"),
                        heard("far-02", "198.51.100.21"),
                    ],
                    0,
                ),
            )
            .await
            .expect("record");
        let st = live_state(pool.clone()).await;
        // A scoped caller's view of a node comes from the alert engine's configuration, which a
        // live core loads on its own; here it is set, as the nodes tests do.
        let meta = [(here, mine), (there, theirs)]
            .into_iter()
            .map(|(node, group)| {
                (
                    yagra_common::NodeId::from(node),
                    crate::alerts::NodeMeta {
                        folder_group: Some(group),
                        folder_chain: vec![group],
                        ..crate::alerts::NodeMeta::default()
                    },
                )
            })
            .collect();
        st.alerts
            .set_config(crate::alerts::AlertConfig::new(Vec::new(), meta));
        let path = format!("/api/v1/nodes/{here}/neighbors");
        let blocked = |body: &serde_json::Value, addr: &str| {
            body["peers"]
                .as_array()
                .expect("peers")
                .iter()
                .find(|p| p["address"] == addr)
                .map(|p| (p["discovery_listed"].clone(), p["setup_blocked"].clone()))
                .expect("the peer")
        };

        let scoped = scoped_token(&st, &[mine]);
        let (status, body) = send(&st, "GET", &path, &scoped, None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(
            blocked(&body, "198.51.100.20"),
            (
                serde_json::json!(false),
                serde_json::json!("found_outside_your_folders")
            )
        );
        assert_eq!(
            blocked(&body, "198.51.100.21").1,
            serde_json::json!("not_listed_yet")
        );

        let admin = token(&st, yagra_common::Role::Admin);
        let (status, body) = send(&st, "GET", &path, &admin, None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(
            blocked(&body, "198.51.100.20"),
            (serde_json::json!(true), serde_json::Value::Null)
        );
    }

    // ── An accepted write (ADR-115) ──────────────────────────────────────────────────

    /// The adjacency settings are written and read back as they were sent.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn adjacency_settings_round_trip_through_the_settings_row(pool: sqlx::PgPool) {
        use crate::api::tests_support::{live_state, send, token};
        let st = live_state(pool.clone()).await;
        let tok = token(&st, yagra_common::Role::Admin);
        let (status, body) = send(
            &st,
            "PUT",
            "/api/v1/settings/neighbors",
            &tok,
            Some(serde_json::json!({ "enabled": true, "interval_secs": 1800 })),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::NO_CONTENT, "{body}");
        let admin = st.admin.clone().expect("live state");
        let settings = admin.repo.get_adjacency_settings().await;
        assert!(settings.neighbors_enabled);
        assert_eq!(settings.neighbors_interval_secs, 1800);
    }
}

#[cfg(test)]
mod peer_tests {
    use super::*;
    use yagra_common::{Neighbor, NeighborProto};

    fn claim(address: &str, id: Uuid, name: &str, visible: bool) -> AddressClaim {
        AddressClaim {
            address: address.parse().unwrap(),
            id,
            name: name.to_owned(),
            visible,
            ifindex: None,
        }
    }

    /// [`classify_peers`] with no names sent and no link readings — every test written before
    /// ADR-180 Inc.4.
    fn classify(
        advertised: &BTreeMap<String, IpAddr>,
        claims: &[AddressClaim],
        listed: &BTreeMap<IpAddr, Uuid>,
        managed: &HashMap<IpAddr, NeighborManagedBy>,
        blockers: &Blockers,
    ) -> Vec<NeighborPeer> {
        classify_peers(
            advertised,
            claims,
            &Evidence {
                listed,
                managed,
                blockers,
                names: &BTreeMap::new(),
                oper: &HashMap::new(),
            },
        )
    }

    fn advertised(addrs: &[&str]) -> BTreeMap<String, IpAddr> {
        addrs
            .iter()
            .map(|a| ((*a).to_owned(), a.parse().unwrap()))
            .collect()
    }

    #[test]
    fn each_address_gets_the_state_its_claimants_give_it() {
        let seen = Uuid::from_u128(1);
        let hidden = Uuid::from_u128(2);
        let (vip_a, vip_b) = (Uuid::from_u128(3), Uuid::from_u128(4));
        let claims = [
            claim("192.0.2.1", seen, "rtr-a", true),
            claim("192.0.2.2", hidden, "rtr-hidden", false),
            claim("192.0.2.3", vip_a, "fw-a", true),
            claim("192.0.2.3", vip_b, "fw-b", true),
        ];
        let row = Uuid::from_u128(9);
        let listed = BTreeMap::from([("192.0.2.4".parse().unwrap(), row)]);
        let peers = classify(
            &advertised(&["192.0.2.1", "192.0.2.2", "192.0.2.3", "192.0.2.4"]),
            &claims,
            &listed,
            &HashMap::new(),
            &Blockers::default(),
        );
        let states: Vec<(&str, NeighborPeerState)> = peers
            .iter()
            .map(|p| (p.address.as_str(), p.state))
            .collect();
        assert_eq!(
            states,
            vec![
                ("192.0.2.1", NeighborPeerState::Node),
                ("192.0.2.2", NeighborPeerState::OutsideScope),
                ("192.0.2.3", NeighborPeerState::Ambiguous),
                ("192.0.2.4", NeighborPeerState::Unregistered),
            ]
        );
        assert_eq!(peers[0].node_id, Some(seen));
        assert_eq!(peers[0].node_name.as_deref(), Some("rtr-a"));
        assert!(peers[3].discovery_listed);
        assert_eq!(peers[3].discovery_id, Some(row));
        assert!(!peers[0].discovery_listed);
        assert_eq!(peers[0].discovery_id, None);
    }

    fn ap(controller: Option<(Uuid, &str)>) -> crate::wireless::ApAtAddress {
        crate::wireless::ApAtAddress {
            ap_id: Uuid::from_u128(70),
            controller: controller.map(|(id, n)| (id, n.to_owned())),
            imported: false,
        }
    }

    /// ADR-179 Inc.3: a controller the caller can see wins, then a Meraki organization, then a
    /// controller it cannot see — which names nothing.
    #[test]
    fn a_managed_address_names_who_adds_it() {
        let wlc = Uuid::from_u128(71);
        let org = (Uuid::from_u128(72), "Acme".to_owned());
        assert_eq!(
            managed_by(Some(&ap(Some((wlc, "wlc01")))), Some(&org)),
            Some(NeighborManagedBy::Controller {
                ap_id: Uuid::from_u128(70),
                controller_node_id: wlc,
                controller_name: "wlc01".into(),
                imported: false,
            })
        );
        assert_eq!(
            managed_by(Some(&ap(None)), Some(&org)),
            Some(NeighborManagedBy::Meraki {
                org_id: org.0,
                org_name: "Acme".into()
            })
        );
        assert_eq!(
            managed_by(Some(&ap(None)), None),
            Some(NeighborManagedBy::ControllerHidden)
        );
        assert_eq!(managed_by(None, None), None);
    }

    /// Only an unregistered address carries a manager: one a node already owns is that node.
    #[test]
    fn only_an_unregistered_address_carries_its_manager() {
        let owner = Uuid::from_u128(1);
        let claims = [claim("192.0.2.1", owner, "ap-node", true)];
        let managed = HashMap::from([
            (
                "192.0.2.1".parse().unwrap(),
                NeighborManagedBy::ControllerHidden,
            ),
            (
                "192.0.2.2".parse().unwrap(),
                NeighborManagedBy::ControllerHidden,
            ),
        ]);
        let peers = classify(
            &advertised(&["192.0.2.1", "192.0.2.2"]),
            &claims,
            &BTreeMap::new(),
            &managed,
            &Blockers::default(),
        );
        assert_eq!(peers[0].state, NeighborPeerState::Node);
        assert_eq!(peers[0].managed_by, None);
        assert_eq!(peers[1].state, NeighborPeerState::Unregistered);
        assert_eq!(
            peers[1].managed_by,
            Some(NeighborManagedBy::ControllerHidden)
        );
    }

    /// The rule the whole scope design rests on: a claimant the caller may not see never lends
    /// the answer its id or its name. An ambiguous address answers no node, and since ADR-180
    /// Inc.4 lists the claimants the caller may see — the hidden one is only counted.
    #[test]
    fn a_hidden_claimant_gives_away_no_id_and_no_name() {
        let claims = [
            claim("192.0.2.2", Uuid::from_u128(2), "rtr-hidden", false),
            claim("192.0.2.3", Uuid::from_u128(3), "fw-a", true),
            claim("192.0.2.3", Uuid::from_u128(4), "fw-hidden", false),
        ];
        let peers = classify(
            &advertised(&["192.0.2.2", "192.0.2.3"]),
            &claims,
            &BTreeMap::new(),
            &HashMap::new(),
            &Blockers::default(),
        );
        for p in &peers {
            assert_eq!(p.node_id, None, "{}", p.address);
            assert_eq!(p.node_name, None, "{}", p.address);
        }
        assert_eq!(peers[1].state, NeighborPeerState::Ambiguous);
        let listed: Vec<&str> = peers[1]
            .also_claimed_by
            .iter()
            .map(|a| a.node_name.as_str())
            .collect();
        assert_eq!(listed, ["fw-a"]);
        assert_eq!(peers[1].also_claimed_total, 2);
        let json = serde_json::to_string(&peers).unwrap();
        assert!(
            !json.contains("rtr-hidden") && !json.contains("fw-hidden"),
            "{json}"
        );
        assert!(!json.contains(&Uuid::from_u128(4).to_string()), "{json}");
    }

    /// One node claiming an address twice (by inventory address and by an interface) is still one
    /// claimant, not an ambiguity.
    #[test]
    fn the_same_node_claiming_an_address_twice_is_one_claimant() {
        let id = Uuid::from_u128(9);
        let claims = [
            claim("192.0.2.9", id, "rtr", true),
            claim("192.0.2.9", id, "rtr", true),
        ];
        let peers = classify(
            &advertised(&["192.0.2.9"]),
            &claims,
            &BTreeMap::new(),
            &HashMap::new(),
            &Blockers::default(),
        );
        assert_eq!(peers[0].state, NeighborPeerState::Node);
    }

    #[test]
    fn an_ipv6_address_matches_by_value_not_by_spelling() {
        let id = Uuid::from_u128(6);
        // The row spells it one way; the claim was parsed from another.
        let claims = [claim("2001:db8::6", id, "rtr6", true)];
        let peers = classify(
            &advertised(&["2001:0db8:0:0:0:0:0:6"]),
            &claims,
            &BTreeMap::new(),
            &HashMap::new(),
            &Blockers::default(),
        );
        assert_eq!(peers[0].state, NeighborPeerState::Node);
        assert_eq!(peers[0].address, "2001:0db8:0:0:0:0:0:6");
    }

    fn port_claim(address: &str, id: Uuid, name: &str, ifindex: u32) -> AddressClaim {
        AddressClaim {
            ifindex: Some(ifindex),
            ..claim(address, id, name, true)
        }
    }

    /// [`classify_peers`] for one address, with the names its rows sent and the link readings.
    fn classify_named(
        claims: &[AddressClaim],
        names: &[&str],
        oper: &HashMap<(Uuid, i64), f64>,
    ) -> NeighborPeer {
        let names = BTreeMap::from([(
            "192.0.2.182".parse().unwrap(),
            names.iter().map(|n| (*n).to_owned()).collect(),
        )]);
        let mut peers = classify_peers(
            &advertised(&["192.0.2.182"]),
            claims,
            &Evidence {
                listed: &BTreeMap::new(),
                managed: &HashMap::new(),
                blockers: &Blockers::default(),
                names: &names,
                oper,
            },
        );
        peers.remove(0)
    }

    /// ADR-180 Inc.4, the case that prompted it: a core switch and two WAN routers all carry the
    /// switch's advertised address. The name picks the switch; the routers stay listed, one with
    /// link and one without.
    #[test]
    fn a_name_picks_one_of_several_claimants_and_the_rest_stay_listed() {
        let (core, wan1, wan2) = (Uuid::from_u128(1), Uuid::from_u128(2), Uuid::from_u128(3));
        let claims = [
            port_claim("192.0.2.182", core, "core-sw-01", 76),
            port_claim("192.0.2.182", wan1, "wan-rtr-01", 1),
            port_claim("192.0.2.182", wan2, "wan-rtr-02", 5),
        ];
        let oper = HashMap::from([((wan1, 1), 1.0), ((wan2, 5), 2.0), ((core, 76), 1.0)]);
        let p = classify_named(&claims, &["core-sw-01"], &oper);
        assert_eq!(p.state, NeighborPeerState::Node);
        assert!(p.matched_by_name);
        assert_eq!(p.node_id, Some(core));
        assert_eq!(p.node_name.as_deref(), Some("core-sw-01"));
        let others: Vec<(&str, ClaimPortState)> = p
            .also_claimed_by
            .iter()
            .map(|a| (a.node_name.as_str(), a.port_state))
            .collect();
        assert_eq!(
            others,
            [
                ("wan-rtr-01", ClaimPortState::Up),
                ("wan-rtr-02", ClaimPortState::LinkDown)
            ]
        );
        assert_eq!(p.also_claimed_total, 2);
    }

    /// A name matching none of the claimants, or two of them, leaves the address ambiguous — and
    /// every claimant is still listed.
    #[test]
    fn a_name_that_does_not_pick_exactly_one_leaves_it_ambiguous() {
        let (a, b) = (Uuid::from_u128(1), Uuid::from_u128(2));
        let claims = [
            claim("192.0.2.182", a, "fw-01.site-a.example.com", true),
            claim("192.0.2.182", b, "fw-01.site-b.example.com", true),
        ];
        let none = HashMap::new();
        for names in [&["sw-99"][..], &["fw-01"][..], &[][..]] {
            let p = classify_named(&claims, names, &none);
            assert_eq!(p.state, NeighborPeerState::Ambiguous, "{names:?}");
            assert!(!p.matched_by_name);
            assert_eq!(p.also_claimed_by.len(), 2);
            assert_eq!(p.also_claimed_total, 2);
        }
    }

    /// CDP's `name(serial)` and a domain on either side still match; a whole-name match beats a
    /// host-label one.
    #[test]
    fn a_serial_suffix_and_a_domain_do_not_stop_a_match() {
        let (a, b) = (Uuid::from_u128(1), Uuid::from_u128(2));
        let claims = [
            claim("192.0.2.182", a, "SW-01", true),
            claim("192.0.2.182", b, "rtr-01", true),
        ];
        let none = HashMap::new();
        for sent in ["sw-01(FOC0000X0AB)", "sw-01.example.com", " SW-01 "] {
            let p = classify_named(&claims, &[sent], &none);
            assert_eq!(p.node_id, Some(a), "{sent}");
        }
        // A node named with its domain, a neighbour sending the bare host name.
        let fqdn = [
            claim("192.0.2.182", a, "sw-01.example.com", true),
            claim("192.0.2.182", b, "rtr-01", true),
        ];
        assert_eq!(classify_named(&fqdn, &["sw-01"], &none).node_id, Some(a));
        // Whole-name first: `sw-01` is exactly one node although both share the host label.
        let both = [
            claim("192.0.2.182", a, "sw-01", true),
            claim("192.0.2.182", b, "sw-01.example.com", true),
        ];
        assert_eq!(classify_named(&both, &["sw-01"], &none).node_id, Some(a));
        // An address is not split at its dots.
        let ips = [
            claim("192.0.2.182", a, "10.0.0.1", true),
            claim("192.0.2.182", b, "10.0.0.2", true),
        ];
        let p = classify_named(&ips, &["10.9.9.9"], &none);
        assert_eq!(p.state, NeighborPeerState::Ambiguous);
    }

    /// Two rows advertising the address name two different claimants: neither is picked.
    #[test]
    fn rows_that_disagree_pick_nobody() {
        let (a, b) = (Uuid::from_u128(1), Uuid::from_u128(2));
        let claims = [
            claim("192.0.2.182", a, "sw-01", true),
            claim("192.0.2.182", b, "sw-02", true),
        ];
        let none = HashMap::new();
        let p = classify_named(&claims, &["sw-01", "sw-02"], &none);
        assert_eq!(p.state, NeighborPeerState::Ambiguous);
        // A second row that names nobody is not disagreement.
        let p = classify_named(&claims, &["sw-01", "unknown-host"], &none);
        assert_eq!(p.node_id, Some(a));
    }

    /// A name that picks a node the caller may not see picks nothing (decision 6): the answer is the
    /// same one a name matching nobody gets, so it says nothing about which hidden node bears the
    /// name on the row. The visible claimant is still listed and the hidden one still counted.
    #[test]
    fn a_name_picking_a_hidden_node_discloses_nothing_about_it() {
        let (hidden, seen) = (Uuid::from_u128(1), Uuid::from_u128(2));
        let claims = [
            port_claim("192.0.2.182", seen, "rtr-seen", 4),
            AddressClaim {
                ifindex: Some(9),
                ..claim("192.0.2.182", hidden, "core-hidden", false)
            },
        ];
        let named = classify_named(&claims, &["core-hidden"], &HashMap::new());
        let unnamed = classify_named(&claims, &["nobody"], &HashMap::new());
        assert_eq!(named, unnamed, "a hidden pick must read like no pick");
        assert_eq!(named.state, NeighborPeerState::Ambiguous);
        assert!(!named.matched_by_name);
        assert_eq!((named.node_id, named.node_name.as_deref()), (None, None));
        assert_eq!(named.also_claimed_by.len(), 1);
        assert_eq!(named.also_claimed_total, 2);
        let json = serde_json::to_string(&named).unwrap();
        assert!(
            !json.contains("core-hidden") && !json.contains(&hidden.to_string()),
            "{json}"
        );
        // Its ports are not read either.
        let names = BTreeMap::from([(
            "192.0.2.182".parse().unwrap(),
            vec!["core-hidden".to_owned()],
        )]);
        assert_eq!(
            ports_to_read(&advertised(&["192.0.2.182"]), &claims, &names),
            [(seen, 4)]
        );
    }

    /// One claimant, outside the caller's folders, is still `outside_scope` — no name decides it.
    #[test]
    fn a_sole_hidden_claimant_is_outside_scope() {
        let hidden = Uuid::from_u128(1);
        let claims = [claim("192.0.2.182", hidden, "core-hidden", false)];
        let p = classify_named(&claims, &["core-hidden"], &HashMap::new());
        assert_eq!(p.state, NeighborPeerState::OutsideScope);
        assert!(!p.matched_by_name);
        assert_eq!(p.also_claimed_total, 0);
    }

    /// Link state: any port up is up; only down readings is link-down; an inventory-only claim, no
    /// reading, or an unrecognised value says nothing. One address carried by one node has no list.
    #[test]
    fn link_state_comes_from_the_ports_carrying_the_address() {
        let n = Uuid::from_u128(1);
        let oper = HashMap::from([((n, 1), 2.0), ((n, 2), 1.0), ((n, 3), 7.0), ((n, 4), 5.0)]);
        let st = |ports: &[u32]| claim_port_state(n, ports.iter().copied(), &oper);
        assert_eq!(st(&[1, 2]), ClaimPortState::Up);
        assert_eq!(st(&[1, 3]), ClaimPortState::LinkDown);
        assert_eq!(st(&[1, 9]), ClaimPortState::LinkDown);
        assert_eq!(st(&[4]), ClaimPortState::Unknown);
        assert_eq!(st(&[9]), ClaimPortState::Unknown);
        assert_eq!(st(&[]), ClaimPortState::Unknown);

        let single = classify_named(&[port_claim("192.0.2.182", n, "sw-01", 1)], &[], &oper);
        assert!(single.also_claimed_by.is_empty());
        assert_eq!(single.also_claimed_total, 0);
        assert!(!single.matched_by_name);
    }

    /// No link readings (the store did not answer) leaves every claimant `unknown`, never `up`.
    #[test]
    fn no_readings_leave_every_claimant_unknown() {
        let (a, b) = (Uuid::from_u128(1), Uuid::from_u128(2));
        let claims = [
            port_claim("192.0.2.182", a, "sw-01", 1),
            port_claim("192.0.2.182", b, "sw-02", 2),
        ];
        let p = classify_named(&claims, &[], &HashMap::new());
        assert!(p
            .also_claimed_by
            .iter()
            .all(|a| a.port_state == ClaimPortState::Unknown));
    }

    /// The list stops at ten; the total counts every other claimant. The store is asked only about
    /// the listed ones' ports carrying the address — not the node answered with, not the eleventh
    /// and beyond, not an address with one claimant (decision 7).
    #[test]
    fn the_list_is_capped_and_the_total_is_not() {
        let claims: Vec<AddressClaim> = (1..=13u32)
            .map(|i| {
                port_claim(
                    "192.0.2.182",
                    Uuid::from_u128(u128::from(i)),
                    &format!("rtr-{i:02}"),
                    i,
                )
            })
            .chain([port_claim("192.0.2.9", Uuid::from_u128(99), "alone", 1)])
            .collect();
        let p = classify_named(&claims, &["rtr-01"], &HashMap::new());
        assert_eq!(p.node_id, Some(Uuid::from_u128(1)));
        assert_eq!(p.also_claimed_by.len(), ALSO_CLAIMED_MAX);
        assert_eq!(p.also_claimed_by[0].node_name, "rtr-02");
        assert_eq!(p.also_claimed_total, 12);
        let names = BTreeMap::from([("192.0.2.182".parse().unwrap(), vec!["rtr-01".to_owned()])]);
        let asked = ports_to_read(&advertised(&["192.0.2.182", "192.0.2.9"]), &claims, &names);
        let want: Vec<(Uuid, i64)> = (2..=11u32)
            .map(|i| (Uuid::from_u128(u128::from(i)), i64::from(i)))
            .collect();
        assert_eq!(asked, want);
    }

    /// The name a row sends: LLDP's system name first, else a text chassis id; never a MAC.
    #[test]
    fn the_name_a_row_sends_is_never_a_mac() {
        let mut lldp = neighbor("00:00:0c:12:34:56", Some(NeighborIdKind::Mac));
        assert_eq!(advertised_name(&lldp), None);
        lldp.remote_sys_name = Some("sw-01".into());
        assert_eq!(advertised_name(&lldp), Some("sw-01"));
        let cdp = neighbor("sw-02(FOC0000X0AB)", Some(NeighborIdKind::Text));
        assert_eq!(advertised_name(&cdp), Some("sw-02(FOC0000X0AB)"));
        assert_eq!(
            peer_name_key("sw-02(FOC0000X0AB)").as_deref(),
            Some("sw-02")
        );
        assert_eq!(peer_name_key("  "), None);
        assert_eq!(peer_name_key("(x)"), None);
    }

    /// A record from before the kind field: a text id is still a name, an address- or MAC-shaped
    /// one is not — a node added by address is *named* that address (decision 9).
    #[test]
    fn an_unlabelled_chassis_is_a_name_only_when_it_does_not_look_like_an_address() {
        assert_eq!(advertised_name(&neighbor("sw-01", None)), Some("sw-01"));
        assert_eq!(advertised_name(&neighbor("10.0.0.5", None)), None);
        assert_eq!(advertised_name(&neighbor("2001:db8::5", None)), None);
        assert_eq!(advertised_name(&neighbor("00-00-0C-12-34-56", None)), None);
        // With the address as the sole evidence, the node named after it is not picked.
        let (by_ip, other) = (Uuid::from_u128(1), Uuid::from_u128(2));
        let mut row = neighbor("10.0.0.5", None);
        row.remote_mgmt_addr = Some("192.0.2.182".into());
        let set = NeighborSet::new(vec![row], 0);
        let claims = [
            claim("192.0.2.182", by_ip, "10.0.0.5", true),
            claim("192.0.2.182", other, "rtr-01", true),
        ];
        let names = advertised_names(&set);
        let p = classify_named(
            &claims,
            &names
                .values()
                .flatten()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            &HashMap::new(),
        );
        assert_eq!(p.state, NeighborPeerState::Ambiguous);
    }

    /// A store that never answers costs the tab the budget and no more, and leaves every port
    /// unknown (decision 8).
    #[tokio::test(start_paused = true)]
    async fn a_link_state_read_that_never_answers_is_given_up_after_the_budget() {
        let started = tokio::time::Instant::now();
        let got = within_budget(std::future::pending(), 2).await;
        assert!(got.is_empty());
        assert_eq!(started.elapsed(), LINK_STATE_BUDGET);
    }

    /// Within the budget, the answer is the store's.
    #[tokio::test(start_paused = true)]
    async fn a_link_state_read_inside_the_budget_is_kept() {
        let port = (Uuid::from_u128(1), 4);
        let got = within_budget(
            async move {
                tokio::time::sleep(LINK_STATE_BUDGET / 2).await;
                HashMap::from([(port, 1.0)])
            },
            1,
        )
        .await;
        assert_eq!(got, HashMap::from([(port, 1.0)]));
    }

    fn neighbor(chassis: &str, kind: Option<NeighborIdKind>) -> Neighbor {
        let mut n = Neighbor::new(NeighborProto::Lldp, "Gi0/1", chassis, "Gi0/2");
        n.remote_chassis_kind = kind;
        n
    }

    #[test]
    fn only_ids_the_device_labelled_as_macs_are_looked_up() {
        let set = NeighborSet::new(
            vec![
                neighbor("00:00:0c:12:34:56", Some(NeighborIdKind::Mac)),
                // Looks like a MAC, but was text on the wire.
                neighbor("00:50:56:ab:cd:ef", Some(NeighborIdKind::Text)),
                // Collected before the kind was recorded.
                neighbor("00:50:56:ab:cd:00", None),
            ],
            0,
        );
        let found = mac_vendors(&set);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].mac, "00:00:0c:12:34:56");
        assert!(found[0].vendor.starts_with("Cisco"), "{}", found[0].vendor);
    }

    /// ADR-179 Inc.9: an unregistered address nothing adds says why, in the list's own order; one
    /// that is listed, managed or monitored says nothing.
    #[test]
    fn an_address_nothing_adds_says_why_in_the_lists_own_order() {
        let ip = |s: &str| s.parse::<IpAddr>().unwrap();
        assert_eq!(
            setup_blocked(ip("127.0.0.1"), true, true),
            SetupBlocked::NotADeviceAddress
        );
        assert_eq!(
            setup_blocked(ip("fe80::1"), false, false),
            SetupBlocked::NotADeviceAddress
        );
        assert_eq!(
            setup_blocked(ip("192.0.2.1"), true, true),
            SetupBlocked::EndStation
        );
        assert_eq!(
            setup_blocked(ip("192.0.2.1"), false, true),
            SetupBlocked::FoundOutsideYourFolders
        );
        assert_eq!(
            setup_blocked(ip("192.0.2.1"), false, false),
            SetupBlocked::NotListedYet
        );

        let blockers = Blockers {
            end_station_only: [ip("192.0.2.2")].into(),
            listed_elsewhere: BTreeSet::new(),
        };
        let listed = BTreeMap::from([(ip("192.0.2.3"), Uuid::from_u128(3))]);
        let managed = HashMap::from([(ip("192.0.2.4"), NeighborManagedBy::ControllerHidden)]);
        let claims = vec![claim("192.0.2.5", Uuid::from_u128(5), "rtr-a", true)];
        let peers = classify(
            &advertised(&[
                "192.0.2.1",
                "192.0.2.2",
                "192.0.2.3",
                "192.0.2.4",
                "192.0.2.5",
            ]),
            &claims,
            &listed,
            &managed,
            &blockers,
        );
        let got: Vec<(&str, Option<SetupBlocked>)> = peers
            .iter()
            .map(|p| (p.address.as_str(), p.setup_blocked))
            .collect();
        assert_eq!(
            got,
            [
                ("192.0.2.1", Some(SetupBlocked::NotListedYet)),
                ("192.0.2.2", Some(SetupBlocked::EndStation)),
                ("192.0.2.3", None),
                ("192.0.2.4", None),
                ("192.0.2.5", None),
            ]
        );
    }

    /// The list skips a row, not an address: one row that is not only an end station admits it.
    #[test]
    fn an_address_is_end_station_only_when_every_row_advertising_it_says_so() {
        let mut phone = neighbor("sep-01", Some(NeighborIdKind::Text));
        phone.remote_mgmt_addr = Some("192.0.2.10".to_owned());
        phone.capabilities = vec![NeighborCapability::Phone, NeighborCapability::Host];
        let mut switch_side = neighbor("sw-09", Some(NeighborIdKind::Text));
        switch_side.remote_mgmt_addr = Some("192.0.2.11".to_owned());
        switch_side.capabilities = vec![NeighborCapability::Phone];
        let mut same_switch = neighbor("sw-09", Some(NeighborIdKind::Text));
        same_switch.proto = NeighborProto::Cdp;
        same_switch.remote_mgmt_addr = Some("192.0.2.11".to_owned());
        same_switch.capabilities = vec![NeighborCapability::Switch];
        let mut silent = neighbor("sw-10", Some(NeighborIdKind::Text));
        silent.remote_mgmt_addr = Some("192.0.2.12".to_owned());
        let got = end_station_only(&NeighborSet::new(
            vec![phone, switch_side, same_switch, silent],
            0,
        ));
        assert_eq!(
            got.into_iter().collect::<Vec<_>>(),
            ["192.0.2.10".parse::<IpAddr>().unwrap()]
        );
    }

    /// ADR-180 Inc.3 decision 2: only a MAC chassis on a row with no usable address is matched by MAC.
    #[test]
    fn only_a_mac_chassis_with_no_usable_address_is_matched_by_mac() {
        let bare = neighbor("0c:8d:db:00:00:01", Some(NeighborIdKind::Mac));
        let mut junk = neighbor("0c:8d:db:00:00:02", Some(NeighborIdKind::Mac));
        junk.remote_mgmt_addr = Some("not-an-address".to_owned());
        let mut addressed = neighbor("0c:8d:db:00:00:03", Some(NeighborIdKind::Mac));
        addressed.remote_mgmt_addr = Some("192.0.2.7".to_owned());
        let named = neighbor("sw-01", Some(NeighborIdKind::Text));
        let got = unaddressed_mac_chassis(&NeighborSet::new(vec![bare, junk, addressed, named], 0));
        assert_eq!(
            got.into_iter().collect::<Vec<_>>(),
            ["0c:8d:db:00:00:01", "0c:8d:db:00:00:02"]
        );
    }

    /// Decision 10: `0.0.0.0` — what a Meraki switch sends over CDP when it has no address — and `::`
    /// are no address. The row is not asked about by address, its name is not a claimant's, and its MAC
    /// chassis goes to the Meraki listing instead. A loopback is still an address, and says why it
    /// cannot be set up.
    #[test]
    fn an_unspecified_management_address_is_no_address() {
        let mut v4 = neighbor("b4:df:91:00:00:01", Some(NeighborIdKind::Mac));
        v4.remote_mgmt_addr = Some("0.0.0.0".to_owned());
        v4.remote_sys_name = Some("ms-01".to_owned());
        let mut v6 = neighbor("b4:df:91:00:00:02", Some(NeighborIdKind::Mac));
        v6.remote_mgmt_addr = Some("::".to_owned());
        let mut lo = neighbor("sw-lo", Some(NeighborIdKind::Text));
        lo.remote_mgmt_addr = Some("127.0.0.1".to_owned());
        let set = NeighborSet::new(vec![v4, v6, lo], 0);
        assert_eq!(
            advertised_addresses(&set).into_keys().collect::<Vec<_>>(),
            ["127.0.0.1"]
        );
        assert!(advertised_names(&set).keys().all(|ip| !ip.is_unspecified()));
        assert!(end_station_only(&set).iter().all(|ip| !ip.is_unspecified()));
        assert_eq!(
            unaddressed_mac_chassis(&set)
                .into_iter()
                .collect::<Vec<_>>(),
            ["b4:df:91:00:00:01", "b4:df:91:00:00:02"]
        );
    }

    #[test]
    fn a_chassis_mac_gets_the_state_of_the_meraki_device_listed_under_it() {
        use crate::meraki_inventory::{DeviceWithMac, MacNode};
        let org = Uuid::from_u128(9);
        let folder = Uuid::from_u128(7);
        let node = Uuid::from_u128(5);
        let listed = |node: Option<MacNode>| DeviceWithMac {
            org_id: org,
            org_name: "org-a".to_owned(),
            product_type: if node.is_some() {
                "appliance"
            } else {
                "wireless"
            }
            .to_owned(),
            node,
        };
        let by_mac = HashMap::from([
            (
                "0c:8d:db:00:00:01".to_owned(),
                listed(Some(MacNode {
                    id: node,
                    name: "mx-01".to_owned(),
                    group_id: Some(folder),
                })),
            ),
            ("0c:8d:db:00:00:02".to_owned(), listed(None)),
        ]);
        let chassis: BTreeSet<String> = [
            "0c:8d:db:00:00:01",
            "0c:8d:db:00:00:02",
            "0c:8d:db:00:00:03",
        ]
        .map(str::to_owned)
        .into();

        let got = classify_chassis(&chassis, &by_mac, &NodeScope::All);
        // A MAC no organization lists is left out.
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].state, NeighborPeerState::Node);
        assert_eq!(got[0].node_id, Some(node));
        assert_eq!(got[0].node_name.as_deref(), Some("mx-01"));
        assert_eq!(got[0].managed_by, None);
        // Inc.4 decision 2: what each device is, from the product the organization lists it as.
        assert_eq!(got[0].capabilities, [NeighborCapability::Router]);
        assert_eq!(got[1].capabilities, [NeighborCapability::WlanAp]);
        assert_eq!(got[1].state, NeighborPeerState::Unregistered);
        assert_eq!(
            got[1].managed_by,
            Some(NeighborManagedBy::Meraki {
                org_id: org,
                org_name: "org-a".to_owned()
            })
        );

        for (product, want) in [
            ("switch", vec![NeighborCapability::Switch]),
            ("cellularGateway", vec![NeighborCapability::Router]),
            ("camera", vec![]),
            ("sensor", vec![]),
        ] {
            assert_eq!(product_capabilities(product), want, "{product}");
        }

        // A node outside the caller's folders gives away no id and no name.
        let hidden = classify_chassis(&chassis, &by_mac, &NodeScope::sees_nothing());
        assert_eq!(hidden[0].state, NeighborPeerState::OutsideScope);
        assert_eq!(
            (hidden[0].node_id, hidden[0].node_name.as_deref()),
            (None, None)
        );
    }

    #[test]
    fn an_address_that_does_not_parse_is_not_asked_about() {
        let mut n = neighbor("sw-01", Some(NeighborIdKind::Text));
        n.remote_mgmt_addr = Some("not-an-address".to_owned());
        let mut m = neighbor("sw-02", Some(NeighborIdKind::Text));
        m.remote_mgmt_addr = Some("192.0.2.5".to_owned());
        let got = advertised_addresses(&NeighborSet::new(vec![n, m], 0));
        assert_eq!(got.keys().collect::<Vec<_>>(), vec!["192.0.2.5"]);
    }
}
