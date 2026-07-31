// SPDX-License-Identifier: AGPL-3.0-only
//! DNS-check persistence: the per-node monitor configuration, the current resolution chain, and
//! the append-on-change history of that chain (ADR-033, migration 0049).
//!
//! Metadata and structured observations, so it all lives in PostgreSQL (store separation). A DNS
//! chain cannot go to the TSDB: `SeriesKey` is fixed at `{node, ifindex, metric}` (ADR-011) and a
//! CNAME target is unbounded free text. This is the I/O adapter; the config and chain types, their
//! canonicalization and the change key live in `yagra-common` (and are tested there). Runtime
//! `sqlx::query` (not the compile-time macro) so the build needs no live database.

use chrono::{DateTime, Utc};
use sqlx::types::Json;
use sqlx::{PgPool, Row};
use std::collections::HashSet;
use std::net::IpAddr;
use uuid::Uuid;
use yagra_common::{DnsChain, DnsCheckConfig, DnsFailure, DnsFailureKind, DnsRecordType};

/// The current resolution chain for a node, plus how long it has held.
#[derive(Debug, Clone)]
pub struct CurrentChain {
    /// The chain exactly as observed.
    pub chain: DnsChain,
    /// Whether that observation reached a terminal record set.
    pub resolved: bool,
    /// Stable failure token when it did not (`nx_domain`, `timeout`, …).
    pub failure_kind: Option<DnsFailureKind>,
    /// When this exact chain was first observed.
    pub first_seen: DateTime<Utc>,
    /// When it was last confirmed still current.
    pub last_seen: DateTime<Utc>,
}

/// One append-on-change history row.
#[derive(Debug, Clone)]
pub struct ChainChange {
    /// Monotonic id — the keyset cursor tiebreaker.
    pub id: i64,
    /// When the change was recorded.
    pub at: DateTime<Utc>,
    /// The chain as of this change.
    pub chain: DnsChain,
    /// Whether it resolved.
    pub resolved: bool,
    /// Stable failure token when it did not.
    pub failure_kind: Option<DnsFailureKind>,
    /// The key this replaced; `None` marks the first-ever observation for the node.
    pub prev_chain_key: Option<String>,
}

/// PostgreSQL-backed store for DNS monitors: config, current chain, and change history.
pub struct DnsCheckRepo {
    pool: PgPool,
}

impl DnsCheckRepo {
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    // ── Configuration (1:1 with the node) ────────────────────────────────────────────

    /// The DNS check for a node, if it has one (None ⇒ not a DNS monitor).
    pub async fn get(&self, node_id: Uuid) -> anyhow::Result<Option<DnsCheckConfig>> {
        // `host()` renders INET as plain text, matching how `NodeRepo` reads `nodes.address` —
        // it keeps the `ipnetwork` sqlx feature out of the build and strips any /32 suffix.
        let row = sqlx::query(
            "SELECT name, record_type, host(resolver_ip) AS resolver_ip, resolver_port, \
                    max_depth, timeout_ms \
             FROM dns_checks WHERE node_id = $1",
        )
        .bind(node_id)
        .fetch_optional(&self.pool)
        .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        let record_type: String = row.try_get("record_type")?;
        let resolver_text: Option<String> = row.try_get("resolver_ip")?;
        let resolver: Option<IpAddr> = resolver_text.and_then(|s| s.parse().ok());
        let resolver_port: i32 = row.try_get("resolver_port")?;
        let max_depth: i16 = row.try_get("max_depth")?;
        let timeout_ms: i32 = row.try_get("timeout_ms")?;
        Ok(Some(DnsCheckConfig {
            name: row.try_get("name")?,
            record_type: DnsRecordType::from_token(&record_type).unwrap_or_default(),
            resolver,
            resolver_port: u16::try_from(resolver_port).unwrap_or(53),
            max_depth: u8::try_from(max_depth).unwrap_or(8),
            timeout_ms: u32::try_from(timeout_ms).unwrap_or(3000),
        }))
    }

    /// Create or replace a node's DNS check (idempotent upsert).
    pub async fn upsert(&self, node_id: Uuid, cfg: &DnsCheckConfig) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO dns_checks \
                (node_id, name, record_type, resolver_ip, resolver_port, max_depth, timeout_ms) \
             VALUES ($1, $2, $3, $4::inet, $5, $6, $7) \
             ON CONFLICT (node_id) DO UPDATE SET \
                name = EXCLUDED.name, record_type = EXCLUDED.record_type, \
                resolver_ip = EXCLUDED.resolver_ip, resolver_port = EXCLUDED.resolver_port, \
                max_depth = EXCLUDED.max_depth, timeout_ms = EXCLUDED.timeout_ms, \
                updated_at = now()",
        )
        .bind(node_id)
        .bind(&cfg.name)
        .bind(cfg.record_type.as_str())
        .bind(cfg.resolver.map(|ip| ip.to_string()))
        .bind(i32::from(cfg.resolver_port))
        .bind(i16::from(cfg.max_depth))
        .bind(i32::try_from(cfg.timeout_ms).unwrap_or(3000))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Delete a node's DNS check. Returns whether a row was removed.
    ///
    /// The observation tables are left alone deliberately: they cascade with the *node*, so
    /// reconfiguring a monitor keeps its history instead of silently discarding it.
    pub async fn delete(&self, node_id: Uuid) -> anyhow::Result<bool> {
        let res = sqlx::query("DELETE FROM dns_checks WHERE node_id = $1")
            .bind(node_id)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() > 0)
    }

    /// Every node that has a DNS check.
    ///
    /// Preloaded once per scheduler sweep so the sweep never adds a per-node round trip at fleet
    /// scale — the same trick `MerakiDeviceRepo::node_ids` exists for. DNS monitors number in the
    /// tens, so this set is tiny.
    pub async fn node_ids(&self) -> anyhow::Result<HashSet<Uuid>> {
        let rows = sqlx::query("SELECT node_id FROM dns_checks")
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter()
            .map(|r| r.try_get::<Uuid, _>("node_id").map_err(Into::into))
            .collect()
    }

    // ── Observations ─────────────────────────────────────────────────────────────────

    /// Record one observed chain: upsert the current state and append a history row **iff** the
    /// content key moved.
    ///
    /// Both happen in a single statement. The upsert's `RETURNING` carries the pre-update key, and
    /// the append is guarded on `prev IS DISTINCT FROM new` — so an unchanged poll writes no
    /// history at all, and PostgreSQL's row lock on `ON CONFLICT DO UPDATE` serializes concurrent
    /// cores, meaning a transition can be neither double-appended nor lost. That is also why this
    /// is not leader-gated.
    ///
    /// The caller must pass a canonicalized chain (the transport canonicalizes before publish);
    /// otherwise TTL countdown and round-robin reordering would each register as a change.
    pub async fn record_observation(&self, node_id: Uuid, chain: &DnsChain) -> anyhow::Result<()> {
        let chain_key = chain.content_key();
        let resolved = chain.resolved();
        let failure_kind = chain.failure.as_ref().map(DnsFailure::kind_token);
        sqlx::query(
            "WITH up AS ( \
                INSERT INTO dns_chains \
                    (node_id, chain_key, prev_chain_key, chain, resolved, failure_kind, resolver, \
                     resolve_ms, first_seen, last_seen) \
                VALUES ($1, $2, NULL, $3, $4, $5, $6, $7, now(), now()) \
                ON CONFLICT (node_id) DO UPDATE SET \
                    prev_chain_key = dns_chains.chain_key, \
                    chain_key = EXCLUDED.chain_key, \
                    chain = EXCLUDED.chain, \
                    resolved = EXCLUDED.resolved, \
                    failure_kind = EXCLUDED.failure_kind, \
                    resolver = EXCLUDED.resolver, \
                    resolve_ms = EXCLUDED.resolve_ms, \
                    first_seen = CASE WHEN dns_chains.chain_key = EXCLUDED.chain_key \
                                      THEN dns_chains.first_seen ELSE now() END, \
                    last_seen = now() \
                RETURNING prev_chain_key, chain_key \
             ) \
             INSERT INTO dns_chain_changes \
                (node_id, at, chain_key, prev_chain_key, chain, resolved, failure_kind, resolver) \
             SELECT $1, now(), up.chain_key, up.prev_chain_key, $3, $4, $5, $6 \
             FROM up WHERE up.prev_chain_key IS DISTINCT FROM up.chain_key",
        )
        .bind(node_id)
        .bind(&chain_key)
        .bind(Json(chain))
        .bind(resolved)
        .bind(failure_kind)
        .bind(&chain.resolver)
        .bind(chain.resolve_ms)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// The node's current chain, if it has ever been observed.
    pub async fn current_chain(&self, node_id: Uuid) -> anyhow::Result<Option<CurrentChain>> {
        let row = sqlx::query(
            "SELECT chain, resolved, failure_kind, first_seen, last_seen \
             FROM dns_chains WHERE node_id = $1",
        )
        .bind(node_id)
        .fetch_optional(&self.pool)
        .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        let chain: Json<DnsChain> = row.try_get("chain")?;
        Ok(Some(CurrentChain {
            chain: chain.0,
            resolved: row.try_get("resolved")?,
            failure_kind: row
                .try_get::<Option<String>, _>("failure_kind")?
                .as_deref()
                .and_then(DnsFailureKind::from_token),
            first_seen: row.try_get("first_seen")?,
            last_seen: row.try_get("last_seen")?,
        }))
    }

    /// A keyset page of change rows, newest first (ADR-019 — never OFFSET).
    ///
    /// `before` is the `(at, id)` of the last row of the previous page.
    pub async fn list_changes(
        &self,
        node_id: Uuid,
        before: Option<(DateTime<Utc>, i64)>,
        limit: i64,
    ) -> anyhow::Result<Vec<ChainChange>> {
        // Two prepared shapes rather than string-built SQL: the cursor is typed and bound, never
        // interpolated.
        let rows = match before {
            Some((at, id)) => {
                sqlx::query(
                    "SELECT id, at, chain, resolved, failure_kind, prev_chain_key \
                     FROM dns_chain_changes \
                     WHERE node_id = $1 AND (at, id) < ($2, $3) \
                     ORDER BY at DESC, id DESC LIMIT $4",
                )
                .bind(node_id)
                .bind(at)
                .bind(id)
                .bind(limit)
                .fetch_all(&self.pool)
                .await?
            }
            None => {
                sqlx::query(
                    "SELECT id, at, chain, resolved, failure_kind, prev_chain_key \
                     FROM dns_chain_changes \
                     WHERE node_id = $1 \
                     ORDER BY at DESC, id DESC LIMIT $2",
                )
                .bind(node_id)
                .bind(limit)
                .fetch_all(&self.pool)
                .await?
            }
        };

        rows.into_iter()
            .map(|row| {
                let chain: Json<DnsChain> = row.try_get("chain")?;
                Ok(ChainChange {
                    id: row.try_get("id")?,
                    at: row.try_get("at")?,
                    chain: chain.0,
                    resolved: row.try_get("resolved")?,
                    failure_kind: row
                        .try_get::<Option<String>, _>("failure_kind")?
                        .as_deref()
                        .and_then(DnsFailureKind::from_token),
                    prev_chain_key: row.try_get("prev_chain_key")?,
                })
            })
            .collect()
    }

    /// Drop history rows older than `retention_secs`. Returns how many were removed.
    pub async fn prune_chain_changes(&self, retention_secs: i64) -> anyhow::Result<u64> {
        let res = sqlx::query(
            "DELETE FROM dns_chain_changes WHERE at < now() - make_interval(secs => $1)",
        )
        .bind(retention_secs as f64)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected())
    }
}
