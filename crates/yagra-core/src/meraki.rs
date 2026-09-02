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
}

impl MerakiOrg {
    fn from_row(row: &sqlx::postgres::PgRow) -> anyhow::Result<Self> {
        let availability_secs: i32 = row.try_get("availability_secs")?;
        let uplink_secs: i32 = row.try_get("uplink_secs")?;
        let traffic_secs: i32 = row.try_get("traffic_secs")?;
        let inventory_secs: i32 = row.try_get("inventory_secs")?;
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

/// Resolve an org's read-only Meraki API key: open the bound credential and, only if it is a
/// `meraki_api` kind, parse out the key. Returns `None` on any failure (missing/ wrong-kind /
/// unparsable) — the caller then skips dispatch. The key is never logged.
pub async fn resolve_meraki_key(creds: &CredentialStore, credential_id: Uuid) -> Option<String> {
    match creds.open(credential_id).await {
        Ok(Some((kind, secret))) if kind == KIND_MERAKI_API => {
            MerakiApiSecret::parse(&secret).ok().map(|s| s.api_key)
        }
        Ok(Some(_)) => {
            tracing::warn!("meraki org credential is not a meraki_api kind");
            None
        }
        Ok(None) => None,
        Err(e) => {
            tracing::warn!(error = %e, "failed to open meraki credential");
            None
        }
    }
}

/// Per-org single-flight tracker: at most one collect outstanding per org so the org's shared API
/// rate budget is never exceeded (the #1 safeguard). Acquired at dispatch and cleared when the
/// collect's first result returns (all fan-out results share the job's id); a lease deadline is the
/// backstop if a result never arrives (poller crash), so an org can't wedge forever.
#[derive(Default)]
pub struct MerakiInflight {
    orgs: Mutex<HashMap<Uuid, Instant>>, // org → lease deadline (presence = in flight)
    jobs: Mutex<HashMap<Uuid, Uuid>>,    // job_id → org, to clear on result
}

impl MerakiInflight {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Try to mark `org` in flight (dispatching `job_id`) for `lease`. Returns `false` if a collect
    /// is already outstanding (and its lease hasn't expired) — the caller then skips this org.
    pub fn acquire(&self, org: Uuid, job_id: Uuid, lease: Duration, now: Instant) -> bool {
        let mut orgs = self.orgs.lock().expect("meraki inflight orgs poisoned");
        if orgs.get(&org).is_some_and(|&deadline| deadline > now) {
            return false;
        }
        orgs.insert(org, now + lease);
        self.jobs
            .lock()
            .expect("meraki inflight jobs poisoned")
            .insert(job_id, org);
        true
    }

    /// Clear the org owning `job_id` (called for every poll result; a no-op for non-Meraki jobs).
    pub fn complete(&self, job_id: Uuid) {
        if let Some(org) = self
            .jobs
            .lock()
            .expect("meraki inflight jobs poisoned")
            .remove(&job_id)
        {
            self.orgs
                .lock()
                .expect("meraki inflight orgs poisoned")
                .remove(&org);
        }
    }

    /// Whether `org` currently has an unexpired outstanding collect.
    #[must_use]
    pub fn is_inflight(&self, org: Uuid, now: Instant) -> bool {
        self.orgs
            .lock()
            .expect("meraki inflight orgs poisoned")
            .get(&org)
            .is_some_and(|&deadline| deadline > now)
    }
}

/// One device to import (pre-resolved by the API handler from the enumerate candidates).
pub struct MerakiImportDevice {
    pub serial: String,
    pub name: String,
    pub model: Option<String>,
    pub product_type: String,
    pub network_id: String,
    pub network_name: String,
    pub lan_ip: Option<IpAddr>,
    pub profile_id: Option<Uuid>,
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
        uplink_secs, traffic_secs, inventory_secs, enabled_tiers, target_rps, group_id, enabled";

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
        // Root group at the HostTree top level (no parent — single-tenant decision).
        sqlx::query(
            "INSERT INTO node_groups (id, name, group_type, parent_id) \
             VALUES ($1, $2, 'region', NULL) ON CONFLICT (id) DO NOTHING",
        )
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

    /// Stamp `last_sync_at = now()` after an enumerate/reconcile.
    pub async fn touch_sync(&self, id: Uuid) -> anyhow::Result<()> {
        sqlx::query(
            "UPDATE meraki_orgs SET last_sync_at = now(), updated_at = now() WHERE id = $1",
        )
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
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

    /// Upsert the org's networks (from enumerate), preserving each `monitored` flag.
    pub async fn upsert_networks(
        &self,
        org_uuid: Uuid,
        networks: &[(String, String)],
    ) -> anyhow::Result<()> {
        let mut tx = self.pool.begin().await?;
        for (network_id, name) in networks {
            sqlx::query(
                "INSERT INTO meraki_org_networks (org_id, network_id, name, last_seen_at) \
                 VALUES ($1, $2, $3, now()) \
                 ON CONFLICT (org_id, network_id) DO UPDATE SET name = EXCLUDED.name, \
                 last_seen_at = now()",
            )
            .bind(org_uuid)
            .bind(network_id)
            .bind(name)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
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

    /// Bulk-import Meraki devices **atomically**: create one HostTree group per network (idempotent,
    /// under the org root group), then each node + its `meraki_devices` row, all in one transaction.
    /// Devices whose serial is already imported are skipped by the caller. Nodes carry no per-node
    /// credential (the org owns the key). Returns how many were inserted.
    pub async fn import_devices(
        &self,
        org: &MerakiOrg,
        devices: &[MerakiImportDevice],
    ) -> anyhow::Result<u32> {
        let mut tx = self.pool.begin().await?;
        let mut created_groups: HashSet<Uuid> = HashSet::new();
        let mut count = 0u32;

        for d in devices {
            // Network group (idempotent), parented to the org root group.
            let group = network_group_id(org.id, &d.network_id);
            if org.group_id.is_some() && created_groups.insert(group) {
                sqlx::query(
                    "INSERT INTO node_groups (id, name, group_type, parent_id) \
                     VALUES ($1, $2, 'site', $3) ON CONFLICT (id) DO NOTHING",
                )
                .bind(group)
                .bind(&d.network_name)
                .bind(org.group_id)
                .execute(&mut *tx)
                .await?;
            }
            let node_group = org.group_id.map(|_| group);

            let node_id = Uuid::new_v4();
            let address = d.lan_ip.unwrap_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED));
            sqlx::query(
                "INSERT INTO nodes (id, name, address, profile_id, vendor, model, group_id) \
                 VALUES ($1, $2, $3::inet, $4, 'Cisco Meraki', $5, $6)",
            )
            .bind(node_id)
            .bind(&d.name)
            .bind(address.to_string())
            .bind(d.profile_id)
            .bind(&d.model)
            .bind(node_group)
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
            count += 1;
        }
        tx.commit().await?;
        Ok(count)
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

    /// Already-imported serials for an org (for the import-dedup + reconciliation diff).
    pub async fn serials(&self, org_uuid: Uuid) -> anyhow::Result<HashSet<String>> {
        let rows = sqlx::query("SELECT serial FROM meraki_devices WHERE org_id = $1")
            .bind(org_uuid)
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter()
            .map(|r| Ok(r.try_get::<String, _>("serial")?))
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
            inventory_secs: 21600,
            enabled_tiers: vec!["availability".into(), "uplink".into(), "inventory".into()],
            target_rps: 2.0,
            group_id: Some(Uuid::nil()),
            enabled: true,
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
        assert_eq!(o.tier_cadence(MerakiTier::Inventory), 21600);
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
        assert_eq!(
            (
                org.availability_secs,
                org.uplink_secs,
                org.traffic_secs,
                org.inventory_secs
            ),
            (300, 300, 1800, 21600),
            "the per-tier cadence defaults are not what the migration declares"
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

    /// **Re-enumerating an org must not take its networks out of scope.**
    ///
    /// 🚨 The conflict clause writes the name and the timestamp and deliberately not `monitored` —
    /// that flag is an operator's choice, and every enumerate would otherwise silently reset it,
    /// which reads as monitoring quietly stopping for the networks somebody asked for.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn re_enumerating_updates_the_name_but_never_the_monitored_flag(pool: sqlx::PgPool) {
        let cred = pgtest::credential(&pool, "meraki-key", "meraki_api").await;
        let repo = MerakiOrgRepo::new(pool.clone());
        let org = repo
            .create("1", "Acme", "https://api.meraki.com", cred)
            .await
            .expect("create");

        repo.upsert_networks(
            org,
            &[
                ("N_1".to_owned(), "Branch".to_owned()),
                ("N_2".to_owned(), "Aylesbury".to_owned()),
            ],
        )
        .await
        .expect("enumerate");
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

        // The next enumerate renames one network and re-reports both.
        repo.upsert_networks(
            org,
            &[
                ("N_1".to_owned(), "Branch (renamed)".to_owned()),
                ("N_2".to_owned(), "Aylesbury".to_owned()),
            ],
        )
        .await
        .expect("re-enumerate");
        assert_eq!(
            pgtest::rows(&pool, "meraki_org_networks").await,
            2,
            "re-enumerating duplicated a network"
        );
        assert_eq!(
            repo.monitored_network_ids(org).await.expect("monitored"),
            vec!["N_1".to_owned()],
            "re-enumerating took a network out of scope, so collection would stop silently"
        );
        let listed = repo.list_networks(org).await.expect("list");
        let branch = listed
            .iter()
            .find(|(id, _, _)| id == "N_1")
            .expect("the renamed network");
        assert_eq!(
            branch.1, "Branch (renamed)",
            "the enumerate did not follow the rename"
        );
        assert!(branch.2, "the monitored flag was lost");
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
        repo.upsert_networks(
            acme,
            &[
                ("N_1".to_owned(), "One".to_owned()),
                ("N_2".to_owned(), "Two".to_owned()),
            ],
        )
        .await
        .expect("enumerate acme");
        repo.upsert_networks(other, &[("N_1".to_owned(), "Theirs".to_owned())])
            .await
            .expect("enumerate other");

        assert!(
            repo.monitored_network_ids(acme)
                .await
                .expect("monitored")
                .is_empty(),
            "a freshly enumerated network was already in scope"
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
        }
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
        assert_eq!(imported, 3);
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
        assert_eq!(
            devices.serials(acme).await.expect("serials"),
            ["Q3-1".to_owned()].into_iter().collect(),
            "the serial set is not scoped to its org"
        );
        assert_eq!(
            devices.serials(other).await.expect("serials"),
            ["Q3-9".to_owned()].into_iter().collect()
        );
        let refs = devices.device_refs(acme).await.expect("device_refs");
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].serial, "Q3-1");

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
    /// `touch_sync` stamps the column that says when the last reconcile happened.
    ///
    /// ⚠️ `last_sync_at` is read through [`pgtest::timestamp_of`] because **nothing in production
    /// selects it** — it is not in `MerakiOrgRepo::COLUMNS`.
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
        repo.upsert_networks(acme, &[("N_1".to_owned(), "One".to_owned())])
            .await
            .expect("enumerate");

        repo.touch_sync(acme).await.expect("touch");
        let synced = pgtest::timestamp_of(&pool, "meraki_orgs", "last_sync_at", "id", acme).await;
        repo.touch_sync(acme).await.expect("touch again");
        assert!(
            pgtest::timestamp_of(&pool, "meraki_orgs", "last_sync_at", "id", acme).await > synced,
            "the reconcile stamp did not move"
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
