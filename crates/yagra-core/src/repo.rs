// SPDX-License-Identifier: AGPL-3.0-only
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

// Only the settings struct: `retention::Row` would collide with `sqlx::Row` above.
use crate::retention::RetentionSettings;

/// A device-class/profile row for the API (id + name + role/vendor metadata).
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct ProfileSummary {
    pub id: Uuid,
    pub name: String,
    /// Functional role token (kebab-case `ProfileCategory`) — the UI's grouping key.
    pub category: String,
    /// Vendor label, if known (descriptive metadata only — never a TSDB label).
    pub vendor: Option<String>,
    /// Per-profile polling-interval override (seconds); `None` ⇒ inherit the global default.
    pub poll_interval_secs: Option<i32>,
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

/// One row for [`NodeRepo::upsert_interfaces_batch`]: `(node_id, ifindex, if_name, if_alias,
/// if_speed)`. A `None` name/alias/speed leaves the stored value untouched (COALESCE).
pub type InterfaceBatchRow = (Uuid, i32, Option<String>, Option<String>, Option<i64>);

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

/// The dependency-graph skeleton for one node: just enough to draw the topology/dependency views
/// (id, display name, upstream parent). Loaded in keyset pages by [`NodeRepo::list_topology_page`]
/// so the endpoints never build one unbounded full-fleet row set (S7).
#[derive(Debug, Clone)]
pub struct TopologyRow {
    pub id: Uuid,
    pub name: String,
    pub parent_id: Option<Uuid>,
}

/// Fixed id for the seeded demo node the walking-skeleton WebUI queries.
const DEMO_NODE_ID: Uuid = Uuid::nil();

/// Ceiling on one server-side node search page.
///
/// One number, deliberately: the API edge and both [`NodeListing`] implementations clamp against
/// it. It used to be written twice — the edge clamped to 500 and documented that as the maximum,
/// while the SQL re-clamped to 100 — so filtering a fleet with thousands of matches silently
/// returned 100 rows with nothing saying the list had been cut (extensibility.md §3: the same
/// fact in two places drifts, and the copy that is wrong is the one nobody reads).
pub const NODE_SEARCH_MAX: i64 = 500;

/// The folder groups a query is restricted to, or `None` for no restriction at all (ADR-014).
///
/// This is deliberately a **group** filter and not a node-id list: expanding a scope to node ids
/// would mean a full-fleet scan on every request, which is exactly what S2/S6/S7 removed. The
/// caller builds it with `api::scope::NodeScope::group_filter`.
///
/// `Some(&[])` is meaningful and must survive: it means "no groups", i.e. match nothing. A scope
/// naming only deleted groups produces it, and collapsing it into `None` would turn a broken scope
/// into unrestricted access. Every query below binds it directly rather than branching on it, so
/// there is no code path that can drop the predicate — see [`NodeRepo::SCOPE_PREDICATE`].
pub type GroupFilter<'a> = Option<&'a [Uuid]>;

/// A read-only source of the node inventory for the API. Implemented by [`NodeRepo`]
/// (live, PostgreSQL) and [`StaticNodeList`] (skeleton mode), so the router doesn't care
/// which is behind it.
///
/// Every method takes a [`GroupFilter`]. That is a signature change made on purpose: it is what
/// forces each call site to answer "what may this caller see" rather than defaulting to the whole
/// fleet, and the compiler asks the question at every one of them.
#[async_trait]
pub trait NodeListing: Send + Sync {
    /// One keyset page: nodes with `id > after` (or from the start), ordered by id,
    /// capped at `limit`. The API paginates with this so large inventories don't load
    /// everything (ui-conventions: scale-aware lists).
    async fn list_page(
        &self,
        groups: GroupFilter<'_>,
        after: Option<Uuid>,
        limit: i64,
    ) -> anyhow::Result<Vec<Node>>;
    /// Node count within the caller's scope — the denominator the fleet summary needs (the paged
    /// `list_page` only ever sees one page). Cheap `count(*)`, no row transfer.
    async fn count(&self, groups: GroupFilter<'_>) -> anyhow::Result<i64>;
    /// Every node's `(id, group_id)` — the lightweight join key for the per-group health rollup
    /// (site-matrix / region-rollup / geo-map widgets). Two columns only; the live state comes
    /// from the in-memory alert engine, so the grouping happens in-process (PG holds no live
    /// state). O(fleet) but a single indexed scan, computed server-side once per poll instead of
    /// shipping the whole inventory to every dashboard client (which under-counted every group
    /// past the first page — a correctness bug, S12/A-1).
    async fn node_group_map(
        &self,
        groups: GroupFilter<'_>,
    ) -> anyhow::Result<Vec<(Uuid, Option<Uuid>)>>;
    /// Nodes whose name or address matches a case-insensitive substring (empty term ⇒ the first
    /// page ordered by name), capped at `limit`. Backs the node-picker's server-side typeahead so
    /// it never loads the whole inventory into the browser (ui-conventions: search is server-side
    /// at fleet scale, A-2).
    async fn search(
        &self,
        groups: GroupFilter<'_>,
        term: &str,
        limit: i64,
    ) -> anyhow::Result<Vec<Node>>;
}

#[async_trait]
impl NodeListing for NodeRepo {
    async fn list_page(
        &self,
        groups: GroupFilter<'_>,
        after: Option<Uuid>,
        limit: i64,
    ) -> anyhow::Result<Vec<Node>> {
        self.list_nodes_page(groups, after, limit).await
    }
    async fn count(&self, groups: GroupFilter<'_>) -> anyhow::Result<i64> {
        Ok(sqlx::query_scalar::<_, i64>(&format!(
            "SELECT count(*) FROM nodes WHERE {}",
            Self::SCOPE_PREDICATE
        ))
        .bind(Self::scope_bind(groups))
        .fetch_one(&self.pool)
        .await?)
    }
    async fn node_group_map(
        &self,
        groups: GroupFilter<'_>,
    ) -> anyhow::Result<Vec<(Uuid, Option<Uuid>)>> {
        let rows = sqlx::query(&format!(
            "SELECT id, group_id FROM nodes WHERE {}",
            Self::SCOPE_PREDICATE
        ))
        .bind(Self::scope_bind(groups))
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| Ok((row.try_get("id")?, row.try_get("group_id")?)))
            .collect()
    }
    async fn search(
        &self,
        groups: GroupFilter<'_>,
        term: &str,
        limit: i64,
    ) -> anyhow::Result<Vec<Node>> {
        let limit = limit.clamp(1, NODE_SEARCH_MAX);
        // Parameterized ILIKE (security.md — never string-format user input into SQL); the `%`
        // wildcards are concatenated in SQL so the term itself is a bound value. `host(address)`
        // strips the netmask so an IP-substring search matches the displayed address.
        let rows = if term.is_empty() {
            sqlx::query(&format!(
                "SELECT {} FROM nodes WHERE {} ORDER BY name, id LIMIT $2",
                Self::NODE_COLUMNS,
                Self::SCOPE_PREDICATE
            ))
            .bind(Self::scope_bind(groups))
            .bind(limit)
            .fetch_all(&self.pool)
            .await?
        } else {
            sqlx::query(&format!(
                "SELECT {} FROM nodes \
                 WHERE {} \
                   AND (name ILIKE '%' || $2 || '%' OR host(address) ILIKE '%' || $2 || '%') \
                 ORDER BY name, id LIMIT $3",
                Self::NODE_COLUMNS,
                Self::SCOPE_PREDICATE
            ))
            .bind(Self::scope_bind(groups))
            .bind(term)
            .bind(limit)
            .fetch_all(&self.pool)
            .await?
        };
        rows.iter().map(node_from_row).collect()
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

impl StaticNodeList {
    /// The in-memory twin of [`NodeRepo::SCOPE_PREDICATE`]. The two are a mirror
    /// (`extensibility.md` §2), so they are asserted against each other in the tests below rather
    /// than each against itself — a skeleton that scoped differently from the live store would be a
    /// silent behavioural difference exactly where nobody looks.
    fn in_scope(groups: GroupFilter<'_>, node: &Node) -> bool {
        match groups {
            None => true,
            // Mirrors `group_id = ANY($1)`: an ungrouped node matches no group, so it is invisible
            // to a scoped caller — the same rule as `Scope::allows`.
            Some(allowed) => node.group.is_some_and(|g| allowed.contains(&g.as_uuid())),
        }
    }
}

#[async_trait]
impl NodeListing for StaticNodeList {
    async fn list_page(
        &self,
        groups: GroupFilter<'_>,
        after: Option<Uuid>,
        limit: i64,
    ) -> anyhow::Result<Vec<Node>> {
        let mut nodes: Vec<Node> = self
            .0
            .iter()
            .filter(|n| Self::in_scope(groups, n))
            .cloned()
            .collect();
        nodes.sort_by_key(|n| n.id.as_uuid());
        Ok(nodes
            .into_iter()
            .filter(|n| after.is_none_or(|a| n.id.as_uuid() > a))
            .take(limit.clamp(1, 501) as usize)
            .collect())
    }
    async fn count(&self, groups: GroupFilter<'_>) -> anyhow::Result<i64> {
        Ok(self.0.iter().filter(|n| Self::in_scope(groups, n)).count() as i64)
    }
    async fn node_group_map(
        &self,
        groups: GroupFilter<'_>,
    ) -> anyhow::Result<Vec<(Uuid, Option<Uuid>)>> {
        Ok(self
            .0
            .iter()
            .filter(|n| Self::in_scope(groups, n))
            .map(|n| (n.id.as_uuid(), n.group.map(|g| g.as_uuid())))
            .collect())
    }
    async fn search(
        &self,
        groups: GroupFilter<'_>,
        term: &str,
        limit: i64,
    ) -> anyhow::Result<Vec<Node>> {
        let t = term.to_lowercase();
        let mut nodes: Vec<Node> = self
            .0
            .iter()
            .filter(|n| Self::in_scope(groups, n))
            .filter(|n| {
                t.is_empty()
                    || n.name.to_lowercase().contains(&t)
                    || n.address.to_string().contains(&t)
            })
            .cloned()
            .collect();
        nodes.sort_by(|a, b| {
            a.name
                .cmp(&b.name)
                .then(a.id.as_uuid().cmp(&b.id.as_uuid()))
        });
        nodes.truncate(limit.clamp(1, NODE_SEARCH_MAX) as usize);
        Ok(nodes)
    }
}

/// The nodes/profiles metadata store.
pub struct NodeRepo {
    pool: PgPool,
}

/// One pre-validated node to bulk-import (borrows from the request to avoid copies).
pub struct NewNode<'a> {
    pub name: &'a str,
    pub address: IpAddr,
    pub profile: Option<Uuid>,
    pub credential: Option<Uuid>,
    pub vendor: Option<&'a str>,
    pub model: Option<&'a str>,
}

impl NodeRepo {
    /// Connect (with retry, so Postgres may start after core) and return the repo.
    pub async fn connect(url: &str) -> anyhow::Result<Self> {
        const MAX_ATTEMPTS: u32 = 30;
        // One pool is shared by every core store (scheduler sweep, result ingest, API, coordinator
        // mirror), so 5 connections is the whole process's DB concurrency ceiling — far too low for
        // the tens-of-thousands-of-nodes target (the scheduler alone builds specs with concurrency
        // 16). Default higher and let deployments tune it via env. Postgres' own `max_connections`
        // (default 100) remains the outer bound; keep the default comfortably under it.
        let max_conns = std::env::var("YAGRA_PG_MAX_CONNECTIONS")
            .ok()
            .and_then(|v| v.parse::<u32>().ok())
            .filter(|&n| n > 0)
            .unwrap_or(20);
        let mut attempt = 0;
        loop {
            let result = PgPoolOptions::new()
                .max_connections(max_conns)
                .acquire_timeout(Duration::from_secs(5))
                .connect(url)
                .await;
            match result {
                Ok(pool) => {
                    tracing::info!(max_connections = max_conns, "connected to PostgreSQL");
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

    /// Cheap liveness probe: `SELECT 1` against the pool. `false` on any failure (DB down,
    /// pool exhausted, or the 5s acquire timeout elapsing). Used by the system-health endpoint.
    pub async fn healthy(&self) -> bool {
        sqlx::query("SELECT 1").execute(&self.pool).await.is_ok()
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

    /// The RBAC group-visibility predicate (ADR-014), **always bound as `$1`** in the queries that
    /// use it. A `NULL` array means unrestricted; an empty array matches nothing.
    ///
    /// It is written as one always-present predicate rather than a conditionally-appended clause on
    /// purpose. A conditional clause has a branch that can be forgotten — and forgetting it fails
    /// *open*, returning the whole fleet to a scoped caller with no error anywhere. Here the only
    /// way to get it wrong is to bind the wrong value, which [`Self::scope_bind`] is the single
    /// source of.
    ///
    /// The trade-off is that the planner cannot use `nodes_group_idx` through the `OR`, so a scoped
    /// list walks the primary-key index filtering as it goes. At the 50k-node target that is one
    /// index scan for a paged query — cheap, and paid only by scoped callers, since an unrestricted
    /// one binds `NULL` and the predicate collapses to true.
    ///
    /// ⚠️ When adding it to a query that already has a `WHERE`, **parenthesize the existing
    /// condition**: `WHERE {SCOPE} AND (a OR b)`. Written as `WHERE {SCOPE} AND a OR b` it parses
    /// as `({SCOPE} AND a) OR b`, and every row matching `b` escapes the scope entirely.
    const SCOPE_PREDICATE: &'static str = "($1::uuid[] IS NULL OR group_id = ANY($1))";

    /// The value to bind for [`Self::SCOPE_PREDICATE`]. `None` ⇒ SQL `NULL` ⇒ no restriction.
    fn scope_bind(groups: GroupFilter<'_>) -> Option<Vec<Uuid>> {
        groups.map(<[Uuid]>::to_vec)
    }

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

    /// One keyset page of nodes ordered by id, starting after `after`, within `groups`.
    ///
    /// The scope predicate lives in the `WHERE`, the cursor in `id > $2` — they are independent, so
    /// paging is unaffected by scoping (a page may simply contain fewer than `limit` rows).
    pub async fn list_nodes_page(
        &self,
        groups: GroupFilter<'_>,
        after: Option<Uuid>,
        limit: i64,
    ) -> anyhow::Result<Vec<Node>> {
        // Upper bound 501 (not 500) so the API can fetch one extra row past a 500-item page to
        // detect "has more" without an extra round-trip; the user-facing limit is capped at 500.
        let limit = limit.clamp(1, 501);
        let rows = match after {
            Some(after) => {
                sqlx::query(&format!(
                    "SELECT {} FROM nodes WHERE {} AND id > $2 ORDER BY id LIMIT $3",
                    Self::NODE_COLUMNS,
                    Self::SCOPE_PREDICATE
                ))
                .bind(Self::scope_bind(groups))
                .bind(after)
                .bind(limit)
                .fetch_all(&self.pool)
                .await?
            }
            None => {
                sqlx::query(&format!(
                    "SELECT {} FROM nodes WHERE {} ORDER BY id LIMIT $2",
                    Self::NODE_COLUMNS,
                    Self::SCOPE_PREDICATE
                ))
                .bind(Self::scope_bind(groups))
                .bind(limit)
                .fetch_all(&self.pool)
                .await?
            }
        };
        rows.iter().map(node_from_row).collect()
    }

    /// One keyset page of the dependency-graph skeleton — only `(id, name, parent_id)` — ordered by
    /// id, starting after `after` (S7). Deliberately light: the topology/coverage endpoints don't
    /// need the full node row (address/creds/tags), and at 50k nodes a large page keeps the
    /// whole-graph assembly to a handful of round-trips instead of returning one unbounded JSON blob.
    /// Scoping note: a scoped caller sees only the in-scope nodes, so a dependency edge whose
    /// parent lies outside the scope arrives with a `parent_id` that is not in the response. The
    /// graph renders it as a root, which is the honest reading — the caller cannot see the parent,
    /// so they cannot be told the child hangs off it.
    pub async fn list_topology_page(
        &self,
        groups: GroupFilter<'_>,
        after: Option<Uuid>,
        limit: i64,
    ) -> anyhow::Result<Vec<TopologyRow>> {
        // Larger page than the node list (its 501 cap is UI-facing); +1 to detect "has more".
        let limit = limit.clamp(1, 5001);
        let rows = match after {
            Some(after) => {
                sqlx::query(&format!(
                    "SELECT id, name, parent_id FROM nodes WHERE {} AND id > $2 \
                     ORDER BY id LIMIT $3",
                    Self::SCOPE_PREDICATE
                ))
                .bind(Self::scope_bind(groups))
                .bind(after)
                .bind(limit)
                .fetch_all(&self.pool)
                .await?
            }
            None => {
                sqlx::query(&format!(
                    "SELECT id, name, parent_id FROM nodes WHERE {} ORDER BY id LIMIT $2",
                    Self::SCOPE_PREDICATE
                ))
                .bind(Self::scope_bind(groups))
                .bind(limit)
                .fetch_all(&self.pool)
                .await?
            }
        };
        rows.iter()
            .map(|row| {
                Ok(TopologyRow {
                    id: row.try_get("id")?,
                    name: row.try_get("name")?,
                    parent_id: row.try_get("parent_id")?,
                })
            })
            .collect()
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

    /// Bulk-import nodes **atomically**: all rows insert in a single transaction, so a failure
    /// partway (e.g. a duplicate name hitting the unique constraint) rolls back the whole batch
    /// instead of leaving a partial import. Returns how many were inserted. Caller pre-validates.
    pub async fn import_nodes(&self, nodes: &[NewNode<'_>]) -> anyhow::Result<u32> {
        let mut tx = self.pool.begin().await?;
        for n in nodes {
            sqlx::query(
                "INSERT INTO nodes (id, name, address, profile_id, credential_id, vendor, model) \
                 VALUES ($1, $2, $3::inet, $4, $5, $6, $7)",
            )
            .bind(Uuid::new_v4())
            .bind(n.name)
            .bind(n.address.to_string())
            .bind(n.profile)
            .bind(n.credential)
            .bind(n.vendor)
            .bind(n.model)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(nodes.len() as u32)
    }

    /// Set (or clear) a node's profile, bound credential, and vendor/model metadata, and optionally
    /// move it to a different poll-pool (ADR-009). Returns whether the node exists.
    ///
    /// `profile`/`credential`/`vendor`/`model` are set to the passed value (a `None` clears) — the
    /// node-edit UI loads the current values and resends them, so an unchanged field is preserved.
    /// `pool` is three-state so the pool can be *left alone* independently: outer `None` = leave the
    /// pool unchanged, inner `None` = clear it to NULL (falls back to the `default` pool), inner
    /// `Some` = set it (the caller has already validated it as a NATS-subject-safe token).
    pub async fn set_node_bindings(
        &self,
        id: Uuid,
        profile: Option<Uuid>,
        credential: Option<Uuid>,
        vendor: Option<&str>,
        model: Option<&str>,
        pool: Option<Option<&str>>,
    ) -> anyhow::Result<bool> {
        let res = sqlx::query(
            "UPDATE nodes SET profile_id = $2, credential_id = $3, vendor = $4, model = $5, \
             pool = CASE WHEN $6::boolean THEN $7::text ELSE pool END, \
             updated_at = now() WHERE id = $1",
        )
        .bind(id)
        .bind(profile)
        .bind(credential)
        .bind(vendor)
        .bind(model)
        // `$6` gates whether the pool is touched at all; `$7` is the new value (NULL when clearing).
        .bind(pool.is_some())
        .bind(pool.flatten())
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

    /// Set (or clear with `None`) a node's own poll-pool (ADR-009/020). `None` ⇒ NULL, so the node
    /// falls back to its folder's pool, else the default pool. Returns whether the node exists.
    ///
    /// Single-purpose on purpose: [`Self::set_node_bindings`] overwrites profile/credential/
    /// vendor/model unconditionally (only its `pool` is three-state-gated), so a caller that wants
    /// to move *just* the pool — the inventory tree's context menu — must not go through it.
    pub async fn set_node_pool(&self, id: Uuid, pool: Option<&str>) -> anyhow::Result<bool> {
        let res = sqlx::query("UPDATE nodes SET pool = $2, updated_at = now() WHERE id = $1")
            .bind(id)
            .bind(pool)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() > 0)
    }

    /// The distinct non-empty pools nodes are assigned to. Feeds the pool picker; the `pool` index
    /// (migration 0001) keeps this cheap even at fleet scale.
    pub async fn distinct_pools(&self) -> anyhow::Result<Vec<String>> {
        let rows: Vec<String> = sqlx::query_scalar(
            "SELECT DISTINCT pool FROM nodes WHERE pool IS NOT NULL AND pool <> ''",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    /// Set (or clear with `None`) a node's **dependency parent** (upstream) — the `parent_id`
    /// edge that feeds parent-down alert suppression and root-cause roll-up (ADR-015). Distinct
    /// from [`Self::set_node_group`], which moves a node in the inventory *folder* tree. Returns
    /// whether the node exists. The caller validates that `parent` exists and that the new edge
    /// introduces no cycle ([`crate::groups::would_create_cycle`]) before calling this.
    pub async fn set_node_parent(&self, id: Uuid, parent: Option<Uuid>) -> anyhow::Result<bool> {
        let res = sqlx::query("UPDATE nodes SET parent_id = $2, updated_at = now() WHERE id = $1")
            .bind(id)
            .bind(parent)
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

    /// The ids of nodes whose `group_id` is in `group_ids` — used to resolve a Troubleshoot
    /// "group" scope to a group + its descendant subgroups (the caller flattens the subtree via
    /// [`crate::groups::group_subtree`]). Parameterized `= ANY($1)` (security.md); empty input
    /// short-circuits so we never run an empty-array query.
    pub async fn nodes_in_groups(&self, group_ids: &[Uuid]) -> anyhow::Result<Vec<Uuid>> {
        if group_ids.is_empty() {
            return Ok(Vec::new());
        }
        let rows = sqlx::query(
            "SELECT id FROM nodes WHERE group_id = ANY($1) ORDER BY sort_order, name, id",
        )
        .bind(group_ids)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(|row| Ok(row.try_get("id")?)).collect()
    }

    /// A group's **direct** member nodes (or the ungrouped bucket when `group` is `None`), ordered
    /// by the tree's sort order, capped at `limit`. Backs the inventory tree's per-group lazy load
    /// (A-3): the tree fetches a group's members only when it is expanded, so the initial view never
    /// pulls the whole fleet. `IS NOT DISTINCT FROM` so a `NULL` group matches the ungrouped rows.
    ///
    /// The scope predicate rides alongside the group filter rather than replacing it: asking for
    /// the ungrouped bucket (`group = None`) as a scoped caller correctly returns nothing, because
    /// an ungrouped node is outside every group scope (`rbac.rs`).
    pub async fn list_nodes_in_group(
        &self,
        groups: GroupFilter<'_>,
        group: Option<Uuid>,
        limit: i64,
    ) -> anyhow::Result<Vec<Node>> {
        let limit = limit.clamp(1, 5001);
        let rows = sqlx::query(&format!(
            "SELECT {} FROM nodes \
             WHERE {} AND group_id IS NOT DISTINCT FROM $2::uuid \
             ORDER BY sort_order, name, id LIMIT $3",
            Self::NODE_COLUMNS,
            Self::SCOPE_PREDICATE
        ))
        .bind(Self::scope_bind(groups))
        .bind(group)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(node_from_row).collect()
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

    /// Fill blank vendor/model
    /// for MANY nodes in one `UPDATE` (the async ingest writer, ADR-025). `COALESCE` preserves any
    /// existing value; a `None` leaves that column alone. `unnest` binds arrays, so the row count is
    /// unbounded by Postgres' parameter ceiling. Dedups keeping the last occurrence per node.
    pub async fn fill_node_identity_batch(
        &self,
        rows: &[(Uuid, Option<String>, Option<String>)],
    ) -> anyhow::Result<()> {
        if rows.is_empty() {
            return Ok(());
        }
        let mut by_node: BTreeMap<Uuid, (Option<String>, Option<String>)> = BTreeMap::new();
        for (node, vendor, model) in rows {
            by_node.insert(*node, (vendor.clone(), model.clone()));
        }
        let ids: Vec<Uuid> = by_node.keys().copied().collect();
        let vendors: Vec<Option<String>> = by_node.values().map(|v| v.0.clone()).collect();
        let models: Vec<Option<String>> = by_node.values().map(|v| v.1.clone()).collect();
        sqlx::query(
            "UPDATE nodes SET \
                vendor = COALESCE(nodes.vendor, t.vendor), \
                model = COALESCE(nodes.model, t.model), \
                updated_at = now() \
             FROM unnest($1::uuid[], $2::text[], $3::text[]) AS t(id, vendor, model) \
             WHERE nodes.id = t.id",
        )
        .bind(&ids)
        .bind(&vendors)
        .bind(&models)
        .execute(&self.pool)
        .await?;
        Ok(())
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

    /// Append one node-state snapshot: a row per `(state, count)`, all sharing the same `now()`
    /// timestamp (single statement). For the fleet health timeline. Low cardinality (≤6 rows).
    pub async fn insert_state_snapshot(&self, counts: &[(String, i64)]) -> anyhow::Result<()> {
        if counts.is_empty() {
            return Ok(());
        }
        let states: Vec<String> = counts.iter().map(|(s, _)| s.clone()).collect();
        // Saturate rather than silently wrap: a per-state node count above i32::MAX is not
        // reachable at the design scale (tens of thousands), so flag it instead of corrupting
        // the timeline with a negative value.
        let nums: Vec<i32> = counts
            .iter()
            .map(|(_, c)| {
                i32::try_from(*c).unwrap_or_else(|_| {
                    tracing::warn!(count = *c, "state-snapshot count exceeds i32; saturating");
                    i32::MAX
                })
            })
            .collect();
        sqlx::query(
            "INSERT INTO node_state_snapshots (ts, state, count) \
             SELECT now(), s, c FROM unnest($1::text[], $2::int[]) AS t(s, c)",
        )
        .bind(&states)
        .bind(&nums)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// State-count snapshots over `[from_s, to_s]` (Unix seconds) as `(ts_unix, state, count)`,
    /// oldest first. Pivoted into per-state series at the API edge.
    pub async fn state_history(
        &self,
        from_s: i64,
        to_s: i64,
    ) -> anyhow::Result<Vec<(i64, String, i64)>> {
        let rows = sqlx::query(
            "SELECT extract(epoch from ts)::bigint AS t, state, count \
             FROM node_state_snapshots \
             WHERE ts >= to_timestamp($1) AND ts <= to_timestamp($2) ORDER BY ts",
        )
        .bind(from_s)
        .bind(to_s)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|r| {
                Ok((
                    r.try_get("t")?,
                    r.try_get("state")?,
                    r.try_get::<i32, _>("count")? as i64,
                ))
            })
            .collect()
    }

    /// Delete state snapshots older than `older_than_secs` (retention). Returns rows removed.
    pub async fn prune_state_snapshots(&self, older_than_secs: i64) -> anyhow::Result<u64> {
        let res = sqlx::query(
            "DELETE FROM node_state_snapshots WHERE ts < now() - ($1::double precision * interval '1 second')",
        )
        .bind(older_than_secs as f64)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected())
    }

    /// The display name of each of the given node ids, in one query. For joining TSDB results
    /// (which carry only the node id, ADR-011) back to human-readable names — e.g. the fleet
    /// Top-N endpoint. Ids absent from the map default to the id string at the call site.
    ///
    /// Scoped, because this is also the batch resolver behind `POST /api/v1/node-names`: a caller
    /// supplies ids and receives names, so without the filter it would answer "does a node with
    /// this id exist, and what is it called" for the entire fleet. An out-of-scope id is simply
    /// omitted, which is what the endpoint already does for an unknown id.
    pub async fn node_names(
        &self,
        groups: GroupFilter<'_>,
        ids: &[Uuid],
    ) -> anyhow::Result<HashMap<Uuid, String>> {
        if ids.is_empty() {
            return Ok(HashMap::new());
        }
        let rows = sqlx::query(&format!(
            "SELECT id, name FROM nodes WHERE {} AND id = ANY($2)",
            Self::SCOPE_PREDICATE
        ))
        .bind(Self::scope_bind(groups))
        .bind(ids)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| Ok((row.try_get("id")?, row.try_get("name")?)))
            .collect()
    }

    /// Node ids whose display name matches a case-insensitive substring, capped at `cap`. Lets the
    /// event-log search (which runs against the log store, ADR-024) still find events by node name
    /// without the name ever entering the log store: the API resolves the name → ids here and
    /// passes them as a `node_id` filter (query-time join, ADR-011).
    pub async fn node_ids_by_name_like(
        &self,
        groups: GroupFilter<'_>,
        term: &str,
        cap: i64,
    ) -> anyhow::Result<Vec<Uuid>> {
        let rows = sqlx::query(&format!(
            "SELECT id FROM nodes WHERE {} AND name ILIKE '%' || $2 || '%' LIMIT $3",
            Self::SCOPE_PREDICATE
        ))
        .bind(Self::scope_bind(groups))
        .bind(term)
        .bind(cap.clamp(1, 200))
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(|row| Ok(row.try_get("id")?)).collect()
    }

    /// Address → node-id map for correlating passive events (syslog/trap source IPs) to
    /// inventory. Snapshotted by the event engine's periodic reload. If two nodes share an
    /// address the first row wins.
    pub async fn address_map(&self) -> anyhow::Result<HashMap<IpAddr, Uuid>> {
        let rows = sqlx::query("SELECT id, host(address) AS address FROM nodes")
            .fetch_all(&self.pool)
            .await?;
        let mut map = HashMap::with_capacity(rows.len());
        for row in rows {
            let id: Uuid = row.try_get("id")?;
            let addr: String = row.try_get("address")?;
            if let Ok(addr) = addr.parse::<IpAddr>() {
                map.entry(addr).or_insert(id);
            }
        }
        Ok(map)
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

    /// Upsert interfaces for MANY nodes in one statement — the async ingest writer (ADR-025)
    /// coalesces many polls, so this must not fan out per node. Names/aliases are device-supplied
    /// metadata kept in PostgreSQL (joined to metrics at query time) — never TSDB labels (ADR-011).
    /// Rows are [`InterfaceBatchRow`]: `(node_id, ifindex, if_name, if_alias, if_speed)`. `unnest`
    /// binds arrays, so the row count is unbounded by the 65535-parameter ceiling. Dedups within the
    /// batch keeping the last occurrence per `(node_id, ifindex)` — `ON CONFLICT` cannot touch the
    /// same key twice in one statement.
    pub async fn upsert_interfaces_batch(&self, rows: &[InterfaceBatchRow]) -> anyhow::Result<()> {
        if rows.is_empty() {
            return Ok(());
        }
        type OwnedIfaceMeta = (Option<String>, Option<String>, Option<i64>);
        let mut by_key: BTreeMap<(Uuid, i32), OwnedIfaceMeta> = BTreeMap::new();
        for (node, ifindex, name, alias, speed) in rows {
            by_key.insert((*node, *ifindex), (name.clone(), alias.clone(), *speed));
        }
        let mut node_ids: Vec<Uuid> = Vec::with_capacity(by_key.len());
        let mut ifindexes: Vec<i32> = Vec::with_capacity(by_key.len());
        let mut names: Vec<Option<String>> = Vec::with_capacity(by_key.len());
        let mut aliases: Vec<Option<String>> = Vec::with_capacity(by_key.len());
        let mut speeds: Vec<Option<i64>> = Vec::with_capacity(by_key.len());
        for ((node, ifindex), (name, alias, speed)) in by_key {
            node_ids.push(node);
            ifindexes.push(ifindex);
            names.push(name);
            aliases.push(alias);
            speeds.push(speed);
        }
        sqlx::query(
            "INSERT INTO interfaces (node_id, ifindex, if_name, if_alias, if_speed, last_seen) \
             SELECT t.node_id, t.ifindex, t.if_name, t.if_alias, t.if_speed, now() \
             FROM unnest($1::uuid[], $2::int[], $3::text[], $4::text[], $5::int8[]) \
                  AS t(node_id, ifindex, if_name, if_alias, if_speed) \
             ON CONFLICT (node_id, ifindex) DO UPDATE SET \
                if_name = COALESCE(EXCLUDED.if_name, interfaces.if_name), \
                if_alias = COALESCE(EXCLUDED.if_alias, interfaces.if_alias), \
                if_speed = COALESCE(EXCLUDED.if_speed, interfaces.if_speed), \
                last_seen = now()",
        )
        .bind(&node_ids)
        .bind(&ifindexes)
        .bind(&names)
        .bind(&aliases)
        .bind(&speeds)
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
        let rows = sqlx::query(
            "SELECT id, name, category, vendor, poll_interval_secs FROM profiles ORDER BY name",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| {
                Ok(ProfileSummary {
                    id: row.try_get("id")?,
                    name: row.try_get("name")?,
                    category: row.try_get("category")?,
                    vendor: row.try_get("vendor")?,
                    poll_interval_secs: row.try_get("poll_interval_secs")?,
                })
            })
            .collect()
    }

    /// The id of one profile with the given `ProfileCategory` token (e.g. `url-check`), if any.
    /// Used to bind a freshly created URL monitor to the built-in URL/HTTP profile so it inherits
    /// the default thresholds. Lowest name wins for determinism if several share the category.
    pub async fn profile_id_for_category(&self, category: &str) -> anyhow::Result<Option<Uuid>> {
        let row =
            sqlx::query("SELECT id FROM profiles WHERE category = $1 ORDER BY name, id LIMIT 1")
                .bind(category)
                .fetch_optional(&self.pool)
                .await?;
        row.map(|r| Ok(r.try_get("id")?)).transpose()
    }

    /// The id of the profile with the given exact name (for binding imported Meraki devices to their
    /// specific built-in API profile), if one exists.
    pub async fn profile_id_for_name(&self, name: &str) -> anyhow::Result<Option<Uuid>> {
        let row = sqlx::query("SELECT id FROM profiles WHERE name = $1 LIMIT 1")
            .bind(name)
            .fetch_optional(&self.pool)
            .await?;
        row.map(|r| Ok(r.try_get("id")?)).transpose()
    }

    /// Create a profile; returns its id. `poll_interval_secs` is the optional per-profile interval
    /// override (`None` ⇒ inherit the global default).
    pub async fn create_profile(
        &self,
        name: &str,
        category: &str,
        vendor: Option<&str>,
        poll_interval_secs: Option<i32>,
    ) -> anyhow::Result<Uuid> {
        let id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO profiles (id, name, category, vendor, poll_interval_secs) \
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(id)
        .bind(name)
        .bind(category)
        .bind(vendor)
        .bind(poll_interval_secs)
        .execute(&self.pool)
        .await?;
        Ok(id)
    }

    /// Update a profile's name / category / vendor / interval override. Returns whether the row
    /// existed. A `None` `poll_interval_secs` clears the override (back to the global default).
    pub async fn update_profile(
        &self,
        id: Uuid,
        name: &str,
        category: &str,
        vendor: Option<&str>,
        poll_interval_secs: Option<i32>,
    ) -> anyhow::Result<bool> {
        let res = sqlx::query(
            "UPDATE profiles SET name = $2, category = $3, vendor = $4, poll_interval_secs = $5, \
             updated_at = now() WHERE id = $1",
        )
        .bind(id)
        .bind(name)
        .bind(category)
        .bind(vendor)
        .bind(poll_interval_secs)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected() > 0)
    }

    /// The global default polling interval (seconds) from the singleton `app_settings` row. Falls
    /// back to the compiled default if the row is somehow absent (it is seeded at startup).
    pub async fn get_default_poll_interval(&self) -> anyhow::Result<u32> {
        let row =
            sqlx::query("SELECT default_poll_interval_secs FROM app_settings WHERE id = TRUE")
                .fetch_optional(&self.pool)
                .await?;
        match row {
            Some(r) => {
                let secs: i32 = r.try_get("default_poll_interval_secs")?;
                Ok(u32::try_from(secs).unwrap_or(crate::config::DEFAULT_POLL_INTERVAL_SECS))
            }
            None => Ok(crate::config::DEFAULT_POLL_INTERVAL_SECS),
        }
    }

    /// Set the global default polling interval (seconds), upserting the singleton row. Callers
    /// validate the bounds at the API edge; the table CHECK is the backstop.
    pub async fn set_default_poll_interval(&self, secs: u32) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO app_settings (id, default_poll_interval_secs, updated_at) \
             VALUES (TRUE, $1, now()) \
             ON CONFLICT (id) DO UPDATE SET default_poll_interval_secs = $1, updated_at = now()",
        )
        .bind(i32::try_from(secs).unwrap_or(i32::MAX))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Seed the singleton settings row on first boot from the env-var initial defaults.
    /// Idempotent: `ON CONFLICT DO NOTHING` preserves any operator-edited value across restarts,
    /// which is also why an *existing* deployment keeps the column defaults from migration 0061
    /// rather than importing `YAGRA_FLOW_RETENTION_DAYS` — see that migration's header for why
    /// importing it would delete flow rows nobody asked to lose.
    pub async fn seed_app_settings(
        &self,
        poll_interval_secs: u32,
        flow_retention_days: u32,
    ) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO app_settings (id, default_poll_interval_secs, flow_retention_days) \
             VALUES (TRUE, $1, $2) ON CONFLICT (id) DO NOTHING",
        )
        .bind(i32::try_from(poll_interval_secs).unwrap_or(i32::MAX))
        .bind(i32::try_from(flow_retention_days).unwrap_or(i32::MAX))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// The operator-configured retention windows (ADR-040). Like `get_meraki_polling_enabled`, this
    /// returns a value rather than a `Result` and degrades to the compiled defaults on any read
    /// failure: a transient database blip must never silently widen or narrow how long data is
    /// kept, and the prune loops call this every tick.
    pub async fn get_retention_settings(&self) -> RetentionSettings {
        let fallback = RetentionSettings::default();
        let Ok(Some(row)) = sqlx::query(
            "SELECT alert_linked_retention_days, unmatched_event_retention_hours, \
                    report_run_retention_days, flow_retention_days \
             FROM app_settings WHERE id = TRUE",
        )
        .fetch_optional(&self.pool)
        .await
        else {
            return fallback;
        };
        let read = |col: &str, default: u32| -> u32 {
            row.try_get::<i32, _>(col)
                .ok()
                .and_then(|v| u32::try_from(v).ok())
                .unwrap_or(default)
        };
        RetentionSettings {
            alert_linked_days: read("alert_linked_retention_days", fallback.alert_linked_days),
            unmatched_event_hours: read(
                "unmatched_event_retention_hours",
                fallback.unmatched_event_hours,
            ),
            report_run_days: read("report_run_retention_days", fallback.report_run_days),
            flow_days: read("flow_retention_days", fallback.flow_days),
        }
    }

    /// Set every retention window at once, upserting the singleton row. The API edge validates the
    /// bounds (`retention::days_in_bounds` / `hours_in_bounds`); the table CHECKs are the backstop.
    pub async fn set_retention_settings(&self, s: &RetentionSettings) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO app_settings (id, alert_linked_retention_days, \
                 unmatched_event_retention_hours, report_run_retention_days, \
                 flow_retention_days, updated_at) \
             VALUES (TRUE, $1, $2, $3, $4, now()) \
             ON CONFLICT (id) DO UPDATE SET alert_linked_retention_days = $1, \
                 unmatched_event_retention_hours = $2, report_run_retention_days = $3, \
                 flow_retention_days = $4, updated_at = now()",
        )
        .bind(i32::try_from(s.alert_linked_days).unwrap_or(i32::MAX))
        .bind(i32::try_from(s.unmatched_event_hours).unwrap_or(i32::MAX))
        .bind(i32::try_from(s.report_run_days).unwrap_or(i32::MAX))
        .bind(i32::try_from(s.flow_days).unwrap_or(i32::MAX))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// The global Cisco Meraki polling kill switch (safeguard). Defaults to `true` (enabled) if the
    /// row is somehow absent or on any read error, so a transient DB blip never silently pauses
    /// monitoring — the operator's explicit `false` is the only thing that halts polling.
    pub async fn get_meraki_polling_enabled(&self) -> bool {
        sqlx::query("SELECT meraki_polling_enabled FROM app_settings WHERE id = TRUE")
            .fetch_optional(&self.pool)
            .await
            .ok()
            .flatten()
            .and_then(|r| r.try_get::<bool, _>("meraki_polling_enabled").ok())
            .unwrap_or(true)
    }

    /// Set the global Meraki polling kill switch, upserting the singleton row.
    pub async fn set_meraki_polling_enabled(&self, enabled: bool) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO app_settings (id, meraki_polling_enabled, updated_at) \
             VALUES (TRUE, $1, now()) \
             ON CONFLICT (id) DO UPDATE SET meraki_polling_enabled = $1, updated_at = now()",
        )
        .bind(enabled)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Per-profile interval overrides (only profiles that set one), keyed by profile id. The
    /// scheduler resolves each node against this map, falling back to the global default.
    pub async fn profile_interval_overrides(&self) -> anyhow::Result<HashMap<Uuid, u32>> {
        let rows = sqlx::query(
            "SELECT id, poll_interval_secs FROM profiles WHERE poll_interval_secs IS NOT NULL",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut map = HashMap::new();
        for row in rows {
            let id: Uuid = row.try_get("id")?;
            let secs: i32 = row.try_get("poll_interval_secs")?;
            if let Ok(secs) = u32::try_from(secs) {
                map.insert(id, secs);
            }
        }
        Ok(map)
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
        // The stable bases live in `crate::seed_ids`, not here: the config-bundle exporter has to
        // recognise a built-in row to leave it out of a bundle, and a filter that disagreed with
        // this seeder would drop operator rows or carry re-keying ones. Same table, both sides.
        use crate::seed_ids::SeedRange;

        // 1. Templates + their metrics; remember name → id for the profile links.
        let mut template_id_by_name: HashMap<&'static str, Uuid> = HashMap::new();
        for (i, template) in yagra_common::builtin_templates().into_iter().enumerate() {
            let template_id = SeedRange::CollectionTemplates.id(i);
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
            let profile_id = SeedRange::Profiles.id(i);
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
            let rule_id = SeedRange::ClassificationRules.id(i);
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
        // 4. Default thresholds for the built-in URL/HTTP endpoint profile so a freshly created URL
        //    monitor alerts out of the box: `http_up` below 0.5 ⇒ critical (down or wrong status),
        //    and `ssl_cert_days_to_expiry` below 30/7 ⇒ warning/critical. Stable ids + ON CONFLICT
        //    DO NOTHING keep operator edits/deletes from being resurrected on the next boot.
        //
        //    NB: `http_up` is a 0/1 gauge and the engine's "below" comparison is INCLUSIVE
        //    (`value <= bound`, thresholds.rs). A bound of 1.0 would therefore fire on the healthy
        //    value 1 too — so the bound sits between the two states (0.5): only 0 (down/wrong-status)
        //    trips it. Migration 0030 corrects already-seeded rows that used the old 1.0 bound.
        if let Some(&url_profile_id) = profile_id_by_name.get("URL / HTTP endpoint") {
            let scope_id = url_profile_id.to_string();
            // (offset, metric, direction, warning, critical, dwell_samples)
            let defaults = [
                (0usize, "http_up", "below", None::<f64>, Some(0.5), 2i32),
                (
                    1usize,
                    "ssl_cert_days_to_expiry",
                    "below",
                    Some(30.0),
                    Some(7.0),
                    1i32,
                ),
            ];
            for (offset, metric, direction, warning, critical, dwell) in defaults {
                sqlx::query(
                    "INSERT INTO thresholds \
                        (id, scope_level, scope_id, metric, direction, warning, critical, dwell_samples) \
                     VALUES ($1, 'profile', $2, $3, $4, $5, $6, $7) ON CONFLICT (id) DO NOTHING",
                )
                .bind(SeedRange::UrlThresholds.id(offset))
                .bind(&scope_id)
                .bind(metric)
                .bind(direction)
                .bind(warning)
                .bind(critical)
                .bind(dwell)
                .execute(&self.pool)
                .await?;
            }
        }
        // 6. Default threshold for the built-in DNS profile (ADR-033), so a freshly created DNS
        //    monitor alerts out of the box: `dns_up` below 0.5 ⇒ critical. It reads 0 whenever the
        //    name does not resolve for ANY reason — NXDOMAIN / SERVFAIL / REFUSED / timeout /
        //    CNAME loop / depth exceeded — so this one threshold covers them all.
        //
        //    NB the bound is 0.5, NOT 1.0. `dns_up` is a 0/1 gauge and the engine's "below"
        //    comparison is INCLUSIVE (`value <= bound`, thresholds.rs), so 1.0 would fire on the
        //    healthy value too. That is exactly the mistake migration 0030 had to correct for
        //    `http_up`; seeds are ON CONFLICT DO NOTHING, so getting it wrong needs a corrective
        //    migration rather than an edit here.
        //
        //    `dns_resolve_ms` is emitted as a graphable gauge but deliberately gets NO seeded
        //    threshold: resolver latency varies far too much between environments for a default.
        //
        //    Reserved stable-id ranges: every one of them is declared in `crate::seed_ids`, which
        //    is also what migration 0020's range-DELETEs are tested against.
        if let Some(&dns_profile_id) = profile_id_by_name.get("DNS name resolution") {
            let scope_id = dns_profile_id.to_string();
            // (offset, metric, direction, warning, critical, dwell_samples)
            let defaults = [(0usize, "dns_up", "below", None::<f64>, Some(0.5), 2i32)];
            for (offset, metric, direction, warning, critical, dwell) in defaults {
                sqlx::query(
                    "INSERT INTO thresholds \
                        (id, scope_level, scope_id, metric, direction, warning, critical, dwell_samples) \
                     VALUES ($1, 'profile', $2, $3, $4, $5, $6, $7) ON CONFLICT (id) DO NOTHING",
                )
                .bind(SeedRange::DnsThresholds.id(offset))
                .bind(&scope_id)
                .bind(metric)
                .bind(direction)
                .bind(warning)
                .bind(critical)
                .bind(dwell)
                .execute(&self.pool)
                .await?;
            }
        }
        tracing::info!(
            "seeded built-in collection templates + device profiles + classification rules"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::IpAddr;

    fn node(id: u128, name: &str, addr: &str, group: Option<u128>) -> Node {
        let mut n = Node::new(
            NodeId::from(Uuid::from_u128(id)),
            name,
            addr.parse::<IpAddr>().unwrap(),
        );
        n.group = group.map(|g| GroupId::from(Uuid::from_u128(g)));
        n
    }

    #[tokio::test]
    async fn static_node_list_node_group_map_reports_group_membership() {
        let list = StaticNodeList(vec![
            node(1, "a", "10.0.0.1", Some(100)),
            node(2, "b", "10.0.0.2", None),
        ]);
        let map = list.node_group_map(None).await.unwrap();
        assert!(map.contains(&(Uuid::from_u128(1), Some(Uuid::from_u128(100)))));
        assert!(map.contains(&(Uuid::from_u128(2), None)));
    }

    /// The skeleton store is a **mirror** of `NodeRepo::SCOPE_PREDICATE` (extensibility.md §2), so
    /// this asserts the rules the SQL encodes rather than the Rust's own internal consistency:
    /// `None` filters nothing, a group set admits only its members, an ungrouped node is invisible
    /// to any scope, and an empty set matches nothing rather than everything.
    #[tokio::test]
    async fn the_skeleton_store_scopes_by_the_same_rules_as_the_sql_predicate() {
        let tokyo = Uuid::from_u128(100);
        let list = StaticNodeList(vec![
            node(1, "in-tokyo", "10.0.0.1", Some(100)),
            node(2, "ungrouped", "10.0.0.2", None),
            node(3, "in-osaka", "10.0.0.3", Some(200)),
        ]);

        // No filter ⇒ everything, including the ungrouped node.
        assert_eq!(list.count(None).await.unwrap(), 3);

        // A group set ⇒ only its members. The ungrouped node is NOT included — matching
        // `Scope::allows`, which never leaks an ungrouped node to a scoped principal.
        let only_tokyo = list.list_page(Some(&[tokyo]), None, 50).await.unwrap();
        assert_eq!(only_tokyo.len(), 1);
        assert_eq!(only_tokyo[0].name, "in-tokyo");
        assert_eq!(list.count(Some(&[tokyo])).await.unwrap(), 1);

        // The inversion that would be a privilege escalation: an empty set matches nothing.
        assert_eq!(list.count(Some(&[])).await.unwrap(), 0);
        assert!(list.search(Some(&[]), "", 50).await.unwrap().is_empty());
        assert!(list.node_group_map(Some(&[])).await.unwrap().is_empty());

        // Scope is applied before the search term, not instead of it.
        let hits = list.search(Some(&[tokyo]), "in-", 50).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].name, "in-tokyo");
    }

    #[tokio::test]
    async fn static_node_list_search_matches_name_or_address_ordered_and_capped() {
        let list = StaticNodeList(vec![
            node(1, "tokyo-edge", "10.0.0.1", None),
            node(2, "osaka-core", "10.0.0.2", None),
            node(3, "nagoya-edge", "192.168.5.9", None),
        ]);
        // Name substring, case-insensitive.
        assert_eq!(list.search(None, "EDGE", 50).await.unwrap().len(), 2);
        // Address substring.
        let by_addr = list.search(None, "192.168", 50).await.unwrap();
        assert_eq!(by_addr.len(), 1);
        assert_eq!(by_addr[0].name, "nagoya-edge");
        // Empty term returns all, ordered by name and capped.
        let capped = list.search(None, "", 2).await.unwrap();
        assert_eq!(capped.len(), 2);
        assert_eq!(capped[0].name, "nagoya-edge"); // nagoya < osaka < tokyo
        assert_eq!(capped[1].name, "osaka-core");
    }

    /// The search cap is one number, and no implementation may re-clamp to its own.
    ///
    /// A source-reading test rather than a behavioural one because the PostgreSQL path needs a
    /// live database — and that is exactly where the regression lived: the API edge clamped to
    /// 500 and documented that as the maximum, while `NodeRepo::search` re-clamped to 100, so
    /// filtering a large fleet silently returned 100 rows. The needle is built at runtime; a
    /// literal written out in this file would match itself and fail forever (testing.md).
    #[test]
    fn the_search_cap_is_declared_once() {
        let src = include_str!("repo.rs");
        // Both needles are assembled at runtime. A literal spelled out here would appear in this
        // file and match itself — the stale one would fail forever, the good one would over-count.
        let stale = format!("clamp(1, {})", 100);
        assert!(
            !src.contains(&stale),
            "a search path re-clamps to its own literal instead of the shared constant"
        );
        let shared = format!("clamp(1, {})", "NODE_SEARCH_MAX");
        // Both implementations clamp, and both clamp against the constant.
        assert_eq!(src.matches(&shared).count(), 2);
    }

    #[tokio::test]
    async fn search_is_capped_by_the_shared_constant_not_a_local_literal() {
        let nodes: Vec<Node> = (0..600u32)
            .map(|i| node(u128::from(i) + 1, &format!("sw-{i:04}"), "10.0.0.1", None))
            .collect();
        let list = StaticNodeList(nodes);
        // Asking for more than the cap yields exactly the cap, not some smaller inner limit.
        let hits = list.search(None, "sw-", NODE_SEARCH_MAX * 2).await.unwrap();
        assert_eq!(hits.len() as i64, NODE_SEARCH_MAX);
    }
}
