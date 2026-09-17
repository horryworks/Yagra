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
use std::collections::HashMap;
use std::net::IpAddr;
use uuid::Uuid;
use yagra_common::{ap_id, WlanApObservation, WlanApState, WlanFlavor, WlanInventory};

/// How long a controller's report that it serves an AP stays authoritative without being repeated.
///
/// Three polls at the default five-minute interval. Past it, another controller saying the AP is not
/// associated is believed — which is what lets an AP whose serving controller went silent be shown as
/// down by the one that is still answering, rather than frozen at its last good state.
pub const OWNER_STALE_AFTER_SECS: i64 = 15 * 60;

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

/// What a controller's last complete inventory said about itself.
#[derive(Debug, Clone, PartialEq)]
pub struct ControllerRow {
    pub node_id: Uuid,
    pub flavor: Option<WlanFlavor>,
    pub aps_reported: i32,
    pub aps_truncated_at: Option<i32>,
    pub last_inventory_at: Option<DateTime<Utc>>,
}

/// Filters for [`WirelessRepo::list_page`].
#[derive(Debug, Clone, Default)]
pub struct ApFilter {
    /// Only APs this controller node reports.
    pub controller_node: Option<Uuid>,
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

impl WirelessRepo {
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
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
        if aps.is_empty() {
            tx.commit().await?;
            return Ok(());
        }

        let ids: Vec<Uuid> = aps.iter().map(|a| ap_id(a.mac)).collect();
        let current_rows = sqlx::query(
            "SELECT ap_id, owner_controller_id, last_associated_at, run_state, state, clients \
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
            col_ip.push(ap.ip.map(|ip| ip.to_string()));
            col_group.push(ap.vendor_group.clone());
            match (verdict.take_state, known) {
                (false, Some(c)) => {
                    col_run_state.push(c.run_state.clone());
                    col_state.push(c.state.clone());
                    col_clients.push(c.clients);
                }
                _ => {
                    col_run_state.push(ap.run_state.clone());
                    col_state.push(ap.state.as_str().to_owned());
                    col_clients.push(clients);
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
                    a.vendor_group, a.run_state, a.state, a.clients, a.node_id, \
                    oc.node_id AS owner_node_id, a.first_seen, a.last_seen, a.last_associated_at \
             FROM wireless_aps a \
             LEFT JOIN wireless_controllers oc ON oc.id = a.owner_controller_id \
             WHERE ($1::UUID[] IS NULL OR EXISTS ( \
                       SELECT 1 FROM wireless_ap_sightings s \
                       JOIN wireless_controllers c ON c.id = s.controller_id \
                       JOIN nodes n ON n.id = c.node_id \
                       WHERE s.ap_id = a.ap_id AND n.group_id = ANY($1))) \
               AND ($2::UUID IS NULL OR EXISTS ( \
                       SELECT 1 FROM wireless_ap_sightings s \
                       JOIN wireless_controllers c ON c.id = s.controller_id \
                       WHERE s.ap_id = a.ap_id AND c.node_id = $2)) \
               AND ($3::TEXT IS NULL OR a.state = $3) \
               AND ($4::TEXT IS NULL \
                    OR strpos(lower(COALESCE(a.name, '')), lower($4)) > 0 \
                    OR strpos(a.mac, lower($4)) > 0 \
                    OR strpos(COALESCE(host(a.ip), ''), $4) > 0 \
                    OR strpos(lower(COALESCE(a.model, '')), lower($4)) > 0) \
               AND ($5::TEXT IS NULL OR (lower(COALESCE(a.name, a.mac)), a.ap_id) > ($5, $6)) \
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
             ORDER BY (s.controller_id = a.owner_controller_id) DESC NULLS LAST, s.last_seen DESC",
        )
        .bind(&ids)
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

    /// A controller's summary, when it has reported an inventory.
    pub async fn controller(&self, node_id: Uuid) -> anyhow::Result<Option<ControllerRow>> {
        let row = sqlx::query(
            "SELECT node_id, flavor, aps_reported, aps_truncated_at, last_inventory_at \
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
            })
        })
        .transpose()
    }
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
        }
    }

    fn inventory(aps: Vec<WlanApObservation>) -> WlanInventory {
        WlanInventory::bounded(WlanFlavor::Huawei, aps, 1024)
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
        let up = [0x54, 0xf6, 0xe2, 0x83, 0x50, 0x80];
        let down = [0x60, 0x10, 0x9e, 0x1e, 0xfc, 0xa0];

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
}
