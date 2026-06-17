//! PostgreSQL metadata repository (nodes inventory).
//!
//! Metadata — nodes, profiles, thresholds, alert history — lives in PostgreSQL (store
//! separation, CLAUDE.md Architecture). This is an I/O adapter (live-only), so it is
//! exercised in deployment, not unit tests; the domain types it returns ([`Node`]) are
//! tested in `yagra-common`. Queries are runtime `sqlx::query` (not the compile-time
//! macro) so the build needs no live database — important for CI.

use std::collections::{BTreeMap, HashMap};
use std::net::{IpAddr, Ipv4Addr};
use std::time::Duration;

use async_trait::async_trait;
use serde::Serialize;
use sqlx::postgres::{PgPool, PgPoolOptions};
use sqlx::types::Json;
use sqlx::Row;
use uuid::Uuid;
use yagra_common::{CollectionKind, CredentialId, GroupId, MetricKind, Node, NodeId, ProfileId};

/// A device-class/profile row for the API (id + name + role/vendor metadata).
#[derive(Debug, Clone, Serialize)]
pub struct ProfileSummary {
    pub id: Uuid,
    pub name: String,
    /// Functional role token (kebab-case `ProfileCategory`) — the UI's grouping key.
    pub category: String,
    /// Vendor label, if known (descriptive metadata only — never a TSDB label).
    pub vendor: Option<String>,
}

/// One interface's stored metadata (from a table walk). Descriptive attributes only —
/// joined to per-interface metrics at query time (thin-label model, ADR-011).
#[derive(Debug, Clone)]
pub struct InterfaceMeta {
    pub ifindex: i32,
    pub if_name: Option<String>,
    pub if_alias: Option<String>,
    pub if_speed: Option<i64>,
    /// `last_seen` as Unix seconds, for staleness checks.
    pub last_seen_s: Option<i64>,
}

/// Interface identity for a fleet Top-N name join (no timestamp — just labels + speed).
pub struct InterfaceIdent {
    pub if_name: Option<String>,
    pub if_alias: Option<String>,
    pub if_speed: Option<i64>,
}

/// Map a `nodes` row (selected via [`NodeRepo::NODE_COLUMNS`]) to a [`Node`].
fn node_from_row(row: &sqlx::postgres::PgRow) -> anyhow::Result<Node> {
    let id: Uuid = row.try_get("id")?;
    let name: String = row.try_get("name")?;
    let parent: Option<Uuid> = row.try_get("parent_id")?;
    let address: String = row.try_get("address")?;
    let profile: Option<Uuid> = row.try_get("profile_id")?;
    let pool: Option<String> = row.try_get("pool")?;
    let credential: Option<Uuid> = row.try_get("credential_id")?;
    let vendor: Option<String> = row.try_get("vendor")?;
    let model: Option<String> = row.try_get("model")?;
    let group: Option<Uuid> = row.try_get("group_id")?;
    let tags: Json<BTreeMap<String, String>> = row.try_get("tags")?;
    let address: IpAddr = address
        .parse()
        .map_err(|e| anyhow::anyhow!("node {id} has unparseable address {address:?}: {e}"))?;
    Ok(Node {
        id: NodeId::from(id),
        name,
        parent: parent.map(NodeId::from),
        address,
        profile: profile.map(ProfileId::from),
        pool,
        credential: credential.map(CredentialId::from),
        vendor,
        model,
        group: group.map(GroupId::from),
        tags: tags.0,
    })
}

/// Fixed id for the seeded demo node the walking-skeleton WebUI queries.
const DEMO_NODE_ID: Uuid = Uuid::nil();

/// A read-only source of the node inventory for the API. Implemented by [`NodeRepo`]
/// (live, PostgreSQL) and [`StaticNodeList`] (skeleton mode), so the router doesn't care
/// which is behind it.
#[async_trait]
pub trait NodeListing: Send + Sync {
    /// One keyset page: nodes with `id > after` (or from the start), ordered by id,
    /// capped at `limit`. The API paginates with this so large inventories don't load
    /// everything (ui-conventions: scale-aware lists).
    async fn list_page(&self, after: Option<Uuid>, limit: i64) -> anyhow::Result<Vec<Node>>;
}

#[async_trait]
impl NodeListing for NodeRepo {
    async fn list_page(&self, after: Option<Uuid>, limit: i64) -> anyhow::Result<Vec<Node>> {
        self.list_nodes_page(after, limit).await
    }
}

/// A fixed node list for skeleton mode (no database). Mirrors the live seed's demo node
/// so the WebUI shows the same nil-id loopback node.
pub struct StaticNodeList(Vec<Node>);

impl StaticNodeList {
    /// The single demo node (nil id → loopback) used by the skeleton.
    #[must_use]
    pub fn demo() -> Self {
        Self(vec![Node::new(
            NodeId::from(DEMO_NODE_ID),
            "demo-localhost",
            IpAddr::V4(Ipv4Addr::LOCALHOST),
        )])
    }
}

#[async_trait]
impl NodeListing for StaticNodeList {
    async fn list_page(&self, after: Option<Uuid>, limit: i64) -> anyhow::Result<Vec<Node>> {
        let mut nodes: Vec<Node> = self.0.clone();
        nodes.sort_by_key(|n| n.id.as_uuid());
        Ok(nodes
            .into_iter()
            .filter(|n| after.is_none_or(|a| n.id.as_uuid() > a))
            .take(limit.clamp(1, 500) as usize)
            .collect())
    }
}

/// The nodes/profiles metadata store.
pub struct NodeRepo {
    pool: PgPool,
}

impl NodeRepo {
    /// Connect (with retry, so Postgres may start after core) and return the repo.
    pub async fn connect(url: &str) -> anyhow::Result<Self> {
        const MAX_ATTEMPTS: u32 = 30;
        let mut attempt = 0;
        loop {
            let result = PgPoolOptions::new()
                .max_connections(5)
                .acquire_timeout(Duration::from_secs(5))
                .connect(url)
                .await;
            match result {
                Ok(pool) => {
                    tracing::info!("connected to PostgreSQL");
                    return Ok(Self { pool });
                }
                Err(e) if attempt < MAX_ATTEMPTS => {
                    attempt += 1;
                    tracing::warn!(error = %e, attempt, "PostgreSQL not ready; retrying in 2s");
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
                Err(e) => anyhow::bail!("PostgreSQL connect failed after {MAX_ATTEMPTS}: {e}"),
            }
        }
    }

    /// A clone of the underlying connection pool (for sibling stores that share the DB,
    /// e.g. the credential store).
    #[must_use]
    pub fn pool(&self) -> PgPool {
        self.pool.clone()
    }

    /// Apply all embedded migrations (expand-contract, ADR-017). Embedded at compile
    /// time, so this needs no database at build.
    pub async fn migrate(&self) -> anyhow::Result<()> {
        sqlx::migrate!("../../migrations").run(&self.pool).await?;
        tracing::info!("database migrations applied");
        Ok(())
    }

    /// Column list shared by the full and paged node queries (`host(address)` strips any
    /// netmask so the INET parses straight to IpAddr).
    const NODE_COLUMNS: &'static str = "id, name, parent_id, host(address) AS address, \
         profile_id, pool, credential_id, vendor, model, group_id, tags";

    /// Every node in the inventory (internal use; the API paginates via [`Self::list_nodes_page`]).
    pub async fn list_nodes(&self) -> anyhow::Result<Vec<Node>> {
        let rows = sqlx::query(&format!("SELECT {} FROM nodes", Self::NODE_COLUMNS))
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(node_from_row).collect()
    }

    /// One node by id, if it exists (for endpoints that need its profile/bindings).
    pub async fn get_node(&self, id: Uuid) -> anyhow::Result<Option<Node>> {
        let row = sqlx::query(&format!(
            "SELECT {} FROM nodes WHERE id = $1",
            Self::NODE_COLUMNS
        ))
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        row.as_ref().map(node_from_row).transpose()
    }

    /// Interfaces discovered on a node (metadata for the interfaces view), ordered by index.
    pub async fn list_interfaces(&self, node_id: Uuid) -> anyhow::Result<Vec<InterfaceMeta>> {
        let rows = sqlx::query(
            "SELECT ifindex, if_name, if_alias, if_speed, \
                    extract(epoch FROM last_seen)::bigint AS last_seen_s \
             FROM interfaces WHERE node_id = $1 ORDER BY ifindex",
        )
        .bind(node_id)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| {
                Ok(InterfaceMeta {
                    ifindex: row.try_get("ifindex")?,
                    if_name: row.try_get("if_name")?,
                    if_alias: row.try_get("if_alias")?,
                    if_speed: row.try_get("if_speed")?,
                    last_seen_s: row.try_get("last_seen_s")?,
                })
            })
            .collect()
    }

    /// One keyset page of nodes ordered by id, starting after `after`.
    pub async fn list_nodes_page(
        &self,
        after: Option<Uuid>,
        limit: i64,
    ) -> anyhow::Result<Vec<Node>> {
        let limit = limit.clamp(1, 500);
        let rows = match after {
            Some(after) => {
                sqlx::query(&format!(
                    "SELECT {} FROM nodes WHERE id > $1 ORDER BY id LIMIT $2",
                    Self::NODE_COLUMNS
                ))
                .bind(after)
                .bind(limit)
                .fetch_all(&self.pool)
                .await?
            }
            None => {
                sqlx::query(&format!(
                    "SELECT {} FROM nodes ORDER BY id LIMIT $1",
                    Self::NODE_COLUMNS
                ))
                .bind(limit)
                .fetch_all(&self.pool)
                .await?
            }
        };
        rows.iter().map(node_from_row).collect()
    }

    /// Create a node; returns its new id. Optional profile, bound credential, parent, and
    /// descriptive vendor/model metadata.
    #[allow(clippy::too_many_arguments)]
    pub async fn create_node(
        &self,
        name: &str,
        address: IpAddr,
        pool: Option<&str>,
        profile: Option<Uuid>,
        credential: Option<Uuid>,
        parent: Option<Uuid>,
        vendor: Option<&str>,
        model: Option<&str>,
    ) -> anyhow::Result<Uuid> {
        let id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO nodes \
             (id, name, address, pool, profile_id, credential_id, parent_id, vendor, model) \
             VALUES ($1, $2, $3::inet, $4, $5, $6, $7, $8, $9)",
        )
        .bind(id)
        .bind(name)
        .bind(address.to_string())
        .bind(pool)
        .bind(profile)
        .bind(credential)
        .bind(parent)
        .bind(vendor)
        .bind(model)
        .execute(&self.pool)
        .await?;
        Ok(id)
    }

    /// Set (or clear) a node's profile, bound credential, and vendor/model metadata. Returns
    /// whether the node exists. All fields are set to the passed value (a `None` clears) — the
    /// node-edit UI loads the current values and resends them, so an unchanged field is preserved.
    pub async fn set_node_bindings(
        &self,
        id: Uuid,
        profile: Option<Uuid>,
        credential: Option<Uuid>,
        vendor: Option<&str>,
        model: Option<&str>,
    ) -> anyhow::Result<bool> {
        let res = sqlx::query(
            "UPDATE nodes SET profile_id = $2, credential_id = $3, vendor = $4, model = $5, \
             updated_at = now() WHERE id = $1",
        )
        .bind(id)
        .bind(profile)
        .bind(credential)
        .bind(vendor)
        .bind(model)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected() > 0)
    }

    /// Move a node into a group (or `None` to ungroup it), appending it to the **end** of the
    /// destination scope (max sort_order + 1) so it lands predictably at the bottom. Returns
    /// whether the node exists. Used by the "Move to…" picker and a drop directly onto a group;
    /// drag-reorder between siblings goes through [`Self::place_node`] instead.
    pub async fn set_node_group(&self, id: Uuid, group: Option<Uuid>) -> anyhow::Result<bool> {
        let res = sqlx::query(
            "UPDATE nodes SET group_id = $2, updated_at = now(), \
             sort_order = (SELECT COALESCE(MAX(sort_order), 0) + 1 FROM nodes \
                           WHERE group_id IS NOT DISTINCT FROM $2::uuid AND id <> $1) \
             WHERE id = $1",
        )
        .bind(id)
        .bind(group)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected() > 0)
    }

    /// The `(id, sort_order)` of the nodes in `group` (NULL ⇒ ungrouped), ordered. Feeds
    /// [`crate::groups::placement_order`] when a drag drops a node before/after a sibling.
    pub async fn ordered_nodes_in_group(
        &self,
        group: Option<Uuid>,
    ) -> anyhow::Result<Vec<(Uuid, f64)>> {
        let rows = sqlx::query(
            "SELECT id, sort_order FROM nodes \
             WHERE group_id IS NOT DISTINCT FROM $1::uuid ORDER BY sort_order, name, id",
        )
        .bind(group)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| Ok((row.try_get("id")?, row.try_get("sort_order")?)))
            .collect()
    }

    /// Assign a node to `group` and set its order in one update (drag reorder). Returns existence.
    pub async fn place_node(
        &self,
        id: Uuid,
        group: Option<Uuid>,
        order: f64,
    ) -> anyhow::Result<bool> {
        let res = sqlx::query(
            "UPDATE nodes SET group_id = $2, sort_order = $3, updated_at = now() WHERE id = $1",
        )
        .bind(id)
        .bind(group)
        .bind(order)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected() > 0)
    }

    /// Fill a node's **blank** vendor/model from an SNMP identity probe — `COALESCE` keeps any
    /// existing (manually set or already-classified) value, so this never clobbers operator input;
    /// it only writes a column that is currently NULL. A `None` argument leaves that column alone.
    /// Returns whether the node exists. Used by the poll-result consumer after `identify()`.
    pub async fn fill_node_identity(
        &self,
        id: Uuid,
        vendor: Option<&str>,
        model: Option<&str>,
    ) -> anyhow::Result<bool> {
        let res = sqlx::query(
            "UPDATE nodes SET vendor = COALESCE(vendor, $2), model = COALESCE(model, $3), \
             updated_at = now() WHERE id = $1",
        )
        .bind(id)
        .bind(vendor)
        .bind(model)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected() > 0)
    }

    /// The `sort_order` of each of the given node ids (for the inventory tree, which orders nodes
    /// within their group). Ids absent from the map default to 0 at the call site. One query.
    pub async fn node_sort_orders(&self, ids: &[Uuid]) -> anyhow::Result<HashMap<Uuid, f64>> {
        if ids.is_empty() {
            return Ok(HashMap::new());
        }
        let rows = sqlx::query("SELECT id, sort_order FROM nodes WHERE id = ANY($1)")
            .bind(ids)
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter()
            .map(|row| Ok((row.try_get("id")?, row.try_get("sort_order")?)))
            .collect()
    }

    /// The display name of each of the given node ids, in one query. For joining TSDB results
    /// (which carry only the node id, ADR-011) back to human-readable names — e.g. the fleet
    /// Top-N endpoint. Ids absent from the map default to the id string at the call site.
    pub async fn node_names(&self, ids: &[Uuid]) -> anyhow::Result<HashMap<Uuid, String>> {
        if ids.is_empty() {
            return Ok(HashMap::new());
        }
        let rows = sqlx::query("SELECT id, name FROM nodes WHERE id = ANY($1)")
            .bind(ids)
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter()
            .map(|row| Ok((row.try_get("id")?, row.try_get("name")?)))
            .collect()
    }

    /// Interface identity (name/alias/speed) for every interface on the given node ids, keyed by
    /// `(node_id, ifindex)`. For joining a fleet interface Top-N (which carries only node UUID +
    /// ifindex from the TSDB, ADR-011) back to human-readable names. Over-fetches all interfaces
    /// of the few nodes in a Top-N result, then the caller filters by the exact pairs — one query.
    pub async fn interface_idents_for(
        &self,
        node_ids: &[Uuid],
    ) -> anyhow::Result<HashMap<(Uuid, i32), InterfaceIdent>> {
        if node_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let rows = sqlx::query(
            "SELECT node_id, ifindex, if_name, if_alias, if_speed \
             FROM interfaces WHERE node_id = ANY($1)",
        )
        .bind(node_ids)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| {
                let node_id: Uuid = row.try_get("node_id")?;
                let ifindex: i32 = row.try_get("ifindex")?;
                Ok((
                    (node_id, ifindex),
                    InterfaceIdent {
                        if_name: row.try_get("if_name")?,
                        if_alias: row.try_get("if_alias")?,
                        if_speed: row.try_get("if_speed")?,
                    },
                ))
            })
            .collect()
    }

    /// Upsert an interface discovered during a table walk: insert it, or refresh its
    /// metadata and `last_seen`. Names/aliases are device-supplied metadata kept in
    /// PostgreSQL (joined to metrics at query time) — never TSDB labels (ADR-011). A
    /// `None` field leaves the stored value untouched (COALESCE). Staleness is judged by
    /// `last_seen` age; rows are not deleted here.
    pub async fn upsert_interface(
        &self,
        node_id: Uuid,
        ifindex: i32,
        if_name: Option<&str>,
        if_alias: Option<&str>,
        if_speed: Option<i64>,
    ) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO interfaces (node_id, ifindex, if_name, if_alias, if_speed, last_seen) \
             VALUES ($1, $2, $3, $4, $5, now()) \
             ON CONFLICT (node_id, ifindex) DO UPDATE SET \
                if_name = COALESCE(EXCLUDED.if_name, interfaces.if_name), \
                if_alias = COALESCE(EXCLUDED.if_alias, interfaces.if_alias), \
                if_speed = COALESCE(EXCLUDED.if_speed, interfaces.if_speed), \
                last_seen = now()",
        )
        .bind(node_id)
        .bind(ifindex)
        .bind(if_name)
        .bind(if_alias)
        .bind(if_speed)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Delete a node by id. Returns whether a row was removed.
    pub async fn delete_node(&self, id: Uuid) -> anyhow::Result<bool> {
        let res = sqlx::query("DELETE FROM nodes WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() > 0)
    }

    /// All device-class profiles.
    pub async fn list_profiles(&self) -> anyhow::Result<Vec<ProfileSummary>> {
        let rows = sqlx::query("SELECT id, name, category, vendor FROM profiles ORDER BY name")
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter()
            .map(|row| {
                Ok(ProfileSummary {
                    id: row.try_get("id")?,
                    name: row.try_get("name")?,
                    category: row.try_get("category")?,
                    vendor: row.try_get("vendor")?,
                })
            })
            .collect()
    }

    /// Create a profile; returns its id.
    pub async fn create_profile(
        &self,
        name: &str,
        category: &str,
        vendor: Option<&str>,
    ) -> anyhow::Result<Uuid> {
        let id = Uuid::new_v4();
        sqlx::query("INSERT INTO profiles (id, name, category, vendor) VALUES ($1, $2, $3, $4)")
            .bind(id)
            .bind(name)
            .bind(category)
            .bind(vendor)
            .execute(&self.pool)
            .await?;
        Ok(id)
    }

    /// Update a profile's name / category / vendor. Returns whether the row existed.
    pub async fn update_profile(
        &self,
        id: Uuid,
        name: &str,
        category: &str,
        vendor: Option<&str>,
    ) -> anyhow::Result<bool> {
        let res = sqlx::query(
            "UPDATE profiles SET name = $2, category = $3, vendor = $4, updated_at = now() \
             WHERE id = $1",
        )
        .bind(id)
        .bind(name)
        .bind(category)
        .bind(vendor)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected() > 0)
    }

    /// Delete a profile. Returns whether a row was removed.
    pub async fn delete_profile(&self, id: Uuid) -> anyhow::Result<bool> {
        let res = sqlx::query("DELETE FROM profiles WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() > 0)
    }

    /// If the inventory is empty, seed a few demo nodes so the walking skeleton shows
    /// real ICMP data immediately. Idempotent: only seeds an empty table, so it won't
    /// resurrect nodes an operator has deleted.
    pub async fn seed_demo_nodes_if_empty(&self) -> anyhow::Result<()> {
        let count: i64 = sqlx::query("SELECT count(*) AS n FROM nodes")
            .fetch_one(&self.pool)
            .await?
            .try_get("n")?;
        if count > 0 {
            return Ok(());
        }

        // The DEMO_NODE_ID (nil) → loopback node is what the WebUI NodeDetail queries; it
        // is always reachable so the end-to-end path is provable even with no internet.
        let demo: [(Uuid, &str, &str); 3] = [
            (DEMO_NODE_ID, "demo-localhost", "127.0.0.1"),
            (
                Uuid::from_u128(0x0000_0000_0000_0000_0000_0000_0101_0101),
                "cloudflare-dns",
                "1.1.1.1",
            ),
            (
                Uuid::from_u128(0x0000_0000_0000_0000_0000_0000_0808_0808),
                "google-dns",
                "8.8.8.8",
            ),
        ];
        for (id, name, addr) in demo {
            sqlx::query(
                "INSERT INTO nodes (id, name, address) VALUES ($1, $2, $3::inet) \
                 ON CONFLICT (id) DO NOTHING",
            )
            .bind(id)
            .bind(name)
            .bind(addr)
            .execute(&self.pool)
            .await?;
        }
        tracing::info!(
            seeded = demo.len(),
            "seeded demo nodes into empty inventory"
        );
        Ok(())
    }

    /// Seed the built-in **collection templates** (Standard SNMP, vendor health) and the
    /// **device profiles** (Generic ping/SNMP, Cisco, Huawei) that reference them. Idempotent
    /// and non-destructive: every row uses a stable id and `ON CONFLICT DO NOTHING`, so the
    /// catalog reliably exists after a deploy without clobbering operator edits. Runs every
    /// boot. Also removes the legacy profile-scope `collection_items` the built-in profiles
    /// used to carry (PR #12) — profiles are now templates-only, so those would be ignored.
    pub async fn seed_builtin_profiles(&self) -> anyhow::Result<()> {
        // Stable bases so ids (and thus existing bindings/links) survive restarts.
        const PROFILE_ID_BASE: u128 = 0x0000_0000_0000_0000_0000_0000_5eed_0000;
        const TEMPLATE_ID_BASE: u128 = 0x0000_0000_0000_0000_0000_0000_5eed_7000;

        // 1. Templates + their metrics; remember name → id for the profile links.
        let mut template_id_by_name: HashMap<&'static str, Uuid> = HashMap::new();
        for (i, template) in yagra_common::builtin_templates().into_iter().enumerate() {
            let template_id = Uuid::from_u128(TEMPLATE_ID_BASE + i as u128);
            template_id_by_name.insert(template.name, template_id);
            sqlx::query(
                "INSERT INTO collection_templates (id, name, description) VALUES ($1, $2, $3) \
                 ON CONFLICT (id) DO NOTHING",
            )
            .bind(template_id)
            .bind(template.name)
            .bind(template.description)
            .execute(&self.pool)
            .await?;
            for item in template.items {
                let collection = match item.kind {
                    CollectionKind::Scalar => "scalar",
                    CollectionKind::Table => "table",
                };
                let metric_kind = match item.metric_kind {
                    MetricKind::Gauge => "gauge",
                    MetricKind::Counter => "counter",
                };
                sqlx::query(
                    "INSERT INTO collection_template_items \
                        (id, template_id, metric_name, oid, collection, metric_kind, enabled) \
                     VALUES ($1, $2, $3, $4, $5, $6, true) \
                     ON CONFLICT (template_id, metric_name) DO NOTHING",
                )
                .bind(Uuid::new_v4())
                .bind(template_id)
                .bind(&item.metric_name)
                .bind(&item.oid)
                .bind(collection)
                .bind(metric_kind)
                .execute(&self.pool)
                .await?;
            }
        }

        // 2. Profiles + their template links; drop any legacy profile-scope collection items.
        let mut profile_id_by_name: HashMap<&'static str, Uuid> = HashMap::new();
        for (i, profile) in yagra_common::builtin_profiles().into_iter().enumerate() {
            let profile_id = Uuid::from_u128(PROFILE_ID_BASE + i as u128);
            profile_id_by_name.insert(profile.name, profile_id);
            sqlx::query(
                "INSERT INTO profiles (id, name, category, vendor) VALUES ($1, $2, $3, $4) \
                 ON CONFLICT (id) DO NOTHING",
            )
            .bind(profile_id)
            .bind(profile.name)
            .bind(profile.category.as_str())
            .bind(profile.vendor)
            .execute(&self.pool)
            .await?;
            // Legacy cleanup: built-in profiles no longer carry direct OIDs (templates-only).
            sqlx::query(
                "DELETE FROM collection_items WHERE scope_level = 'profile' AND scope_id = $1",
            )
            .bind(profile_id)
            .execute(&self.pool)
            .await?;
            for template_name in profile.templates {
                if let Some(template_id) = template_id_by_name.get(template_name) {
                    sqlx::query(
                        "INSERT INTO profile_collection_templates (profile_id, template_id) \
                         VALUES ($1, $2) ON CONFLICT DO NOTHING",
                    )
                    .bind(profile_id)
                    .bind(template_id)
                    .execute(&self.pool)
                    .await?;
                }
            }
        }
        // 3. Built-in classification rules (discovery → suggested profile). Stable ids +
        //    ON CONFLICT DO NOTHING so operator edits survive restarts; references the profile
        //    ids seeded just above. Rules for an unknown profile name are skipped defensively.
        const RULE_ID_BASE: u128 = 0x0000_0000_0000_0000_0000_0000_5eed_8000;
        for (i, rule) in yagra_common::builtin_classification_rules()
            .into_iter()
            .enumerate()
        {
            let Some(&profile_id) = profile_id_by_name.get(rule.profile_name) else {
                tracing::warn!(
                    profile = rule.profile_name,
                    "skipping seed rule for unknown profile"
                );
                continue;
            };
            let rule_id = Uuid::from_u128(RULE_ID_BASE + i as u128);
            sqlx::query(
                "INSERT INTO classification_rules \
                    (id, priority, sysobjectid_prefix, sysdescr_regex, profile_id, vendor, model) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7) ON CONFLICT (id) DO NOTHING",
            )
            .bind(rule_id)
            .bind(rule.priority)
            .bind(rule.sysobjectid_prefix)
            .bind(rule.sysdescr_regex)
            .bind(profile_id)
            .bind(rule.vendor)
            .bind(rule.model)
            .execute(&self.pool)
            .await?;
        }
        tracing::info!(
            "seeded built-in collection templates + device profiles + classification rules"
        );
        Ok(())
    }
}
