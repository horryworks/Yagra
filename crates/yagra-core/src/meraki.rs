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
}
