// SPDX-License-Identifier: AGPL-3.0-only
//! Wireless controllers and their access points, as stored (ADR-064, migration 0121).
//!
//! A controller's poll publishes an inventory — every AP it manages and what it says about each.
//! This module turns those inventories into three tables and one rule:
//!
//! * [`WirelessRepo::record_inventory`] writes a controller's view (its **sightings**) and folds it
//!   into each AP's row;
//! * [`ownership`] decides, per AP, whether that view is allowed to set the AP's state — the rule an
//!   HA pair needs, because both members report the same AP and only one of them is serving it;
//! * [`WirelessRepo::list_page`] reads the AP list back, scoped by the controllers the caller can see.
//!
//! ## Why the ownership rule is a pure function
//!
//! It has two callers that must never disagree: the writer here, which decides what the AP list
//! shows, and — from increment B2 — the ingest path, which decides whether a controller's numbers
//! become samples on the AP's node. Measured on the PoC's AC6508 pair: the standby answers `standby`
//! for every AP the active serves, **with CPU, memory and radio values of 0**. Written into the AP's
//! row or published as samples, those zeros would replace real readings on every poll. One function,
//! tested once, is what keeps the two answers the same.

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use sqlx::{PgPool, Row};
use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
use uuid::Uuid;
use yagra_common::{ap_id, WlanApObservation, WlanApState, WlanFlavor, WlanInventory};

/// How long a controller's report that it serves an AP stays authoritative without being repeated.
///
/// Three polls at the default five-minute interval. Past it, another controller saying the AP is not
/// associated is believed — which is what lets an AP whose serving controller went silent be shown as
/// down by the one that is still answering, rather than frozen at its last good state.
pub const OWNER_STALE_AFTER_SECS: i64 = 15 * 60;

/// The word the AP list shows for an AP its controller's table no longer lists (ADR-064 増分 F, F9).
///
/// Not a vendor word — the controller said nothing, which is the point — so it is spelled so an
/// operator reading the list sees why the state changed. The AP tab shows `run_state` untranslated
/// (ADR-064 決定 14), and this is the one value Yagra writes there itself.
pub const ABSENT_RUN_STATE: &str = "not_listed";

/// The controller that last reported an AP associated, and when.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ownership {
    pub controller: Uuid,
    pub last_associated_at: DateTime<Utc>,
}

/// What one controller's statement about one AP is allowed to change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Verdict {
    /// The AP's serving controller after this statement. Changes only when a controller reports the
    /// AP associated; a report of standby or trouble never takes ownership.
    pub owner: Option<Ownership>,
    /// Whether this statement sets the AP's state, clients and — from increment B2 — its samples.
    /// `false` exactly when **another** controller serves the AP and said so recently.
    pub take_state: bool,
}

/// Decide what `reporter`'s statement that an AP is `state` may change, given who currently serves
/// it (ADR-064 決定 8c, as revised by R4).
///
/// * **Associated** — the reporter serves the AP: it becomes the owner, and its state stands.
/// * **Backup or not associated, while another controller served it within `stale_after`** — the
///   serving controller's word stands. This is the case the HA standby is in on every poll.
/// * **Backup or not associated, and nobody else serves it now** — the statement stands. An AP down
///   on both members, an AP whose serving controller has gone silent, and an AP whose only monitored
///   controller is a standby are all shown as the controller that is answering describes them.
#[must_use]
pub fn ownership(
    current: Option<Ownership>,
    reporter: Uuid,
    state: WlanApState,
    now: DateTime<Utc>,
    stale_after: ChronoDuration,
) -> Verdict {
    match state {
        WlanApState::Associated => Verdict {
            owner: Some(Ownership {
                controller: reporter,
                last_associated_at: now,
            }),
            take_state: true,
        },
        WlanApState::Backup | WlanApState::NotAssociated => {
            let served_elsewhere = current.is_some_and(|o| {
                o.controller != reporter && now - o.last_associated_at <= stale_after
            });
            Verdict {
                owner: current,
                take_state: !served_elsewhere,
            }
        }
    }
}

/// One AP as the list shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct ApRow {
    pub ap_id: Uuid,
    pub mac: String,
    pub name: Option<String>,
    pub serial: Option<String>,
    pub model: Option<String>,
    pub sw_version: Option<String>,
    pub ip: Option<IpAddr>,
    pub vendor_group: Option<String>,
    pub run_state: String,
    /// `None` for a token this binary does not know — a newer core wrote it.
    pub state: Option<WlanApState>,
    pub clients: Option<i32>,
    pub node_id: Option<Uuid>,
    /// The serving controller's **node**.
    pub owner_node_id: Option<Uuid>,
    pub first_seen: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
    pub last_associated_at: Option<DateTime<Utc>>,
    /// What each controller that reports this AP said, serving controller first.
    pub sightings: Vec<SightingRow>,
}

/// One controller's view of one AP.
#[derive(Debug, Clone, PartialEq)]
pub struct SightingRow {
    pub controller_node_id: Option<Uuid>,
    pub run_state: String,
    pub state: Option<WlanApState>,
    pub clients: Option<i32>,
    pub last_seen: DateTime<Utc>,
    pub last_associated_at: Option<DateTime<Utc>>,
}

/// What a controller's last complete inventory said about itself, and how its APs are imported.
#[derive(Debug, Clone, PartialEq)]
pub struct ControllerRow {
    pub node_id: Uuid,
    pub flavor: Option<WlanFlavor>,
    pub aps_reported: i32,
    pub aps_truncated_at: Option<i32>,
    pub last_inventory_at: Option<DateTime<Utc>>,
    /// Whether this controller's APs become nodes (ADR-064 決定 8). **On unless an operator turns it
    /// off** — since ADR-064 R22 the column defaults to `TRUE` (migration 0123), and that default is
    /// the only place it is written: the row is created by the first inventory, whose INSERT does
    /// not name the column.
    pub import_aps: bool,
    /// The most APs this controller may import.
    pub max_aps: i32,
    /// Where its imported AP nodes are filed. `None` ⇒ the folder the controller node itself is
    /// in. No folder is created: a name derived from one member of an HA pair is wrong as soon as
    /// the pair switches over, and the standby would file its own APs in a second one (ADR-064 B2
    /// の手直し). Which controller serves an AP is `wireless_aps.owner_controller_id`, not a name.
    /// ⚠️ `migrations/0121`'s column comment still describes the first design and cannot be edited.
    pub ap_group_id: Option<Uuid>,
    /// How many APs the cap left out on the importer's last pass.
    pub aps_over_cap: i32,
}

/// What an operator sets on a controller (`PUT /api/v1/nodes/{node_id}/wireless-controller`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ControllerSettings {
    pub import_aps: bool,
    pub max_aps: i32,
    pub ap_group_id: Option<Uuid>,
}

/// An imported AP's node and who serves it — what the ingest fan-out needs to publish the AP's
/// numbers under its node (`wireless_fanout`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApBinding {
    pub ap_id: Uuid,
    pub node_id: Uuid,
    pub owner: Option<Ownership>,
}

/// What one pass of the importer did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ImportPass {
    /// AP nodes created.
    pub imported: u32,
    /// APs eligible but left out by a controller's cap, summed over controllers.
    pub over_cap: u32,
}

/// What importing one AP by hand did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportOne {
    /// The AP is now the node with this id.
    Imported(Uuid),
    /// The AP already has this node; nothing was written.
    AlreadyImported(Uuid),
    /// No AP with that id.
    NotFound,
    /// No controller that reports it is still monitored, so there is nowhere to file it.
    NoController,
    /// Filing it where its controller files APs would put it in a folder the caller cannot see
    /// (ADR-014). Nothing was written.
    OutOfScope,
}

/// What one attempt to file an AP as a node did — [`WirelessRepo::import_ap`]'s answer.
enum Filed {
    Imported,
    /// Imported by someone else, or its controller removed, since it was picked.
    Gone,
    /// Its destination folder is outside the caller's scope; nothing was written.
    OutOfScope,
}

/// Filters for [`WirelessRepo::list_page`].
#[derive(Debug, Clone, Default)]
pub struct ApFilter {
    /// Only APs this controller node reports.
    pub controller_node: Option<Uuid>,
    /// Only the AP imported as this node.
    pub node: Option<Uuid>,
    /// Only this AP.
    pub ap: Option<Uuid>,
    pub state: Option<WlanApState>,
    /// A case-insensitive substring of the name, MAC, address or model. Matched literally —
    /// `%` and `_` are characters, not wildcards.
    pub search: Option<String>,
}

/// Stores the AP inventories.
#[derive(Clone)]
pub struct WirelessRepo {
    pool: PgPool,
}

/// What a neighbour's management address is to the AP inventory (ADR-179 増分 3): the AP some
/// controller reports at that address, and a controller the caller can see that reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApAtAddress {
    pub ap_id: Uuid,
    /// A controller node reporting this AP that the caller may see, with its name — the AP's
    /// current owner when that one is visible. `None`: every controller reporting it is outside
    /// the caller's scope.
    pub controller: Option<(Uuid, String)>,
    /// The AP is already a node (at an address other than this one, or the neighbour would have
    /// matched that node).
    pub imported: bool,
}

impl WirelessRepo {
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// The AP a controller reports at each of `addresses` (ADR-179 増分 3), so the Neighbors tab can
    /// send an access point to the controller that manages it instead of registering it by hand —
    /// which would leave a second node for the same AP once the controller imports it.
    ///
    /// Only APs some controller still reports (a sighting exists); the controller named is one the
    /// caller could see, by the same rule as [`Self::list_page`]. When two APs share an address,
    /// the one seen last wins, unless only the other has a visible controller.
    pub async fn aps_at(
        &self,
        addresses: &[IpAddr],
        groups: Option<&[Uuid]>,
    ) -> anyhow::Result<HashMap<IpAddr, ApAtAddress>> {
        if addresses.is_empty() {
            return Ok(HashMap::new());
        }
        let text: Vec<String> = addresses.iter().map(ToString::to_string).collect();
        let rows = sqlx::query(
            "SELECT a.ap_id, host(a.ip) AS ip, a.node_id IS NOT NULL AS imported, \
                    v.id AS ctl_id, v.name AS ctl_name \
             FROM wireless_aps a \
             LEFT JOIN LATERAL ( \
                 SELECT n.id, n.name FROM wireless_ap_sightings s \
                 JOIN wireless_controllers c ON c.id = s.controller_id \
                 JOIN nodes n ON n.id = c.node_id \
                 WHERE s.ap_id = a.ap_id AND ($1::UUID[] IS NULL OR n.group_id = ANY($1)) \
                 ORDER BY (c.id = a.owner_controller_id) DESC NULLS LAST, n.name, n.id \
                 LIMIT 1) v ON TRUE \
             WHERE a.ip = ANY($2::text[]::inet[]) \
               AND EXISTS (SELECT 1 FROM wireless_ap_sightings s WHERE s.ap_id = a.ap_id) \
             ORDER BY a.last_seen DESC, a.ap_id",
        )
        .bind(groups.map(<[Uuid]>::to_vec))
        .bind(&text)
        .fetch_all(&self.pool)
        .await?;
        let mut out: HashMap<IpAddr, ApAtAddress> = HashMap::new();
        for r in rows {
            let Some(ip) = r
                .try_get::<Option<String>, _>("ip")?
                .and_then(|s| s.parse::<IpAddr>().ok())
            else {
                continue;
            };
            let ctl_id: Option<Uuid> = r.try_get("ctl_id")?;
            let ctl_name: Option<String> = r.try_get("ctl_name")?;
            let found = ApAtAddress {
                ap_id: r.try_get("ap_id")?,
                controller: ctl_id.zip(ctl_name),
                imported: r.try_get("imported")?,
            };
            match out.get(&ip) {
                Some(kept) if kept.controller.is_some() || found.controller.is_none() => {}
                _ => {
                    out.insert(ip, found);
                }
            }
        }
        Ok(out)
    }

    /// Record one controller's complete inventory (ADR-064).
    ///
    /// Only ever called with an inventory the poller published — a walk that missed a column publishes
    /// none (決定 9b) — so this never reads a half table as APs leaving. APs the controller no longer
    /// reports are not touched: their sighting's `last_seen` simply stops advancing (決定 10).
    ///
    /// One transaction per inventory, holding the AP rows it reads, so two controllers of a pair
    /// arriving in one batch decide ownership one after the other rather than over each other.
    ///
    /// A controller node deleted since its poll is not an error: the inventory is dropped.
    pub async fn record_inventory(
        &self,
        controller_node: Uuid,
        inventory: &WlanInventory,
        now: DateTime<Utc>,
    ) -> anyhow::Result<()> {
        let aps: Vec<WlanApObservation> = inventory
            .aps
            .iter()
            .cloned()
            .map(WlanApObservation::sanitized)
            .collect();
        let mut tx = self.pool.begin().await?;
        let controller = sqlx::query(
            "INSERT INTO wireless_controllers \
                 (id, source, node_id, flavor, aps_reported, aps_truncated_at, last_inventory_at, \
                  updated_at) \
             VALUES ($1, 'snmp', $1, $2, $3, $4, $5, $5) \
             ON CONFLICT (id) DO UPDATE SET \
                 flavor = EXCLUDED.flavor, \
                 aps_reported = EXCLUDED.aps_reported, \
                 aps_truncated_at = EXCLUDED.aps_truncated_at, \
                 last_inventory_at = EXCLUDED.last_inventory_at, \
                 updated_at = EXCLUDED.updated_at",
        )
        .bind(controller_node)
        .bind(inventory.flavor.as_str())
        .bind(i32::try_from(aps.len()).unwrap_or(i32::MAX))
        .bind(
            inventory
                .truncated_at
                .map(|n| i32::try_from(n).unwrap_or(i32::MAX)),
        )
        .bind(now)
        .execute(&mut *tx)
        .await;
        match controller {
            Ok(_) => {}
            Err(sqlx::Error::Database(db)) if db.is_foreign_key_violation() => {
                tracing::debug!(node = %controller_node, "controller deleted since its poll; inventory dropped");
                return Ok(());
            }
            Err(e) => return Err(e.into()),
        }
        // A Cisco controller drops an AP it has lost from its table (ADR-064 増分 F, F9): an AP this
        // controller last served and no longer lists reads as not associated — here, in the AP list,
        // and in the fan-out, on the AP's node, both asking `absence_is_evidence`. Its `last_seen`
        // does not move, because nothing reported it, and its owner does not either: whoever serves
        // it now takes it by saying so. Before the empty-list return, since an empty table is the
        // case where every one of them is gone.
        if inventory.absence_is_evidence() {
            let listed: Vec<Uuid> = aps.iter().map(|a| ap_id(a.mac)).collect();
            sqlx::query(
                "UPDATE wireless_aps SET state = $3, run_state = $4, clients = NULL \
                 WHERE owner_controller_id = $1 AND NOT (ap_id = ANY($2)) \
                   AND (state <> $3 OR run_state <> $4)",
            )
            .bind(controller_node)
            .bind(&listed)
            .bind(WlanApState::NotAssociated.as_str())
            .bind(ABSENT_RUN_STATE)
            .execute(&mut *tx)
            .await?;
            sqlx::query(
                "UPDATE wireless_ap_sightings SET state = $3, run_state = $4, clients = NULL \
                 WHERE controller_id = $1 AND NOT (ap_id = ANY($2)) \
                   AND (state <> $3 OR run_state <> $4)",
            )
            .bind(controller_node)
            .bind(&listed)
            .bind(WlanApState::NotAssociated.as_str())
            .bind(ABSENT_RUN_STATE)
            .execute(&mut *tx)
            .await?;
        }
        if aps.is_empty() {
            tx.commit().await?;
            return Ok(());
        }

        let ids: Vec<Uuid> = aps.iter().map(|a| ap_id(a.mac)).collect();
        let current_rows = sqlx::query(
            "SELECT ap_id, owner_controller_id, last_associated_at, run_state, state, clients, \
                    host(ip) AS ip, name, node_id \
             FROM wireless_aps WHERE ap_id = ANY($1) FOR UPDATE",
        )
        .bind(&ids)
        .fetch_all(&mut *tx)
        .await?;
        struct Current {
            owner: Option<Ownership>,
            run_state: String,
            state: String,
            clients: Option<i32>,
            ip: Option<String>,
            name: Option<String>,
            node: Option<Uuid>,
        }
        let mut current: HashMap<Uuid, Current> = HashMap::with_capacity(current_rows.len());
        for row in current_rows {
            let owner_id: Option<Uuid> = row.try_get("owner_controller_id")?;
            let associated: Option<DateTime<Utc>> = row.try_get("last_associated_at")?;
            current.insert(
                row.try_get("ap_id")?,
                Current {
                    owner: owner_id
                        .zip(associated)
                        .map(|(controller, last_associated_at)| Ownership {
                            controller,
                            last_associated_at,
                        }),
                    run_state: row.try_get("run_state")?,
                    state: row.try_get("state")?,
                    clients: row.try_get("clients")?,
                    ip: row.try_get("ip")?,
                    name: row.try_get("name")?,
                    node: row.try_get("node_id")?,
                },
            );
        }

        let stale_after = ChronoDuration::seconds(OWNER_STALE_AFTER_SECS);
        let n = aps.len();
        let mut col_id = Vec::with_capacity(n);
        let mut col_mac = Vec::with_capacity(n);
        let mut col_name = Vec::with_capacity(n);
        let mut col_serial = Vec::with_capacity(n);
        let mut col_model = Vec::with_capacity(n);
        let mut col_version = Vec::with_capacity(n);
        let mut col_ip: Vec<Option<String>> = Vec::with_capacity(n);
        let mut col_group = Vec::with_capacity(n);
        let mut col_run_state = Vec::with_capacity(n);
        let mut col_state = Vec::with_capacity(n);
        let mut col_clients: Vec<Option<i32>> = Vec::with_capacity(n);
        let mut col_owner: Vec<Option<Uuid>> = Vec::with_capacity(n);
        let mut col_associated: Vec<Option<DateTime<Utc>>> = Vec::with_capacity(n);
        let mut sight_state = Vec::with_capacity(n);
        let mut sight_run_state = Vec::with_capacity(n);
        let mut sight_clients: Vec<Option<i32>> = Vec::with_capacity(n);
        let mut sight_associated: Vec<Option<DateTime<Utc>>> = Vec::with_capacity(n);
        // What an imported AP's node follows (ADR-064 increment B2): its name, while nobody has
        // renamed the node, and its address, from the report that is accepted.
        let mut rename_node: Vec<Uuid> = Vec::new();
        let mut rename_to: Vec<String> = Vec::new();
        let mut rename_from: Vec<String> = Vec::new();
        let mut readdress_node: Vec<Uuid> = Vec::new();
        let mut readdress_to: Vec<String> = Vec::new();
        for (ap, id) in aps.iter().zip(&ids) {
            let known = current.get(id);
            let verdict = ownership(
                known.and_then(|c| c.owner),
                controller_node,
                ap.state,
                now,
                stale_after,
            );
            let clients = ap.clients.map(|c| i32::try_from(c).unwrap_or(i32::MAX));
            col_id.push(*id);
            col_mac.push(ap.mac.to_string());
            col_name.push(ap.name.clone());
            col_serial.push(ap.serial.clone());
            col_model.push(ap.model.clone());
            col_version.push(ap.sw_version.clone());
            col_group.push(ap.vendor_group.clone());
            // The address follows the report that is accepted, like the state: a controller whose
            // word does not stand must not blank the address the serving one gave — and the serving
            // one reporting none (Huawei's 255.255.255.255 for an AP that is down) does clear it.
            // Found by the database test: a standby that answers no address, arriving after the
            // active, emptied the AP's IP on every poll.
            match (verdict.take_state, known) {
                (false, Some(c)) => {
                    col_run_state.push(c.run_state.clone());
                    col_state.push(c.state.clone());
                    col_clients.push(c.clients);
                    col_ip.push(c.ip.clone());
                }
                _ => {
                    col_run_state.push(ap.run_state.clone());
                    col_state.push(ap.state.as_str().to_owned());
                    col_clients.push(clients);
                    col_ip.push(ap.ip.map(|ip| ip.to_string()));
                }
            }
            if let Some((c, node)) = known.and_then(|c| c.node.map(|node| (c, node))) {
                // The node was named after the AP (or its MAC) when it was imported. While it still
                // carries the name the AP had, nobody has renamed it, so it follows the controller.
                if let Some(new) = ap.name.as_ref().filter(|new| c.name.as_ref() != Some(*new)) {
                    rename_node.push(node);
                    rename_to.push(new.clone());
                    rename_from.push(c.name.clone().unwrap_or_else(|| ap.mac.to_string()));
                }
                // Only a known address moves it: an AP that is down reports none, and the node keeps
                // the last one rather than reading `0.0.0.0`.
                if let Some(ip) = ap.ip.filter(|_| verdict.take_state) {
                    readdress_node.push(node);
                    readdress_to.push(ip.to_string());
                }
            }
            col_owner.push(verdict.owner.map(|o| o.controller));
            col_associated.push(verdict.owner.map(|o| o.last_associated_at));
            sight_state.push(ap.state.as_str().to_owned());
            sight_run_state.push(ap.run_state.clone());
            sight_clients.push(clients);
            sight_associated.push((ap.state == WlanApState::Associated).then_some(now));
        }

        // Descriptive strings keep what was stored when this controller did not report one, so a
        // controller that answers fewer columns never blanks a name another one read.
        sqlx::query(
            "INSERT INTO wireless_aps \
                 (ap_id, mac, name, serial, model, sw_version, ip, vendor_group, run_state, state, \
                  clients, owner_controller_id, last_associated_at, first_seen, last_seen) \
             SELECT u.ap_id, u.mac, u.name, u.serial, u.model, u.sw_version, u.ip::inet, \
                    u.vendor_group, u.run_state, u.state, u.clients, u.owner, u.associated, $14, $14 \
             FROM UNNEST($1::uuid[], $2::text[], $3::text[], $4::text[], $5::text[], $6::text[], \
                         $7::text[], $8::text[], $9::text[], $10::text[], $11::int4[], $12::uuid[], \
                         $13::timestamptz[]) \
                  AS u(ap_id, mac, name, serial, model, sw_version, ip, vendor_group, run_state, \
                       state, clients, owner, associated) \
             ON CONFLICT (ap_id) DO UPDATE SET \
                 name = COALESCE(EXCLUDED.name, wireless_aps.name), \
                 serial = COALESCE(EXCLUDED.serial, wireless_aps.serial), \
                 model = COALESCE(EXCLUDED.model, wireless_aps.model), \
                 sw_version = COALESCE(EXCLUDED.sw_version, wireless_aps.sw_version), \
                 ip = EXCLUDED.ip, \
                 vendor_group = COALESCE(EXCLUDED.vendor_group, wireless_aps.vendor_group), \
                 run_state = EXCLUDED.run_state, \
                 state = EXCLUDED.state, \
                 clients = EXCLUDED.clients, \
                 owner_controller_id = EXCLUDED.owner_controller_id, \
                 last_associated_at = COALESCE(EXCLUDED.last_associated_at, \
                                               wireless_aps.last_associated_at), \
                 last_seen = EXCLUDED.last_seen",
        )
        .bind(&col_id)
        .bind(&col_mac)
        .bind(&col_name)
        .bind(&col_serial)
        .bind(&col_model)
        .bind(&col_version)
        .bind(&col_ip)
        .bind(&col_group)
        .bind(&col_run_state)
        .bind(&col_state)
        .bind(&col_clients)
        .bind(&col_owner)
        .bind(&col_associated)
        .bind(now)
        .execute(&mut *tx)
        .await?;

        sqlx::query(
            "INSERT INTO wireless_ap_sightings \
                 (ap_id, controller_id, run_state, state, clients, last_associated_at, first_seen, \
                  last_seen) \
             SELECT u.ap_id, $1, u.run_state, u.state, u.clients, u.associated, $7, $7 \
             FROM UNNEST($2::uuid[], $3::text[], $4::text[], $5::int4[], $6::timestamptz[]) \
                  AS u(ap_id, run_state, state, clients, associated) \
             ON CONFLICT (ap_id, controller_id) DO UPDATE SET \
                 run_state = EXCLUDED.run_state, \
                 state = EXCLUDED.state, \
                 clients = EXCLUDED.clients, \
                 last_associated_at = COALESCE(EXCLUDED.last_associated_at, \
                                               wireless_ap_sightings.last_associated_at), \
                 last_seen = EXCLUDED.last_seen",
        )
        .bind(controller_node)
        .bind(&col_id)
        .bind(&sight_run_state)
        .bind(&sight_state)
        .bind(&sight_clients)
        .bind(&sight_associated)
        .bind(now)
        .execute(&mut *tx)
        .await?;

        if !rename_node.is_empty() {
            sqlx::query(
                "UPDATE nodes SET name = u.new, updated_at = now() \
                 FROM UNNEST($1::uuid[], $2::text[], $3::text[]) AS u(id, new, old) \
                 WHERE nodes.id = u.id AND nodes.name = u.old",
            )
            .bind(&rename_node)
            .bind(&rename_to)
            .bind(&rename_from)
            .execute(&mut *tx)
            .await?;
        }
        if !readdress_node.is_empty() {
            sqlx::query(
                "UPDATE nodes SET address = u.ip::inet, updated_at = now() \
                 FROM UNNEST($1::uuid[], $2::text[]) AS u(id, ip) \
                 WHERE nodes.id = u.id AND nodes.address IS DISTINCT FROM u.ip::inet",
            )
            .bind(&readdress_node)
            .bind(&readdress_to)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// One page of APs, ordered by name (the MAC when an AP has none), then id.
    ///
    /// `groups` is the caller's scope: `None` is unrestricted, `Some(&[])` matches nothing. An AP is
    /// visible when **a controller the caller can see reports it** — the AP has no node of its own
    /// until it is imported, so the controllers are what bound it. An AP whose every reporting
    /// controller has been deleted is visible only to an unrestricted caller.
    ///
    /// 🚨 **Visible is not the same as every field visible** (ADR-014). One AP can be reported by an
    /// HA pair filed in two folders, and a caller who sees one member must not learn the other from
    /// the row: the sightings are narrowed to controllers in `groups`, and `owner_node_id` and
    /// `node_id` come back `None` when the node they name is outside it. A `controller_node`
    /// filter naming a controller outside `groups` matches nothing, so it cannot be used to ask
    /// which APs that controller reports. (`api::wireless::node_wireless` reads with `None` and
    /// narrows the same three things itself, because it is called for a node already proved
    /// visible.)
    ///
    /// `after` is the keyset cursor: the sort key and id of the last row of the previous page
    /// (ADR-019 — never OFFSET).
    pub async fn list_page(
        &self,
        groups: Option<&[Uuid]>,
        filter: &ApFilter,
        after: Option<(String, Uuid)>,
        limit: i64,
    ) -> anyhow::Result<Vec<ApRow>> {
        let rows = sqlx::query(
            "SELECT a.ap_id, a.mac, a.name, a.serial, a.model, a.sw_version, host(a.ip) AS ip, \
                    a.vendor_group, a.run_state, a.state, a.clients, \
                    CASE WHEN $1::UUID[] IS NULL OR apn.group_id = ANY($1) \
                         THEN a.node_id END AS node_id, \
                    CASE WHEN $1::UUID[] IS NULL OR ocn.group_id = ANY($1) \
                         THEN oc.node_id END AS owner_node_id, \
                    a.first_seen, a.last_seen, a.last_associated_at \
             FROM wireless_aps a \
             LEFT JOIN wireless_controllers oc ON oc.id = a.owner_controller_id \
             LEFT JOIN nodes ocn ON ocn.id = oc.node_id \
             LEFT JOIN nodes apn ON apn.id = a.node_id \
             WHERE ($1::UUID[] IS NULL OR EXISTS ( \
                       SELECT 1 FROM wireless_ap_sightings s \
                       JOIN wireless_controllers c ON c.id = s.controller_id \
                       JOIN nodes n ON n.id = c.node_id \
                       WHERE s.ap_id = a.ap_id AND n.group_id = ANY($1))) \
               AND ($2::UUID IS NULL OR (EXISTS ( \
                       SELECT 1 FROM wireless_ap_sightings s \
                       JOIN wireless_controllers c ON c.id = s.controller_id \
                       WHERE s.ap_id = a.ap_id AND c.node_id = $2) \
                    AND ($1::UUID[] IS NULL OR EXISTS ( \
                       SELECT 1 FROM nodes fnode WHERE fnode.id = $2 AND fnode.group_id = ANY($1))))) \
               AND ($3::TEXT IS NULL OR a.state = $3) \
               AND ($4::TEXT IS NULL \
                    OR strpos(lower(COALESCE(a.name, '')), lower($4)) > 0 \
                    OR strpos(a.mac, lower($4)) > 0 \
                    OR strpos(COALESCE(host(a.ip), ''), lower($4)) > 0 \
                    OR strpos(lower(COALESCE(a.model, '')), lower($4)) > 0) \
               AND ($5::TEXT IS NULL OR (lower(COALESCE(a.name, a.mac)), a.ap_id) > ($5, $6)) \
               AND ($8::UUID IS NULL OR a.node_id = $8) \
               AND ($9::UUID IS NULL OR a.ap_id = $9) \
             ORDER BY lower(COALESCE(a.name, a.mac)), a.ap_id \
             LIMIT $7",
        )
        .bind(groups.map(<[Uuid]>::to_vec))
        .bind(filter.controller_node)
        .bind(filter.state.map(WlanApState::as_str))
        .bind(filter.search.as_deref())
        .bind(after.as_ref().map(|(key, _)| key.clone()))
        .bind(after.map(|(_, id)| id))
        .bind(limit)
        .bind(filter.node)
        .bind(filter.ap)
        .fetch_all(&self.pool)
        .await?;

        let mut out: Vec<ApRow> = Vec::with_capacity(rows.len());
        for row in rows {
            let ip: Option<String> = row.try_get("ip")?;
            let state: String = row.try_get("state")?;
            out.push(ApRow {
                ap_id: row.try_get("ap_id")?,
                mac: row.try_get("mac")?,
                name: row.try_get("name")?,
                serial: row.try_get("serial")?,
                model: row.try_get("model")?,
                sw_version: row.try_get("sw_version")?,
                ip: ip.and_then(|s| s.parse().ok()),
                vendor_group: row.try_get("vendor_group")?,
                run_state: row.try_get("run_state")?,
                state: WlanApState::from_token(&state),
                clients: row.try_get("clients")?,
                node_id: row.try_get("node_id")?,
                owner_node_id: row.try_get("owner_node_id")?,
                first_seen: row.try_get("first_seen")?,
                last_seen: row.try_get("last_seen")?,
                last_associated_at: row.try_get("last_associated_at")?,
                sightings: Vec::new(),
            });
        }
        if out.is_empty() {
            return Ok(out);
        }

        let ids: Vec<Uuid> = out.iter().map(|a| a.ap_id).collect();
        let sightings = sqlx::query(
            "SELECT s.ap_id, c.node_id AS controller_node_id, s.run_state, s.state, s.clients, \
                    s.last_seen, s.last_associated_at \
             FROM wireless_ap_sightings s \
             JOIN wireless_controllers c ON c.id = s.controller_id \
             JOIN wireless_aps a ON a.ap_id = s.ap_id \
             WHERE s.ap_id = ANY($1) \
               AND ($2::UUID[] IS NULL OR EXISTS ( \
                       SELECT 1 FROM nodes cn \
                       WHERE cn.id = c.node_id AND cn.group_id = ANY($2))) \
             ORDER BY (s.controller_id = a.owner_controller_id) DESC NULLS LAST, s.last_seen DESC",
        )
        .bind(&ids)
        .bind(groups.map(<[Uuid]>::to_vec))
        .fetch_all(&self.pool)
        .await?;
        let mut by_ap: HashMap<Uuid, Vec<SightingRow>> = HashMap::new();
        for row in sightings {
            let state: String = row.try_get("state")?;
            by_ap
                .entry(row.try_get("ap_id")?)
                .or_default()
                .push(SightingRow {
                    controller_node_id: row.try_get("controller_node_id")?,
                    run_state: row.try_get("run_state")?,
                    state: WlanApState::from_token(&state),
                    clients: row.try_get("clients")?,
                    last_seen: row.try_get("last_seen")?,
                    last_associated_at: row.try_get("last_associated_at")?,
                });
        }
        for ap in &mut out {
            ap.sightings = by_ap.remove(&ap.ap_id).unwrap_or_default();
        }
        Ok(out)
    }

    /// The sort key [`Self::list_page`] orders by, for building the next page's cursor.
    #[must_use]
    pub fn sort_key(row: &ApRow) -> String {
        row.name
            .clone()
            .unwrap_or_else(|| row.mac.clone())
            .to_lowercase()
    }

    /// A controller's summary, when it has reported an inventory or been given import settings.
    pub async fn controller(&self, node_id: Uuid) -> anyhow::Result<Option<ControllerRow>> {
        let row = sqlx::query(
            "SELECT node_id, flavor, aps_reported, aps_truncated_at, last_inventory_at, import_aps, \
                    max_aps, ap_group_id, aps_over_cap \
             FROM wireless_controllers WHERE node_id = $1",
        )
        .bind(node_id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|row| {
            let flavor: Option<String> = row.try_get("flavor")?;
            Ok(ControllerRow {
                node_id: row.try_get("node_id")?,
                flavor: flavor.as_deref().and_then(WlanFlavor::from_token),
                aps_reported: row.try_get("aps_reported")?,
                aps_truncated_at: row.try_get("aps_truncated_at")?,
                last_inventory_at: row.try_get("last_inventory_at")?,
                import_aps: row.try_get("import_aps")?,
                max_aps: row.try_get("max_aps")?,
                ap_group_id: row.try_get("ap_group_id")?,
                aps_over_cap: row.try_get("aps_over_cap")?,
            })
        })
        .transpose()
    }

    /// Store how a controller's APs are imported (ADR-064 決定 8).
    ///
    /// Upserts, because an operator may switch import on before the controller's first AP walk has
    /// run — the row the walk would have created is created here instead, and the walk's upsert
    /// leaves these three columns alone. `false` when the node no longer exists.
    pub async fn set_controller_settings(
        &self,
        node_id: Uuid,
        settings: ControllerSettings,
    ) -> anyhow::Result<bool> {
        let written = sqlx::query(
            "INSERT INTO wireless_controllers (id, source, node_id, import_aps, max_aps, ap_group_id) \
             VALUES ($1, 'snmp', $1, $2, $3, $4) \
             ON CONFLICT (id) DO UPDATE SET \
                 import_aps = EXCLUDED.import_aps, \
                 max_aps = EXCLUDED.max_aps, \
                 ap_group_id = EXCLUDED.ap_group_id, \
                 updated_at = now()",
        )
        .bind(node_id)
        .bind(settings.import_aps)
        .bind(settings.max_aps)
        .bind(settings.ap_group_id)
        .execute(&self.pool)
        .await;
        match written {
            Ok(_) => Ok(true),
            Err(sqlx::Error::Database(db)) if db.is_foreign_key_violation() => Ok(false),
            Err(e) => Err(e.into()),
        }
    }

    /// Of `ids`, the nodes that are imported access points — the `wireless_ap` input to
    /// `NodeKind::resolve` for one page of nodes. Empty input asks nothing.
    pub async fn filter_ap_nodes(&self, ids: &[Uuid]) -> anyhow::Result<HashSet<Uuid>> {
        if ids.is_empty() {
            return Ok(HashSet::new());
        }
        let rows = sqlx::query("SELECT node_id FROM wireless_aps WHERE node_id = ANY($1)")
            .bind(ids)
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter()
            .map(|r| Ok(r.try_get::<Uuid, _>("node_id")?))
            .collect()
    }

    /// Every imported AP's node — the scheduler's once-per-round preload, so no AP node is polled
    /// per node (an AP is answered for by its controller, and its address is often `0.0.0.0`).
    pub async fn ap_node_ids(&self) -> anyhow::Result<HashSet<Uuid>> {
        let rows = sqlx::query("SELECT node_id FROM wireless_aps WHERE node_id IS NOT NULL")
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter()
            .map(|r| Ok(r.try_get::<Uuid, _>("node_id")?))
            .collect()
    }

    /// Whether `node` is an imported access point.
    pub async fn is_ap_node(&self, node: Uuid) -> anyhow::Result<bool> {
        Ok(self.filter_ap_nodes(&[node]).await?.contains(&node))
    }

    /// Every imported AP with its node and serving controller — what the ingest fan-out restores
    /// before it publishes anything, and refreshes while it runs.
    pub async fn ap_bindings(&self) -> anyhow::Result<Vec<ApBinding>> {
        let rows = sqlx::query(
            "SELECT ap_id, node_id, owner_controller_id, last_associated_at \
             FROM wireless_aps WHERE node_id IS NOT NULL",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| {
                let owner: Option<Uuid> = row.try_get("owner_controller_id")?;
                let associated: Option<DateTime<Utc>> = row.try_get("last_associated_at")?;
                Ok(ApBinding {
                    ap_id: row.try_get("ap_id")?,
                    node_id: row.try_get("node_id")?,
                    owner: owner
                        .zip(associated)
                        .map(|(controller, last_associated_at)| Ownership {
                            controller,
                            last_associated_at,
                        }),
                })
            })
            .collect()
    }

    /// One pass of the importer (ADR-064 決定 8, 改訂 R7): every AP that has **ever** been in
    /// service, is reported by a controller with import switched on, and has never been imported
    /// becomes a node — up to each controller's cap.
    ///
    /// * **Which controller files it**: the one serving it when that one imports, otherwise the
    ///   first reporting controller that does. So an HA pair with import on for both puts each AP
    ///   under the member serving it, once.
    /// * **An AP someone deleted stays deleted**: `imported_at` outlives the node (`ON DELETE SET
    ///   NULL`), and an AP that carries it is never picked again. `POST …/import` is the way back.
    /// * **The cap is shown, never silent**: an AP left out is counted into the controller's
    ///   `aps_over_cap`, which the controller's page reads.
    pub async fn import_pending(&self, now: DateTime<Utc>) -> anyhow::Result<ImportPass> {
        let candidates = sqlx::query(
            "SELECT a.ap_id, c.controller_id, c.max_aps \
             FROM wireless_aps a \
             JOIN LATERAL ( \
                 SELECT wc.id AS controller_id, wc.max_aps \
                 FROM wireless_ap_sightings s \
                 JOIN wireless_controllers wc ON wc.id = s.controller_id \
                 WHERE s.ap_id = a.ap_id AND wc.import_aps AND wc.node_id IS NOT NULL \
                 ORDER BY (wc.id = a.owner_controller_id) DESC NULLS LAST, wc.id \
                 LIMIT 1) c ON TRUE \
             WHERE a.node_id IS NULL AND a.imported_at IS NULL \
               AND a.last_associated_at IS NOT NULL \
             ORDER BY c.controller_id, lower(COALESCE(a.name, a.mac)), a.ap_id",
        )
        .fetch_all(&self.pool)
        .await?;
        let already: HashMap<Uuid, i64> = sqlx::query(
            "SELECT s.controller_id, count(*) AS n \
             FROM wireless_ap_sightings s \
             JOIN wireless_aps a ON a.ap_id = s.ap_id \
             WHERE a.node_id IS NOT NULL \
             GROUP BY s.controller_id",
        )
        .fetch_all(&self.pool)
        .await?
        .into_iter()
        .map(|row| Ok((row.try_get("controller_id")?, row.try_get("n")?)))
        .collect::<anyhow::Result<_>>()?;

        let mut pass = ImportPass::default();
        let mut used: HashMap<Uuid, i64> = HashMap::new();
        let mut over: HashMap<Uuid, i32> = HashMap::new();
        for row in candidates {
            let ap: Uuid = row.try_get("ap_id")?;
            let controller: Uuid = row.try_get("controller_id")?;
            let max_aps: i32 = row.try_get("max_aps")?;
            let taken = used
                .entry(controller)
                .or_insert_with(|| already.get(&controller).copied().unwrap_or(0));
            if *taken >= i64::from(max_aps) {
                *over.entry(controller).or_default() += 1;
                pass.over_cap += 1;
                continue;
            }
            if matches!(
                self.import_ap(ap, controller, now, None).await?,
                Filed::Imported
            ) {
                *taken += 1;
                pass.imported += 1;
            }
        }
        let (ids, counts): (Vec<Uuid>, Vec<i32>) = over.into_iter().unzip();
        sqlx::query(
            "UPDATE wireless_controllers c \
             SET aps_over_cap = COALESCE(u.over, 0), updated_at = now() \
             FROM wireless_controllers wc \
             LEFT JOIN UNNEST($1::uuid[], $2::int4[]) AS u(id, over) ON u.id = wc.id \
             WHERE c.id = wc.id AND c.aps_over_cap IS DISTINCT FROM COALESCE(u.over, 0)",
        )
        .bind(&ids)
        .bind(&counts)
        .execute(&self.pool)
        .await?;
        Ok(pass)
    }

    /// Import one AP by hand, whatever its state, its controller's switch or an earlier deletion —
    /// an operator asking for one AP is the decision the importer otherwise waits for.
    ///
    /// `groups` is the caller's scope, as in [`Self::list_page`]. 🚨 **It narrows both halves of
    /// the choice** (ADR-014): only a controller the caller can see may file the AP, and the folder
    /// that controller files into must be one the caller can see too. Without it, a scoped operator
    /// who sees an HA pair's standby could create a node in the active member's folder — a node
    /// they cannot open, in an inventory whose administrator never asked for it.
    pub async fn import_one(
        &self,
        ap: Uuid,
        groups: Option<&[Uuid]>,
        now: DateTime<Utc>,
    ) -> anyhow::Result<ImportOne> {
        let row = sqlx::query(
            "SELECT a.node_id, c.controller_id \
             FROM wireless_aps a \
             LEFT JOIN LATERAL ( \
                 SELECT wc.id AS controller_id \
                 FROM wireless_ap_sightings s \
                 JOIN wireless_controllers wc ON wc.id = s.controller_id \
                 WHERE s.ap_id = a.ap_id AND wc.node_id IS NOT NULL \
                   AND ($2::UUID[] IS NULL OR EXISTS ( \
                           SELECT 1 FROM nodes cn \
                           WHERE cn.id = wc.node_id AND cn.group_id = ANY($2))) \
                 ORDER BY (wc.id = a.owner_controller_id) DESC NULLS LAST, s.last_seen DESC \
                 LIMIT 1) c ON TRUE \
             WHERE a.ap_id = $1",
        )
        .bind(ap)
        .bind(groups.map(<[Uuid]>::to_vec))
        .fetch_optional(&self.pool)
        .await?;
        let Some(row) = row else {
            return Ok(ImportOne::NotFound);
        };
        if let Some(node) = row.try_get::<Option<Uuid>, _>("node_id")? {
            return Ok(ImportOne::AlreadyImported(node));
        }
        let Some(controller) = row.try_get::<Option<Uuid>, _>("controller_id")? else {
            return Ok(ImportOne::NoController);
        };
        match self.import_ap(ap, controller, now, groups).await? {
            Filed::Imported => return Ok(ImportOne::Imported(ap)),
            Filed::OutOfScope => return Ok(ImportOne::OutOfScope),
            Filed::Gone => {}
        }
        // Lost a race with the importer or another request: report what is there now.
        let node: Option<Uuid> =
            sqlx::query_scalar("SELECT node_id FROM wireless_aps WHERE ap_id = $1")
                .bind(ap)
                .fetch_optional(&self.pool)
                .await?
                .flatten();
        Ok(node.map_or(ImportOne::NotFound, ImportOne::AlreadyImported))
    }

    /// Create AP `ap`'s node, filed for `controller`, in one transaction. [`Filed::Gone`] when the
    /// AP was imported (or its controller removed) since it was picked; [`Filed::OutOfScope`] when
    /// `allowed` is a caller's scope and the destination folder is not in it — decided inside the
    /// transaction, against the settings the insert would use.
    ///
    /// The node's id **is** the AP id — MAC-derived, so the same AP is the same node whichever
    /// controller files it, and a re-import after a deletion brings back its history. Its address is
    /// the AP's, or `0.0.0.0` while the controller reports none; nothing polls it either way.
    ///
    /// It is filed in the controller's `ap_group_id`, else in the folder the controller node is in
    /// (root when it is in none). **This creates no folder**, and it never re-files a node that
    /// already exists — an operator who moves an AP keeps it where they put it.
    async fn import_ap(
        &self,
        ap: Uuid,
        controller: Uuid,
        now: DateTime<Utc>,
        allowed: Option<&[Uuid]>,
    ) -> anyhow::Result<Filed> {
        let mut tx = self.pool.begin().await?;
        let row = sqlx::query(
            "SELECT a.mac, a.name, host(a.ip) AS ip, a.model, wc.flavor, wc.ap_group_id, \
                    n.group_id AS controller_group \
             FROM wireless_aps a \
             JOIN wireless_controllers wc ON wc.id = $2 \
             JOIN nodes n ON n.id = wc.node_id \
             WHERE a.ap_id = $1 AND a.node_id IS NULL \
             FOR UPDATE OF a",
        )
        .bind(ap)
        .bind(controller)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(row) = row else {
            return Ok(Filed::Gone);
        };
        let mac: String = row.try_get("mac")?;
        let name: Option<String> = row.try_get("name")?;
        let ip: Option<String> = row.try_get("ip")?;
        let flavor: Option<String> = row.try_get("flavor")?;
        let folder: Option<Uuid> = match row.try_get::<Option<Uuid>, _>("ap_group_id")? {
            Some(group) => Some(group),
            // Beside the controller, in whatever folder it is in — including none.
            None => row.try_get::<Option<Uuid>, _>("controller_group")?,
        };
        // A scoped caller may only file into a folder it can see — the top level included, which
        // no scope contains (`NodeScope::allows_group(None)` is false for every scoped caller).
        // Dropping `tx` here rolls back and releases the row lock.
        if let Some(groups) = allowed {
            if !folder.is_some_and(|f| groups.contains(&f)) {
                return Ok(Filed::OutOfScope);
            }
        }
        // Appended over the destination folder's whole scope (ADR-162). Left at its DEFAULT 0 an
        // imported AP would sit above every sub-folder of the controller's folder.
        let ap_order = crate::groups::append_base_sql("$7", "");
        sqlx::query(&format!(
            "INSERT INTO nodes \
               (id, name, address, profile_id, vendor, model, group_id, sort_order) \
             VALUES ($1, $2, $3::inet, (SELECT id FROM profiles WHERE id = $4), $5, $6, $7, \
               {ap_order} + 1) \
             ON CONFLICT (id) DO NOTHING"
        ))
        .bind(ap)
        .bind(name.unwrap_or_else(|| mac.clone()))
        .bind(ip.unwrap_or_else(|| "0.0.0.0".to_owned()))
        .bind(wireless_ap_profile_id())
        .bind(
            flavor
                .as_deref()
                .and_then(WlanFlavor::from_token)
                .map(WlanFlavor::vendor),
        )
        .bind(row.try_get::<Option<String>, _>("model")?)
        .bind(folder)
        .execute(&mut *tx)
        .await?;
        sqlx::query("UPDATE wireless_aps SET node_id = $1, imported_at = $2 WHERE ap_id = $1")
            .bind(ap)
            .bind(now)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(Filed::Imported)
    }
}

/// The seed id of the built-in "Wireless AP (via controller)" profile — every imported AP node
/// carries it. `None` only if the profile were dropped from the built-in list, which the ordering
/// test in `yagra-common` would refuse first.
fn wireless_ap_profile_id() -> Option<Uuid> {
    yagra_common::builtin_profiles()
        .iter()
        .position(|p| p.name == yagra_common::WIRELESS_AP_PROFILE)
        .map(|i| crate::seed_ids::SeedRange::Profiles.id(i))
}

#[cfg(test)]
mod tests {
    use super::*;
    use yagra_common::ApMac;

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_800_000_000 + secs, 0).unwrap()
    }

    const ACTIVE: Uuid = Uuid::from_u128(1);
    const STANDBY: Uuid = Uuid::from_u128(2);

    fn stale() -> ChronoDuration {
        ChronoDuration::seconds(OWNER_STALE_AFTER_SECS)
    }

    /// The measured pair, poll by poll: the active says associated, the standby says backup. The
    /// active owns the AP and the standby's word never sets its state.
    #[test]
    fn the_standby_of_a_pair_never_sets_the_state_the_active_reported() {
        let v = ownership(None, ACTIVE, WlanApState::Associated, at(0), stale());
        assert!(v.take_state);
        assert_eq!(v.owner.map(|o| o.controller), Some(ACTIVE));
        let v = ownership(v.owner, STANDBY, WlanApState::Backup, at(10), stale());
        assert!(
            !v.take_state,
            "the standby's zeros must not replace the active's readings"
        );
        assert_eq!(v.owner.map(|o| o.controller), Some(ACTIVE));
        // The order the two reports arrive in does not change the answer.
        let first = ownership(None, STANDBY, WlanApState::Backup, at(0), stale());
        let then = ownership(first.owner, ACTIVE, WlanApState::Associated, at(5), stale());
        let again = ownership(then.owner, STANDBY, WlanApState::Backup, at(10), stale());
        assert!(!again.take_state);
    }

    /// An AP down on both members: the active says so first, and the standby's same answer changes
    /// nothing while the active's ownership is fresh.
    #[test]
    fn an_ap_down_on_both_members_reads_down() {
        let owned = Some(Ownership {
            controller: ACTIVE,
            last_associated_at: at(0),
        });
        let v = ownership(owned, ACTIVE, WlanApState::NotAssociated, at(60), stale());
        assert!(
            v.take_state,
            "the serving controller reporting trouble is believed"
        );
        assert_eq!(
            v.owner, owned,
            "reporting trouble does not give up or refresh ownership"
        );
    }

    /// A switchover: the old standby now says associated, and takes the AP at once.
    #[test]
    fn a_switchover_moves_ownership_on_the_first_associated_report() {
        let owned = Some(Ownership {
            controller: ACTIVE,
            last_associated_at: at(0),
        });
        let v = ownership(owned, STANDBY, WlanApState::Associated, at(30), stale());
        assert!(v.take_state);
        assert_eq!(v.owner.map(|o| o.controller), Some(STANDBY));
        // And the old active, now standby, cannot take the state back.
        let v = ownership(v.owner, ACTIVE, WlanApState::Backup, at(40), stale());
        assert!(!v.take_state);
    }

    /// The serving controller went silent: once its claim is stale, the controller still answering is
    /// believed, so the AP is not frozen at its last good state.
    #[test]
    fn a_stale_owner_no_longer_outranks_the_controller_that_is_answering() {
        let owned = Some(Ownership {
            controller: ACTIVE,
            last_associated_at: at(0),
        });
        let fresh = ownership(
            owned,
            STANDBY,
            WlanApState::NotAssociated,
            at(OWNER_STALE_AFTER_SECS),
            stale(),
        );
        assert!(
            !fresh.take_state,
            "at the edge of the window the owner still stands"
        );
        let late = ownership(
            owned,
            STANDBY,
            WlanApState::NotAssociated,
            at(OWNER_STALE_AFTER_SECS + 1),
            stale(),
        );
        assert!(late.take_state);
        assert_eq!(
            late.owner, owned,
            "ownership changes only on an associated report"
        );
    }

    /// Only a standby is monitored: nobody owns the AP, so what the standby says is what is shown.
    #[test]
    fn a_lone_standby_is_shown_as_it_reports() {
        let v = ownership(None, STANDBY, WlanApState::Backup, at(0), stale());
        assert!(v.take_state);
        assert_eq!(v.owner, None);
    }

    #[test]
    fn the_sort_key_is_the_lowercased_name_or_the_mac() {
        let mut row = ApRow {
            ap_id: Uuid::nil(),
            mac: "aa:bb:cc:dd:ee:ff".into(),
            name: Some("Site-AP-1".into()),
            serial: None,
            model: None,
            sw_version: None,
            ip: None,
            vendor_group: None,
            run_state: "normal".into(),
            state: Some(WlanApState::Associated),
            clients: None,
            node_id: None,
            owner_node_id: None,
            first_seen: at(0),
            last_seen: at(0),
            last_associated_at: None,
            sightings: Vec::new(),
        };
        assert_eq!(WirelessRepo::sort_key(&row), "site-ap-1");
        row.name = None;
        assert_eq!(WirelessRepo::sort_key(&row), "aa:bb:cc:dd:ee:ff");
    }

    // ── Database (ADR-114) ────────────────────────────────────────────────────

    use crate::pgtest;

    fn observation(
        mac: [u8; 6],
        name: &str,
        run_state: &str,
        state: WlanApState,
        clients: u32,
    ) -> WlanApObservation {
        WlanApObservation {
            mac: ApMac::new(mac),
            name: Some(name.to_owned()),
            serial: Some(format!("SN-{name}")),
            model: Some("AirEngine5776-26".into()),
            sw_version: Some("V600R024C00SPC100".into()),
            ip: (state == WlanApState::Associated).then(|| "10.0.0.27".parse().unwrap()),
            vendor_group: Some("default".into()),
            run_state: run_state.to_owned(),
            state,
            clients: Some(clients),
            cpu_pct: Some(if state == WlanApState::Backup { 0 } else { 3 }),
            mem_pct: None,
            temp_c: None,
            cpu_temp_c: None,
            power_state: None,
            radios: Vec::new(),
        }
    }

    fn inventory(aps: Vec<WlanApObservation>) -> WlanInventory {
        WlanInventory::bounded(WlanFlavor::Huawei, aps, 1024)
    }

    fn cisco_inventory(aps: Vec<WlanApObservation>, uptime: Option<u64>) -> WlanInventory {
        let mut inv = WlanInventory::bounded(WlanFlavor::CiscoAirespace, aps, 1024);
        inv.controller_uptime_secs = uptime;
        inv
    }

    /// ADR-064 増分 F, F9, through the database: an AP a Cisco controller served and no longer
    /// lists reads as not associated in the list — without its `last_seen` moving, since nothing
    /// reported it — while an AP another controller serves, and every AP of a table read inside the
    /// grace after a boot, is left exactly as it was. The first assertion is the accepting one.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn an_ap_a_cisco_controller_stops_listing_reads_as_not_associated(pool: sqlx::PgPool) {
        let wlc = pgtest::node(&pool, "wlc01", 1, None).await;
        let other = pgtest::node(&pool, "wlc02", 2, None).await;
        let repo = WirelessRepo::new(pool.clone());
        let grace = yagra_common::WLAN_ABSENCE_GRACE_AFTER_BOOT_SECS;
        let (stays, leaves, moves) = (
            [0, 0x5d, 0x73, 0, 0, 1],
            [0, 0x5d, 0x73, 0, 0, 2],
            [0, 0x5d, 0x73, 0, 0, 3],
        );
        let assoc = |mac, name| observation(mac, name, "associated", WlanApState::Associated, 2);
        repo.record_inventory(
            wlc,
            &cisco_inventory(
                vec![
                    assoc(stays, "ap01"),
                    assoc(leaves, "ap02"),
                    assoc(moves, "ap03"),
                ],
                Some(grace),
            ),
            at(0),
        )
        .await
        .expect("first inventory");
        // The third AP moves to the other controller, which says it serves it.
        repo.record_inventory(
            other,
            &cisco_inventory(vec![assoc(moves, "ap03")], Some(grace)),
            at(5),
        )
        .await
        .expect("the other controller takes the third AP");

        let state_of = |name: &'static str| {
            let repo = &repo;
            async move {
                repo.list_page(None, &ApFilter::default(), None, 10)
                    .await
                    .expect("list")
                    .into_iter()
                    .find(|a| a.name.as_deref() == Some(name))
                    .expect(name)
            }
        };

        // Inside the grace after a boot, a short table changes nothing.
        repo.record_inventory(
            wlc,
            &cisco_inventory(vec![assoc(stays, "ap01")], Some(grace - 1)),
            at(10),
        )
        .await
        .expect("just after boot");
        assert_eq!(state_of("ap02").await.state, Some(WlanApState::Associated));

        repo.record_inventory(
            wlc,
            &cisco_inventory(vec![assoc(stays, "ap01")], Some(grace + 300)),
            at(20),
        )
        .await
        .expect("past the grace");
        let gone = state_of("ap02").await;
        assert_eq!(gone.state, Some(WlanApState::NotAssociated));
        assert_eq!(gone.run_state, ABSENT_RUN_STATE);
        assert_eq!(gone.clients, None);
        assert_eq!(
            gone.last_seen,
            at(0),
            "nothing reported it, so it was last seen when it was"
        );
        assert_eq!(
            gone.owner_node_id,
            Some(wlc),
            "the owner does not move on absence"
        );
        let sighting = gone
            .sightings
            .iter()
            .find(|s| s.controller_node_id == Some(wlc))
            .expect("this controller's sighting");
        assert_eq!(sighting.state, Some(WlanApState::NotAssociated));

        assert_eq!(state_of("ap01").await.state, Some(WlanApState::Associated));
        assert_eq!(
            state_of("ap03").await.state,
            Some(WlanApState::Associated),
            "the other controller serves it; this one's table has no say"
        );

        // Listed again: the controller's own word stands, as always.
        repo.record_inventory(
            wlc,
            &cisco_inventory(
                vec![assoc(stays, "ap01"), assoc(leaves, "ap02")],
                Some(grace + 600),
            ),
            at(30),
        )
        .await
        .expect("back");
        let back = state_of("ap02").await;
        assert_eq!(
            (back.state, back.run_state.as_str()),
            (Some(WlanApState::Associated), "associated")
        );
    }

    /// The pair, end to end through the database: both members report the same two APs, and the list
    /// shows the active's view with both sightings beside it — whichever inventory is written first.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_pair_reports_one_ap_list_with_the_active_view(pool: sqlx::PgPool) {
        let site = pgtest::group(&pool, "site").await;
        let active = pgtest::node(&pool, "wac001", 1, Some(site)).await;
        let standby = pgtest::node(&pool, "wac002", 2, Some(site)).await;
        let repo = WirelessRepo::new(pool.clone());
        let up = [0x54, 0xf6, 0xe2, 0x0a, 0x02, 0x80];
        let down = [0x60, 0x10, 0x9e, 0x0a, 0x03, 0xa0];

        // The standby's inventory lands first — the harder order.
        repo.record_inventory(
            standby,
            &inventory(vec![
                observation(up, "ap-001", "standby", WlanApState::Backup, 5),
                observation(down, "ap-005", "fault", WlanApState::NotAssociated, 0),
            ]),
            at(0),
        )
        .await
        .expect("standby inventory");
        repo.record_inventory(
            active,
            &inventory(vec![
                observation(up, "ap-001", "normal", WlanApState::Associated, 5),
                observation(down, "ap-005", "fault", WlanApState::NotAssociated, 0),
            ]),
            at(5),
        )
        .await
        .expect("active inventory");
        // And the standby again, after the active: it must not take the AP back.
        repo.record_inventory(
            standby,
            &inventory(vec![observation(
                up,
                "ap-001",
                "standby",
                WlanApState::Backup,
                5,
            )]),
            at(10),
        )
        .await
        .expect("standby again");

        let page = repo
            .list_page(None, &ApFilter::default(), None, 10)
            .await
            .expect("list");
        assert_eq!(page.len(), 2, "one row per AP, not per controller");
        let ap = page
            .iter()
            .find(|a| a.name.as_deref() == Some("ap-001"))
            .unwrap();
        assert_eq!(ap.state, Some(WlanApState::Associated));
        assert_eq!(ap.run_state, "normal");
        assert_eq!(ap.owner_node_id, Some(active));
        assert_eq!(
            ap.ip,
            Some("10.0.0.27".parse().unwrap()),
            "the standby's missing address did not blank it"
        );
        assert_eq!(ap.sightings.len(), 2);
        assert_eq!(
            ap.sightings[0].controller_node_id,
            Some(active),
            "the serving controller first"
        );
        assert_eq!(ap.sightings[1].state, Some(WlanApState::Backup));
        let broken = page
            .iter()
            .find(|a| a.name.as_deref() == Some("ap-005"))
            .unwrap();
        assert_eq!(broken.state, Some(WlanApState::NotAssociated));
        assert_eq!(broken.last_associated_at, None, "never in service");

        let summary = repo
            .controller(active)
            .await
            .expect("controller")
            .expect("a row");
        assert_eq!(summary.aps_reported, 2);
        assert_eq!(summary.flavor, Some(WlanFlavor::Huawei));
        assert_eq!(pgtest::rows(&pool, "wireless_ap_sightings").await, 4);
    }

    /// **The scope rule, executed**, and the filters and cursor that share its statement. The
    /// acceptance side first: a predicate that refuses everything reads exactly like one that works.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn the_ap_list_is_scoped_by_the_reporting_controller_and_pages_by_cursor(
        pool: sqlx::PgPool,
    ) {
        let mine = pgtest::group(&pool, "mine").await;
        let theirs = pgtest::group(&pool, "theirs").await;
        let ours = pgtest::node(&pool, "ours", 1, Some(mine)).await;
        let alien = pgtest::node(&pool, "alien", 2, Some(theirs)).await;
        let repo = WirelessRepo::new(pool.clone());
        repo.record_inventory(
            ours,
            &inventory(vec![
                observation(
                    [0, 0, 0, 0, 0, 1],
                    "b-ap",
                    "normal",
                    WlanApState::Associated,
                    1,
                ),
                observation(
                    [0, 0, 0, 0, 0, 2],
                    "a-ap",
                    "fault",
                    WlanApState::NotAssociated,
                    0,
                ),
            ]),
            at(0),
        )
        .await
        .unwrap();
        repo.record_inventory(
            alien,
            &inventory(vec![observation(
                [0, 0, 0, 0, 0, 3],
                "c-ap",
                "normal",
                WlanApState::Associated,
                2,
            )]),
            at(0),
        )
        .await
        .unwrap();

        let all = repo
            .list_page(None, &ApFilter::default(), None, 10)
            .await
            .unwrap();
        assert_eq!(all.len(), 3, "an unrestricted caller sees every AP");
        let names: Vec<_> = all.iter().filter_map(|a| a.name.clone()).collect();
        assert_eq!(names, ["a-ap", "b-ap", "c-ap"], "ordered by name");

        let scoped = repo
            .list_page(Some(&[mine]), &ApFilter::default(), None, 10)
            .await
            .unwrap();
        assert_eq!(
            scoped.len(),
            2,
            "an AP reported only outside the scope was listed"
        );
        assert!(repo
            .list_page(Some(&[]), &ApFilter::default(), None, 10)
            .await
            .unwrap()
            .is_empty());

        let by_controller = ApFilter {
            controller_node: Some(alien),
            ..ApFilter::default()
        };
        assert_eq!(
            repo.list_page(None, &by_controller, None, 10)
                .await
                .unwrap()
                .len(),
            1
        );
        let down = ApFilter {
            state: Some(WlanApState::NotAssociated),
            ..ApFilter::default()
        };
        assert_eq!(
            repo.list_page(None, &down, None, 10).await.unwrap().len(),
            1
        );
        // Literal, not a pattern: `%` matches nothing here.
        let pct = ApFilter {
            search: Some("%".into()),
            ..ApFilter::default()
        };
        assert!(repo
            .list_page(None, &pct, None, 10)
            .await
            .unwrap()
            .is_empty());
        let by_mac = ApFilter {
            search: Some("00:00:00:00:00:03".into()),
            ..ApFilter::default()
        };
        assert_eq!(
            repo.list_page(None, &by_mac, None, 10).await.unwrap().len(),
            1
        );

        // ⚠️ Bounded: a cursor that stopped being applied would return the first row forever.
        let mut seen = Vec::new();
        let mut after: Option<(String, Uuid)> = None;
        for _ in 0..8 {
            let page = repo
                .list_page(None, &ApFilter::default(), after.clone(), 1)
                .await
                .unwrap();
            let Some(row) = page.first() else { break };
            assert_eq!(page.len(), 1, "LIMIT is not being applied");
            seen.push(row.name.clone().unwrap());
            after = Some((WirelessRepo::sort_key(row), row.ap_id));
        }
        assert_eq!(seen, ["a-ap", "b-ap", "c-ap"]);
    }

    /// A controller deleted between its poll and the write drops the inventory without an error, and
    /// an AP a controller stops reporting keeps its row (決定 10).
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_vanished_controller_or_ap_deletes_nothing_and_fails_nothing(pool: sqlx::PgPool) {
        let repo = WirelessRepo::new(pool.clone());
        let gone = Uuid::new_v4();
        repo.record_inventory(
            gone,
            &inventory(vec![observation(
                [1, 1, 1, 1, 1, 1],
                "x",
                "normal",
                WlanApState::Associated,
                1,
            )]),
            at(0),
        )
        .await
        .expect("a deleted controller is not an error");
        assert_eq!(pgtest::rows(&pool, "wireless_aps").await, 0);

        let wac = pgtest::node(&pool, "wac", 3, None).await;
        repo.record_inventory(
            wac,
            &inventory(vec![observation(
                [2, 2, 2, 2, 2, 2],
                "kept",
                "normal",
                WlanApState::Associated,
                1,
            )]),
            at(0),
        )
        .await
        .unwrap();
        repo.record_inventory(wac, &inventory(Vec::new()), at(300))
            .await
            .unwrap();
        assert_eq!(
            pgtest::rows(&pool, "wireless_aps").await,
            1,
            "an unreported AP was deleted"
        );
        let summary = repo.controller(wac).await.unwrap().unwrap();
        assert_eq!(summary.aps_reported, 0);
    }

    /// The importer (ADR-064 決定 8, 改訂 R7 and R22): on by default once a controller's first
    /// inventory arrives; nothing while import is off; with it on, only APs that have been in
    /// service, up to the cap, with the rest counted; each under the member of a pair that serves
    /// it; and an AP whose node someone deleted is never brought back.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn the_importer_takes_in_service_aps_up_to_the_cap_and_never_undoes_a_deletion(
        pool: sqlx::PgPool,
    ) {
        let site = pgtest::group(&pool, "site").await;
        let active = pgtest::node(&pool, "wac001", 1, Some(site)).await;
        let standby = pgtest::node(&pool, "wac002", 2, Some(site)).await;
        let repo = WirelessRepo::new(pool.clone());
        let up = |n: u8, name: &str| {
            observation(
                [0, 0, 0, 0, 1, n],
                name,
                "normal",
                WlanApState::Associated,
                1,
            )
        };
        let backup = |n: u8, name: &str| {
            observation([0, 0, 0, 0, 1, n], name, "standby", WlanApState::Backup, 1)
        };
        let fault = observation(
            [0, 0, 0, 0, 2, 1],
            "never",
            "fault",
            WlanApState::NotAssociated,
            0,
        );
        repo.record_inventory(
            standby,
            &inventory(vec![
                backup(1, "a"),
                backup(2, "b"),
                backup(3, "c"),
                fault.clone(),
            ]),
            at(0),
        )
        .await
        .unwrap();
        repo.record_inventory(
            active,
            &inventory(vec![up(1, "a"), up(2, "b"), up(3, "c"), fault]),
            at(1),
        )
        .await
        .unwrap();

        // Registered by its first inventory, a controller imports by default (ADR-064 R22). That is
        // the column default doing it — `record_inventory` never names the column.
        for node in [active, standby] {
            assert!(
                repo.controller(node).await.unwrap().unwrap().import_aps,
                "a controller's first inventory leaves import on"
            );
        }
        // Switched off, nothing becomes a node.
        for node in [active, standby] {
            assert!(repo
                .set_controller_settings(
                    node,
                    ControllerSettings {
                        import_aps: false,
                        max_aps: 2,
                        ap_group_id: None,
                    },
                )
                .await
                .unwrap());
        }
        assert_eq!(
            repo.import_pending(at(2)).await.unwrap(),
            ImportPass::default()
        );
        assert_eq!(
            pgtest::rows(&pool, "nodes").await,
            2,
            "nothing is imported while import is off"
        );

        for node in [active, standby] {
            assert!(repo
                .set_controller_settings(
                    node,
                    ControllerSettings {
                        import_aps: true,
                        max_aps: 2,
                        ap_group_id: None,
                    },
                )
                .await
                .unwrap());
        }
        let pass = repo.import_pending(at(3)).await.unwrap();
        assert_eq!(
            pass,
            ImportPass {
                imported: 2,
                over_cap: 1
            },
            "three in service, cap two; the never-in-service AP is not a candidate"
        );
        let ap_nodes = repo.ap_node_ids().await.unwrap();
        assert_eq!(ap_nodes.len(), 2);
        assert_eq!(
            repo.controller(active).await.unwrap().unwrap().aps_over_cap,
            1
        );
        // Filed in the controller's own folder, and no folder was created for them: a name taken
        // from one member of a pair would be wrong after a switchover (ADR-064 B2 の手直し).
        let folders: Vec<Option<Uuid>> =
            sqlx::query_scalar("SELECT DISTINCT group_id FROM nodes WHERE id = ANY($1)")
                .bind(ap_nodes.iter().copied().collect::<Vec<_>>())
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(folders, vec![Some(site)]);
        assert_eq!(
            pgtest::rows(&pool, "node_groups").await,
            1,
            "the importer invented a folder"
        );
        let bindings = repo.ap_bindings().await.unwrap();
        assert!(
            bindings
                .iter()
                .all(|b| b.owner.map(|o| o.controller) == Some(active)),
            "{bindings:?}"
        );

        // Delete one imported AP's node and raise the cap: the other two come in, the deleted
        // one does not.
        let deleted = *ap_nodes.iter().next().unwrap();
        sqlx::query("DELETE FROM nodes WHERE id = $1")
            .bind(deleted)
            .execute(&pool)
            .await
            .unwrap();
        repo.set_controller_settings(
            active,
            ControllerSettings {
                import_aps: true,
                max_aps: 1024,
                ap_group_id: None,
            },
        )
        .await
        .unwrap();
        let pass = repo.import_pending(at(4)).await.unwrap();
        assert_eq!(pass.imported, 1, "only the AP the cap left out");
        let now_nodes = repo.ap_node_ids().await.unwrap();
        assert!(!now_nodes.contains(&deleted), "a deleted AP node came back");
        assert_eq!(
            repo.controller(active).await.unwrap().unwrap().aps_over_cap,
            0
        );
        // …but asking for it by hand does.
        assert_eq!(
            repo.import_one(deleted, None, at(5)).await.unwrap(),
            ImportOne::Imported(deleted)
        );
        assert!(repo.is_ap_node(deleted).await.unwrap());
    }

    /// An imported AP's node follows the controller's name for it until someone renames the node,
    /// and follows its address from the report that stands — never blanking it.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn an_ap_node_follows_its_name_until_renamed_and_keeps_its_last_address(
        pool: sqlx::PgPool,
    ) {
        let wac = pgtest::node(&pool, "wac", 1, None).await;
        let repo = WirelessRepo::new(pool.clone());
        let mac = [0, 0, 0, 0, 3, 1];
        let ap = ap_id(ApMac::new(mac));
        let mut obs = observation(mac, "old-name", "normal", WlanApState::Associated, 1);
        repo.record_inventory(wac, &inventory(vec![obs.clone()]), at(0))
            .await
            .unwrap();
        assert_eq!(
            repo.import_one(ap, None, at(1)).await.unwrap(),
            ImportOne::Imported(ap)
        );
        let read = |pool: sqlx::PgPool| async move {
            sqlx::query_as::<_, (String, String)>(
                "SELECT name, host(address) FROM nodes WHERE id = $1",
            )
            .bind(ap)
            .fetch_one(&pool)
            .await
            .unwrap()
        };
        assert_eq!(
            read(pool.clone()).await,
            ("old-name".into(), "10.0.0.27".into())
        );

        obs.name = Some("new-name".into());
        obs.ip = Some("10.0.0.28".parse().unwrap());
        repo.record_inventory(wac, &inventory(vec![obs.clone()]), at(300))
            .await
            .unwrap();
        assert_eq!(
            read(pool.clone()).await,
            ("new-name".into(), "10.0.0.28".into())
        );

        // The AP goes down: no address reported, and the node keeps the last one.
        let down = observation(mac, "new-name", "fault", WlanApState::NotAssociated, 0);
        repo.record_inventory(wac, &inventory(vec![down]), at(600))
            .await
            .unwrap();
        assert_eq!(read(pool.clone()).await.1, "10.0.0.28");

        // An operator renames the node: the controller's next rename no longer reaches it.
        sqlx::query("UPDATE nodes SET name = 'lobby' WHERE id = $1")
            .bind(ap)
            .execute(&pool)
            .await
            .unwrap();
        obs.name = Some("renamed-on-controller".into());
        repo.record_inventory(wac, &inventory(vec![obs]), at(900))
            .await
            .unwrap();
        assert_eq!(read(pool).await.0, "lobby");
    }

    /// ADR-179 増分 3: an address a controller reports an AP at names that AP and the controller —
    /// and a caller who cannot see the controller learns only that one exists.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn an_ap_address_names_the_ap_and_a_controller_the_caller_can_see(pool: sqlx::PgPool) {
        let mine = pgtest::group(&pool, "mine").await;
        let theirs = pgtest::group(&pool, "theirs").await;
        let wlc = pgtest::node(&pool, "wlc01", 1, Some(mine)).await;
        let repo = WirelessRepo::new(pool.clone());
        let mac = [0, 0x5d, 0x73, 0, 0, 9];
        repo.record_inventory(
            wlc,
            &cisco_inventory(
                vec![observation(
                    mac,
                    "ap09",
                    "associated",
                    WlanApState::Associated,
                    1,
                )],
                None,
            ),
            at(0),
        )
        .await
        .expect("inventory");
        let at_ap: IpAddr = "10.0.0.27".parse().unwrap();
        let elsewhere: IpAddr = "10.0.0.28".parse().unwrap();

        let all = repo.aps_at(&[at_ap, elsewhere], None).await.expect("aps");
        assert_eq!(all.len(), 1, "only the address an AP is reported at");
        let found = &all[&at_ap];
        assert_eq!(found.ap_id, yagra_common::ap_id(ApMac::new(mac)));
        assert_eq!(found.controller, Some((wlc, "wlc01".to_owned())));
        assert!(!found.imported);

        let scoped = repo.aps_at(&[at_ap], Some(&[mine])).await.expect("aps");
        assert_eq!(scoped[&at_ap].controller, Some((wlc, "wlc01".to_owned())));
        let hidden = repo.aps_at(&[at_ap], Some(&[theirs])).await.expect("aps");
        assert_eq!(
            hidden[&at_ap].controller, None,
            "the controller is not named"
        );
        assert_eq!(hidden[&at_ap].ap_id, found.ap_id);
    }
}
