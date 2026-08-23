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

/// Build the config from its stored columns, degrading rather than failing.
///
/// Split out of [`DnsCheckRepo::get`] so the degradation rules are reachable without a database:
/// every one of them decides what a *live monitor* does when a column is outside what this build
/// understands, and "silently poll with a different record type" is not something to leave untested.
/// A row is written only through [`DnsCheckRepo::upsert`], so out-of-range values mean an older or
/// newer binary wrote them — the reader's job is to keep monitoring, not to refuse.
fn config_from_columns(
    name: String,
    record_type: &str,
    resolver_text: Option<&str>,
    resolver_port: i32,
    max_depth: i16,
    timeout_ms: i32,
) -> DnsCheckConfig {
    DnsCheckConfig {
        name,
        record_type: DnsRecordType::from_token(record_type).unwrap_or_default(),
        // An unparseable address reads as "no resolver configured", i.e. the poller's own —
        // which is the same thing a NULL column means.
        resolver: resolver_text.and_then(|s| s.parse::<IpAddr>().ok()),
        resolver_port: u16::try_from(resolver_port).unwrap_or(53),
        max_depth: u8::try_from(max_depth).unwrap_or(8),
        timeout_ms: u32::try_from(timeout_ms).unwrap_or(3000),
    }
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
        Ok(Some(config_from_columns(
            row.try_get("name")?,
            &row.try_get::<String, _>("record_type")?,
            row.try_get::<Option<String>, _>("resolver_ip")?.as_deref(),
            row.try_get("resolver_port")?,
            row.try_get("max_depth")?,
            row.try_get("timeout_ms")?,
        )))
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

    /// Of the given node ids, which are DNS monitors — the page-scoped variant of
    /// [`Self::node_ids`], for the inventory list's kind badge.
    ///
    /// Bounded by the page size rather than by how many monitors exist: `/nodes` keyset-pages a
    /// 50k fleet and calls this once per page. Empty input short-circuits so we never send
    /// `= ANY('{}')`. Mirrors `MerakiDeviceRepo::filter_meraki`.
    pub async fn filter_dns(&self, node_ids: &[Uuid]) -> anyhow::Result<HashSet<Uuid>> {
        if node_ids.is_empty() {
            return Ok(HashSet::new());
        }
        let rows = sqlx::query("SELECT node_id FROM dns_checks WHERE node_id = ANY($1)")
            .bind(node_ids)
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

#[cfg(test)]
mod tests {
    use super::*;

    /// This module's own source, for the SQL-shape assertions below. The append-on-change rule and
    /// the keyset cursor live entirely inside SQL strings, so nothing else can catch a rewrite that
    /// changes their meaning — the peer stores (`events.rs`, `logstore.rs`, `flowstore.rs`,
    /// `reports.rs`) all pin their statements the same way.
    const SRC: &str = include_str!("dns_check.rs");

    /// This module's code, comments stripped — see
    /// [`crate::module_source::code_no_comments`] for why both.
    fn production_source() -> String {
        crate::module_source::code_no_comments("src", "dns_check")
    }

    #[test]
    fn an_unknown_record_type_reads_as_the_default_rather_than_failing() {
        // Written by a newer binary, read by this one: keep monitoring with the default type.
        let c = config_from_columns("a.test".into(), "SRV", None, 53, 8, 3000);
        assert_eq!(c.record_type, DnsRecordType::default());
        // A known token is honoured, so the fallback is not swallowing everything.
        let c = config_from_columns("a.test".into(), "AAAA", None, 53, 8, 3000);
        assert_eq!(c.record_type, DnsRecordType::Aaaa);
    }

    #[test]
    fn an_unusable_resolver_column_means_the_pollers_own_resolver() {
        // NULL and garbage must land on the same behaviour — "resolve however the poller does" —
        // because the alternative is a monitor that silently queries nothing.
        assert_eq!(
            config_from_columns("a.test".into(), "A", None, 53, 8, 3000).resolver,
            None
        );
        assert_eq!(
            config_from_columns("a.test".into(), "A", Some("not-an-ip"), 53, 8, 3000).resolver,
            None
        );
        let v6 = config_from_columns("a.test".into(), "A", Some("2001:db8::1"), 53, 8, 3000);
        assert_eq!(v6.resolver, Some("2001:db8::1".parse::<IpAddr>().unwrap()));
    }

    #[test]
    fn out_of_range_numeric_columns_fall_back_to_the_documented_defaults() {
        // Each `try_from` guards a column whose SQL type is wider than the config's. A negative
        // port is not "port 0", it is a row this build cannot honour — so use the default.
        let c = config_from_columns("a.test".into(), "A", None, -1, -1, -1);
        assert_eq!((c.resolver_port, c.max_depth, c.timeout_ms), (53, 8, 3000));
        let c = config_from_columns("a.test".into(), "A", None, 70_000, 300, 9_000);
        assert_eq!((c.resolver_port, c.max_depth, c.timeout_ms), (53, 8, 9_000));
    }

    #[test]
    fn in_range_values_survive_unchanged() {
        let c = config_from_columns("a.test".into(), "CNAME", Some("9.9.9.9"), 5353, 4, 1500);
        assert_eq!(c.name, "a.test");
        assert_eq!(c.record_type, DnsRecordType::Cname);
        assert_eq!(c.resolver_port, 5353);
        assert_eq!(c.max_depth, 4);
        assert_eq!(c.timeout_ms, 1500);
    }

    #[test]
    fn history_is_appended_only_when_the_chain_key_actually_moved() {
        // The whole point of the CTE: an unchanged poll writes no history row. Losing this guard
        // turns a once-per-change table into one row per poll per node — which at fleet scale is
        // the difference between a readable timeline and an unusable one.
        assert!(SRC.contains("WHERE up.prev_chain_key IS DISTINCT FROM up.chain_key"));
        // And the append reads the keys the upsert just returned, not the caller's guess.
        assert!(SRC.contains("RETURNING prev_chain_key, chain_key"));
    }

    #[test]
    fn first_seen_survives_an_unchanged_observation() {
        // "How long has this chain held" is the column's only purpose; resetting it on every poll
        // would make every chain look brand new. Matched on the distinctive fragments rather than
        // the whole clause, so re-wrapping the string literal does not fail the test.
        assert!(SRC.contains("first_seen = CASE WHEN dns_chains.chain_key = EXCLUDED.chain_key"));
        assert!(SRC.contains("THEN dns_chains.first_seen ELSE now() END"));
    }

    #[test]
    fn change_paging_is_keyset_and_never_offset() {
        // ADR-019. The tuple comparison is what makes the cursor stable across inserts.
        assert!(SRC.contains("WHERE node_id = $1 AND (at, id) < ($2, $3)"));
        assert!(SRC.contains("ORDER BY at DESC, id DESC LIMIT"));
        assert!(
            !production_source().contains("OFFSET"),
            "OFFSET paging reintroduced — rows shift under the reader as history is appended"
        );
    }

    #[test]
    fn the_page_scoped_filter_is_a_set_query_not_a_per_row_lookup() {
        // `/nodes` keyset-pages a 50k fleet and asks this once per page to badge the rows. A
        // per-node `WHERE node_id = $1` would be a round trip per row; the unscoped
        // `SELECT node_id FROM dns_checks` returns rows the page cannot use. The empty guard stops
        // `= ANY('{}')` being sent on every page of a fleet with no DNS monitors.
        let src = production_source();
        assert!(src.contains("WHERE node_id = ANY($1)"));
        assert!(src.contains("if node_ids.is_empty()"));
    }

    #[test]
    fn every_statement_binds_its_values_instead_of_interpolating_them() {
        // The cursor and the retention window are caller-supplied and the node id is a path
        // parameter, so none of them may ever be concatenated into a statement.
        let src = production_source();
        for builder in ["format!(", "push_str("] {
            assert!(
                !src.contains(builder),
                "SQL may be being built by string concatenation ({builder}); bind the value instead"
            );
        }
    }
}
