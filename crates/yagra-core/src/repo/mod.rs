// SPDX-License-Identifier: AGPL-3.0-only
//! PostgreSQL metadata: the node inventory, the deployment's own settings, and the bootstrap.
//!
//! Metadata — nodes, profiles, thresholds, alert history — lives in PostgreSQL (store separation,
//! CLAUDE.md Architecture). This is an I/O adapter (live-only), so it is exercised in deployment,
//! not unit tests; the domain types it returns ([`Node`]) are tested in `yagra-common`. Queries are
//! runtime `sqlx::query` (not the compile-time macro) so the build needs no live database.
//!
//! ## Where a method goes: the table its SQL names
//!
//! One rule, and it is mechanical so that the next person does not have to guess. `nodes` reads and
//! writes are in [`nodes`], `interfaces` in [`interfaces`], `profiles` in [`profiles`],
//! `app_settings` in [`settings`], `node_state_snapshots` in [`snapshots`], `_sqlx_migrations` in
//! [`migrate`]. [`guards`] holds it as a test rather than as this paragraph: a statement naming a
//! table its file has not declared fails the build, and so does a file nobody declared.
//!
//! 🚨 **The rule decides against the name when the two disagree, and two methods needed it.**
//! `suppression_opt_outs` and `set_suppression_opt_out` read a **per-node column** and had spent
//! their whole life filed among the deployment settings, purely because the previous method
//! happened to be one. A name is a claim; the SQL is the fact.
//!
//! Two files are not about one table and say so:
//!
//! * [`seed`] writes eight, because bootstrapping is by definition writing the whole catalogue.
//!   It declares all eight in [`guards`] with that as its reason.
//! * [`listing`] reads `nodes`, and is separate from [`nodes`] anyway — it holds [`NodeListing`],
//!   the trait the API sees, and both of its implementations, so the *mirror* between the SQL
//!   predicate and its in-memory twin sits in one file with the tests that pin them together.
//!
//! [`defaults`] holds no SQL at all: it is the `const` table of seeded alert rules that [`seed`]
//! inserts, split from the writer on the line between a `const` and the `async fn` that writes it.
//!
//! ## What lives here rather than in one of those files
//!
//! [`NodeRepo`] itself, its connection, and the SQL vocabulary every file shares
//! ([`NodeRepo::NODE_COLUMNS`], [`NodeRepo::SCOPE_PREDICATE`], [`NodeRepo::scope_bind`],
//! [`node_from_row`], [`GroupFilter`]). They are here and not in a sibling because **a child module
//! can see its parent's private items**, so nothing shared from this file needs widening — which is
//! why the whole split cost two `pub(super)` rather than the twenty it was budgeted at.
//!
//! Every public name is re-exported below, so the thirty files that say `crate::repo::X` did not
//! change when this became a directory (ADR-094).

use std::collections::BTreeMap;
use std::net::IpAddr;
use std::time::Duration;

use sqlx::postgres::{PgPool, PgPoolOptions};
use sqlx::types::Json;
use sqlx::Row;
use uuid::Uuid;
use yagra_common::{CredentialId, GroupId, Node, NodeId, ProfileId};

mod defaults;
mod interfaces;
mod listing;
mod migrate;
mod nodes;
mod profiles;
mod seed;
mod settings;
mod snapshots;

#[cfg(test)]
mod guards;

// Everything a caller outside this module could name before the split, still named the same way.
//
// ⚠️ `TopologyRow` and `InterfaceIdent` are flagged unused and are re-exported anyway: both are the
// return type of a `pub` method here, and no caller happens to write the name. Dropping them would
// make those two signatures unnameable from outside `repo` — a narrowing of the surface disguised
// as a warning fix, and the one difference this split would have made to a caller.
#[allow(unused_imports)]
pub use interfaces::{InterfaceBatchRow, InterfaceIdent, InterfaceMeta, InterfaceUpsert};
pub use listing::{NodeListing, StaticNodeList, NODE_SCAN_MAX, NODE_SEARCH_MAX};
pub use migrate::embedded_migrations;
#[allow(unused_imports)]
pub use nodes::TopologyRow;
pub use profiles::ProfileSummary;

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

/// The nodes/profiles metadata store.
pub struct NodeRepo {
    pool: PgPool,
}

/// What a notification says about a node beyond its id (ADR-039).
///
/// An `Alert` carries ids only, which is correct for the engine and useless in an email — the
/// subject line read `node 6f1c9d2a-… is critical` until this existed. `group` and `profile` are
/// optional because a node need have neither.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeFacts {
    /// Display name.
    pub name: String,
    /// Monitored address, without any netmask.
    pub address: String,
    /// Inventory folder name.
    pub group: Option<String>,
    /// Monitoring profile name.
    pub profile: Option<String>,
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
}
