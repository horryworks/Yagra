// SPDX-License-Identifier: AGPL-3.0-only
//! Cisco Meraki orchestration: org/device/network persistence, the collect-job builder, and the
//! read-only API-key resolver.
//!
//! A Meraki organization ([`MerakiOrg`]) is the org-scoped polling + rate-limit unit; its devices
//! are ordinary nodes discriminated by a `meraki_devices` row (mirroring the url-check pattern).
//! Metadata, so it all lives in PostgreSQL (store separation). Runtime `sqlx::query` (not the
//! compile-time macro) so the build needs no live database — consistent with [`crate::repo`].
//!
//! The integration is strictly **read-only**: this module only resolves/inlines the API key and
//! shapes jobs; every byte of Meraki I/O goes through `yagra_transport::meraki` (GET-only).

use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, Ipv4Addr};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use sqlx::{PgPool, Row};
use uuid::Uuid;
use yagra_bus::{MerakiCollectCheck, MerakiDeviceRef};
use yagra_common::{MerakiDeviceConfig, MerakiTier};

use crate::meraki_filing::{Filing, MerakiFiled};
use crate::secrets::{CredentialStore, MerakiApiSecret, KIND_MERAKI_API};

/// Default page-size cap sent to paginated Dashboard endpoints.
const DEFAULT_PER_PAGE: u32 = 1000;
/// Default per-request timeout for a collect job (ms).
const DEFAULT_COLLECT_TIMEOUT_MS: u32 = 30_000;

/// Fixed namespace for deriving stable (idempotent) Meraki group ids via UUIDv5, so re-import /
/// re-sync never duplicates the org→network group tree.
const MERAKI_GROUP_NS: Uuid = Uuid::from_u128(0x6d65_7261_6b69_0000_0000_0000_0000_0001);

/// The deterministic HostTree root group id for an org (so create + import agree).
#[must_use]
pub fn org_group_id(org_uuid: Uuid) -> Uuid {
    Uuid::new_v5(&MERAKI_GROUP_NS, org_uuid.as_bytes())
}

/// The deterministic group id for a network within an org.
#[must_use]
pub fn network_group_id(org_uuid: Uuid, network_id: &str) -> Uuid {
    Uuid::new_v5(
        &MERAKI_GROUP_NS,
        format!("{org_uuid}:{network_id}").as_bytes(),
    )
}

/// Namespace for the node ids below. Its own, so a node id can never equal a folder id whatever a
/// serial happens to spell.
const MERAKI_NODE_NS: Uuid = Uuid::from_u128(0x6d65_7261_6b69_0000_0000_0000_0000_0002);

/// The node id a Meraki device gets when it is imported (ADR-164) — derived from its serial alone.
///
/// A node's id is the key of everything Yagra remembers about it outside PostgreSQL: its series in
/// the TSDB, its alert history. A random id meant that deleting a device and importing it again
/// produced a stranger with an empty past. Keyed by the serial, the same device comes back as
/// itself — the choice ADR-064 made for an access point.
///
/// 🚨 **The organization is deliberately not part of it.** A serial is unique across all of Meraki,
/// and an organization's own id is random per registration, so including it would lose the history
/// on exactly the day someone removes an organization and adds it back.
///
/// ⚠️ Nodes imported before this existed keep their random ids; nothing re-keys them.
#[must_use]
pub fn device_node_id(serial: &str) -> Uuid {
    Uuid::new_v5(&MERAKI_NODE_NS, serial.as_bytes())
}

/// A Cisco Meraki organization row (the polling + rate-limit unit).
#[derive(Debug, Clone)]
pub struct MerakiOrg {
    pub id: Uuid,
    pub org_id: String,
    pub name: String,
    pub base_url: String,
    pub credential_id: Uuid,
    pub availability_secs: u32,
    pub uplink_secs: u32,
    pub traffic_secs: u32,
    pub inventory_secs: u32,
    pub enabled_tiers: Vec<String>,
    pub target_rps: f64,
    pub group_id: Option<Uuid>,
    pub enabled: bool,
    /// When the last **successful** inventory sync ran. A failure never moves it (ADR-164 決定 3).
    pub last_sync_at: Option<chrono::DateTime<chrono::Utc>>,
    /// `None` until a sync has run: "has not synced yet" is not "failed".
    pub last_sync_ok: Option<bool>,
    /// Why the last sync failed — a [`crate::meraki_sync::MerakiSyncFailure`] token, never upstream
    /// text. `None` after a success.
    pub last_sync_error: Option<String>,
    /// Whether the sync turns newly listed devices into nodes (ADR-164 Inc.4, migration 0125).
    /// New organizations start on; the ones that existed before the switch start off.
    pub import_devices: bool,
    /// Whether an imported device is filed by its address into the folder whose IP range holds it.
    pub file_by_prefix: bool,
    /// The most nodes automatic import lets this organization hold.
    pub max_devices: u32,
    /// How many devices that cap left out on the last sync.
    pub devices_over_cap: u32,
    /// The collect tiers that are failing right now, as the health loop last wrote them
    /// (migration 0127, ADR-164 決定 18). Empty means none is *known* to be failing.
    pub collect_failures: Vec<crate::meraki_health::TierFailure>,
}

impl MerakiOrg {
    fn from_row(row: &sqlx::postgres::PgRow) -> anyhow::Result<Self> {
        let availability_secs: i32 = row.try_get("availability_secs")?;
        let uplink_secs: i32 = row.try_get("uplink_secs")?;
        let traffic_secs: i32 = row.try_get("traffic_secs")?;
        let inventory_secs: i32 = row.try_get("inventory_secs")?;
        let max_devices: i32 = row.try_get("max_devices")?;
        let devices_over_cap: i32 = row.try_get("devices_over_cap")?;
        Ok(Self {
            id: row.try_get("id")?,
            org_id: row.try_get("org_id")?,
            name: row.try_get("name")?,
            base_url: row.try_get("base_url")?,
            credential_id: row.try_get("credential_id")?,
            availability_secs: availability_secs.max(0) as u32,
            uplink_secs: uplink_secs.max(0) as u32,
            traffic_secs: traffic_secs.max(0) as u32,
            inventory_secs: inventory_secs.max(0) as u32,
            enabled_tiers: row.try_get("enabled_tiers")?,
            target_rps: row.try_get("target_rps")?,
            group_id: row.try_get("group_id")?,
            enabled: row.try_get("enabled")?,
            last_sync_at: row.try_get("last_sync_at")?,
            last_sync_ok: row.try_get("last_sync_ok")?,
            last_sync_error: row.try_get("last_sync_error")?,
            import_devices: row.try_get("import_devices")?,
            file_by_prefix: row.try_get("file_by_prefix")?,
            max_devices: max_devices.max(0) as u32,
            devices_over_cap: devices_over_cap.max(0) as u32,
            // Read leniently: an entry a newer core wrote with a tier or a shape this build does
            // not know costs that entry, never the organization (the row is read by the
            // scheduler, and failing it here would stop the collects it describes).
            collect_failures: crate::meraki_health::TierFailure::from_stored(
                row.try_get::<sqlx::types::Json<serde_json::Value>, _>("collect_failures")?
                    .0,
            ),
        })
    }

    /// The enabled tiers parsed to [`MerakiTier`] (unknown tokens skipped). Inventory is never a
    /// recurring collect tier (reconciliation is operator-initiated), so it is filtered out here.
    #[must_use]
    pub fn active_tiers(&self) -> Vec<MerakiTier> {
        self.enabled_tiers
            .iter()
            .filter_map(|t| MerakiTier::from_token(t))
            .filter(|t| *t != MerakiTier::Inventory)
            .collect()
    }

    /// The cadence (seconds) for a tier.
    #[must_use]
    pub fn tier_cadence(&self, tier: MerakiTier) -> u32 {
        match tier {
            MerakiTier::Availability => self.availability_secs,
            MerakiTier::Uplink => self.uplink_secs,
            MerakiTier::Traffic => self.traffic_secs,
            MerakiTier::Inventory => self.inventory_secs,
        }
    }
}

/// Build the collect-job check for `(org, tier)` given the resolved key, the serial→node_id map,
/// and the in-scope networks. Pure — unit-tested without a database.
#[must_use]
pub fn build_collect_check(
    org: &MerakiOrg,
    tier: MerakiTier,
    api_key: String,
    devices: Vec<MerakiDeviceRef>,
    network_ids: Vec<String>,
) -> MerakiCollectCheck {
    MerakiCollectCheck {
        org_id: org.org_id.clone(),
        meraki_org_uuid: org.id,
        tier,
        base_url: org.base_url.clone(),
        api_key,
        devices,
        network_ids,
        per_page: DEFAULT_PER_PAGE,
        target_rps: org.target_rps,
        timeout_ms: DEFAULT_COLLECT_TIMEOUT_MS,
    }
}

/// Why the scheduler sends an organization no collect this tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoCollect {
    /// The organization watches no network, so there is nothing a collect may ask about.
    NothingWatched,
    /// Which networks it watches could not be read.
    Unreadable,
}

/// The networks a collect for one organization may ask about — or why none is sent (ADR-164 決定 16).
///
/// 🚨 **An empty list must never reach the poller.** On the bus an empty `network_ids` means
/// "every network" (`yagra_bus::MerakiCollectCheck`), and a poller from before this change still
/// reads it that way. An organization watching nothing was therefore collected whole, while
/// `MerakiDeviceCounts.monitored_unwatched` and the automatic import both read the same state as
/// "nothing is collected" — the page said "N not collected" about nodes that were being collected.
/// A failed read widened the collect the same way, through `unwrap_or_default()`. Both are now a
/// tick with no collect. Pure — the scheduler loop itself has no test.
pub fn networks_to_collect<E>(watched: Result<Vec<String>, E>) -> Result<Vec<String>, NoCollect> {
    match watched {
        Ok(ids) if ids.is_empty() => Err(NoCollect::NothingWatched),
        Ok(ids) => Ok(ids),
        Err(_) => Err(NoCollect::Unreadable),
    }
}

/// Why a stored credential did not yield a Meraki API key. Carries no secret and no upstream text.
#[derive(Debug)]
pub enum SavedKeyError {
    /// No credential has that id.
    NotFound,
    /// The credential exists and is some other kind — an SNMP community, a NetBox token.
    WrongKind,
    /// It could not be read or unsealed, or what it unsealed to is not a key document.
    Unreadable(anyhow::Error),
}

/// Open a stored credential as a Meraki API key, saying *why* when it is not one.
///
/// 🚨 **The kind is checked before the secret is looked at, and that is the security property.**
/// Whoever calls this goes on to send the result to the Dashboard API as a bearer key. A caller
/// naming some other credential's id must get `WrongKind` back, never that credential's secret on
/// its way to a server (ADR-164 Inc.6, where onboarding began to accept a credential by id).
/// The key is never logged.
pub async fn open_saved_meraki_key(
    creds: &CredentialStore,
    credential_id: Uuid,
) -> Result<String, SavedKeyError> {
    match creds.open(credential_id).await {
        Ok(Some((kind, secret))) if kind == KIND_MERAKI_API => MerakiApiSecret::parse(&secret)
            .map(|s| s.api_key)
            .map_err(|e| SavedKeyError::Unreadable(anyhow::anyhow!("parse meraki key: {e}"))),
        Ok(Some(_)) => Err(SavedKeyError::WrongKind),
        Ok(None) => Err(SavedKeyError::NotFound),
        Err(e) => Err(SavedKeyError::Unreadable(e)),
    }
}

/// Resolve an org's read-only Meraki API key, or `None` on any failure (missing / wrong-kind /
/// unparsable) — the caller then skips dispatch. [`open_saved_meraki_key`] with the reason logged
/// instead of returned, for the two background callers that have nobody to return it to.
pub async fn resolve_meraki_key(creds: &CredentialStore, credential_id: Uuid) -> Option<String> {
    match open_saved_meraki_key(creds, credential_id).await {
        Ok(key) => Some(key),
        Err(SavedKeyError::WrongKind) => {
            tracing::warn!("meraki org credential is not a meraki_api kind");
            None
        }
        Err(SavedKeyError::NotFound) => None,
        Err(SavedKeyError::Unreadable(e)) => {
            tracing::warn!(error = %e, "failed to open meraki credential");
            None
        }
    }
}

/// Per-org single-flight tracker: at most one Dashboard API session outstanding per org so the
/// org's shared API rate budget is never exceeded (the #1 safeguard). Acquired at dispatch and
/// cleared when the collect's first result returns (all fan-out results share the job's id); a
/// lease deadline is the backstop if a result never arrives (poller crash), so an org can't wedge
/// forever. The inventory sync takes the same flight (`meraki_sync.rs`).
///
/// Since ADR-164 決定 18 it also knows **which job, and which tier, holds the flight**, for two
/// reasons:
///
/// - 🚨 **A result may only release the flight its own job took.** The earlier version kept a
///   `job → org` map beside an `org → deadline` one and cleared the org for *any* job it still
///   remembered — so a straggler from a collect whose lease had already run out released the flight
///   of whatever had been dispatched since, and a second session started against the same
///   organization. That was theoretical while the collector was the flight's only user and stopped
///   being so when the sync joined it. (The old `jobs` map also kept one entry forever for every
///   collect that was never answered.)
/// - A collect flight whose lease runs out **unanswered** is evidence: a poller from before the
///   collect report existed fails silently, a pool with no live poller never picks the job up, a
///   poller can crash mid-collect. [`Self::take_unanswered`] hands those to
///   [`crate::meraki_health`], which counts them as failures (`no_answer`).
///
/// It carries the [`crate::meraki_health::MerakiCollectHealth`] record for the same reason it
/// exists at all: it is the one Meraki handle the result-ingest path already holds.
#[derive(Default)]
pub struct MerakiInflight {
    flights: Mutex<HashMap<Uuid, Flight>>, // org → who holds it
    unanswered: Mutex<Vec<(Uuid, MerakiTier)>>,
    /// How each organization's collects have been ending (決定 18).
    pub health: crate::meraki_health::MerakiCollectHealth,
}

#[derive(Debug, Clone, Copy)]
struct Flight {
    job: Uuid,
    /// `None` for the inventory sync: its failures are recorded on the row, not counted here.
    tier: Option<MerakiTier>,
    deadline: Instant,
}

impl MerakiInflight {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Try to mark `org` in flight for the inventory sync's `job_id`. Returns `false` if a session
    /// is already outstanding (and its lease hasn't expired) — the caller then skips this org.
    pub fn acquire(&self, org: Uuid, job_id: Uuid, lease: Duration, now: Instant) -> bool {
        self.take(org, job_id, None, lease, now)
    }

    /// [`Self::acquire`] for a collect of `tier`, which is what makes an unanswered lease evidence.
    pub fn acquire_collect(
        &self,
        org: Uuid,
        job_id: Uuid,
        tier: MerakiTier,
        lease: Duration,
        now: Instant,
    ) -> bool {
        self.take(org, job_id, Some(tier), lease, now)
    }

    fn take(
        &self,
        org: Uuid,
        job: Uuid,
        tier: Option<MerakiTier>,
        lease: Duration,
        now: Instant,
    ) -> bool {
        let mut flights = self.flights.lock().expect("meraki inflight poisoned");
        if let Some(held) = flights.get(&org) {
            if held.deadline > now {
                return false;
            }
            // The lease ran out with no result: whoever held it was never answered.
            if let Some(tier) = held.tier {
                self.unanswered
                    .lock()
                    .expect("meraki unanswered poisoned")
                    .push((org, tier));
            }
        }
        flights.insert(
            org,
            Flight {
                job,
                tier,
                deadline: now + lease,
            },
        );
        true
    }

    /// Release `org`'s flight **if `job_id` is the job that holds it** (called for every poll
    /// result; a no-op for non-Meraki jobs). Returns the organization and the tier that was
    /// answered, so a result from a poller that sends no collect report still counts as an answer.
    pub fn complete(&self, job_id: Uuid) -> Option<(Uuid, Option<MerakiTier>)> {
        let mut flights = self.flights.lock().expect("meraki inflight poisoned");
        let org = flights
            .iter()
            .find_map(|(org, held)| (held.job == job_id).then_some(*org))?;
        let held = flights.remove(&org)?;
        Some((org, held.tier))
    }

    /// The collect flights whose lease has run out with no result, each handed over once.
    pub fn take_unanswered(&self, now: Instant) -> Vec<(Uuid, MerakiTier)> {
        let mut out =
            std::mem::take(&mut *self.unanswered.lock().expect("meraki unanswered poisoned"));
        let mut flights = self.flights.lock().expect("meraki inflight poisoned");
        flights.retain(|org, held| {
            if held.deadline > now {
                return true;
            }
            if let Some(tier) = held.tier {
                out.push((*org, tier));
            }
            false
        });
        out
    }

    /// Whether `org` currently has an unexpired outstanding session.
    #[must_use]
    pub fn is_inflight(&self, org: Uuid, now: Instant) -> bool {
        self.flights
            .lock()
            .expect("meraki inflight poisoned")
            .get(&org)
            .is_some_and(|held| held.deadline > now)
    }
}

/// One device to import, with **where it goes already decided** (ADR-164).
///
/// The caller resolves the profile and the [`Filing`]; [`MerakiOrgRepo::import_devices`] only
/// writes. That split is what lets the manual import and the automatic one be one writer: they
/// differ in which devices they pick, never in what happens to a picked device.
pub struct MerakiImportDevice {
    pub serial: String,
    pub name: String,
    pub model: Option<String>,
    pub product_type: String,
    pub network_id: String,
    pub network_name: String,
    pub lan_ip: Option<IpAddr>,
    pub profile_id: Option<Uuid>,
    pub filing: Filing,
}

/// What one [`MerakiOrgRepo::import_devices`] call did. A device it skipped appears in neither
/// number.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MerakiImportOutcome {
    pub imported: u32,
    pub filed: MerakiFiled,
}

/// PostgreSQL-backed store for Meraki orgs + their network scope + device import.
pub struct MerakiOrgRepo {
    pool: PgPool,
}

impl MerakiOrgRepo {
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    const COLUMNS: &'static str = "id, org_id, name, base_url, credential_id, availability_secs, \
        uplink_secs, traffic_secs, inventory_secs, enabled_tiers, target_rps, group_id, enabled, \
        last_sync_at, last_sync_ok, last_sync_error, import_devices, file_by_prefix, max_devices, \
        devices_over_cap, collect_failures";

    /// Every org (for the Integrations UI).
    pub async fn list(&self) -> anyhow::Result<Vec<MerakiOrg>> {
        let rows = sqlx::query(&format!(
            "SELECT {} FROM meraki_orgs ORDER BY name, id",
            Self::COLUMNS
        ))
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(MerakiOrg::from_row).collect()
    }

    /// Only enabled orgs (for the scheduler).
    pub async fn list_enabled(&self) -> anyhow::Result<Vec<MerakiOrg>> {
        let rows = sqlx::query(&format!(
            "SELECT {} FROM meraki_orgs WHERE enabled = true ORDER BY name, id",
            Self::COLUMNS
        ))
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(MerakiOrg::from_row).collect()
    }

    /// One org by internal id.
    pub async fn get(&self, id: Uuid) -> anyhow::Result<Option<MerakiOrg>> {
        let row = sqlx::query(&format!(
            "SELECT {} FROM meraki_orgs WHERE id = $1",
            Self::COLUMNS
        ))
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        row.as_ref().map(MerakiOrg::from_row).transpose()
    }

    /// Create an org and its HostTree root group (idempotent group id). Returns the new org id.
    pub async fn create(
        &self,
        org_id: &str,
        name: &str,
        base_url: &str,
        credential_id: Uuid,
    ) -> anyhow::Result<Uuid> {
        let id = Uuid::new_v4();
        let group = org_group_id(id);
        let mut tx = self.pool.begin().await?;
        // Root group at the HostTree top level (no parent — single-tenant decision). `sort_order`
        // is appended over the whole top-level scope (ADR-162): left at its DEFAULT 0 the org
        // folder would sit above everything an operator has arranged there.
        let order = crate::groups::append_base_sql("NULL", "");
        sqlx::query(&format!(
            "INSERT INTO node_groups (id, name, group_type, parent_id, sort_order) \
             VALUES ($1, $2, 'region', NULL, {order} + 1) ON CONFLICT (id) DO NOTHING"
        ))
        .bind(group)
        .bind(name)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO meraki_orgs (id, org_id, name, base_url, credential_id, group_id) \
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(id)
        .bind(org_id)
        .bind(name)
        .bind(base_url)
        .bind(credential_id)
        .bind(group)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(id)
    }

    /// Enable/disable an org (pause without losing config/history). Returns whether it exists.
    pub async fn set_enabled(&self, id: Uuid, enabled: bool) -> anyhow::Result<bool> {
        let res =
            sqlx::query("UPDATE meraki_orgs SET enabled = $2, updated_at = now() WHERE id = $1")
                .bind(id)
                .bind(enabled)
                .execute(&self.pool)
                .await?;
        Ok(res.rows_affected() > 0)
    }

    /// Update per-tier cadence, enabled tiers, and the rate budget. Returns whether it exists.
    #[allow(clippy::too_many_arguments)]
    pub async fn update_cadence(
        &self,
        id: Uuid,
        availability_secs: i32,
        uplink_secs: i32,
        traffic_secs: i32,
        inventory_secs: i32,
        enabled_tiers: &[String],
        target_rps: f64,
    ) -> anyhow::Result<bool> {
        let res = sqlx::query(
            "UPDATE meraki_orgs SET availability_secs = $2, uplink_secs = $3, traffic_secs = $4, \
             inventory_secs = $5, enabled_tiers = $6, target_rps = $7, updated_at = now() \
             WHERE id = $1",
        )
        .bind(id)
        .bind(availability_secs)
        .bind(uplink_secs)
        .bind(traffic_secs)
        .bind(inventory_secs)
        .bind(enabled_tiers)
        .bind(target_rps)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected() > 0)
    }

    /// Record a **successful** inventory sync.
    ///
    /// 🚨 There is deliberately no `ok` flag that could be passed `false` beside a timestamp — the
    /// shape `NetboxRepo::record_success` has, for the same reason. `last_sync_at` is what decides
    /// when the next sync is due and what the row shows as "last sync", and it moves only here.
    ///
    /// This replaced `touch_sync`, which the import wizard's enumerate called after a read that
    /// could have been cut short. Stamping "synced" on that was harmless while nothing read the
    /// column; it would now postpone the real sync by a whole interval.
    pub async fn record_sync_success(&self, id: Uuid) -> anyhow::Result<()> {
        sqlx::query(
            "UPDATE meraki_orgs SET last_sync_at = now(), last_sync_ok = TRUE, \
                    last_sync_error = NULL, updated_at = now() WHERE id = $1",
        )
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Record a **failed** inventory sync: the reason, and nothing else. `last_sync_at` stays where
    /// the last success left it. `reason` is a `MerakiSyncFailure` token.
    pub async fn record_sync_failure(&self, id: Uuid, reason: &str) -> anyhow::Result<()> {
        sqlx::query(
            "UPDATE meraki_orgs SET last_sync_ok = FALSE, last_sync_error = $2, \
                    updated_at = now() WHERE id = $1",
        )
        .bind(id)
        .bind(reason)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Set how the sync imports this organization's devices (ADR-164 Inc.4). Returns whether the
    /// organization exists.
    ///
    /// Switching the import **off** also clears `devices_over_cap`: that number is a statement about
    /// the last import pass, and with no pass running it would stay on the page forever, describing
    /// a cap nothing is applying.
    pub async fn set_import_settings(
        &self,
        id: Uuid,
        import_devices: bool,
        file_by_prefix: bool,
        max_devices: i32,
    ) -> anyhow::Result<bool> {
        let res = sqlx::query(
            "UPDATE meraki_orgs SET import_devices = $2, file_by_prefix = $3, max_devices = $4, \
                    devices_over_cap = CASE WHEN $2 THEN devices_over_cap ELSE 0 END, \
                    updated_at = now() \
             WHERE id = $1",
        )
        .bind(id)
        .bind(import_devices)
        .bind(file_by_prefix)
        .bind(max_devices)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected() > 0)
    }

    /// Write back how many devices the cap left out on this sync. Returns how many rows changed —
    /// zero on the ordinary sync, where the number is what it already was (ADR-164 決定 4: a sync
    /// that finds nothing changed writes nothing).
    pub async fn record_over_cap(&self, id: Uuid, over: u32) -> anyhow::Result<u64> {
        let res = sqlx::query(
            "UPDATE meraki_orgs SET devices_over_cap = $2, updated_at = now() \
             WHERE id = $1 AND devices_over_cap IS DISTINCT FROM $2",
        )
        .bind(id)
        .bind(i32::try_from(over).unwrap_or(i32::MAX))
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected())
    }

    /// Fully remove an org: delete its device **nodes** (which cascades their `meraki_devices`
    /// rows), the org row (cascades its network scope), and its HostTree groups (root + per-network),
    /// all in one transaction. Returns whether the org existed. Metrics history in the TSDB is left
    /// (rebuildable / harmless); config is gone.
    pub async fn purge(&self, id: Uuid) -> anyhow::Result<bool> {
        let group = org_group_id(id);
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "DELETE FROM nodes WHERE id IN (SELECT node_id FROM meraki_devices WHERE org_id = $1)",
        )
        .bind(id)
        .execute(&mut *tx)
        .await?;
        let res = sqlx::query("DELETE FROM meraki_orgs WHERE id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        // Root group + its per-network child groups (now empty).
        sqlx::query("DELETE FROM node_groups WHERE id = $1 OR parent_id = $1")
            .bind(group)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(res.rows_affected() > 0)
    }

    // ── Network scope ────────────────────────────────────────────────────────────────────

    /// Record the networks a sync saw: insert the new ones, rename the renamed ones, and leave every
    /// other row untouched. Returns how many rows were written.
    ///
    /// The `WHERE` on the conflict arm is what makes an unchanged network a no-op rather than an
    /// update to the same values — this runs every five minutes, and rewriting every row each time
    /// (which the import wizard's own upsert did, to bump `last_seen_at`) is thousands of dead
    /// tuples a day for a table that changes a few times a year (ADR-164 決定 4).
    ///
    /// `watch_new` is the flag a network **seen for the first time** is stored with. It is the
    /// organization's `import_devices`: with automatic import on, a new site is watched from the
    /// sync that finds it (migration 0125, which overturns 0040 on exactly this point).
    /// 🚨 It is only ever the INSERT's value. The conflict arm does not name `monitored`, so a
    /// network an operator took out of scope stays out whatever this argument says.
    pub async fn record_networks(
        &self,
        org_uuid: Uuid,
        networks: &[(String, String)],
        watch_new: bool,
    ) -> anyhow::Result<u64> {
        if networks.is_empty() {
            return Ok(0);
        }
        // One entry per network id: PostgreSQL refuses an `ON CONFLICT DO UPDATE` that would touch
        // the same row twice in one statement, and a listing is not ours to trust.
        let unique: std::collections::BTreeMap<&str, &str> = networks
            .iter()
            .map(|(id, name)| (id.as_str(), name.as_str()))
            .collect();
        let (ids, names): (Vec<&str>, Vec<&str>) = unique.into_iter().unzip();
        let res = sqlx::query(
            "INSERT INTO meraki_org_networks (org_id, network_id, name, monitored, last_seen_at) \
             SELECT $1, t.id, t.name, $4, now() \
             FROM unnest($2::text[], $3::text[]) AS t(id, name) \
             ON CONFLICT (org_id, network_id) DO UPDATE \
               SET name = EXCLUDED.name, last_seen_at = now() \
               WHERE meraki_org_networks.name IS DISTINCT FROM EXCLUDED.name",
        )
        .bind(org_uuid)
        .bind(&ids)
        .bind(&names)
        .bind(watch_new)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected())
    }

    /// Set the `monitored` flag for a set of the org's networks.
    pub async fn set_networks_monitored(
        &self,
        org_uuid: Uuid,
        network_ids: &[String],
        monitored: bool,
    ) -> anyhow::Result<()> {
        if network_ids.is_empty() {
            return Ok(());
        }
        sqlx::query(
            "UPDATE meraki_org_networks SET monitored = $3 \
             WHERE org_id = $1 AND network_id = ANY($2)",
        )
        .bind(org_uuid)
        .bind(network_ids)
        .bind(monitored)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// The org's networks: `(network_id, name, monitored)`.
    pub async fn list_networks(
        &self,
        org_uuid: Uuid,
    ) -> anyhow::Result<Vec<(String, String, bool)>> {
        let rows = sqlx::query(
            "SELECT network_id, name, monitored FROM meraki_org_networks \
             WHERE org_id = $1 ORDER BY name, network_id",
        )
        .bind(org_uuid)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|r| {
                Ok((
                    r.try_get("network_id")?,
                    r.try_get("name")?,
                    r.try_get("monitored")?,
                ))
            })
            .collect()
    }

    /// The org's monitored network ids (in-scope), for narrowing collect API calls.
    /// Write which collect tiers are failing (the health loop, on change only). `false` when the
    /// organization is gone.
    pub async fn record_collect_failures(
        &self,
        org_uuid: Uuid,
        failures: &[crate::meraki_health::TierFailure],
    ) -> anyhow::Result<bool> {
        let res = sqlx::query(
            "UPDATE meraki_orgs SET collect_failures = $2, updated_at = now() \
             WHERE id = $1 AND collect_failures IS DISTINCT FROM $2",
        )
        .bind(org_uuid)
        .bind(sqlx::types::Json(failures))
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected() > 0)
    }

    /// `(organization, its name, its nodes)` for every organization — what the alert engine's
    /// snapshot of Meraki organizations is built from (ADR-164 決定 18). An organization with no
    /// imported device is listed with no nodes: its name is still what an alert about it is
    /// called.
    pub async fn alert_bindings(&self) -> anyhow::Result<Vec<(Uuid, String, Vec<Uuid>)>> {
        let rows = sqlx::query(
            "SELECT o.id, o.name, \
                    COALESCE(array_agg(d.node_id) FILTER (WHERE d.node_id IS NOT NULL), \
                             '{}'::uuid[]) AS nodes \
             FROM meraki_orgs o LEFT JOIN meraki_devices d ON d.org_id = o.id \
             GROUP BY o.id, o.name",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|r| Ok((r.try_get("id")?, r.try_get("name")?, r.try_get("nodes")?)))
            .collect()
    }

    pub async fn monitored_network_ids(&self, org_uuid: Uuid) -> anyhow::Result<Vec<String>> {
        let rows = sqlx::query(
            "SELECT network_id FROM meraki_org_networks WHERE org_id = $1 AND monitored = true",
        )
        .bind(org_uuid)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|r| Ok(r.try_get("network_id")?))
            .collect()
    }

    // ── Import ───────────────────────────────────────────────────────────────────────────

    /// The advisory lock [`Self::import_devices`] serialises on.
    ///
    /// 🚨 Its own key. Not [`crate::leader`]'s, which the leader holds as a *session* lock for its
    /// whole life, and not `NodeRepo`'s import key either — a Meraki device is identified by its
    /// serial and an address import by its address, so the two contend on nothing and sharing a
    /// key would only make one wait for the other. Transaction-scoped, so a failed import cannot
    /// leave it held. (The bytes spell `YAGRMRKI`.)
    const IMPORT_LOCK_KEY: i64 = 0x5941_4752_4d52_4b49;

    /// Turn Meraki devices into nodes, **atomically** — the one writer every import goes through
    /// (ADR-164). Each device arrives with its [`Filing`] decided; this only writes.
    ///
    /// - **A serial that is already a node is skipped, and that is decided in here**, under
    ///   [`Self::IMPORT_LOCK_KEY`]. The caller used to read the bound serials first and filter, so
    ///   two imports of one device both saw it free; the second then hit `meraki_devices.serial`'s
    ///   `UNIQUE` and took the whole batch down with a 500. ⚠️ The read is across **every**
    ///   organization, because that is what the constraint spans.
    /// - **A device goes to the folder its filing names, otherwise under `Organization ▸ Network`.**
    ///   A network's folder is created only when a device actually lands in it, so an organization
    ///   whose devices all match an IP range grows no parallel tree.
    /// - **The organization's own folder is put back if an operator deleted it** — same
    ///   deterministic id, and the row re-pointed at it. Deleting it sets `meraki_orgs.group_id`
    ///   to NULL (`ON DELETE SET NULL`), and until this writer every later device was then filed
    ///   nowhere at all, at the top of the tree, with nothing saying why.
    /// - A matched folder that was deleted between the match and this transaction is read as
    ///   no match, rather than failing the batch on the foreign key.
    /// - The node id comes from [`device_node_id`], and `meraki_inventory.imported_at` is stamped
    ///   in this transaction — so "this device was a node once" cannot be lost to a crash between
    ///   the two, which is the fact that keeps a deleted device from being imported again.
    ///
    /// Nodes carry no per-node credential (the organization owns the key).
    pub async fn import_devices(
        &self,
        org: &MerakiOrg,
        devices: &[MerakiImportDevice],
    ) -> anyhow::Result<MerakiImportOutcome> {
        let mut outcome = MerakiImportOutcome::default();
        if devices.is_empty() {
            return Ok(outcome);
        }
        let mut tx = self.pool.begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(Self::IMPORT_LOCK_KEY)
            .execute(&mut *tx)
            .await?;

        let wanted: Vec<String> = devices.iter().map(|d| d.serial.clone()).collect();
        let mut taken: HashSet<String> =
            sqlx::query_scalar("SELECT serial FROM meraki_devices WHERE serial = ANY($1)")
                .bind(&wanted)
                .fetch_all(&mut *tx)
                .await?
                .into_iter()
                .collect();
        let chosen: Vec<Uuid> = devices.iter().filter_map(|d| d.filing.folder()).collect();
        let standing: HashSet<Uuid> =
            sqlx::query_scalar("SELECT id FROM node_groups WHERE id = ANY($1)")
                .bind(&chosen)
                .fetch_all(&mut *tx)
                .await?
                .into_iter()
                .collect();

        let root = org_group_id(org.id);
        let mut root_ready = false;
        let mut networks_ready: HashSet<Uuid> = HashSet::new();
        let mut created: Vec<String> = Vec::new();

        for d in devices {
            // `insert`, not `contains`: a serial listed twice in one batch is one device too.
            if !taken.insert(d.serial.clone()) {
                continue;
            }
            let filing = match &d.filing {
                Filing::Matched { folder, .. } if !standing.contains(folder) => Filing::Unmatched,
                other => other.clone(),
            };
            let group = match filing.folder() {
                Some(folder) => folder,
                None => {
                    if !root_ready {
                        // Top level, appended over that whole scope (ADR-162). A no-op when the
                        // folder is where `create` put it — or wherever an operator moved it.
                        let order = crate::groups::append_base_sql("NULL", "");
                        sqlx::query(&format!(
                            "INSERT INTO node_groups (id, name, group_type, parent_id, sort_order) \
                             VALUES ($1, $2, 'region', NULL, {order} + 1) \
                             ON CONFLICT (id) DO NOTHING"
                        ))
                        .bind(root)
                        .bind(&org.name)
                        .execute(&mut *tx)
                        .await?;
                        sqlx::query(
                            "UPDATE meraki_orgs SET group_id = $2, updated_at = now() \
                             WHERE id = $1 AND group_id IS DISTINCT FROM $2",
                        )
                        .bind(org.id)
                        .bind(root)
                        .execute(&mut *tx)
                        .await?;
                        root_ready = true;
                    }
                    // Network folder (idempotent), parented to the org root folder. Appended over
                    // the parent's whole scope (ADR-162) — that folder holds nodes as well.
                    let network = network_group_id(org.id, &d.network_id);
                    if networks_ready.insert(network) {
                        let order = crate::groups::append_base_sql("$3", "");
                        sqlx::query(&format!(
                            "INSERT INTO node_groups (id, name, group_type, parent_id, sort_order) \
                             VALUES ($1, $2, 'site', $3, {order} + 1) ON CONFLICT (id) DO NOTHING"
                        ))
                        .bind(network)
                        .bind(&d.network_name)
                        .bind(root)
                        .execute(&mut *tx)
                        .await?;
                    }
                    network
                }
            };

            let node_id = device_node_id(&d.serial);
            let address = d.lan_ip.unwrap_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED));
            // Appended over the destination folder's whole scope (ADR-162). Left at its DEFAULT 0
            // every imported device would sit above that folder's sub-folders.
            //
            // `ON CONFLICT DO NOTHING` because the id is derived: a node can already stand at it
            // with no binding (one carried here by a configuration bundle, which knows nothing of
            // `meraki_devices`). That node *is* this device, so it is bound below where it stands
            // rather than failing the batch on the primary key.
            let node_order = crate::groups::append_base_sql("$6", "");
            sqlx::query(&format!(
                "INSERT INTO nodes \
                   (id, name, address, profile_id, vendor, model, group_id, sort_order) \
                 VALUES ($1, $2, $3::inet, $4, 'Cisco Meraki', $5, $6, {node_order} + 1) \
                 ON CONFLICT (id) DO NOTHING"
            ))
            .bind(node_id)
            .bind(&d.name)
            .bind(address.to_string())
            .bind(d.profile_id)
            .bind(&d.model)
            .bind(group)
            .execute(&mut *tx)
            .await?;

            sqlx::query(
                "INSERT INTO meraki_devices \
                 (node_id, org_id, serial, network_id, product_type, model) \
                 VALUES ($1, $2, $3, $4, $5, $6)",
            )
            .bind(node_id)
            .bind(org.id)
            .bind(&d.serial)
            .bind(&d.network_id)
            .bind(&d.product_type)
            .bind(&d.model)
            .execute(&mut *tx)
            .await?;
            outcome.imported += 1;
            outcome.filed.count(&filing);
            created.push(d.serial.clone());
        }

        // A device imported before the organization's first sync has no inventory row yet, and this
        // touches nothing; the next sync writes the row and takes the stamp from the binding.
        if !created.is_empty() {
            sqlx::query(
                "UPDATE meraki_inventory SET imported_at = COALESCE(imported_at, now()) \
                 WHERE org_id = $1 AND serial = ANY($2)",
            )
            .bind(org.id)
            .bind(&created)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(outcome)
    }
}

/// PostgreSQL-backed store for the per-node Meraki device bindings.
pub struct MerakiDeviceRepo {
    pool: PgPool,
}

impl MerakiDeviceRepo {
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// The set of node ids that are Meraki devices — loaded once per scheduler round so the per-node
    /// loop can skip them without a per-node lookup (they are polled by the org collector).
    pub async fn node_ids(&self) -> anyhow::Result<HashSet<Uuid>> {
        let rows = sqlx::query("SELECT node_id FROM meraki_devices")
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter()
            .map(|r| Ok(r.try_get::<Uuid, _>("node_id")?))
            .collect()
    }

    /// Of the given node ids, which are Meraki devices — a page-scoped variant of [`Self::node_ids`]
    /// for the node-list badge (bounded by the page size, not a full-table scan). Empty input
    /// short-circuits so we never run an empty-array query.
    pub async fn filter_meraki(&self, node_ids: &[Uuid]) -> anyhow::Result<HashSet<Uuid>> {
        if node_ids.is_empty() {
            return Ok(HashSet::new());
        }
        let rows = sqlx::query("SELECT node_id FROM meraki_devices WHERE node_id = ANY($1)")
            .bind(node_ids)
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter()
            .map(|r| Ok(r.try_get::<Uuid, _>("node_id")?))
            .collect()
    }

    /// The serial→node_id map for an org, inlined into its collect jobs so the stateless poller can
    /// attribute each API row to a node.
    pub async fn device_refs(&self, org_uuid: Uuid) -> anyhow::Result<Vec<MerakiDeviceRef>> {
        let rows = sqlx::query("SELECT serial, node_id FROM meraki_devices WHERE org_id = $1")
            .bind(org_uuid)
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter()
            .map(|r| {
                Ok(MerakiDeviceRef {
                    serial: r.try_get("serial")?,
                    node_id: yagra_common::NodeId::from(r.try_get::<Uuid, _>("node_id")?),
                })
            })
            .collect()
    }

    /// Every Meraki node's serial, by node id — one of the duplicate check's serial sources (ADR-148).
    pub async fn serials_by_node(&self) -> anyhow::Result<std::collections::HashMap<Uuid, String>> {
        let rows = sqlx::query("SELECT node_id, serial FROM meraki_devices")
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter()
            .map(|r| {
                Ok((
                    r.try_get::<Uuid, _>("node_id")?,
                    r.try_get::<String, _>("serial")?,
                ))
            })
            .collect()
    }

    /// The Meraki binding for a node, if it is a Meraki device (joins the org for its `org_id`).
    pub async fn get(&self, node_id: Uuid) -> anyhow::Result<Option<MerakiDeviceConfig>> {
        let row = sqlx::query(
            "SELECT d.org_id, o.org_id AS meraki_org_id, d.serial, d.network_id, d.product_type, \
                    d.model \
             FROM meraki_devices d JOIN meraki_orgs o ON o.id = d.org_id WHERE d.node_id = $1",
        )
        .bind(node_id)
        .fetch_optional(&self.pool)
        .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        Ok(Some(MerakiDeviceConfig {
            org_uuid: row.try_get("org_id")?,
            org_id: row.try_get("meraki_org_id")?,
            serial: row.try_get("serial")?,
            network_id: row.try_get("network_id")?,
            product_type: row.try_get("product_type")?,
            model: row.try_get("model")?,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn org() -> MerakiOrg {
        MerakiOrg {
            id: Uuid::nil(),
            org_id: "123456".into(),
            name: "Acme".into(),
            base_url: "https://api.meraki.com".into(),
            credential_id: Uuid::nil(),
            availability_secs: 300,
            uplink_secs: 300,
            traffic_secs: 1800,
            // Not the column default (300): it has to differ from the other three, or
            // `tier_cadence_maps_each_tier` could not tell Inventory from Availability.
            inventory_secs: 900,
            enabled_tiers: vec!["availability".into(), "uplink".into(), "inventory".into()],
            target_rps: 2.0,
            group_id: Some(Uuid::nil()),
            enabled: true,
            last_sync_at: None,
            last_sync_ok: None,
            last_sync_error: None,
            import_devices: true,
            file_by_prefix: true,
            max_devices: 1000,
            devices_over_cap: 0,
            collect_failures: Vec::new(),
        }
    }

    #[test]
    fn active_tiers_parse_and_drop_inventory() {
        let tiers = org().active_tiers();
        assert!(tiers.contains(&MerakiTier::Availability));
        assert!(tiers.contains(&MerakiTier::Uplink));
        // Inventory is reconciliation-only, never a recurring collect.
        assert!(!tiers.contains(&MerakiTier::Inventory));
    }

    #[test]
    fn tier_cadence_maps_each_tier() {
        let o = org();
        assert_eq!(o.tier_cadence(MerakiTier::Availability), 300);
        assert_eq!(o.tier_cadence(MerakiTier::Traffic), 1800);
        assert_eq!(o.tier_cadence(MerakiTier::Inventory), 900);
    }

    #[test]
    fn build_collect_check_carries_org_fields() {
        let o = org();
        let check = build_collect_check(
            &o,
            MerakiTier::Uplink,
            "key".into(),
            vec![MerakiDeviceRef {
                serial: "Q2-A".into(),
                node_id: yagra_common::NodeId::from(Uuid::nil()),
            }],
            vec!["N_1".into()],
        );
        assert_eq!(check.org_id, "123456");
        assert_eq!(check.meraki_org_uuid, o.id);
        assert_eq!(check.tier, MerakiTier::Uplink);
        assert_eq!(check.target_rps, 2.0);
        assert_eq!(check.devices.len(), 1);
        assert_eq!(check.network_ids, vec!["N_1".to_string()]);
    }

    /// 決定 16. The poller reads an empty list as "every network", so the two states that used to
    /// produce one — nothing watched, and a read that failed — must produce no collect instead.
    #[test]
    fn an_organization_that_watches_nothing_is_sent_no_collect() {
        let nothing: Result<Vec<String>, anyhow::Error> = Ok(Vec::new());
        assert_eq!(
            networks_to_collect(nothing),
            Err(NoCollect::NothingWatched),
            "an empty list reached the poller, which collects the whole organization for it"
        );
    }

    #[test]
    fn a_failed_read_of_the_watched_networks_is_not_widened_to_every_network() {
        let failed: Result<Vec<String>, anyhow::Error> = Err(anyhow::anyhow!("pool timed out"));
        assert_eq!(
            networks_to_collect(failed),
            Err(NoCollect::Unreadable),
            "a read that failed became a collect of every network"
        );
    }

    #[test]
    fn the_watched_networks_are_what_a_collect_asks_about() {
        let watched: Result<Vec<String>, anyhow::Error> = Ok(vec!["N_1".into(), "N_2".into()]);
        assert_eq!(
            networks_to_collect(watched),
            Ok(vec!["N_1".to_string(), "N_2".to_string()])
        );
    }

    #[test]
    fn inflight_single_flights_per_org_and_clears_on_result() {
        let f = MerakiInflight::new();
        let org = Uuid::from_u128(1);
        let now = Instant::now();
        let lease = Duration::from_secs(300);
        // First dispatch acquires; a second (any tier) is refused while outstanding.
        assert!(f.acquire(org, Uuid::from_u128(10), lease, now));
        assert!(!f.acquire(org, Uuid::from_u128(11), lease, now));
        assert!(f.is_inflight(org, now));
        // The result for the first job clears the org; then a new collect can acquire.
        f.complete(Uuid::from_u128(10));
        assert!(!f.is_inflight(org, now));
        assert!(f.acquire(org, Uuid::from_u128(12), lease, now));
    }

    #[test]
    fn inflight_lease_expiry_allows_redispatch() {
        let f = MerakiInflight::new();
        let org = Uuid::from_u128(2);
        let now = Instant::now();
        assert!(f.acquire(org, Uuid::from_u128(20), Duration::from_secs(1), now));
        // A poll far in the future sees the lease expired → re-acquire (backstop for a lost result).
        let later = now + Duration::from_secs(5);
        assert!(!f.is_inflight(org, later));
        assert!(f.acquire(org, Uuid::from_u128(21), Duration::from_secs(1), later));
    }

    /// 🚨 The defect (/verify 17-5): any job the tracker still remembered cleared the org's
    /// *current* flight, so a straggler from an expired collect let a second session start.
    #[test]
    fn a_late_result_of_an_expired_job_does_not_release_the_flight_that_followed_it() {
        let f = MerakiInflight::new();
        let org = Uuid::from_u128(3);
        let now = Instant::now();
        let lease = Duration::from_secs(300);
        let (old, current) = (Uuid::from_u128(30), Uuid::from_u128(31));
        assert!(f.acquire_collect(org, old, MerakiTier::Availability, lease, now));
        let later = now + lease + Duration::from_secs(1);
        assert!(
            f.acquire(org, current, lease, later),
            "the sync takes the expired flight"
        );

        assert_eq!(
            f.complete(old),
            None,
            "a straggler released a flight it does not hold"
        );
        assert!(f.is_inflight(org, later));
        assert_eq!(f.complete(current), Some((org, None)));
        assert!(!f.is_inflight(org, later));
    }

    /// A collect nobody answered is evidence (決定 18) — reported once, whether the next dispatch or
    /// the health loop is what notices the lease has run out. The sync's flight never is: its
    /// failures are recorded on the row.
    #[test]
    fn a_collect_whose_lease_runs_out_unanswered_is_handed_over_exactly_once() {
        let f = MerakiInflight::new();
        let (a, b, c) = (Uuid::from_u128(4), Uuid::from_u128(5), Uuid::from_u128(6));
        let now = Instant::now();
        let lease = Duration::from_secs(300);
        assert!(f.acquire_collect(a, Uuid::from_u128(40), MerakiTier::Availability, lease, now));
        assert!(f.acquire_collect(b, Uuid::from_u128(50), MerakiTier::Uplink, lease, now));
        assert!(f.acquire(c, Uuid::from_u128(60), lease, now));
        assert_eq!(f.take_unanswered(now), vec![], "nothing has run out yet");

        let later = now + lease + Duration::from_secs(1);
        // `a` is noticed by the next dispatch, `b` and `c` by the sweep.
        assert!(f.acquire_collect(a, Uuid::from_u128(41), MerakiTier::Uplink, lease, later));
        let mut got = f.take_unanswered(later);
        got.sort_by_key(|(org, _)| *org);
        assert_eq!(
            got,
            vec![(a, MerakiTier::Availability), (b, MerakiTier::Uplink)]
        );
        assert_eq!(
            f.take_unanswered(later),
            vec![],
            "handed over a second time"
        );
        assert!(
            f.is_inflight(a, later),
            "the flight that replaced it is untouched"
        );

        // An answered collect is never reported, however late the sweep comes.
        assert_eq!(
            f.complete(Uuid::from_u128(41)),
            Some((a, Some(MerakiTier::Uplink)))
        );
        assert_eq!(f.take_unanswered(later + lease + lease), vec![]);
    }

    #[test]
    fn group_ids_are_deterministic_and_distinct() {
        let org_uuid = Uuid::from_u128(1);
        assert_eq!(org_group_id(org_uuid), org_group_id(org_uuid)); // stable
        assert_ne!(org_group_id(org_uuid), network_group_id(org_uuid, "N_1"));
        assert_ne!(
            network_group_id(org_uuid, "N_1"),
            network_group_id(org_uuid, "N_2")
        );
    }

    /// The node id is a function of the serial and of nothing else — in particular not of the
    /// organization, whose own id is random per registration.
    #[test]
    fn a_devices_node_id_depends_on_its_serial_alone() {
        assert_eq!(
            device_node_id("Q2XX-AAAA-BBBB"),
            device_node_id("Q2XX-AAAA-BBBB")
        );
        assert_ne!(
            device_node_id("Q2XX-AAAA-BBBB"),
            device_node_id("Q2XX-AAAA-BBBC")
        );
        // Pinned by value: changing the namespace or the input would re-key every Meraki node
        // imported since ADR-164 and orphan its history, and nothing else would notice.
        assert_eq!(
            device_node_id("Q2XX-AAAA-BBBB"),
            Uuid::new_v5(
                &Uuid::from_u128(0x6d65_7261_6b69_0000_0000_0000_0000_0002),
                b"Q2XX-AAAA-BBBB"
            )
        );
        // Its own namespace, so it cannot equal a folder id derived from the same bytes.
        let as_org = Uuid::from_u128(7);
        assert_ne!(
            Uuid::new_v5(&MERAKI_NODE_NS, as_org.as_bytes()),
            org_group_id(as_org)
        );
    }

    // --- Running the SQL, not reading it (ADR-114/116) -----------------------------------------
    //
    // Two of this file's twenty-four statements had ever reached a server, both through the API.
    // The org lifecycle below is the half the scheduler reads on every sweep.
    use crate::pgtest;

    /// Creating an org also creates its HostTree root group, in one transaction, and the row reads
    /// back with the cadence defaults the migration declares.
    ///
    /// 🚨 The defaults are the point of reading them here: they are `DEFAULT` clauses in the
    /// migration and `CHECK`-bounded, so nothing in Rust would notice one changing.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn an_org_is_created_with_its_root_group_and_reads_back(pool: sqlx::PgPool) {
        let cred = pgtest::credential(&pool, "meraki-key", "meraki_api").await;
        let repo = MerakiOrgRepo::new(pool.clone());
        assert!(repo.list().await.expect("list").is_empty());

        let id = repo
            .create("123456", "Acme", "https://api.meraki.com", cred)
            .await
            .expect("create");

        let org = repo
            .get(id)
            .await
            .expect("get")
            .expect("the org just created");
        assert_eq!(org.org_id, "123456");
        assert_eq!(org.name, "Acme");
        assert_eq!(org.base_url, "https://api.meraki.com");
        assert_eq!(org.credential_id, cred);
        assert!(org.enabled, "a new org was created paused");
        assert_eq!(
            org.group_id,
            Some(org_group_id(id)),
            "the org is not bound to its own deterministic root group"
        );
        // The last one is the device sync interval. It was 21600 until migration 0124 made the sync
        // what brings a new device in, and moved its default to five minutes (ADR-164).
        assert_eq!(
            (
                org.availability_secs,
                org.uplink_secs,
                org.traffic_secs,
                org.inventory_secs
            ),
            (300, 300, 1800, 300),
            "the per-tier cadence defaults are not what the migrations declare"
        );
        assert!((org.target_rps - 2.0).abs() < f64::EPSILON);
        assert_eq!(
            org.enabled_tiers,
            vec![
                "availability".to_owned(),
                "uplink".to_owned(),
                "traffic".to_owned()
            ],
            "the default tier set changed"
        );

        // The root group is a real row, named after the org, at the top of the tree.
        assert_eq!(pgtest::rows(&pool, "node_groups").await, 1);
        let groups = crate::groups::GroupRepo::new(pool.clone())
            .list()
            .await
            .expect("groups");
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].id, org_group_id(id));
        assert_eq!(groups[0].name, "Acme");
        assert_eq!(
            groups[0].parent_id, None,
            "the org's root group was filed under something"
        );

        assert!(
            repo.get(Uuid::new_v4()).await.expect("get").is_none(),
            "an id that does not exist returned an org"
        );
    }

    /// The scheduler reads only the enabled orgs; the Integrations page reads all of them. Pausing
    /// an org must not lose its configuration.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn only_enabled_orgs_reach_the_scheduler_and_pausing_keeps_the_configuration(
        pool: sqlx::PgPool,
    ) {
        let cred = pgtest::credential(&pool, "meraki-key", "meraki_api").await;
        let repo = MerakiOrgRepo::new(pool.clone());
        let zed = repo
            .create("2", "Zed", "https://api.meraki.com", cred)
            .await
            .expect("create zed");
        let acme = repo
            .create("1", "Acme", "https://api.meraki.com", cred)
            .await
            .expect("create acme");

        // Both listings are ordered by name, so the UI and the scheduler agree on an order.
        let all = repo.list().await.expect("list");
        assert_eq!(
            all.iter().map(|o| o.name.as_str()).collect::<Vec<_>>(),
            vec!["Acme", "Zed"],
            "the listing is not ordered by name"
        );
        assert_eq!(repo.list_enabled().await.expect("enabled").len(), 2);

        assert!(
            repo.set_enabled(zed, false).await.expect("disable"),
            "disabling an org that exists reported that it did not"
        );
        let enabled = repo.list_enabled().await.expect("enabled");
        assert_eq!(
            enabled.len(),
            1,
            "a paused org is still being polled: {:?}",
            enabled.iter().map(|o| &o.name).collect::<Vec<_>>()
        );
        assert_eq!(enabled[0].id, acme);
        assert_eq!(
            repo.list().await.expect("list").len(),
            2,
            "pausing an org removed it from the Integrations page"
        );
        let paused = repo.get(zed).await.expect("get").expect("zed");
        assert!(!paused.enabled);
        assert_eq!(paused.org_id, "2", "pausing an org lost its configuration");

        assert!(
            repo.set_enabled(zed, true).await.expect("enable"),
            "re-enabling was refused"
        );
        assert_eq!(repo.list_enabled().await.expect("enabled").len(), 2);

        assert!(
            !repo
                .set_enabled(Uuid::new_v4(), false)
                .await
                .expect("disable"),
            "disabling an org that does not exist reported success"
        );
    }

    /// The per-tier cadence and the rate budget round-trip, an unknown org is reported rather than
    /// silently succeeding, and the migration's `CHECK` bounds are a real backstop.
    ///
    /// 🚨 The bounds are asserted by writing through them. They exist because a cadence of a few
    /// seconds against a cloud API with a shared rate budget is how an org gets itself throttled,
    /// and the API edge's own validation is not the only thing standing between an operator and
    /// that — this is.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn the_cadence_round_trips_and_the_check_bounds_refuse_an_absurd_one(pool: sqlx::PgPool) {
        let cred = pgtest::credential(&pool, "meraki-key", "meraki_api").await;
        let repo = MerakiOrgRepo::new(pool.clone());
        let id = repo
            .create("1", "Acme", "https://api.meraki.com", cred)
            .await
            .expect("create");

        let tiers = vec!["availability".to_owned(), "inventory".to_owned()];
        assert!(
            repo.update_cadence(id, 600, 900, 3600, 43200, &tiers, 4.5)
                .await
                .expect("update"),
            "updating an org that exists reported that it did not"
        );
        let org = repo.get(id).await.expect("get").expect("the org");
        assert_eq!(
            (
                org.availability_secs,
                org.uplink_secs,
                org.traffic_secs,
                org.inventory_secs
            ),
            (600, 900, 3600, 43200)
        );
        assert!((org.target_rps - 4.5).abs() < f64::EPSILON);
        assert_eq!(
            org.enabled_tiers, tiers,
            "the enabled tier set did not survive the round trip"
        );

        assert!(
            !repo
                .update_cadence(Uuid::new_v4(), 600, 600, 3600, 43200, &tiers, 2.0)
                .await
                .expect("update"),
            "updating an org that does not exist reported success"
        );

        // Below the floor, above the ceiling, and outside the rate budget: each refused by the
        // column's own constraint, with the stored row untouched.
        for (label, res) in [
            (
                "availability below the floor",
                repo.update_cadence(id, 30, 600, 3600, 43200, &tiers, 2.0)
                    .await,
            ),
            (
                "inventory above the ceiling",
                repo.update_cadence(id, 600, 600, 3600, 999_999, &tiers, 2.0)
                    .await,
            ),
            (
                "a rate budget over the cap",
                repo.update_cadence(id, 600, 600, 3600, 43200, &tiers, 99.0)
                    .await,
            ),
        ] {
            assert!(res.is_err(), "{label} was accepted");
        }
        let after = repo.get(id).await.expect("get").expect("the org");
        assert_eq!(
            after.availability_secs, 600,
            "a refused write landed anyway"
        );
        assert!((after.target_rps - 4.5).abs() < f64::EPSILON);
    }

    /// **A later sync must not change which of an organization's networks are in scope.**
    ///
    /// 🚨 The conflict clause writes the name and the timestamp and deliberately not `monitored` —
    /// that flag is an operator's choice, and every sync would otherwise silently reset it. In one
    /// direction that reads as monitoring quietly stopping for the networks somebody asked for; in
    /// the other (`watch_new`, ADR-164 Inc.4) as a site somebody took out of scope coming back.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_later_sync_renames_a_network_but_never_moves_its_monitored_flag(pool: sqlx::PgPool) {
        let cred = pgtest::credential(&pool, "meraki-key", "meraki_api").await;
        let repo = MerakiOrgRepo::new(pool.clone());
        let org = repo
            .create("1", "Acme", "https://api.meraki.com", cred)
            .await
            .expect("create");

        repo.record_networks(
            org,
            &[
                ("N_1".to_owned(), "Branch".to_owned()),
                ("N_2".to_owned(), "Aylesbury".to_owned()),
            ],
            false,
        )
        .await
        .expect("first sync");
        assert_eq!(
            repo.list_networks(org).await.expect("list"),
            vec![
                ("N_2".to_owned(), "Aylesbury".to_owned(), false),
                ("N_1".to_owned(), "Branch".to_owned(), false),
            ],
            "networks are not listed by name, or arrive already in scope"
        );

        repo.set_networks_monitored(org, &["N_1".to_owned()], true)
            .await
            .expect("monitor");
        assert_eq!(
            repo.monitored_network_ids(org).await.expect("monitored"),
            vec!["N_1".to_owned()]
        );

        // The next sync renames one network and re-reports both.
        repo.record_networks(
            org,
            &[
                ("N_1".to_owned(), "Branch (renamed)".to_owned()),
                ("N_2".to_owned(), "Aylesbury".to_owned()),
            ],
            false,
        )
        .await
        .expect("second sync");
        assert_eq!(
            pgtest::rows(&pool, "meraki_org_networks").await,
            2,
            "a second sync duplicated a network"
        );
        assert_eq!(
            repo.monitored_network_ids(org).await.expect("monitored"),
            vec!["N_1".to_owned()],
            "a second sync took a network out of scope, so collection would stop silently"
        );
        let listed = repo.list_networks(org).await.expect("list");
        let branch = listed
            .iter()
            .find(|(id, _, _)| id == "N_1")
            .expect("the renamed network");
        assert_eq!(
            branch.1, "Branch (renamed)",
            "the sync did not follow the rename"
        );
        assert!(branch.2, "the monitored flag was lost");

        // With automatic import on, a network seen for the FIRST time is watched from that sync —
        // and that is all the argument may do. N_2 was left out of scope by an operator, and a
        // sync that re-reports it with `watch_new` must leave it out.
        repo.record_networks(
            org,
            &[
                ("N_2".to_owned(), "Aylesbury".to_owned()),
                ("N_3".to_owned(), "Camden".to_owned()),
            ],
            true,
        )
        .await
        .expect("sync with automatic import on");
        let mut monitored = repo.monitored_network_ids(org).await.expect("monitored");
        monitored.sort();
        assert_eq!(
            monitored,
            vec!["N_1".to_owned(), "N_3".to_owned()],
            "a new network must arrive watched, and a network taken out of scope must stay out"
        );
    }

    /// Only the monitored networks narrow the collect calls, and an empty selection changes
    /// nothing at all rather than clearing the lot.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn only_monitored_networks_narrow_the_collect_calls(pool: sqlx::PgPool) {
        let cred = pgtest::credential(&pool, "meraki-key", "meraki_api").await;
        let repo = MerakiOrgRepo::new(pool.clone());
        let acme = repo
            .create("1", "Acme", "https://api.meraki.com", cred)
            .await
            .expect("acme");
        let other = repo
            .create("2", "Other", "https://api.meraki.com", cred)
            .await
            .expect("other");
        repo.record_networks(
            acme,
            &[
                ("N_1".to_owned(), "One".to_owned()),
                ("N_2".to_owned(), "Two".to_owned()),
            ],
            false,
        )
        .await
        .expect("sync acme");
        repo.record_networks(other, &[("N_1".to_owned(), "Theirs".to_owned())], false)
            .await
            .expect("sync other");

        assert!(
            repo.monitored_network_ids(acme)
                .await
                .expect("monitored")
                .is_empty(),
            "a network found with automatic import off was already in scope"
        );

        repo.set_networks_monitored(acme, &["N_1".to_owned(), "N_2".to_owned()], true)
            .await
            .expect("monitor both");
        let mut monitored = repo.monitored_network_ids(acme).await.expect("monitored");
        monitored.sort();
        assert_eq!(monitored, vec!["N_1".to_owned(), "N_2".to_owned()]);
        assert!(
            repo.monitored_network_ids(other)
                .await
                .expect("monitored")
                .is_empty(),
            "the selection reached another org's network of the same id"
        );

        repo.set_networks_monitored(acme, &["N_2".to_owned()], false)
            .await
            .expect("unmonitor one");
        assert_eq!(
            repo.monitored_network_ids(acme).await.expect("monitored"),
            vec!["N_1".to_owned()]
        );

        // The early return: an empty selection is a no-op, not "clear everything".
        repo.set_networks_monitored(acme, &[], false)
            .await
            .expect("empty");
        assert_eq!(
            repo.monitored_network_ids(acme).await.expect("monitored"),
            vec!["N_1".to_owned()],
            "an empty selection changed the scope"
        );
    }

    fn device(serial: &str, network: &str, network_name: &str) -> MerakiImportDevice {
        MerakiImportDevice {
            serial: serial.to_owned(),
            name: format!("dev-{serial}"),
            model: Some("MR46".to_owned()),
            product_type: "wireless".to_owned(),
            network_id: network.to_owned(),
            network_name: network_name.to_owned(),
            lan_ip: Some("10.4.0.9".parse().expect("addr")),
            profile_id: None,
            // What the match says when no folder carries a range: the network folder.
            filing: Filing::Unmatched,
        }
    }

    fn filed_in(mut d: MerakiImportDevice, folder: Uuid) -> MerakiImportDevice {
        d.filing = Filing::Matched {
            folder,
            prefix: "10.4.0.0/24".to_owned(),
        };
        d
    }

    async fn group_of(pool: &sqlx::PgPool, serial: &str) -> Option<Uuid> {
        sqlx::query_scalar(
            "SELECT n.group_id FROM nodes n JOIN meraki_devices d ON d.node_id = n.id \
             WHERE d.serial = $1",
        )
        .bind(serial)
        .fetch_one(pool)
        .await
        .expect("the node's folder")
    }

    /// Importing devices creates one HostTree group per network under the org's root, one node per
    /// device, and the binding that makes the node a Meraki device — all in one transaction.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn importing_devices_creates_a_node_and_one_group_per_network(pool: sqlx::PgPool) {
        let cred = pgtest::credential(&pool, "meraki-key", "meraki_api").await;
        let repo = MerakiOrgRepo::new(pool.clone());
        let id = repo
            .create("1", "Acme", "https://api.meraki.com", cred)
            .await
            .expect("create");
        let org = repo.get(id).await.expect("get").expect("the org");

        let mut headless = device("Q3-3", "N_2", "Two");
        headless.lan_ip = None;
        let imported = repo
            .import_devices(
                &org,
                &[
                    device("Q3-1", "N_1", "One"),
                    device("Q3-2", "N_1", "One"),
                    headless,
                ],
            )
            .await
            .expect("import");
        assert_eq!(imported.imported, 3);
        assert_eq!(
            imported.filed,
            MerakiFiled {
                unmatched: 3,
                ..MerakiFiled::default()
            },
            "every device was filed by what its filing said"
        );
        assert_eq!(pgtest::rows(&pool, "nodes").await, 3);
        assert_eq!(pgtest::rows(&pool, "meraki_devices").await, 3);
        assert_eq!(
            pgtest::rows(&pool, "node_groups").await,
            3,
            "expected the org root plus one group per network, created once each"
        );

        let groups = crate::groups::GroupRepo::new(pool.clone())
            .list()
            .await
            .expect("groups");
        let one = groups
            .iter()
            .find(|g| g.id == network_group_id(id, "N_1"))
            .expect("the group for N_1");
        assert_eq!(one.name, "One");
        assert_eq!(
            one.parent_id,
            Some(org_group_id(id)),
            "a network group was not filed under the org's root"
        );

        let nodes = pgtest::repo(pool.clone())
            .list_nodes()
            .await
            .expect("nodes");
        let first = nodes
            .iter()
            .find(|n| n.name == "dev-Q3-1")
            .expect("the first device");
        assert_eq!(first.vendor.as_deref(), Some("Cisco Meraki"));
        assert_eq!(first.model.as_deref(), Some("MR46"));
        assert_eq!(
            first.group,
            Some(yagra_common::GroupId(network_group_id(id, "N_1"))),
            "the node was not filed in its network's group"
        );
        assert_eq!(
            first.address,
            "10.4.0.9".parse::<std::net::IpAddr>().unwrap()
        );
        let unaddressed = nodes
            .iter()
            .find(|n| n.name == "dev-Q3-3")
            .expect("the device with no LAN address");
        assert_eq!(
            unaddressed.address,
            std::net::IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            "a device Meraki reported no address for did not fall back to the placeholder"
        );

        // The binding, read back through the join that supplies the org's *external* id.
        let devices = MerakiDeviceRepo::new(pool.clone());
        let bound = devices
            .get(first.id.as_uuid())
            .await
            .expect("get")
            .expect("the binding");
        assert_eq!(bound.org_uuid, id);
        assert_eq!(
            bound.org_id, "1",
            "the join did not supply the org's Meraki-side id"
        );
        assert_eq!(bound.serial, "Q3-1");
        assert_eq!(bound.network_id, "N_1");
        assert_eq!(bound.product_type, "wireless");
        assert_eq!(bound.model.as_deref(), Some("MR46"));
    }

    async fn acme(pool: &sqlx::PgPool) -> (MerakiOrgRepo, MerakiOrg) {
        let cred = pgtest::credential(pool, "meraki-key", "meraki_api").await;
        let repo = MerakiOrgRepo::new(pool.clone());
        let id = repo
            .create("1", "Acme", "https://api.meraki.com", cred)
            .await
            .expect("create");
        let org = repo.get(id).await.expect("get").expect("the org");
        (repo, org)
    }

    /// A device goes where its filing says, and a network's folder exists only if a device landed
    /// in it — an organization whose devices all match an IP range grows no parallel tree.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_device_is_filed_where_its_filing_says_and_only_used_network_folders_exist(
        pool: sqlx::PgPool,
    ) {
        let (repo, org) = acme(&pool).await;
        let site = pgtest::group(&pool, "Matsuyama").await;

        let mut ambiguous = device("Q3-4", "N_1", "One");
        ambiguous.filing = Filing::Ambiguous { folders: 2 };
        let outcome = repo
            .import_devices(
                &org,
                &[
                    filed_in(device("Q3-1", "N_2", "Two"), site),
                    filed_in(device("Q3-2", "N_3", "Three"), site),
                    device("Q3-3", "N_1", "One"),
                    ambiguous,
                ],
            )
            .await
            .expect("import");
        assert_eq!(outcome.imported, 4);
        assert_eq!(
            outcome.filed,
            MerakiFiled {
                matched: 2,
                ambiguous: 1,
                unmatched: 1,
                no_address: 0
            }
        );
        assert_eq!(group_of(&pool, "Q3-1").await, Some(site));
        assert_eq!(group_of(&pool, "Q3-2").await, Some(site));
        let n1 = network_group_id(org.id, "N_1");
        assert_eq!(group_of(&pool, "Q3-3").await, Some(n1));
        assert_eq!(
            group_of(&pool, "Q3-4").await,
            Some(n1),
            "an ambiguous address was resolved instead of falling to the network folder"
        );
        // The site, the org's root and N_1. N_2 and N_3 held only matched devices.
        assert_eq!(
            pgtest::rows(&pool, "node_groups").await,
            3,
            "a network folder was created for devices that were filed elsewhere"
        );
    }

    /// What the alert engine's snapshot of Meraki organizations is built from (ADR-164 決定 18): every
    /// organization with its name and its nodes — including one with no imported device, whose name
    /// is still what an alert about it is called.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn every_organization_is_listed_with_its_name_and_its_nodes(pool: sqlx::PgPool) {
        let cred = pgtest::credential(&pool, "meraki-key", "meraki_api").await;
        let repo = MerakiOrgRepo::new(pool.clone());
        let acme = repo
            .create("1", "Acme", "https://api.meraki.com", cred)
            .await
            .expect("create");
        let empty = repo
            .create("2", "Nothing imported", "https://api.meraki.com", cred)
            .await
            .expect("create");
        let org = repo.get(acme).await.expect("get").expect("the org");
        repo.import_devices(
            &org,
            &[device("Q3-1", "N_1", "One"), device("Q3-2", "N_2", "Two")],
        )
        .await
        .expect("import");

        let mut got = repo.alert_bindings().await.expect("bindings");
        got.sort_by(|a, b| a.1.cmp(&b.1));
        assert_eq!(got.len(), 2);
        let (id, name, mut nodes) = got[0].clone();
        nodes.sort_unstable();
        let mut want = vec![device_node_id("Q3-1"), device_node_id("Q3-2")];
        want.sort_unstable();
        assert_eq!((id, name.as_str()), (acme, "Acme"));
        assert_eq!(nodes, want);
        assert_eq!(got[1], (empty, "Nothing imported".to_owned(), Vec::new()));
    }

    /// Which collects are failing is kept on the row (migration 0127) so that a core that has just
    /// started — or a standby that never heard the reports — can still say so. Written only when it
    /// changes, and read back by the same `list()` the scheduler uses.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn the_failing_collects_are_kept_on_the_row_and_written_only_on_change(
        pool: sqlx::PgPool,
    ) {
        use crate::meraki_health::TierFailure;
        use crate::meraki_sync::MerakiSyncFailure;
        let cred = pgtest::credential(&pool, "meraki-key", "meraki_api").await;
        let repo = MerakiOrgRepo::new(pool.clone());
        let id = repo
            .create("1", "Acme", "https://api.meraki.com", cred)
            .await
            .expect("create");
        let fresh = repo.get(id).await.expect("get").expect("the org");
        assert_eq!(
            fresh.collect_failures,
            Vec::new(),
            "a new organization is failing nothing"
        );

        let failing = vec![TierFailure {
            tier: MerakiTier::Availability,
            reason: MerakiSyncFailure::Auth,
            since_unix_ms: 1_790_000_000_000,
            failures: 3,
        }];
        assert!(repo
            .record_collect_failures(id, &failing)
            .await
            .expect("write"));
        assert!(
            !repo
                .record_collect_failures(id, &failing)
                .await
                .expect("write"),
            "the same value was written a second time — this runs every 15 seconds"
        );
        let listed = repo.list().await.expect("list");
        assert_eq!(listed[0].collect_failures, failing);

        assert!(repo.record_collect_failures(id, &[]).await.expect("clear"));
        let cleared = repo.get(id).await.expect("get").expect("the org");
        assert_eq!(cleared.collect_failures, Vec::new());
        assert!(
            !repo
                .record_collect_failures(Uuid::new_v4(), &failing)
                .await
                .expect("write"),
            "an organization that is gone"
        );
    }

    /// Which serials are already nodes is decided inside the writer, under its lock: two imports of
    /// one device leave one node, and neither fails. Before ADR-164 the caller filtered first, so
    /// the loser hit `meraki_devices.serial`'s UNIQUE and the whole batch answered 500.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn two_imports_of_one_device_leave_one_node_and_neither_fails(pool: sqlx::PgPool) {
        let (repo, org) = acme(&pool).await;
        let first = [device("Q3-1", "N_1", "One"), device("Q3-2", "N_1", "One")];
        let second = [device("Q3-1", "N_1", "One"), device("Q3-3", "N_1", "One")];
        let (a, b) = tokio::join!(
            repo.import_devices(&org, &first),
            repo.import_devices(&org, &second)
        );
        let (a, b) = (a.expect("first import"), b.expect("second import"));
        assert_eq!(
            a.imported + b.imported,
            3,
            "the shared serial was counted by both imports, or by neither"
        );
        assert_eq!(pgtest::rows(&pool, "meraki_devices").await, 3);
        assert_eq!(pgtest::rows(&pool, "nodes").await, 3);

        // Again, alone: everything is already there, so nothing is created and nothing is counted.
        let again = repo.import_devices(&org, &first).await.expect("again");
        assert_eq!(again, MerakiImportOutcome::default());
        // …and a serial named twice in one batch is one device.
        let twice = [device("Q3-9", "N_1", "One"), device("Q3-9", "N_1", "One")];
        let once = repo.import_devices(&org, &twice).await.expect("twice");
        assert_eq!(once.imported, 1);
    }

    /// The UNIQUE on `meraki_devices.serial` spans every organization, so the skip has to as well:
    /// a serial bound under another organization is left alone, and the rest of the batch lands.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_serial_bound_under_another_organization_is_skipped_not_fatal(pool: sqlx::PgPool) {
        let (repo, org) = acme(&pool).await;
        let cred = pgtest::credential(&pool, "other-key", "meraki_api").await;
        let other = repo
            .create("2", "Other", "https://api.meraki.com", cred)
            .await
            .expect("create");
        let other = repo.get(other).await.expect("get").expect("the other org");
        repo.import_devices(&other, &[device("Q3-1", "N_9", "Nine")])
            .await
            .expect("the other org's import");

        let outcome = repo
            .import_devices(
                &org,
                &[device("Q3-1", "N_1", "One"), device("Q3-2", "N_1", "One")],
            )
            .await
            .expect("a serial held elsewhere must not fail the batch");
        assert_eq!(outcome.imported, 1);
        assert_eq!(
            group_of(&pool, "Q3-1").await,
            Some(network_group_id(other.id, "N_9")),
            "the other organization's device was moved"
        );
    }

    /// A device deleted and imported again comes back as itself: same node id, so its series and
    /// its alert history are its own again. The id is a function of the serial alone.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_deleted_device_comes_back_under_the_same_node_id(pool: sqlx::PgPool) {
        let (repo, org) = acme(&pool).await;
        let id_of = |pool: sqlx::PgPool| async move {
            sqlx::query_scalar::<_, Uuid>(
                "SELECT node_id FROM meraki_devices WHERE serial = 'Q3-1'",
            )
            .fetch_one(&pool)
            .await
            .expect("the binding")
        };
        repo.import_devices(&org, &[device("Q3-1", "N_1", "One")])
            .await
            .expect("import");
        let first = id_of(pool.clone()).await;
        assert_eq!(first, device_node_id("Q3-1"));
        assert_ne!(first, device_node_id("Q3-2"));

        sqlx::query("DELETE FROM nodes WHERE id = $1")
            .bind(first)
            .execute(&pool)
            .await
            .expect("delete the node");
        assert_eq!(pgtest::rows(&pool, "meraki_devices").await, 0);

        let back = repo
            .import_devices(&org, &[device("Q3-1", "N_1", "One")])
            .await
            .expect("import again");
        assert_eq!(
            back.imported, 1,
            "a deleted device could not be imported by hand"
        );
        assert_eq!(id_of(pool.clone()).await, first);
    }

    /// An operator who deletes the organization's folder sets `meraki_orgs.group_id` to NULL
    /// (`ON DELETE SET NULL`). The next import puts the folder back under the same id and points
    /// the row at it — before ADR-164 every later device was filed nowhere, at the top of the tree.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_deleted_organization_folder_is_put_back_under_the_same_id(pool: sqlx::PgPool) {
        let (repo, org) = acme(&pool).await;
        let root = org_group_id(org.id);
        sqlx::query("DELETE FROM node_groups WHERE id = $1")
            .bind(root)
            .execute(&pool)
            .await
            .expect("delete the folder");
        let org = repo.get(org.id).await.expect("get").expect("the org");
        assert_eq!(org.group_id, None, "the fixture did not orphan the org");

        repo.import_devices(&org, &[device("Q3-1", "N_1", "One")])
            .await
            .expect("import");
        let org = repo.get(org.id).await.expect("get").expect("the org");
        assert_eq!(org.group_id, Some(root), "the row was not pointed back");
        let n1 = network_group_id(org.id, "N_1");
        assert_eq!(group_of(&pool, "Q3-1").await, Some(n1));
        let parent: Option<Uuid> =
            sqlx::query_scalar("SELECT parent_id FROM node_groups WHERE id = $1")
                .bind(n1)
                .fetch_one(&pool)
                .await
                .expect("the network folder");
        assert_eq!(parent, Some(root));
    }

    /// A matched folder deleted between the match and the write is no match: the device falls to
    /// its network folder and is counted that way, instead of the batch failing on the foreign key.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_matched_folder_that_is_gone_is_read_as_no_match(pool: sqlx::PgPool) {
        let (repo, org) = acme(&pool).await;
        let gone = Uuid::from_u128(0xdead);
        let outcome = repo
            .import_devices(&org, &[filed_in(device("Q3-1", "N_1", "One"), gone)])
            .await
            .expect("a vanished folder must not fail the import");
        assert_eq!(
            outcome.filed,
            MerakiFiled {
                unmatched: 1,
                ..MerakiFiled::default()
            }
        );
        assert_eq!(
            group_of(&pool, "Q3-1").await,
            Some(network_group_id(org.id, "N_1"))
        );
    }

    /// The import stamps the inventory row in its own transaction, once. A second import of the
    /// same device creates nothing and so moves nothing.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn an_import_stamps_the_inventory_row_once(pool: sqlx::PgPool) {
        let (repo, org) = acme(&pool).await;
        for serial in ["Q3-1", "Q3-2"] {
            sqlx::query("INSERT INTO meraki_inventory (org_id, serial) VALUES ($1, $2)")
                .bind(org.id)
                .bind(serial)
                .execute(&pool)
                .await
                .expect("an inventory row");
        }
        let stamp = |pool: sqlx::PgPool, serial: &'static str| async move {
            sqlx::query_scalar::<_, Option<chrono::DateTime<chrono::Utc>>>(
                "SELECT imported_at FROM meraki_inventory WHERE serial = $1",
            )
            .bind(serial)
            .fetch_one(&pool)
            .await
            .expect("the inventory row")
        };

        repo.import_devices(&org, &[device("Q3-1", "N_1", "One")])
            .await
            .expect("import");
        let first = stamp(pool.clone(), "Q3-1").await;
        assert!(first.is_some(), "the imported device was not stamped");
        assert_eq!(
            stamp(pool.clone(), "Q3-2").await,
            None,
            "a device that was not imported was stamped"
        );

        repo.import_devices(&org, &[device("Q3-1", "N_1", "One")])
            .await
            .expect("import again");
        assert_eq!(stamp(pool.clone(), "Q3-1").await, first, "the stamp moved");
    }

    /// The device reads answer for the org they were asked about, and `filter_meraki` keeps only
    /// the nodes that are Meraki devices out of a mixed list.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn the_device_reads_are_scoped_to_their_org(pool: sqlx::PgPool) {
        let cred = pgtest::credential(&pool, "meraki-key", "meraki_api").await;
        let repo = MerakiOrgRepo::new(pool.clone());
        let plain = pgtest::node(&pool, "an-snmp-switch", 1, None).await;

        let mut ids = Vec::new();
        for (org_id, name, serial, net) in
            [("1", "Acme", "Q3-1", "N_1"), ("2", "Other", "Q3-9", "N_9")]
        {
            let uuid = repo
                .create(org_id, name, "https://api.meraki.com", cred)
                .await
                .expect("create");
            let org = repo.get(uuid).await.expect("get").expect("org");
            repo.import_devices(&org, &[device(serial, net, "Net")])
                .await
                .expect("import");
            ids.push(uuid);
        }
        let (acme, other) = (ids[0], ids[1]);

        let devices = MerakiDeviceRepo::new(pool.clone());
        assert_eq!(
            devices.node_ids().await.expect("node_ids").len(),
            2,
            "the fleet-wide set did not return both orgs' devices"
        );
        let refs = devices.device_refs(acme).await.expect("device_refs");
        assert_eq!(refs.len(), 1, "the device refs are not scoped to their org");
        assert_eq!(refs[0].serial, "Q3-1");
        let theirs = devices.device_refs(other).await.expect("device_refs");
        assert_eq!(theirs.len(), 1);
        assert_eq!(theirs[0].serial, "Q3-9");

        let mut mixed: Vec<Uuid> = devices
            .node_ids()
            .await
            .expect("node_ids")
            .into_iter()
            .collect();
        mixed.push(plain);
        let meraki_only = devices.filter_meraki(&mixed).await.expect("filter");
        assert_eq!(
            meraki_only.len(),
            2,
            "the filter dropped a Meraki device or kept the SNMP node"
        );
        assert!(
            !meraki_only.contains(&plain),
            "an ordinary node was reported as a Meraki device"
        );
        assert!(
            devices.filter_meraki(&[]).await.expect("filter").is_empty(),
            "filtering an empty list returned something"
        );
        assert!(
            devices.get(plain).await.expect("get").is_none(),
            "an ordinary node has a Meraki binding"
        );
    }

    /// Purging removes the org, its device nodes and its groups, and leaves every other org alone.
    /// On the way: the two sync-outcome writers, in both directions (ADR-164 決定 3) — a success
    /// moves `last_sync_at`, a failure records its reason and leaves the stamp exactly where it was.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn purging_an_org_takes_its_devices_and_groups_and_leaves_the_others(pool: sqlx::PgPool) {
        let cred = pgtest::credential(&pool, "meraki-key", "meraki_api").await;
        let repo = MerakiOrgRepo::new(pool.clone());
        let mut ids = Vec::new();
        for (org_id, name, serial) in [("1", "Acme", "Q3-1"), ("2", "Other", "Q3-9")] {
            let uuid = repo
                .create(org_id, name, "https://api.meraki.com", cred)
                .await
                .expect("create");
            let org = repo.get(uuid).await.expect("get").expect("org");
            repo.import_devices(&org, &[device(serial, "N_1", "One")])
                .await
                .expect("import");
            ids.push(uuid);
        }
        let (acme, other) = (ids[0], ids[1]);
        repo.record_networks(acme, &[("N_1".to_owned(), "One".to_owned())], false)
            .await
            .expect("record networks");

        let fresh = repo.get(acme).await.expect("get").expect("org");
        assert_eq!(
            (
                fresh.last_sync_at,
                fresh.last_sync_ok,
                fresh.last_sync_error
            ),
            (None, None, None),
            "an organization that has never synced must not read as failed"
        );

        repo.record_sync_success(acme).await.expect("success");
        let ok = repo.get(acme).await.expect("get").expect("org");
        let synced = ok.last_sync_at.expect("a success stamps last_sync_at");
        assert_eq!(
            (ok.last_sync_ok, ok.last_sync_error.clone()),
            (Some(true), None)
        );

        repo.record_sync_failure(acme, "auth")
            .await
            .expect("failure");
        let failed = repo.get(acme).await.expect("get").expect("org");
        assert_eq!(
            failed.last_sync_at,
            Some(synced),
            "a failed sync moved last_sync_at — the next sync would be postponed by a failure"
        );
        assert_eq!(
            (failed.last_sync_ok, failed.last_sync_error.as_deref()),
            (Some(false), Some("auth"))
        );

        repo.record_sync_success(acme).await.expect("success again");
        let again = repo.get(acme).await.expect("get").expect("org");
        assert!(
            again.last_sync_at.expect("stamp") > synced,
            "the stamp did not move"
        );
        assert_eq!(
            (again.last_sync_ok, again.last_sync_error),
            (Some(true), None),
            "a success must clear the previous failure's reason"
        );

        // `record_networks` writes what changed and nothing else (ADR-164 決定 4).
        let nets = |pairs: &[(&str, &str)]| -> Vec<(String, String)> {
            pairs
                .iter()
                .map(|(i, n)| ((*i).to_owned(), (*n).to_owned()))
                .collect()
        };
        assert_eq!(
            repo.record_networks(acme, &nets(&[("N_1", "One"), ("N_2", "Two")]), false)
                .await
                .expect("record"),
            1,
            "N_1 is already stored under that name; only N_2 is new"
        );
        assert_eq!(
            repo.record_networks(acme, &nets(&[("N_1", "One"), ("N_2", "Two")]), false)
                .await
                .expect("record again"),
            0,
            "an unchanged listing must write nothing"
        );
        assert_eq!(
            repo.record_networks(
                acme,
                &nets(&[("N_2", "Two, renamed"), ("N_2", "Two, renamed")]),
                false,
            )
            .await
            .expect("rename, listed twice"),
            1
        );
        let stored = repo.list_networks(acme).await.expect("list");
        assert_eq!(
            stored,
            [
                ("N_1".to_owned(), "One".to_owned(), false),
                ("N_2".to_owned(), "Two, renamed".to_owned(), false),
            ],
            "a network the sync found must not become monitored by being found"
        );

        assert_eq!(pgtest::rows(&pool, "nodes").await, 2);
        assert_eq!(pgtest::rows(&pool, "node_groups").await, 4);

        assert!(
            repo.purge(acme).await.expect("purge"),
            "purge reported the org missing"
        );
        assert_eq!(
            pgtest::rows(&pool, "meraki_orgs").await,
            1,
            "purging took more than the org it was asked about"
        );
        assert_eq!(
            pgtest::rows(&pool, "nodes").await,
            1,
            "the purged org's device nodes survived, or another org's did not"
        );
        assert_eq!(
            pgtest::rows(&pool, "meraki_devices").await,
            1,
            "the device bindings did not cascade with their nodes"
        );
        assert_eq!(
            pgtest::rows(&pool, "meraki_org_networks").await,
            0,
            "the network scope did not cascade with the org"
        );
        assert_eq!(
            pgtest::rows(&pool, "node_groups").await,
            2,
            "the purged org's root and network groups were not removed, or the other org's were"
        );
        assert!(
            repo.get(other).await.expect("get").is_some(),
            "purging one org removed another"
        );

        assert!(
            !repo.purge(acme).await.expect("purge"),
            "purging an org that is already gone reported success"
        );
    }
}
