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

use std::collections::{HashMap, HashSet};
use std::net::IpAddr;

use chrono::{DateTime, Utc};
use sqlx::{PgPool, Row};
use uuid::Uuid;
use yagra_transport::{MerakiInventory, MerakiInventoryDevice};

/// One device as a complete sync saw it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeenDevice {
    pub serial: String,
    pub name: String,
    pub model: Option<String>,
    pub product_type: String,
    pub network_id: String,
    /// The address Meraki reports, if it is a usable one. See [`usable_address`].
    pub lan_ip: Option<IpAddr>,
    /// Whether Meraki reports the device up *in this listing* (online or alerting).
    pub online: bool,
}

impl SeenDevice {
    /// Read one device out of a transport inventory.
    #[must_use]
    pub fn from_transport(d: &MerakiInventoryDevice) -> Self {
        Self {
            serial: d.info.serial.clone(),
            name: d.info.name.clone(),
            model: d.info.model.clone(),
            product_type: d.info.product_type.clone(),
            network_id: d.info.network_id.clone(),
            lan_ip: d.info.lan_ip.as_deref().and_then(usable_address),
            online: d.availability.is_some_and(|a| a.is_up()),
        }
    }
}

/// Every device of a transport inventory, as the planner wants them. A serial listed twice keeps
/// its first entry: the table's key is the serial, and two rows for one would make the plan depend
/// on the order of a listing nobody controls.
#[must_use]
pub fn seen_devices(inventory: &MerakiInventory) -> Vec<SeenDevice> {
    let mut taken: HashSet<&str> = HashSet::new();
    inventory
        .devices
        .iter()
        .filter(|d| taken.insert(d.info.serial.as_str()))
        .map(SeenDevice::from_transport)
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

/// What one sync writes. Empty when nothing changed, which is the ordinary case.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SyncPlan {
    /// New rows, rows whose description changed, and rows crossing a transition.
    pub writes: Vec<DeviceWrite>,
    /// Stored serials the listing no longer contains and that are not already marked.
    pub newly_missing: Vec<String>,
}

impl SyncPlan {
    /// Whether the sync has anything to write.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.writes.is_empty() && self.newly_missing.is_empty()
    }
}

/// Decide what a sync writes, from what is stored, what a **complete** listing contained, and which
/// serials have a node (`bound`: serial → when it was bound).
///
/// Pure. A device is written when it is new, when anything Meraki says about it changed, when it is
/// online for the first time, when it has come back from missing, or when it has a node and no
/// `imported_at` yet. Everything else is left alone — including a device that is merely *offline
/// again*: `online` is a fact about this listing, not a column.
#[must_use]
pub fn plan_sync(
    stored: &[StoredDevice],
    seen: &[SeenDevice],
    bound: &HashMap<String, DateTime<Utc>>,
) -> SyncPlan {
    let by_serial: HashMap<&str, &StoredDevice> =
        stored.iter().map(|r| (r.serial.as_str(), r)).collect();
    let mut plan = SyncPlan::default();

    for device in seen {
        let row = by_serial.get(device.serial.as_str()).copied();
        let first_online = device.online && row.is_none_or(|r| r.first_online_at.is_none());
        let imported_at = match row {
            Some(r) if r.imported_at.is_some() => None,
            _ => bound.get(&device.serial).copied(),
        };
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

/// What a device's row is shown as (ADR-164 決定 9). Serialized as the snake_case token.
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
}

impl MerakiDeviceCounts {
    /// Fold one device in.
    fn add(&mut self, state: MerakiDeviceState) {
        match state {
            MerakiDeviceState::Monitored => {
                self.seen += 1;
                self.monitored += 1;
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
    pub state: MerakiDeviceState,
    pub node_id: Option<Uuid>,
    /// The folder the device's node is filed in. `None` without a node, and for a node at the top
    /// of the tree.
    pub node_group_id: Option<Uuid>,
    pub first_seen_at: DateTime<Utc>,
    pub missing_since: Option<DateTime<Utc>>,
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

    /// Which of an organization's serials have a node, and when each was bound.
    pub async fn bound(&self, org: Uuid) -> anyhow::Result<HashMap<String, DateTime<Utc>>> {
        let rows = sqlx::query("SELECT serial, created_at FROM meraki_devices WHERE org_id = $1")
            .bind(org)
            .fetch_all(&self.pool)
            .await?;
        rows.iter()
            .map(|r| Ok((r.try_get("serial")?, r.try_get("created_at")?)))
            .collect()
    }

    /// Write one sync's plan, atomically. Returns how many rows were touched.
    ///
    /// 🚨 The `COALESCE`s are not decoration. The planner only asks for `first_online_at` or
    /// `imported_at` on a row that has none, but two syncs of one organization can plan from the
    /// same snapshot (a manual one beside the periodic one on another core) — and the later write
    /// must not move a timestamp the earlier one set.
    pub async fn apply(&self, org: Uuid, plan: &SyncPlan) -> anyhow::Result<u64> {
        if plan.is_empty() {
            return Ok(0);
        }
        let mut touched = 0u64;
        let mut tx = self.pool.begin().await?;
        for w in &plan.writes {
            let d = &w.device;
            touched += sqlx::query(
                "INSERT INTO meraki_inventory \
                   (org_id, serial, name, model, product_type, network_id, lan_ip, \
                    first_online_at, imported_at) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, CASE WHEN $8 THEN now() END, $9) \
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
            .bind(&d.serial)
            .bind(&d.name)
            .bind(&d.model)
            .bind(&d.product_type)
            .bind(&d.network_id)
            .bind(d.lan_ip.map(|a| a.to_string()))
            .bind(w.first_online)
            .bind(w.imported_at)
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
        tx.commit().await?;
        Ok(touched)
    }

    /// An organization's devices, as the API lists them. Rows [`classify`] declines are left out.
    pub async fn devices(&self, org: Uuid) -> anyhow::Result<Vec<DeviceRecord>> {
        let rows = sqlx::query(
            "SELECT i.serial, i.name, i.model, i.product_type, i.network_id, i.lan_ip, \
                    i.first_seen_at, i.first_online_at, i.missing_since, i.imported_at, \
                    d.node_id, nd.group_id AS node_group_id, n.name AS network_name, \
                    COALESCE(n.monitored, false) AS monitored \
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
            out.push(DeviceRecord {
                serial: r.try_get("serial")?,
                name: r.try_get("name")?,
                model: r.try_get("model")?,
                product_type: r.try_get("product_type")?,
                network_id: r.try_get("network_id")?,
                network_name: r.try_get("network_name")?,
                network_monitored: r.try_get("monitored")?,
                lan_ip: lan_ip.as_deref().and_then(usable_address),
                state,
                node_id,
                node_group_id: r.try_get("node_group_id")?,
                first_seen_at: r.try_get("first_seen_at")?,
                missing_since,
            });
        }
        Ok(out)
    }

    /// Every organization's counts, for the organization list. An organization with no rows yet is
    /// absent from the map — the caller reads that as all-zero.
    ///
    /// Folded here through [`classify`] rather than counted in SQL with `FILTER` clauses: those
    /// would be a second spelling of the same five-way rule, and the list and its own counts
    /// disagreeing is exactly what two spellings produce.
    pub async fn counts(&self) -> anyhow::Result<HashMap<Uuid, MerakiDeviceCounts>> {
        let rows = sqlx::query(
            "SELECT i.org_id, i.first_online_at, i.missing_since, i.imported_at, d.node_id \
             FROM meraki_inventory i \
             LEFT JOIN meraki_devices d ON d.org_id = i.org_id AND d.serial = i.serial",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut out: HashMap<Uuid, MerakiDeviceCounts> = HashMap::new();
        for r in &rows {
            let node_id: Option<Uuid> = r.try_get("node_id")?;
            let missing_since: Option<DateTime<Utc>> = r.try_get("missing_since")?;
            if let Some(state) = classify(facts_of(r, node_id.is_some(), missing_since.is_some())?)
            {
                out.entry(r.try_get("org_id")?).or_default().add(state);
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

    fn nobody() -> HashMap<String, DateTime<Utc>> {
        HashMap::new()
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
        let bound = HashMap::from([("Q2-A".to_owned(), at(-3600))]);

        let first = plan_sync(&[], std::slice::from_ref(&a), &bound);
        assert_eq!(first.writes[0].imported_at, Some(at(-3600)));

        // Already recorded: not a reason to write, and never a reason to move it.
        let mut row = stored_as(&a);
        row.imported_at = Some(at(-3600));
        assert!(plan_sync(&[row], &[a], &bound).is_empty());
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
            c.add(s);
        }
        // One of each: four are listed by Meraki, one (Missing) is not.
        assert_eq!(
            c,
            MerakiDeviceCounts {
                seen: 4,
                monitored: 1,
                new: 1,
                missing: 1
            }
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
        let got = seen_devices(&inv);
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
}
