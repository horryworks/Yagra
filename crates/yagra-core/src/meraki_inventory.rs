// SPDX-License-Identifier: AGPL-3.0-only
//! What Meraki says exists (ADR-164): the `meraki_inventory` table, the pure plan that decides which
//! of its rows a sync writes, and the one reading of what state a device is in.
//!
//! Until this module the only record of a Meraki device was its node, so core could not say "three
//! devices are not monitored", could not tell a device an operator deleted from one it had never
//! seen, and had nowhere to notice a monitored device disappearing from the Dashboard.
//!
//! Three rules hold the table up, and each is enforced somewhere a test can reach:
//! - **A sync writes only what changed** ([`plan_sync`], pure). The sync runs every five minutes per
//!   organization; rewriting every row each time would be thousands of dead tuples an hour for a
//!   table whose contents move a few times a month.
//! - **The three timestamps are written at a transition, once.** The planner decides *whether*; the
//!   statement in [`MerakiInventoryRepo::apply`] is written so a second write cannot move one
//!   (`COALESCE`), because the planner being right is not a property of the database.
//! - **`missing_since` is only ever derived from a complete listing.** This module cannot check
//!   that — it is `yagra_transport::fetch_inventory`'s contract, which returns an error rather than
//!   a short answer — so [`plan_sync`] takes the listing as given and says so here.
//!
//! Since ADR-164 Inc.8 the same plan also says what an **imported node follows** (決定 14): its
//! address, its name while nobody has renamed it, and the network its binding names. That makes
//! [`MerakiInventoryRepo::apply`] a writer of `nodes` and `meraki_devices` as well, and it is here
//! rather than beside the importer for one reason — a rename is only visible at the moment the
//! stored name changes, so it has to commit with the row that changes it.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::net::IpAddr;

use chrono::{DateTime, Utc};
use sqlx::{PgPool, Row};
use uuid::Uuid;
use yagra_common::MerakiHaRole;
use yagra_transport::{MerakiInventory, MerakiInventoryDevice, MerakiLanAddress};

/// One device as a complete sync saw it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeenDevice {
    pub serial: String,
    pub name: String,
    pub model: Option<String>,
    pub product_type: String,
    pub network_id: String,
    /// The device's LAN address, if a usable one is known. See [`usable_address`], and for an MX
    /// [`choose_lan_address`].
    pub lan_ip: Option<IpAddr>,
    /// Whether Meraki reports the device up *in this listing* (online or alerting).
    pub online: bool,
}

impl SeenDevice {
    /// Read one device out of a transport inventory. `lans` is what [`lan_addresses`] chose for each
    /// network whose LAN side has been read; an MX takes its address from there, never from what the
    /// listing says (決定 28), and one whose network has not been read has none yet.
    #[must_use]
    pub fn from_transport(d: &MerakiInventoryDevice, lans: &LanAddresses) -> Self {
        let lan_ip = if takes_lan_from_vlans(&d.info.product_type) {
            lans.get(&d.info.network_id).copied().flatten()
        } else {
            d.info.lan_ip.as_deref().and_then(usable_address)
        };
        Self {
            serial: d.info.serial.clone(),
            name: d.info.name.clone(),
            model: d.info.model.clone(),
            product_type: d.info.product_type.clone(),
            network_id: d.info.network_id.clone(),
            lan_ip,
            online: d.availability.is_some_and(|a| a.is_up()),
        }
    }
}

/// Every device of a transport inventory, as the planner wants them. A serial listed twice keeps
/// its first entry: the table's key is the serial, and two rows for one would make the plan depend
/// on the order of a listing nobody controls.
#[must_use]
pub fn seen_devices(inventory: &MerakiInventory, lans: &LanAddresses) -> Vec<SeenDevice> {
    let mut taken: HashSet<&str> = HashSet::new();
    inventory
        .devices
        .iter()
        .filter(|d| taken.insert(d.info.serial.as_str()))
        .map(|d| SeenDevice::from_transport(d, lans))
        .collect()
}

// ── An MX's address comes from its network's LAN side (ADR-164 決定 28) ──────────────────────────
//
// An MX reports no `lanIp` (686 of 686 on a real organization); it reports `wan1Ip`, which is in
// nobody's IP ranges, is often dynamic, and filed sites into another site's folder. So its address
// is one of the MX's own VLAN addresses, read per network (the organization-wide listing is beta
// and answered 404), remembered on `meraki_org_networks`, and chosen again on every sync.

/// The product type whose address comes from its network's VLANs rather than from `lanIp`.
const LAN_FROM_VLANS: &str = "appliance";

/// Whether a device of this product type takes its address from its network's VLANs (決定 28).
#[must_use]
pub fn takes_lan_from_vlans(product_type: &str) -> bool {
    product_type == LAN_FROM_VLANS
}

/// How long a network's LAN addresses stand before a sync reads them again. VLANs are configuration
/// and move a few times a year, and an organization of 350 networks is 350 requests a round.
pub const LAN_REFRESH: chrono::Duration = chrono::Duration::hours(24);

/// What a network last said about its MX's LAN side.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkLan {
    /// Usable addresses in the order [`choose_lan_address`] reads them ([`lan_order`]). Empty for a
    /// network with no LAN side at all — which is an answer, not a network still to be read.
    pub ips: Vec<IpAddr>,
    pub read_at: DateTime<Utc>,
}

/// Per network whose LAN side has been read, the address its MX takes (`None` when it has no LAN
/// address). A network that has never been read is absent.
pub type LanAddresses = HashMap<String, Option<IpAddr>>;

/// A network's addresses in the order the choice reads them: by VLAN number, a VLAN whose number
/// could not be read after every numbered one; an unusable address dropped; each address once.
#[must_use]
pub fn lan_order(addrs: &[MerakiLanAddress]) -> Vec<IpAddr> {
    let mut sorted: Vec<&MerakiLanAddress> = addrs.iter().collect();
    // Stable, so two VLANs with no readable number keep the Dashboard's order.
    sorted.sort_by_key(|a| (a.vlan_id.is_none(), a.vlan_id));
    let mut seen = HashSet::new();
    sorted
        .into_iter()
        .filter_map(|a| usable_address(&a.appliance_ip))
        .filter(|ip| seen.insert(*ip))
        .collect()
}

/// Which of the organization's MX networks this sync should read, most needed first: every network
/// never read (in id order — a network's MX is not imported until it has been, see
/// [`DeviceRecord::lan_pending`]), then every one read longer than [`LAN_REFRESH`] ago, oldest
/// first. The caller reads as many as its budget allows; the rest wait for the next sync.
#[must_use]
pub fn lan_reads_due(
    mx_networks: &BTreeSet<String>,
    stored: &HashMap<String, NetworkLan>,
    now: DateTime<Utc>,
) -> Vec<String> {
    let mut stale: Vec<(DateTime<Utc>, &String)> = Vec::new();
    let mut due: Vec<String> = Vec::new();
    for network in mx_networks {
        match stored.get(network) {
            None => due.push(network.clone()),
            Some(lan) if now - lan.read_at >= LAN_REFRESH => stale.push((lan.read_at, network)),
            Some(_) => {}
        }
    }
    stale.sort();
    due.extend(stale.into_iter().map(|(_, n)| n.clone()));
    due
}

/// The address an MX takes from its network's LAN side: the first of `ips` (they are in VLAN order)
/// that lies inside some folder's IP range, else the first. `None` when the network has none.
///
/// Why not simply the lowest VLAN: on a real organization it was outside the corporate ranges in 29
/// networks of 342 — a VLAN 1 left at "Default" or a guest VLAN, and **the same subnet reused by up to
/// eight sites**, so the lowest VLAN alone would give eight sites' MX one address and could file
/// them into whichever folder claims it. In every one of the 29 the next VLAN or the one after was
/// inside the ranges. The ranges are the operator's own statement of which addresses belong to a
/// site, so they decide.
#[must_use]
pub fn choose_lan_address(ips: &[IpAddr], in_a_range: &HashSet<IpAddr>) -> Option<IpAddr> {
    ips.iter()
        .find(|ip| in_a_range.contains(ip))
        .or_else(|| ips.first())
        .copied()
}

/// [`choose_lan_address`] for every network whose LAN side has been read.
#[must_use]
pub fn lan_addresses(
    lans: &HashMap<String, NetworkLan>,
    in_a_range: &HashSet<IpAddr>,
) -> LanAddresses {
    lans.iter()
        .map(|(network, lan)| (network.clone(), choose_lan_address(&lan.ips, in_a_range)))
        .collect()
}

/// The networks in `inventory` that hold at least one device whose address comes from its VLANs.
#[must_use]
pub fn networks_with_an_mx(inventory: &MerakiInventory) -> BTreeSet<String> {
    inventory
        .devices
        .iter()
        .filter(|d| takes_lan_from_vlans(&d.info.product_type) && !d.info.network_id.is_empty())
        .map(|d| d.info.network_id.clone())
        .collect()
}

/// Parse an address Meraki reported, keeping it only if it can identify a device.
///
/// The unspecified address is dropped: the importer writes `0.0.0.0` on a node that has none, and
/// feeding that to the IP-range match would file every address-less device into whichever folder
/// happens to hold `0.0.0.0/0`.
#[must_use]
pub fn usable_address(text: &str) -> Option<IpAddr> {
    let addr: IpAddr = text.trim().parse().ok()?;
    (!addr.is_unspecified()).then_some(addr)
}

/// A stored row, as the planner needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredDevice {
    pub serial: String,
    pub name: String,
    pub model: Option<String>,
    pub product_type: String,
    pub network_id: String,
    pub lan_ip: Option<IpAddr>,
    pub first_online_at: Option<DateTime<Utc>>,
    pub missing_since: Option<DateTime<Utc>>,
    pub imported_at: Option<DateTime<Utc>>,
}

/// One row the sync writes. Carries every descriptive column, because the statement is an upsert;
/// the two flags are the transitions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceWrite {
    pub device: SeenDevice,
    /// Stamp `first_online_at` now. Only ever true for a row that has none.
    pub first_online: bool,
    /// Backfill `imported_at` with the moment the device's node was bound. Only ever set for a row
    /// that has none — a device imported before this table existed (ADR-164 決定 6).
    pub imported_at: Option<DateTime<Utc>>,
}

/// How many rows one inventory statement carries. The rows travel as eight arrays, so the bind
/// count does not grow with it; the chunk only bounds how much one statement holds in memory for
/// an organization at the 50,000-device cap.
const WRITE_CHUNK: usize = 5_000;

/// The writes of a plan with one entry per serial — the first, which is the rule
/// [`seen_devices`] already applies to a listing.
///
/// 🚨 Not tidiness: the writes go out as one `INSERT … ON CONFLICT DO UPDATE` per chunk, and
/// PostgreSQL refuses a statement that would touch the same row twice. [`plan_sync`] is handed a
/// de-duplicated listing by the sync, but it is a `pub fn` over a slice, and a plan built any other
/// way must cost a repeated row rather than the whole sync.
#[must_use]
pub fn unique_writes(writes: &[DeviceWrite]) -> Vec<&DeviceWrite> {
    let mut taken: HashSet<&str> = HashSet::new();
    writes
        .iter()
        .filter(|w| taken.insert(w.device.serial.as_str()))
        .collect()
}

/// One column of a chunk of writes, as the array the statement unnests.
fn column<'a, T>(rows: &[&'a DeviceWrite], of: impl Fn(&'a DeviceWrite) -> T) -> Vec<T> {
    rows.iter().copied().map(of).collect()
}

/// The name a device's node is given: Meraki's, or the serial when Meraki has none.
///
/// One function for the importer and for [`plan_follow`], and that is the point. A rename is
/// recognised by comparing the node's name with the name the *previous* Meraki name produced; two
/// spellings of "a blank name becomes the serial" would make every unnamed device look renamed by
/// an operator, permanently, and nothing would say so.
#[must_use]
pub fn node_name_for(meraki_name: &str, serial: &str) -> String {
    if meraki_name.trim().is_empty() {
        serial.to_owned()
    } else {
        meraki_name.to_owned()
    }
}

/// A device's node, as the planner needs it (ADR-164 決定 14): when it was bound, and what the node
/// and its binding say **now**.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundNode {
    pub bound_at: DateTime<Utc>,
    /// The node's current name. An operator can change it, which is what a rename must respect.
    pub name: String,
    /// The node's current address — `0.0.0.0` on a node imported while Meraki reported none.
    /// Nothing but this sync can change it: no screen edits a node's address.
    pub address: IpAddr,
    /// The network the binding names. Written once at import, so it goes stale when a device moves.
    pub network_id: String,
}

/// What one imported node takes from Meraki in this sync. At least one field is set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeFollow {
    pub serial: String,
    /// `(from, to)`: rename the node to `to`, **only while it is still called `from`**. The
    /// statement repeats that condition, because an operator can rename the node between the read
    /// this plan was made from and the write.
    pub rename: Option<(String, String)>,
    /// The address to give the node.
    pub address: Option<IpAddr>,
    /// The network to record on the binding.
    pub network_id: Option<String>,
}

/// Decide what a node follows, from the stored row, what this listing says, and the node as it
/// stands. Pure.
///
/// The three are decided differently on purpose:
/// - **The address** is compared with the node's *current* one, so a node that drifted before this
///   existed — or was imported at `0.0.0.0` — is put right by the first sync. An address Meraki
///   does not report moves nothing: a device that is down must not turn a good address into none.
/// - **The name** follows a *change* on Meraki's side, and only while the node still carries the
///   name the previous Meraki name gave it. A node whose name differs for any other reason was
///   renamed by a person, or drifted before this existed; the two cannot be told apart, so neither
///   is touched.
/// - **The binding's network** is a copy with no other writer, so it is simply kept current.
///
/// The folder is never part of it (決定 6).
#[must_use]
pub fn plan_follow(
    row: Option<&StoredDevice>,
    device: &SeenDevice,
    node: &BoundNode,
) -> Option<NodeFollow> {
    let rename = row.and_then(|r| {
        let from = node_name_for(&r.name, &device.serial);
        let to = node_name_for(&device.name, &device.serial);
        (from != to && node.name == from).then_some((from, to))
    });
    let address = device.lan_ip.filter(|ip| *ip != node.address);
    let network_id = (!device.network_id.is_empty() && device.network_id != node.network_id)
        .then(|| device.network_id.clone());
    (rename.is_some() || address.is_some() || network_id.is_some()).then(|| NodeFollow {
        serial: device.serial.clone(),
        rename,
        address,
        network_id,
    })
}

/// What one sync writes. Empty when nothing changed, which is the ordinary case.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SyncPlan {
    /// New rows, rows whose description changed, and rows crossing a transition.
    pub writes: Vec<DeviceWrite>,
    /// Stored serials the listing no longer contains and that are not already marked.
    pub newly_missing: Vec<String>,
    /// What imported nodes take from this listing (決定 14).
    pub follows: Vec<NodeFollow>,
}

impl SyncPlan {
    /// Whether the sync has anything to write.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.writes.is_empty() && self.newly_missing.is_empty() && self.follows.is_empty()
    }
}

/// What [`MerakiInventoryRepo::apply`] changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Applied {
    /// Inventory rows touched.
    pub rows: u64,
    /// Nodes that took something from Meraki — counted once each, and only when a statement
    /// actually changed a row. Zero is what keeps the configuration generation still.
    pub followed: u32,
}

/// Decide what a sync writes, from what is stored, what a **complete** listing contained, and which
/// serials have a node (`bound`: serial → the node as it stands).
///
/// Pure. A device is written when it is new, when anything Meraki says about it changed, when it is
/// online for the first time, when it has come back from missing, or when it has a node and no
/// `imported_at` yet. Everything else is left alone — including a device that is merely *offline
/// again*: `online` is a fact about this listing, not a column. A device with a node also gets a
/// [`NodeFollow`] when [`plan_follow`] finds one.
#[must_use]
pub fn plan_sync(
    stored: &[StoredDevice],
    seen: &[SeenDevice],
    bound: &HashMap<String, BoundNode>,
) -> SyncPlan {
    let by_serial: HashMap<&str, &StoredDevice> =
        stored.iter().map(|r| (r.serial.as_str(), r)).collect();
    let mut plan = SyncPlan::default();

    for device in seen {
        let row = by_serial.get(device.serial.as_str()).copied();
        let node = bound.get(&device.serial);
        let first_online = device.online && row.is_none_or(|r| r.first_online_at.is_none());
        let imported_at = match row {
            Some(r) if r.imported_at.is_some() => None,
            _ => node.map(|n| n.bound_at),
        };
        plan.follows
            .extend(node.and_then(|n| plan_follow(row, device, n)));
        let described_the_same = row.is_some_and(|r| {
            r.name == device.name
                && r.model == device.model
                && r.product_type == device.product_type
                && r.network_id == device.network_id
                && r.lan_ip == device.lan_ip
        });
        let came_back = row.is_some_and(|r| r.missing_since.is_some());
        if !described_the_same || first_online || came_back || imported_at.is_some() {
            plan.writes.push(DeviceWrite {
                device: device.clone(),
                first_online,
                imported_at,
            });
        }
    }

    let listed: HashSet<&str> = seen.iter().map(|d| d.serial.as_str()).collect();
    plan.newly_missing = stored
        .iter()
        .filter(|r| r.missing_since.is_none() && !listed.contains(r.serial.as_str()))
        .map(|r| r.serial.clone())
        .collect();
    plan
}

/// What a device's row is shown as: read from the facts the inventory keeps about it (ADR-164
/// 決定 3), never stored. Serialized as the snake_case token.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize, utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum MerakiDeviceState {
    /// It has a node, and Meraki still lists it.
    Monitored,
    /// Meraki lists it, it has been online, and it has never been a node here.
    New,
    /// Meraki lists it and has never reported it online — registered, not yet connected.
    NeverOnline,
    /// It was a node here and an operator deleted that node. Automatic import leaves it alone.
    Deleted,
    /// It has a node, and Meraki no longer lists it. The node and its alerts are left as they are:
    /// a device missing from a listing is not evidence that it recovered (ADR-156 決定 3).
    Missing,
}

#[cfg(test)]
impl MerakiDeviceState {
    /// Every state. Test-only: the state is never stored, so production has no token to parse and
    /// nothing to iterate — serde is the only spelling, and a test pins what it produces.
    const ALL: [Self; 5] = [
        Self::Monitored,
        Self::New,
        Self::NeverOnline,
        Self::Deleted,
        Self::Missing,
    ];
}

/// The four facts a device's state is read from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeviceFacts {
    pub has_node: bool,
    pub missing: bool,
    pub was_imported: bool,
    pub seen_online: bool,
}

/// The one reading of a device's state, shared by the device list and the organization's counts.
///
/// `None` means the row is not shown at all: Meraki no longer lists the device and nothing here
/// depends on it. The row is kept — if the device comes back, what was known about it still is.
///
/// Order matters and is the point: having a node outranks everything, because a monitored device
/// that has gone missing is the one case an operator has to be told about.
#[must_use]
pub fn classify(f: DeviceFacts) -> Option<MerakiDeviceState> {
    match (f.has_node, f.missing) {
        (true, true) => Some(MerakiDeviceState::Missing),
        (true, false) => Some(MerakiDeviceState::Monitored),
        (false, true) => None,
        (false, false) if f.was_imported => Some(MerakiDeviceState::Deleted),
        (false, false) if !f.seen_online => Some(MerakiDeviceState::NeverOnline),
        (false, false) => Some(MerakiDeviceState::New),
    }
}

/// How many of an organization's devices are in each state an operator is told about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, utoipa::ToSchema)]
pub struct MerakiDeviceCounts {
    /// Devices Meraki currently lists, whatever their state here.
    pub seen: u32,
    /// Of those, the ones that are nodes.
    pub monitored: u32,
    /// Listed, online at least once, never a node here.
    pub new: u32,
    /// Nodes whose device Meraki no longer lists.
    pub missing: u32,
    /// Of `monitored`, the ones in a network this organization does not watch (決定 15). Collection
    /// asks the Dashboard about watched networks only, so **nothing is collected for these**: the
    /// node keeps the last state it was seen in and raises nothing. It happens when a device is
    /// moved into an unwatched network, and when a network holding nodes is un-watched. An
    /// organization that watches no network at all is sent no collect, so there it is every
    /// monitored device (決定 16).
    pub monitored_unwatched: u32,
}

impl MerakiDeviceCounts {
    /// Fold one device in. `network_watched` is whether the device's network is one the
    /// organization watches — `false` for a network the sync has not recorded.
    fn add(&mut self, state: MerakiDeviceState, network_watched: bool) {
        match state {
            MerakiDeviceState::Monitored => {
                self.seen += 1;
                self.monitored += 1;
                if !network_watched {
                    self.monitored_unwatched += 1;
                }
            }
            MerakiDeviceState::New => {
                self.seen += 1;
                self.new += 1;
            }
            MerakiDeviceState::NeverOnline | MerakiDeviceState::Deleted => self.seen += 1,
            MerakiDeviceState::Missing => self.missing += 1,
        }
    }
}

/// One device as the API lists it: the stored row, its state, and what it is bound to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceRecord {
    pub serial: String,
    pub name: String,
    pub model: Option<String>,
    pub product_type: String,
    pub network_id: String,
    /// The network's name, when the sync has seen that network.
    pub network_name: Option<String>,
    /// Whether the device's network is one the organization watches. `false` for a network the sync
    /// has not recorded.
    pub network_monitored: bool,
    pub lan_ip: Option<IpAddr>,
    /// An MX whose network's LAN side has never been read (決定 28): its address is not known yet,
    /// so the sync does not import it — an import files the node once and never moves it (決定 6).
    pub lan_pending: bool,
    pub state: MerakiDeviceState,
    pub node_id: Option<Uuid>,
    /// The folder the device's node is filed in. `None` without a node, and for a node at the top
    /// of the tree.
    pub node_group_id: Option<Uuid>,
    pub first_seen_at: DateTime<Utc>,
    pub missing_since: Option<DateTime<Utc>>,
    /// The MX's configured warm-spare role, when the sync has read one (ADR-164 決定 26).
    pub ha_role: Option<MerakiHaRole>,
}

/// One MX's warm-spare pair as the inventory records it (ADR-164 決定 26).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HaPair {
    /// This MX's configured role.
    pub role: MerakiHaRole,
    /// The other MX of its network — `None` when there is none, or more than one and so no telling
    /// which is the pair.
    pub partner: Option<HaPartner>,
}

/// The other MX of a pair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HaPartner {
    pub serial: String,
    pub name: String,
    pub role: Option<MerakiHaRole>,
    /// Its node, when it has been imported.
    pub node_id: Option<Uuid>,
}

/// PostgreSQL-backed store for `meraki_inventory`.
pub struct MerakiInventoryRepo {
    pool: PgPool,
}

impl MerakiInventoryRepo {
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// An organization's stored rows, for the planner.
    pub async fn stored(&self, org: Uuid) -> anyhow::Result<Vec<StoredDevice>> {
        let rows = sqlx::query(
            "SELECT serial, name, model, product_type, network_id, lan_ip, first_online_at, \
                    missing_since, imported_at \
             FROM meraki_inventory WHERE org_id = $1",
        )
        .bind(org)
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|r| {
                let lan_ip: Option<String> = r.try_get("lan_ip")?;
                Ok(StoredDevice {
                    serial: r.try_get("serial")?,
                    name: r.try_get("name")?,
                    model: r.try_get("model")?,
                    product_type: r.try_get("product_type")?,
                    network_id: r.try_get("network_id")?,
                    lan_ip: lan_ip.as_deref().and_then(usable_address),
                    first_online_at: r.try_get("first_online_at")?,
                    missing_since: r.try_get("missing_since")?,
                    imported_at: r.try_get("imported_at")?,
                })
            })
            .collect()
    }

    /// Which of an organization's serials have a node: when each was bound, and what the node and
    /// the binding say now — what [`plan_follow`] compares Meraki's answer with.
    ///
    /// `host(address)`, not `address::TEXT`: the cast appends the netmask (`10.0.0.1/32`), which
    /// does not parse as an address.
    pub async fn bound(&self, org: Uuid) -> anyhow::Result<HashMap<String, BoundNode>> {
        let rows = sqlx::query(
            "SELECT d.serial, d.created_at, d.network_id, n.name, host(n.address) AS address \
             FROM meraki_devices d JOIN nodes n ON n.id = d.node_id \
             WHERE d.org_id = $1",
        )
        .bind(org)
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|r| {
                let address: String = r.try_get("address")?;
                Ok((
                    r.try_get("serial")?,
                    BoundNode {
                        bound_at: r.try_get("created_at")?,
                        name: r.try_get("name")?,
                        address: address.parse()?,
                        network_id: r.try_get("network_id")?,
                    },
                ))
            })
            .collect()
    }

    /// Write one sync's plan, atomically.
    ///
    /// 🚨 The `COALESCE`s are not decoration. The planner only asks for `first_online_at` or
    /// `imported_at` on a row that has none, but two syncs of one organization can plan from the
    /// same snapshot (a manual one beside the periodic one on another core) — and the later write
    /// must not move a timestamp the earlier one set.
    ///
    /// 🚨 **The follows commit with the rows, in this transaction, and that is load-bearing.** A
    /// rename is planned from the difference between the stored name and the listed one. Once the
    /// row carries the new name that difference is gone — so a node update written separately, and
    /// lost, would never be planned again. Each statement also repeats its own condition (the
    /// rename is a compare-and-swap on the old name, the other two are `IS DISTINCT FROM`), which
    /// is what makes two syncs from one snapshot, or an operator's rename in between, harmless.
    pub async fn apply(&self, org: Uuid, plan: &SyncPlan) -> anyhow::Result<Applied> {
        if plan.is_empty() {
            return Ok(Applied::default());
        }
        let mut touched = 0u64;
        let mut tx = self.pool.begin().await?;
        // One statement per chunk, not one per row: an organization's first sync writes every
        // device it holds, and a row at a time that was thousands of round trips inside this
        // transaction (the shape `record_networks` already has).
        for rows in unique_writes(&plan.writes).chunks(WRITE_CHUNK) {
            let lan_ips: Vec<Option<String>> = rows
                .iter()
                .map(|w| w.device.lan_ip.map(|a| a.to_string()))
                .collect();
            touched += sqlx::query(
                "INSERT INTO meraki_inventory \
                   (org_id, serial, name, model, product_type, network_id, lan_ip, \
                    first_online_at, imported_at) \
                 SELECT $1, w.serial, w.name, w.model, w.product_type, w.network_id, w.lan_ip, \
                        CASE WHEN w.first_online THEN now() END, w.imported_at \
                 FROM unnest($2::text[], $3::text[], $4::text[], $5::text[], $6::text[], \
                             $7::text[], $8::bool[], $9::timestamptz[]) \
                   AS w(serial, name, model, product_type, network_id, lan_ip, \
                        first_online, imported_at) \
                 ON CONFLICT (org_id, serial) DO UPDATE SET \
                   name = EXCLUDED.name, model = EXCLUDED.model, \
                   product_type = EXCLUDED.product_type, network_id = EXCLUDED.network_id, \
                   lan_ip = EXCLUDED.lan_ip, \
                   first_online_at = \
                     COALESCE(meraki_inventory.first_online_at, EXCLUDED.first_online_at), \
                   imported_at = COALESCE(meraki_inventory.imported_at, EXCLUDED.imported_at), \
                   missing_since = NULL",
            )
            .bind(org)
            .bind(column(rows, |w| w.device.serial.as_str()))
            .bind(column(rows, |w| w.device.name.as_str()))
            .bind(column(rows, |w| w.device.model.as_deref()))
            .bind(column(rows, |w| w.device.product_type.as_str()))
            .bind(column(rows, |w| w.device.network_id.as_str()))
            .bind(lan_ips)
            .bind(column(rows, |w| w.first_online))
            .bind(column(rows, |w| w.imported_at))
            .execute(&mut *tx)
            .await?
            .rows_affected();
        }
        if !plan.newly_missing.is_empty() {
            touched += sqlx::query(
                "UPDATE meraki_inventory SET missing_since = now() \
                 WHERE org_id = $1 AND serial = ANY($2) AND missing_since IS NULL",
            )
            .bind(org)
            .bind(&plan.newly_missing)
            .execute(&mut *tx)
            .await?
            .rows_affected();
        }

        let mut followed = 0u32;
        for f in &plan.follows {
            let mut changed = false;
            if let Some((from, to)) = &f.rename {
                changed |= sqlx::query(
                    "UPDATE nodes SET name = $3, updated_at = now() \
                     FROM meraki_devices d \
                     WHERE d.org_id = $1 AND d.serial = $2 AND nodes.id = d.node_id \
                       AND nodes.name = $4",
                )
                .bind(org)
                .bind(&f.serial)
                .bind(to)
                .bind(from)
                .execute(&mut *tx)
                .await?
                .rows_affected()
                    > 0;
            }
            if let Some(address) = f.address {
                changed |= sqlx::query(
                    "UPDATE nodes SET address = $3::inet, updated_at = now() \
                     FROM meraki_devices d \
                     WHERE d.org_id = $1 AND d.serial = $2 AND nodes.id = d.node_id \
                       AND nodes.address IS DISTINCT FROM $3::inet",
                )
                .bind(org)
                .bind(&f.serial)
                .bind(address.to_string())
                .execute(&mut *tx)
                .await?
                .rows_affected()
                    > 0;
            }
            if let Some(network_id) = &f.network_id {
                changed |= sqlx::query(
                    "UPDATE meraki_devices SET network_id = $3, updated_at = now() \
                     WHERE org_id = $1 AND serial = $2 AND network_id IS DISTINCT FROM $3",
                )
                .bind(org)
                .bind(&f.serial)
                .bind(network_id)
                .execute(&mut *tx)
                .await?
                .rows_affected()
                    > 0;
            }
            followed += u32::from(changed);
        }
        tx.commit().await?;
        Ok(Applied {
            rows: touched,
            followed,
        })
    }

    /// An organization's devices, as the API lists them. Rows [`classify`] declines are left out.
    pub async fn devices(&self, org: Uuid) -> anyhow::Result<Vec<DeviceRecord>> {
        let rows = sqlx::query(
            "SELECT i.serial, i.name, i.model, i.product_type, i.network_id, i.lan_ip, \
                    i.first_seen_at, i.first_online_at, i.missing_since, i.imported_at, i.ha_role, \
                    d.node_id, nd.group_id AS node_group_id, n.name AS network_name, \
                    COALESCE(n.monitored, false) AS monitored, \
                    n.lan_read_at IS NULL AS lan_unread \
             FROM meraki_inventory i \
             LEFT JOIN meraki_devices d ON d.org_id = i.org_id AND d.serial = i.serial \
             LEFT JOIN nodes nd ON nd.id = d.node_id \
             LEFT JOIN meraki_org_networks n \
                    ON n.org_id = i.org_id AND n.network_id = i.network_id \
             WHERE i.org_id = $1 \
             ORDER BY i.name, i.serial",
        )
        .bind(org)
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for r in &rows {
            let node_id: Option<Uuid> = r.try_get("node_id")?;
            let missing_since: Option<DateTime<Utc>> = r.try_get("missing_since")?;
            let Some(state) = classify(facts_of(r, node_id.is_some(), missing_since.is_some())?)
            else {
                continue;
            };
            let lan_ip: Option<String> = r.try_get("lan_ip")?;
            let product_type: String = r.try_get("product_type")?;
            // NULL through the LEFT JOIN too: a network the sync has not recorded is not read.
            let lan_unread: bool = r.try_get("lan_unread")?;
            out.push(DeviceRecord {
                serial: r.try_get("serial")?,
                name: r.try_get("name")?,
                model: r.try_get("model")?,
                lan_pending: takes_lan_from_vlans(&product_type) && lan_unread,
                product_type,
                network_id: r.try_get("network_id")?,
                network_name: r.try_get("network_name")?,
                network_monitored: r.try_get("monitored")?,
                lan_ip: lan_ip.as_deref().and_then(usable_address),
                state,
                node_id,
                node_group_id: r.try_get("node_group_id")?,
                first_seen_at: r.try_get("first_seen_at")?,
                missing_since,
                ha_role: r
                    .try_get::<Option<String>, _>("ha_role")?
                    .as_deref()
                    .and_then(MerakiHaRole::from_token),
            });
        }
        Ok(out)
    }

    /// Record each MX's warm-spare role (ADR-164 決定 26): the rows whose role changed. Only rows
    /// that exist are written — so this runs after [`Self::apply`], which creates new devices' rows —
    /// and a device the list does not name keeps what it has.
    ///
    /// Outside `apply`'s one transaction on purpose. That transaction exists because a rename is
    /// planned from a stored-vs-listed difference the same write erases; a role has no such
    /// follow-up, and nothing else reads it back within the sync.
    pub async fn record_ha_roles(
        &self,
        org: Uuid,
        roles: &[(String, Option<MerakiHaRole>)],
    ) -> anyhow::Result<u64> {
        if roles.is_empty() {
            return Ok(0);
        }
        let serials: Vec<String> = roles.iter().map(|(s, _)| s.clone()).collect();
        let tokens: Vec<Option<String>> = roles
            .iter()
            .map(|(_, r)| r.map(|r| r.as_str().to_owned()))
            .collect();
        let done = sqlx::query(
            "UPDATE meraki_inventory i SET ha_role = r.role \
             FROM unnest($2::text[], $3::text[]) AS r(serial, role) \
             WHERE i.org_id = $1 AND i.serial = r.serial AND i.ha_role IS DISTINCT FROM r.role",
        )
        .bind(org)
        .bind(&serials)
        .bind(&tokens)
        .execute(&self.pool)
        .await?;
        Ok(done.rows_affected())
    }

    /// `serial`'s warm-spare pair (ADR-164 決定 26), or `None` when it holds no role — it is not an
    /// MX, its warm spare is not enabled, or no role has been read yet.
    ///
    /// The partner is the other MX of the same network that Meraki still lists. Measured on a real
    /// organization: every pair was exactly two MX in one network (336 pairs, 14 single MX). More
    /// than one candidate is not a pair anyone can name, and reads as no partner.
    pub async fn ha_pair(&self, org: Uuid, serial: &str) -> anyhow::Result<Option<HaPair>> {
        let rows = sqlx::query(
            "SELECT i.ha_role AS own_role, p.serial AS p_serial, p.name AS p_name, \
                    p.ha_role AS p_role, d.node_id AS p_node \
             FROM meraki_inventory i \
             LEFT JOIN meraki_inventory p \
                    ON p.org_id = i.org_id AND p.network_id = i.network_id \
                   AND p.serial <> i.serial AND p.product_type = 'appliance' \
                   AND p.missing_since IS NULL \
             LEFT JOIN meraki_devices d ON d.org_id = p.org_id AND d.serial = p.serial \
             WHERE i.org_id = $1 AND i.serial = $2",
        )
        .bind(org)
        .bind(serial)
        .fetch_all(&self.pool)
        .await?;
        let Some(first) = rows.first() else {
            return Ok(None);
        };
        let Some(role) = first
            .try_get::<Option<String>, _>("own_role")?
            .as_deref()
            .and_then(MerakiHaRole::from_token)
        else {
            return Ok(None);
        };
        let mut partners = Vec::new();
        for r in &rows {
            let Some(p_serial) = r.try_get::<Option<String>, _>("p_serial")? else {
                continue;
            };
            partners.push(HaPartner {
                serial: p_serial,
                name: r
                    .try_get::<Option<String>, _>("p_name")?
                    .unwrap_or_default(),
                role: r
                    .try_get::<Option<String>, _>("p_role")?
                    .as_deref()
                    .and_then(MerakiHaRole::from_token),
                node_id: r.try_get("p_node")?,
            });
        }
        let partner = if partners.len() == 1 {
            partners.pop()
        } else {
            None
        };
        Ok(Some(HaPair { role, partner }))
    }

    /// Every organization's counts, for the organization list. An organization with no rows yet is
    /// absent from the map — the caller reads that as all-zero.
    ///
    /// Folded here through [`classify`] rather than counted in SQL with `FILTER` clauses: those
    /// would be a second spelling of the same five-way rule, and the list and its own counts
    /// disagreeing is exactly what two spellings produce.
    pub async fn counts(&self) -> anyhow::Result<HashMap<Uuid, MerakiDeviceCounts>> {
        // The network join is the one [`Self::devices`] makes, spelled the same way on purpose:
        // `monitored_unwatched` has to be the number of rows that list marks "not watched".
        let rows = sqlx::query(
            "SELECT i.org_id, i.first_online_at, i.missing_since, i.imported_at, d.node_id, \
                    COALESCE(n.monitored, false) AS monitored \
             FROM meraki_inventory i \
             LEFT JOIN meraki_devices d ON d.org_id = i.org_id AND d.serial = i.serial \
             LEFT JOIN meraki_org_networks n \
                    ON n.org_id = i.org_id AND n.network_id = i.network_id",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut out: HashMap<Uuid, MerakiDeviceCounts> = HashMap::new();
        for r in &rows {
            let node_id: Option<Uuid> = r.try_get("node_id")?;
            let missing_since: Option<DateTime<Utc>> = r.try_get("missing_since")?;
            if let Some(state) = classify(facts_of(r, node_id.is_some(), missing_since.is_some())?)
            {
                out.entry(r.try_get("org_id")?)
                    .or_default()
                    .add(state, r.try_get("monitored")?);
            }
        }
        Ok(out)
    }
}

/// Read a row's facts. `has_node` and `missing` are passed in because both callers need the
/// underlying values as well.
fn facts_of(
    r: &sqlx::postgres::PgRow,
    has_node: bool,
    missing: bool,
) -> anyhow::Result<DeviceFacts> {
    let imported_at: Option<DateTime<Utc>> = r.try_get("imported_at")?;
    let first_online_at: Option<DateTime<Utc>> = r.try_get("first_online_at")?;
    Ok(DeviceFacts {
        has_node,
        missing,
        was_imported: imported_at.is_some(),
        seen_online: first_online_at.is_some(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use yagra_transport::{MerakiAvailability, MerakiDeviceInfo};

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_800_000_000 + secs, 0).expect("in range")
    }

    fn seen(serial: &str, online: bool) -> SeenDevice {
        SeenDevice {
            serial: serial.into(),
            name: format!("dev-{serial}"),
            model: Some("MX67".into()),
            product_type: "appliance".into(),
            network_id: "N_1".into(),
            lan_ip: Some("10.0.0.1".parse().expect("ip")),
            online,
        }
    }

    /// ADR-164 Inc.11. The writes go out as one upsert per chunk, and PostgreSQL refuses an upsert
    /// that names a row twice — so a repeated serial keeps its first entry, in the order given.
    #[test]
    fn a_serial_planned_twice_is_written_once_and_the_first_entry_wins() {
        let write = |serial: &str, name: &str| DeviceWrite {
            device: SeenDevice {
                name: name.into(),
                ..seen(serial, true)
            },
            first_online: false,
            imported_at: None,
        };
        let writes = [
            write("Q2-A", "first"),
            write("Q2-B", "only"),
            write("Q2-A", "second"),
        ];
        let unique: Vec<(&str, &str)> = unique_writes(&writes)
            .into_iter()
            .map(|w| (w.device.serial.as_str(), w.device.name.as_str()))
            .collect();
        assert_eq!(unique, [("Q2-A", "first"), ("Q2-B", "only")]);

        // One column of those rows is the values in row order — what the statement unnests.
        let rows = unique_writes(&writes);
        assert_eq!(column(&rows, |w| w.device.name.as_str()), ["first", "only"]);
    }

    /// The row a sync of `d` leaves behind.
    fn stored_as(d: &SeenDevice) -> StoredDevice {
        StoredDevice {
            serial: d.serial.clone(),
            name: d.name.clone(),
            model: d.model.clone(),
            product_type: d.product_type.clone(),
            network_id: d.network_id.clone(),
            lan_ip: d.lan_ip,
            first_online_at: d.online.then(|| at(0)),
            missing_since: None,
            imported_at: None,
        }
    }

    fn nobody() -> HashMap<String, BoundNode> {
        HashMap::new()
    }

    /// The node an import of `d` leaves behind: named, addressed and bound as the importer does it.
    fn node_of(d: &SeenDevice) -> BoundNode {
        BoundNode {
            bound_at: at(-3600),
            name: node_name_for(&d.name, &d.serial),
            address: d
                .lan_ip
                .unwrap_or(IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED)),
            network_id: d.network_id.clone(),
        }
    }

    /// `d`'s row after the sync that recorded its node.
    fn imported_as(d: &SeenDevice) -> StoredDevice {
        StoredDevice {
            imported_at: Some(at(-3600)),
            ..stored_as(d)
        }
    }

    fn bound_as(d: &SeenDevice, node: BoundNode) -> HashMap<String, BoundNode> {
        HashMap::from([(d.serial.clone(), node)])
    }

    /// ADR-164 決定 4. The sync runs every five minutes; the ordinary sync finds what it found last
    /// time and must write nothing at all.
    #[test]
    fn a_sync_that_finds_nothing_changed_writes_nothing() {
        let devices = [seen("Q2-A", true), seen("Q2-B", false)];
        let stored: Vec<StoredDevice> = devices.iter().map(stored_as).collect();
        assert!(plan_sync(&stored, &devices, &nobody()).is_empty());
    }

    /// …and a device going offline again is not a change. `online` is a fact about one listing,
    /// not a column, so a flapping device must not turn into a write every five minutes.
    #[test]
    fn a_device_that_is_offline_again_is_not_a_change() {
        let was_online = stored_as(&seen("Q2-A", true));
        assert!(plan_sync(&[was_online], &[seen("Q2-A", false)], &nobody()).is_empty());
    }

    #[test]
    fn a_new_device_is_written_and_stamped_online_only_if_it_is() {
        let plan = plan_sync(&[], &[seen("Q2-A", true), seen("Q2-B", false)], &nobody());
        let flags: Vec<(&str, bool)> = plan
            .writes
            .iter()
            .map(|w| (w.device.serial.as_str(), w.first_online))
            .collect();
        assert_eq!(flags, [("Q2-A", true), ("Q2-B", false)]);
        assert!(plan.newly_missing.is_empty());
    }

    #[test]
    fn first_online_is_stamped_once_the_first_time_the_device_is_up() {
        let never = stored_as(&seen("Q2-A", false));
        let plan = plan_sync(
            std::slice::from_ref(&never),
            &[seen("Q2-A", true)],
            &nobody(),
        );
        assert_eq!(plan.writes.len(), 1);
        assert!(plan.writes[0].first_online);

        // Still offline: nothing to say yet.
        assert!(plan_sync(&[never], &[seen("Q2-A", false)], &nobody()).is_empty());
    }

    #[test]
    fn a_changed_description_is_written_without_restamping_online() {
        let mut row = stored_as(&seen("Q2-A", true));
        row.name = "old name".into();
        let plan = plan_sync(&[row], &[seen("Q2-A", true)], &nobody());
        assert_eq!(plan.writes.len(), 1);
        assert!(
            !plan.writes[0].first_online,
            "it already has a first_online_at"
        );
    }

    /// Both directions of "missing". Marked once when it disappears — not again on every later
    /// sync — and written again when it comes back, which is what clears the mark.
    #[test]
    fn missing_is_marked_once_and_cleared_by_coming_back() {
        let a = seen("Q2-A", true);
        let mut row = stored_as(&a);

        let gone = plan_sync(std::slice::from_ref(&row), &[], &nobody());
        assert_eq!(gone.newly_missing, ["Q2-A"]);
        assert!(gone.writes.is_empty());

        row.missing_since = Some(at(60));
        assert!(
            plan_sync(std::slice::from_ref(&row), &[], &nobody()).is_empty(),
            "already marked: a second sync must not write it again"
        );

        let back = plan_sync(&[row], std::slice::from_ref(&a), &nobody());
        assert_eq!(
            back.writes.len(),
            1,
            "the write is what clears missing_since"
        );
        assert!(back.newly_missing.is_empty());
    }

    /// The acceptance criterion the plan names: a device seen in this sync is never reported
    /// missing by this sync, whatever was stored.
    #[test]
    fn a_device_the_sync_just_saw_is_never_marked_missing() {
        let devices = [seen("Q2-A", true), seen("Q2-B", false)];
        let mut stored: Vec<StoredDevice> = devices.iter().map(stored_as).collect();
        stored[1].missing_since = Some(at(5));
        stored.push(stored_as(&seen("Q2-GONE", true)));
        let plan = plan_sync(&stored, &devices, &nobody());
        assert_eq!(plan.newly_missing, ["Q2-GONE"]);
    }

    /// ADR-164 決定 6. A device imported before this table existed has a node and no row. Its first
    /// sync must record that it was imported, or deleting the node later would read as "never
    /// imported" and automatic import would put it straight back.
    #[test]
    fn a_device_that_already_has_a_node_is_backfilled_as_imported() {
        let a = seen("Q2-A", true);
        let bound = bound_as(&a, node_of(&a));

        let first = plan_sync(&[], std::slice::from_ref(&a), &bound);
        assert_eq!(first.writes[0].imported_at, Some(at(-3600)));

        // Already recorded: not a reason to write, and never a reason to move it.
        assert!(plan_sync(&[imported_as(&a)], &[a], &bound).is_empty());
    }

    /// ADR-164 決定 14, the rename. Meraki's name changed and the node still carries the name the
    /// old one gave it, so nobody has renamed it and it follows.
    #[test]
    fn a_rename_in_meraki_reaches_a_node_that_still_carries_the_old_name() {
        let was = seen("Q2-A", true);
        let now = SeenDevice {
            name: "lobby-ap".into(),
            ..was.clone()
        };
        let plan = plan_sync(&[imported_as(&was)], &[now], &bound_as(&was, node_of(&was)));
        assert_eq!(
            plan.follows,
            [NodeFollow {
                serial: "Q2-A".into(),
                rename: Some(("dev-Q2-A".into(), "lobby-ap".into())),
                address: None,
                network_id: None,
            }]
        );
        assert_eq!(plan.writes.len(), 1, "the row takes the new name too");
    }

    /// …and the other direction: a person renamed the node, so Meraki's rename stays in the
    /// inventory and goes no further. The row is still written — only the node is left alone.
    #[test]
    fn a_rename_in_meraki_never_overwrites_a_name_an_operator_chose() {
        let was = seen("Q2-A", true);
        let now = SeenDevice {
            name: "lobby-ap".into(),
            ..was.clone()
        };
        let renamed_here = BoundNode {
            name: "Reception AP (do not touch)".into(),
            ..node_of(&was)
        };
        let plan = plan_sync(&[imported_as(&was)], &[now], &bound_as(&was, renamed_here));
        assert!(plan.follows.is_empty(), "{:?}", plan.follows);
        assert_eq!(plan.writes.len(), 1);
    }

    /// A name that differs with **no change on Meraki's side** is not a rename to follow: it is an
    /// operator's rename, or a drift from before this existed, and the two look the same.
    #[test]
    fn a_name_that_already_differed_is_left_alone() {
        let a = seen("Q2-A", true);
        let drifted = BoundNode {
            name: "an older name".into(),
            ..node_of(&a)
        };
        assert!(plan_sync(
            &[imported_as(&a)],
            std::slice::from_ref(&a),
            &bound_as(&a, drifted)
        )
        .is_empty());
    }

    /// A device with no name in Meraki is imported under its serial, so the rename is recognised
    /// through [`node_name_for`] in both directions. Comparing the raw names would never match the
    /// node called `Q2-A` against a stored name of `""`.
    #[test]
    fn an_unnamed_device_follows_through_its_serial_in_both_directions() {
        let unnamed = SeenDevice {
            name: "  ".into(),
            ..seen("Q2-A", true)
        };
        let named = SeenDevice {
            name: "edge-fw".into(),
            ..unnamed.clone()
        };
        assert_eq!(node_name_for(&unnamed.name, &unnamed.serial), "Q2-A");
        // The renames a plan holds, as a list: a plan with none must fail by saying so, not by
        // indexing past the end of an empty one.
        let renames = |plan: &SyncPlan| -> Vec<(String, String)> {
            plan.follows
                .iter()
                .filter_map(|f| f.rename.clone())
                .collect()
        };

        let gains = plan_sync(
            &[imported_as(&unnamed)],
            std::slice::from_ref(&named),
            &bound_as(&unnamed, node_of(&unnamed)),
        );
        assert_eq!(
            renames(&gains),
            [("Q2-A".to_owned(), "edge-fw".to_owned())],
            "a device imported under its serial did not take the name Meraki gave it later"
        );

        let loses = plan_sync(
            &[imported_as(&named)],
            std::slice::from_ref(&unnamed),
            &bound_as(&named, node_of(&named)),
        );
        assert_eq!(
            renames(&loses),
            [("edge-fw".to_owned(), "Q2-A".to_owned())],
            "a device whose name was removed in Meraki did not go back to its serial"
        );
    }

    /// The address is compared with the node's **current** one, not with the stored row — so a node
    /// that drifted before this existed is put right although nothing changed in the inventory.
    #[test]
    fn a_node_at_another_address_is_readdressed_even_when_the_row_did_not_change() {
        let a = seen("Q2-A", true);
        let stale = BoundNode {
            address: "10.9.9.9".parse().expect("ip"),
            ..node_of(&a)
        };
        let plan = plan_sync(
            &[imported_as(&a)],
            std::slice::from_ref(&a),
            &bound_as(&a, stale),
        );
        assert!(plan.writes.is_empty(), "the inventory row is unchanged");
        assert_eq!(plan.follows.len(), 1);
        assert_eq!(plan.follows[0].address, "10.0.0.1".parse().ok());
        assert_eq!(plan.follows[0].rename, None);
    }

    /// Both directions of "no address". A node imported at `0.0.0.0` gets its address the first
    /// time Meraki reports one; a listing that reports none moves nothing — a device that is down
    /// must not turn a good address into `0.0.0.0`.
    #[test]
    fn a_missing_address_never_replaces_a_good_one_and_a_good_one_replaces_none() {
        let addressed = seen("Q2-A", true);
        let bare = SeenDevice {
            lan_ip: None,
            ..addressed.clone()
        };

        let gains = plan_sync(
            &[imported_as(&bare)],
            std::slice::from_ref(&addressed),
            &bound_as(&bare, node_of(&bare)),
        );
        assert_eq!(gains.follows[0].address, "10.0.0.1".parse().ok());

        let loses = plan_sync(
            &[imported_as(&addressed)],
            std::slice::from_ref(&bare),
            &bound_as(&addressed, node_of(&addressed)),
        );
        assert!(loses.follows.is_empty(), "{:?}", loses.follows);
        assert_eq!(
            loses.writes.len(),
            1,
            "the row records that Meraki reports none"
        );
    }

    /// The binding's network is kept current. Nothing in the plan names a folder: a device that
    /// moves network stays where it was filed (決定 6).
    #[test]
    fn a_device_that_moved_network_has_its_binding_corrected() {
        let was = seen("Q2-A", true);
        let now = SeenDevice {
            network_id: "N_2".into(),
            ..was.clone()
        };
        let plan = plan_sync(&[imported_as(&was)], &[now], &bound_as(&was, node_of(&was)));
        assert_eq!(
            plan.follows,
            [NodeFollow {
                serial: "Q2-A".into(),
                rename: None,
                address: None,
                network_id: Some("N_2".into()),
            }]
        );
    }

    /// The ordinary sync, with nodes this time: everything agrees, so the plan is empty and the
    /// configuration generation stays where it is.
    #[test]
    fn a_node_that_already_agrees_with_meraki_follows_nothing() {
        let a = seen("Q2-A", true);
        let plan = plan_sync(
            &[imported_as(&a)],
            std::slice::from_ref(&a),
            &bound_as(&a, node_of(&a)),
        );
        assert!(plan.is_empty(), "{plan:?}");
    }

    /// A device with no node has nothing to follow, whatever changed about it.
    #[test]
    fn a_device_without_a_node_follows_nothing() {
        let was = seen("Q2-A", true);
        let now = SeenDevice {
            name: "renamed".into(),
            network_id: "N_2".into(),
            lan_ip: "10.0.0.2".parse().ok(),
            ..was.clone()
        };
        let plan = plan_sync(&[stored_as(&was)], &[now], &nobody());
        assert!(plan.follows.is_empty());
        assert_eq!(plan.writes.len(), 1);
    }

    #[test]
    fn the_state_of_a_device_is_read_in_one_order() {
        let f = |has_node, missing, was_imported, seen_online| DeviceFacts {
            has_node,
            missing,
            was_imported,
            seen_online,
        };
        use MerakiDeviceState::*;
        // A node outranks everything, including having gone missing.
        assert_eq!(classify(f(true, false, true, true)), Some(Monitored));
        assert_eq!(classify(f(true, true, true, true)), Some(Missing));
        // A node imported before the table existed has no imported_at for one sync; still a node.
        assert_eq!(classify(f(true, false, false, false)), Some(Monitored));
        // No node: deleted outranks the online history, because it is the operator's decision.
        assert_eq!(classify(f(false, false, true, true)), Some(Deleted));
        assert_eq!(classify(f(false, false, true, false)), Some(Deleted));
        assert_eq!(classify(f(false, false, false, false)), Some(NeverOnline));
        assert_eq!(classify(f(false, false, false, true)), Some(New));
        // Gone from Meraki with nothing here depending on it: not shown.
        assert_eq!(classify(f(false, true, false, true)), None);
        assert_eq!(classify(f(false, true, true, true)), None);
    }

    #[test]
    fn counts_are_the_same_reading_as_the_list() {
        let mut c = MerakiDeviceCounts::default();
        for s in MerakiDeviceState::ALL {
            c.add(s, true);
        }
        // One of each: four are listed by Meraki, one (Missing) is not.
        assert_eq!(
            c,
            MerakiDeviceCounts {
                seen: 4,
                monitored: 1,
                new: 1,
                missing: 1,
                monitored_unwatched: 0,
            }
        );
    }

    /// ADR-164 決定 15. Only a **monitored** device in an unwatched network is the silent case: a
    /// device with no node has nothing that could go quiet, and one Meraki no longer lists is
    /// already reported as missing.
    #[test]
    fn only_a_monitored_device_in_an_unwatched_network_is_counted_as_not_collected() {
        let mut c = MerakiDeviceCounts::default();
        for s in MerakiDeviceState::ALL {
            c.add(s, false);
        }
        assert_eq!(c.monitored_unwatched, 1);
        assert_eq!(
            (c.seen, c.monitored, c.new, c.missing),
            (4, 1, 1, 1),
            "the watch flag moves no other count"
        );
    }

    /// The five tokens are what the WebUI builds its label keys from and what an API client
    /// filters on, so they are pinned by value: renaming a variant must fail here, not there.
    #[test]
    fn every_state_is_published_under_a_pinned_token() {
        let tokens: Vec<String> = MerakiDeviceState::ALL
            .iter()
            .map(|s| serde_json::to_value(s).expect("serialize"))
            .map(|v| v.as_str().expect("a string").to_owned())
            .collect();
        assert_eq!(
            tokens,
            ["monitored", "new", "never_online", "deleted", "missing"]
        );
        for (s, token) in MerakiDeviceState::ALL.iter().zip(&tokens) {
            let back: MerakiDeviceState =
                serde_json::from_value(serde_json::Value::String(token.clone())).expect("parse");
            assert_eq!(back, *s);
        }
    }

    #[test]
    fn an_unusable_address_is_no_address() {
        assert_eq!(usable_address("10.1.2.3"), "10.1.2.3".parse().ok());
        assert_eq!(usable_address(" 2001:db8::1 "), "2001:db8::1".parse().ok());
        // What the importer writes on a node with no address — never a match key.
        assert_eq!(usable_address("0.0.0.0"), None);
        assert_eq!(usable_address("::"), None);
        assert_eq!(usable_address(""), None);
        assert_eq!(usable_address("not an address"), None);
    }

    #[test]
    fn a_serial_listed_twice_is_taken_once_and_online_means_up() {
        let device = |serial: &str, name: &str, availability| MerakiInventoryDevice {
            info: MerakiDeviceInfo {
                serial: serial.into(),
                name: name.into(),
                model: None,
                product_type: "switch".into(),
                network_id: "N_1".into(),
                lan_ip: Some("0.0.0.0".into()),
            },
            availability,
        };
        let inv = MerakiInventory {
            networks: Vec::new(),
            devices: vec![
                device("Q2-A", "first", Some(MerakiAvailability::Alerting)),
                device("Q2-A", "second", Some(MerakiAvailability::Offline)),
                device("Q2-B", "b", Some(MerakiAvailability::Dormant)),
                device("Q2-C", "c", None),
            ],
        };
        let got = seen_devices(&inv, &LanAddresses::new());
        let brief: Vec<(&str, &str, bool)> = got
            .iter()
            .map(|d| (d.serial.as_str(), d.name.as_str(), d.online))
            .collect();
        assert_eq!(
            brief,
            [
                ("Q2-A", "first", true),
                ("Q2-B", "b", false),
                ("Q2-C", "c", false)
            ]
        );
        assert!(got.iter().all(|d| d.lan_ip.is_none()));
    }

    // ── An MX's address from its network's LAN side (ADR-164 決定 28) ───────────────────────────

    fn ip(s: &str) -> IpAddr {
        s.parse().expect("ip")
    }

    fn lan(vlan_id: Option<u32>, ip: &str) -> MerakiLanAddress {
        MerakiLanAddress {
            vlan_id,
            appliance_ip: ip.to_owned(),
        }
    }

    #[test]
    fn a_networks_addresses_are_read_in_vlan_order_once_each() {
        let got = lan_order(&[
            lan(Some(30), "10.3.0.1"),
            lan(None, "10.9.0.1"),
            lan(Some(1), "192.168.128.1"),
            lan(Some(20), "10.2.0.1"),
            // The same address twice, and one that identifies nothing.
            lan(Some(21), "10.2.0.1"),
            lan(Some(5), "0.0.0.0"),
            lan(Some(6), "not an address"),
        ]);
        assert_eq!(
            got,
            [
                ip("192.168.128.1"),
                ip("10.2.0.1"),
                ip("10.3.0.1"),
                ip("10.9.0.1")
            ]
        );
    }

    /// The case that decided the rule: VLAN 1 left at a default subnet several sites share, and the
    /// site's own VLAN next. The ranges pick the site's; with no range holding any, the lowest wins.
    #[test]
    fn an_mx_takes_the_lowest_vlan_inside_a_range_else_the_lowest() {
        let ips = [ip("192.168.128.1"), ip("10.2.0.1"), ip("10.3.0.1")];
        let ranges: HashSet<IpAddr> = [ip("10.2.0.1"), ip("10.3.0.1")].into();
        assert_eq!(choose_lan_address(&ips, &ranges), Some(ip("10.2.0.1")));
        assert_eq!(
            choose_lan_address(&ips, &HashSet::new()),
            Some(ip("192.168.128.1"))
        );
        // A network with no LAN side: nothing to take, and no WAN address to fall back to.
        assert_eq!(choose_lan_address(&[], &ranges), None);
    }

    #[test]
    fn never_read_networks_come_first_then_the_stalest() {
        let now = at(10 * 86_400);
        let mx: BTreeSet<String> = ["N_new_b", "N_new_a", "N_old", "N_older", "N_fresh"]
            .iter()
            .map(|s| (*s).to_owned())
            .collect();
        let read = |secs_ago: i64| NetworkLan {
            ips: vec![ip("10.0.0.1")],
            read_at: at(10 * 86_400 - secs_ago),
        };
        let stored: HashMap<String, NetworkLan> = [
            ("N_old", read(86_400)),
            ("N_older", read(3 * 86_400)),
            ("N_fresh", read(86_399)),
            // Read, and not an MX network any more: never asked again.
            ("N_gone", read(9 * 86_400)),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v))
        .collect();
        assert_eq!(
            lan_reads_due(&mx, &stored, now),
            ["N_new_a", "N_new_b", "N_older", "N_old"]
        );
    }

    /// An MX takes the chosen address of its network and never the one the listing carries; an MX in
    /// a network nobody has read has none; everything else still takes `lanIp`.
    #[test]
    fn only_an_mx_takes_its_address_from_the_lan_choice() {
        let device = |serial: &str, product_type: &str, network: &str, lan_ip: Option<&str>| {
            MerakiInventoryDevice {
                info: MerakiDeviceInfo {
                    serial: serial.into(),
                    name: serial.into(),
                    model: None,
                    product_type: product_type.into(),
                    network_id: network.into(),
                    lan_ip: lan_ip.map(str::to_owned),
                },
                availability: Some(MerakiAvailability::Online),
            }
        };
        let inv = MerakiInventory {
            networks: Vec::new(),
            devices: vec![
                device("mx-read", "appliance", "N_1", Some("198.51.100.20")),
                device("mx-unread", "appliance", "N_2", None),
                device("mx-no-lan", "appliance", "N_3", None),
                device("switch", "switch", "N_1", Some("10.1.0.5")),
            ],
        };
        let lans: LanAddresses = [
            ("N_1".to_owned(), Some(ip("10.1.0.1"))),
            ("N_3".to_owned(), None),
        ]
        .into();
        let seen = seen_devices(&inv, &lans);
        let got: Vec<(&str, Option<IpAddr>)> =
            seen.iter().map(|d| (d.serial.as_str(), d.lan_ip)).collect();
        assert_eq!(
            got,
            [
                ("mx-read", Some(ip("10.1.0.1"))),
                ("mx-unread", None),
                ("mx-no-lan", None),
                ("switch", Some(ip("10.1.0.5"))),
            ]
        );
        assert_eq!(
            networks_with_an_mx(&inv).into_iter().collect::<Vec<_>>(),
            ["N_1", "N_2", "N_3"]
        );
    }
}
