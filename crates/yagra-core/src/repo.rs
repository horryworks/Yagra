//! PostgreSQL metadata repository (nodes inventory).
//!
//! Metadata — nodes, profiles, thresholds, alert history — lives in PostgreSQL (store
//! separation, CLAUDE.md Architecture). This is an I/O adapter (live-only), so it is
//! exercised in deployment, not unit tests; the domain types it returns ([`Node`]) are
//! tested in `yagra-common`. Queries are runtime `sqlx::query` (not the compile-time
//! macro) so the build needs no live database — important for CI.

use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv4Addr};
use std::time::Duration;

use async_trait::async_trait;
use serde::Serialize;
use sqlx::postgres::{PgPool, PgPoolOptions};
use sqlx::types::Json;
use sqlx::Row;
use uuid::Uuid;
use yagra_common::{CredentialId, Node, NodeId, ProfileId};

/// A device-class/profile row for the API (id + name).
#[derive(Debug, Clone, Serialize)]
pub struct ProfileSummary {
    pub id: Uuid,
    pub name: String,
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
    const NODE_COLUMNS: &'static str =
        "id, name, parent_id, host(address) AS address, profile_id, pool, credential_id, tags";

    /// Every node in the inventory (internal use; the API paginates via [`Self::list_nodes_page`]).
    pub async fn list_nodes(&self) -> anyhow::Result<Vec<Node>> {
        let rows = sqlx::query(&format!("SELECT {} FROM nodes", Self::NODE_COLUMNS))
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(node_from_row).collect()
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

    /// Create a node; returns its new id. Optional profile, bound credential, and parent.
    pub async fn create_node(
        &self,
        name: &str,
        address: IpAddr,
        pool: Option<&str>,
        profile: Option<Uuid>,
        credential: Option<Uuid>,
        parent: Option<Uuid>,
    ) -> anyhow::Result<Uuid> {
        let id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO nodes (id, name, address, pool, profile_id, credential_id, parent_id) \
             VALUES ($1, $2, $3::inet, $4, $5, $6, $7)",
        )
        .bind(id)
        .bind(name)
        .bind(address.to_string())
        .bind(pool)
        .bind(profile)
        .bind(credential)
        .bind(parent)
        .execute(&self.pool)
        .await?;
        Ok(id)
    }

    /// Set (or clear) a node's profile and bound credential. Returns whether the node exists.
    pub async fn set_node_bindings(
        &self,
        id: Uuid,
        profile: Option<Uuid>,
        credential: Option<Uuid>,
    ) -> anyhow::Result<bool> {
        let res = sqlx::query(
            "UPDATE nodes SET profile_id = $2, credential_id = $3, updated_at = now() \
             WHERE id = $1",
        )
        .bind(id)
        .bind(profile)
        .bind(credential)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected() > 0)
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
        let rows = sqlx::query("SELECT id, name FROM profiles ORDER BY name")
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter()
            .map(|row| {
                Ok(ProfileSummary {
                    id: row.try_get("id")?,
                    name: row.try_get("name")?,
                })
            })
            .collect()
    }

    /// Create a profile; returns its id.
    pub async fn create_profile(&self, name: &str) -> anyhow::Result<Uuid> {
        let id = Uuid::new_v4();
        sqlx::query("INSERT INTO profiles (id, name) VALUES ($1, $2)")
            .bind(id)
            .bind(name)
            .execute(&self.pool)
            .await?;
        Ok(id)
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
}
