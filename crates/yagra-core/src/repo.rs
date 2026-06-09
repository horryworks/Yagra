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
use sqlx::postgres::{PgPool, PgPoolOptions};
use sqlx::types::Json;
use sqlx::Row;
use uuid::Uuid;
use yagra_common::{Node, NodeId, ProfileId};

/// Fixed id for the seeded demo node the walking-skeleton WebUI queries.
const DEMO_NODE_ID: Uuid = Uuid::nil();

/// A read-only source of the node inventory for the API. Implemented by [`NodeRepo`]
/// (live, PostgreSQL) and [`StaticNodeList`] (skeleton mode), so the router doesn't care
/// which is behind it.
#[async_trait]
pub trait NodeListing: Send + Sync {
    /// All nodes in the inventory.
    async fn list(&self) -> anyhow::Result<Vec<Node>>;
}

#[async_trait]
impl NodeListing for NodeRepo {
    async fn list(&self) -> anyhow::Result<Vec<Node>> {
        self.list_nodes().await
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
    async fn list(&self) -> anyhow::Result<Vec<Node>> {
        Ok(self.0.clone())
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

    /// Apply all embedded migrations (expand-contract, ADR-017). Embedded at compile
    /// time, so this needs no database at build.
    pub async fn migrate(&self) -> anyhow::Result<()> {
        sqlx::migrate!("../../migrations").run(&self.pool).await?;
        tracing::info!("database migrations applied");
        Ok(())
    }

    /// Every node in the inventory (full table scan is fine at MVP scale; paginated
    /// pool-scoped queries land with distributed scheduling).
    pub async fn list_nodes(&self) -> anyhow::Result<Vec<Node>> {
        // `host(address)` strips any netmask so the INET parses straight to IpAddr.
        let rows = sqlx::query(
            "SELECT id, name, parent_id, host(address) AS address, profile_id, pool, tags \
             FROM nodes",
        )
        .fetch_all(&self.pool)
        .await?;

        let mut nodes = Vec::with_capacity(rows.len());
        for row in rows {
            let id: Uuid = row.try_get("id")?;
            let name: String = row.try_get("name")?;
            let parent: Option<Uuid> = row.try_get("parent_id")?;
            let address: String = row.try_get("address")?;
            let profile: Option<Uuid> = row.try_get("profile_id")?;
            let pool: Option<String> = row.try_get("pool")?;
            let tags: Json<BTreeMap<String, String>> = row.try_get("tags")?;

            let address: IpAddr = address.parse().map_err(|e| {
                anyhow::anyhow!("node {id} has unparseable address {address:?}: {e}")
            })?;

            nodes.push(Node {
                id: NodeId::from(id),
                name,
                parent: parent.map(NodeId::from),
                address,
                profile: profile.map(ProfileId::from),
                pool,
                tags: tags.0,
            });
        }
        Ok(nodes)
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
