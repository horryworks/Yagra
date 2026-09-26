// SPDX-License-Identifier: AGPL-3.0-only
//! **PostgreSQL** for the passive-event pipeline (ADR-095): event rows, event sources, event rules.
//!
//! Every statement lives here or in [`super::sql`], and `super::guards` makes that a build
//! failure rather than a convention — [`super::engine`] and [`super::ingest`] are hot paths, and
//! a synchronous query added to either is the kind of thing that reads fine and is found in
//! production. The tables this file may name are declared there, in both directions against the
//! directory.

use sqlx::{PgPool, Row};
use uuid::Uuid;
use yagra_common::trap_oid_name;

// The vocabulary lives in the parent, which a child can see without any widening — see
// `super`'s doc for why that is what decides where a thing goes here.
use super::sql::{
    agg_auth_sources_sql, agg_counts_by_bucket_sql, agg_severity_counts_sql, bind_event_filter,
    list_events_sql, stats_grouped_sql, stats_series_sql,
};
use super::*;

/// PostgreSQL persistence for event sources, rules, and the event log.
pub struct EventRepo {
    pool: PgPool,
}

/// References a queued event named that were deleted before it was written (ADR-141). Each is
/// written as NULL on the one retry; empty on the first attempt.
#[derive(Default)]
struct GoneReferences {
    nodes: std::collections::HashSet<Uuid>,
    rules: std::collections::HashSet<Uuid>,
    sources: std::collections::HashSet<Uuid>,
}

impl GoneReferences {
    /// `id`, unless it names something in `gone`.
    fn kept(gone: &std::collections::HashSet<Uuid>, id: Option<Uuid>) -> Option<Uuid> {
        id.filter(|id| !gone.contains(id))
    }
}

impl EventRepo {
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    // ── Sources ──

    pub async fn list_sources(&self) -> anyhow::Result<Vec<EventSourceView>> {
        let rows = sqlx::query(
            "SELECT id, name, kind, enabled, node_id, created_at \
             FROM event_sources ORDER BY name",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| {
                Ok(EventSourceView {
                    id: row.try_get("id")?,
                    name: row.try_get("name")?,
                    kind: event_kind_from_stored(row.try_get("kind")?),
                    enabled: row.try_get("enabled")?,
                    node_id: row.try_get("node_id")?,
                    created_at: row.try_get("created_at")?,
                })
            })
            .collect()
    }

    /// Create a webhook source; returns its id and the **plaintext token (shown once)**.
    pub async fn create_source(
        &self,
        name: &str,
        node_id: Option<Uuid>,
    ) -> anyhow::Result<(Uuid, String)> {
        let id = Uuid::new_v4();
        let token = generate_token();
        sqlx::query(
            "INSERT INTO event_sources (id, name, kind, enabled, node_id, token_hash) \
             VALUES ($1, $2, 'webhook', true, $3, $4)",
        )
        .bind(id)
        .bind(name)
        .bind(node_id)
        .bind(hash_token(&token))
        .execute(&self.pool)
        .await?;
        Ok((id, token))
    }

    /// Full-replace update of a source's editable fields (token untouched).
    pub async fn update_source(
        &self,
        id: Uuid,
        name: &str,
        enabled: bool,
        node_id: Option<Uuid>,
    ) -> anyhow::Result<bool> {
        let res = sqlx::query(
            "UPDATE event_sources SET name = $2, enabled = $3, node_id = $4, updated_at = now() \
             WHERE id = $1",
        )
        .bind(id)
        .bind(name)
        .bind(enabled)
        .bind(node_id)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected() > 0)
    }

    /// Replace the source's token; returns the new **plaintext token (shown once)**,
    /// or `None` if the source doesn't exist.
    pub async fn rotate_token(&self, id: Uuid) -> anyhow::Result<Option<String>> {
        let token = generate_token();
        let res = sqlx::query(
            "UPDATE event_sources SET token_hash = $2, updated_at = now() WHERE id = $1",
        )
        .bind(id)
        .bind(hash_token(&token))
        .execute(&self.pool)
        .await?;
        Ok((res.rows_affected() > 0).then_some(token))
    }

    pub async fn delete_source(&self, id: Uuid) -> anyhow::Result<bool> {
        let res = sqlx::query("DELETE FROM event_sources WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() > 0)
    }

    /// Verify a webhook source's bearer token (constant-time hash compare).
    pub async fn verify_token(&self, id: Uuid, token: &str) -> anyhow::Result<TokenVerify> {
        let row = sqlx::query(
            "SELECT token_hash, enabled, node_id FROM event_sources \
             WHERE id = $1 AND kind = 'webhook'",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        let Some(row) = row else {
            return Ok(TokenVerify::UnknownOrDisabled);
        };
        if !row.try_get::<bool, _>("enabled")? {
            return Ok(TokenVerify::UnknownOrDisabled);
        }
        let stored: Option<String> = row.try_get("token_hash")?;
        let Some(stored) = stored else {
            return Ok(TokenVerify::UnknownOrDisabled);
        };
        if constant_time_eq(stored.as_bytes(), hash_token(token).as_bytes()) {
            Ok(TokenVerify::Ok {
                node_id: row.try_get("node_id")?,
            })
        } else {
            Ok(TokenVerify::BadToken)
        }
    }

    // ── Rules ──

    pub async fn list_rules(&self) -> anyhow::Result<Vec<StoredEventRule>> {
        let rows = sqlx::query(
            "SELECT id, name, enabled, source_kind, source_id, node_id, match_kind, pattern, \
                    clear_pattern, severity, ttl_secs, min_count, window_secs, created_at \
             FROM event_rules ORDER BY name",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| {
                Ok(StoredEventRule {
                    id: row.try_get("id")?,
                    name: row.try_get("name")?,
                    enabled: row.try_get("enabled")?,
                    source_kind: row.try_get("source_kind")?,
                    source_id: row.try_get("source_id")?,
                    node_id: row.try_get("node_id")?,
                    match_kind: EventMatchKind::from_stored(row.try_get("match_kind")?),
                    pattern: row.try_get("pattern")?,
                    clear_pattern: row.try_get("clear_pattern")?,
                    severity: parse_severity(row.try_get("severity")?),
                    ttl_secs: row.try_get("ttl_secs")?,
                    min_count: row.try_get("min_count")?,
                    window_secs: row.try_get("window_secs")?,
                    created_at: row.try_get("created_at")?,
                })
            })
            .collect()
    }

    pub async fn create_rule(&self, p: &RuleParams<'_>) -> anyhow::Result<Uuid> {
        let id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO event_rules (id, name, enabled, source_kind, source_id, node_id, \
             match_kind, pattern, clear_pattern, severity, ttl_secs, min_count, window_secs) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)",
        )
        .bind(id)
        .bind(p.name)
        .bind(p.enabled)
        .bind(p.source_kind)
        .bind(p.source_id)
        .bind(p.node_id)
        .bind(p.match_kind)
        .bind(p.pattern)
        .bind(p.clear_pattern)
        .bind(p.severity)
        .bind(p.ttl_secs)
        .bind(p.min_count)
        .bind(p.window_secs)
        .execute(&self.pool)
        .await?;
        Ok(id)
    }

    pub async fn update_rule(&self, id: Uuid, p: &RuleParams<'_>) -> anyhow::Result<bool> {
        let res = sqlx::query(
            "UPDATE event_rules SET name = $2, enabled = $3, source_kind = $4, source_id = $5, \
             node_id = $6, match_kind = $7, pattern = $8, clear_pattern = $9, severity = $10, \
             ttl_secs = $11, min_count = $12, window_secs = $13, updated_at = now() \
             WHERE id = $1",
        )
        .bind(id)
        .bind(p.name)
        .bind(p.enabled)
        .bind(p.source_kind)
        .bind(p.source_id)
        .bind(p.node_id)
        .bind(p.match_kind)
        .bind(p.pattern)
        .bind(p.clear_pattern)
        .bind(p.severity)
        .bind(p.ttl_secs)
        .bind(p.min_count)
        .bind(p.window_secs)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected() > 0)
    }

    pub async fn delete_rule(&self, id: Uuid) -> anyhow::Result<bool> {
        let res = sqlx::query("DELETE FROM event_rules WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() > 0)
    }

    // ── Event log ──

    /// Persist a batch of received events in one multi-row INSERT (best-effort — the caller runs
    /// off the matcher's hot path, and a DB hiccup must not stop alerting). Returns rows inserted.
    ///
    /// 🚨 **A record queued before the node, rule or source it names was deleted still carries the
    /// old id** (ADR-141). `ON DELETE SET NULL` reaches only rows that already exist, so that one
    /// record used to fail the whole statement and every other event in the flush with it. On exactly
    /// that error the ids that are gone are looked up and the batch is written **once** more with those
    /// references cleared. The event is kept: it is still a fact that arrived, and a cleared reference
    /// is what the delete would have left on a row stored a moment earlier.
    pub async fn insert_events_batch(&self, records: &[&PersistRecord]) -> anyhow::Result<u64> {
        if records.is_empty() {
            return Ok(0);
        }
        match self
            .insert_event_rows(records, &GoneReferences::default())
            .await
        {
            Ok(inserted) => Ok(inserted),
            Err(sqlx::Error::Database(db)) if db.is_foreign_key_violation() => {
                let gone = self.gone_references(records).await?;
                tracing::debug!(
                    nodes = gone.nodes.len(),
                    rules = gone.rules.len(),
                    sources = gone.sources.len(),
                    "events named references deleted since they were queued; stored without them"
                );
                // A reference deleted in the window of this retry fails it too, and is reported
                // like any other error rather than retried again.
                Ok(self.insert_event_rows(records, &gone).await?)
            }
            Err(e) => Err(e.into()),
        }
    }

    /// Which of the ids these records name no longer exist. Asked only after a foreign-key
    /// violation, never on the normal path.
    async fn gone_references(&self, records: &[&PersistRecord]) -> anyhow::Result<GoneReferences> {
        let named = |pick: fn(&PersistRecord) -> Option<Uuid>| -> Vec<Uuid> {
            let mut ids: Vec<Uuid> = records.iter().filter_map(|r| pick(r)).collect();
            ids.sort_unstable();
            ids.dedup();
            ids
        };
        let (nodes, rules, sources) = (
            named(|r| r.node_id),
            named(|r| r.matched_rule_id),
            named(|r| r.source_id),
        );
        let present_nodes: Vec<Uuid> =
            sqlx::query_scalar("SELECT id FROM nodes WHERE id = ANY($1)")
                .bind(&nodes)
                .fetch_all(&self.pool)
                .await?;
        let present_rules: Vec<Uuid> =
            sqlx::query_scalar("SELECT id FROM event_rules WHERE id = ANY($1)")
                .bind(&rules)
                .fetch_all(&self.pool)
                .await?;
        let present_sources: Vec<Uuid> =
            sqlx::query_scalar("SELECT id FROM event_sources WHERE id = ANY($1)")
                .bind(&sources)
                .fetch_all(&self.pool)
                .await?;
        let missing = |named: Vec<Uuid>, present: Vec<Uuid>| {
            let present: std::collections::HashSet<Uuid> = present.into_iter().collect();
            named
                .into_iter()
                .filter(|id| !present.contains(id))
                .collect()
        };
        Ok(GoneReferences {
            nodes: missing(nodes, present_nodes),
            rules: missing(rules, present_rules),
            sources: missing(sources, present_sources),
        })
    }

    /// The multi-row INSERT itself, with every reference in `gone` written as NULL.
    ///
    /// Returns the `sqlx` error unconverted, because the caller's retry turns on whether it is a
    /// foreign-key violation.
    async fn insert_event_rows(
        &self,
        records: &[&PersistRecord],
        gone: &GoneReferences,
    ) -> Result<u64, sqlx::Error> {
        let mut qb = sqlx::QueryBuilder::new(
            "INSERT INTO events (id, kind, at_unix_ms, source_ip, node_id, source_id, pool, \
             facility, syslog_severity, hostname, app_name, trap_oid, signature, varbinds, \
             message, matched_rule_id, action) ",
        );
        qb.push_values(records.iter(), |mut b, r| {
            let m = &r.msg;
            let varbinds = (!m.varbinds.is_empty())
                .then(|| serde_json::to_value(&m.varbinds).unwrap_or(serde_json::Value::Null));
            b.push_bind(m.event_id)
                .push_bind(m.kind.as_str())
                .push_bind(m.at_unix_ms);
            // `events.source_ip` is INET; bind the text form and cast (mirrors the `$n::inet` in
            // the old single-row insert).
            b.push_bind(m.source_ip.map(|ip| ip.to_string()))
                .push_unseparated("::inet");
            b.push_bind(GoneReferences::kept(&gone.nodes, r.node_id))
                .push_bind(GoneReferences::kept(&gone.sources, r.source_id))
                .push_bind(m.pool.clone())
                .push_bind(m.facility.map(i16::from))
                .push_bind(m.syslog_severity.map(i16::from))
                .push_bind(m.hostname.clone())
                .push_bind(m.app_name.clone())
                .push_bind(m.trap_oid.clone())
                .push_bind(r.signature.clone())
                .push_bind(varbinds)
                .push_bind(m.message.clone())
                .push_bind(GoneReferences::kept(&gone.rules, r.matched_rule_id))
                .push_bind(r.action.as_str());
        });
        Ok(qb.build().execute(&self.pool).await?.rows_affected())
    }

    /// Keyset-paged event list, newest first by **event time** — the same ordering and the same
    /// cursor column the VictoriaLogs path uses (`| sort by (_time) desc`), so a deployment with
    /// the log store enabled and one without return the same page for the same request.
    pub async fn list_events(
        &self,
        filter: &EventFilter,
        limit: i64,
    ) -> anyhow::Result<Vec<EventRow>> {
        let rows = bind_event_filter(sqlx::query(&list_events_sql()), filter)
            .bind(limit.clamp(1, 500))
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter()
            .map(|row| {
                let trap_oid: Option<String> = row.try_get("trap_oid")?;
                let trap_name = trap_oid
                    .as_deref()
                    .and_then(trap_oid_name)
                    .map(str::to_owned);
                Ok(EventRow {
                    id: row.try_get("id")?,
                    kind: event_kind_from_stored(row.try_get("kind")?),
                    at_unix_ms: row.try_get("at_unix_ms")?,
                    recorded_at: row.try_get("recorded_at")?,
                    source_ip: row.try_get("source_ip")?,
                    node_id: row.try_get("node_id")?,
                    source_id: row.try_get("source_id")?,
                    pool: row.try_get("pool")?,
                    facility: row.try_get("facility")?,
                    syslog_severity: row.try_get("syslog_severity")?,
                    hostname: row.try_get("hostname")?,
                    app_name: row.try_get("app_name")?,
                    trap_oid,
                    trap_name,
                    varbinds: row.try_get("varbinds")?,
                    message: row.try_get("message")?,
                    matched_rule_id: row.try_get("matched_rule_id")?,
                    action: EventAction::from_stored(row.try_get("action")?),
                })
            })
            .collect()
    }

    // ── Troubleshoot analytics (ADR-022 event/flow increment) ──
    // Read-only aggregates over `events` for the passive-monitoring analyses. All are parameterized
    // (never string-interpolated) and take the shared [`EventFilter`], so the window, the group
    // scope (ADR-014) and the time basis are the same ones `/events` and `/events/stats` use.
    //
    // ⚠️ These answer about **PostgreSQL**, which holds only alert-linked rows once a log store is
    // configured (ADR-024). That is why each has a `LogStore` twin of the same name and why the
    // analyses reach both through a router rather than calling either directly — see
    // `analysis/mod.rs`'s `agg_*` methods. `event_flap_stats` is the exception, and deliberately: every
    // action it counts is alert-linked, so PostgreSQL is complete for it either way.

    /// Per-(node, time-bucket) event counts. Uncorrelated events (no node) are excluded — an event
    /// storm is attributed to a device.
    pub async fn event_counts_by_bucket(
        &self,
        filter: &EventFilter,
        bucket_secs: i64,
    ) -> anyhow::Result<Vec<EventBucketCount>> {
        let b = bucket_secs.max(1);
        let rows = bind_event_filter(sqlx::query(&agg_counts_by_bucket_sql()), filter)
            .bind(b)
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter()
            .map(|row| {
                Ok(EventBucketCount {
                    node_id: row.try_get("node_id")?,
                    bucket_start_s: row.try_get::<i64, _>("bucket")?,
                    count: row.try_get::<i64, _>("n")?,
                })
            })
            .collect()
    }

    /// Fire/clear churn per (node, rule) across `[from_ms, to_ms]` — the raw material for
    /// `event_flap` (repeated linkDown/linkUp, BGP session churn). Only alert-linked rows.
    ///
    /// **The one analytics aggregate with no `LogStore` twin, and that is correct rather than an
    /// omission**: it requires `matched_rule_id IS NOT NULL` and counts only `fired`/`refreshed`/
    /// `cleared`, every one of which satisfies [`EventAction::is_alert_linked`] — the same
    /// predicate `flush_persist` keeps. PostgreSQL is therefore complete for this question whether
    /// or not a log store is configured. Pinned by `event_flap_only_counts_rows_postgresql_keeps`,
    /// so nobody "completes the set" by giving it a twin it does not need. It also keeps the
    /// `from_ms`/`to_ms` signature for the same reason: no scope push-down is needed where the
    /// caller already restricts by node.
    pub async fn event_flap_stats(
        &self,
        from_ms: i64,
        to_ms: i64,
    ) -> anyhow::Result<Vec<EventFlapStat>> {
        let rows = sqlx::query(
            "SELECT e.node_id, e.matched_rule_id, COALESCE(r.name, 'rule') AS rule_name, \
                    count(*) FILTER (WHERE e.action IN ('fired','refreshed')) AS fires, \
                    count(*) FILTER (WHERE e.action = 'cleared') AS clears \
             FROM events e LEFT JOIN event_rules r ON r.id = e.matched_rule_id \
             WHERE e.node_id IS NOT NULL AND e.matched_rule_id IS NOT NULL \
               AND e.at_unix_ms >= $1 AND e.at_unix_ms <= $2 \
             GROUP BY e.node_id, e.matched_rule_id, r.name",
        )
        .bind(from_ms)
        .bind(to_ms)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| {
                Ok(EventFlapStat {
                    node_id: row.try_get("node_id")?,
                    rule_id: row.try_get("matched_rule_id")?,
                    rule_name: row.try_get("rule_name")?,
                    fires: row.try_get::<i64, _>("fires")?,
                    clears: row.try_get::<i64, _>("clears")?,
                })
            })
            .collect()
    }

    /// Per-(node, syslog-severity) counts — the input to `severity_shift`.
    pub async fn event_severity_counts(
        &self,
        filter: &EventFilter,
    ) -> anyhow::Result<Vec<EventSeverityCount>> {
        let rows = bind_event_filter(sqlx::query(&agg_severity_counts_sql()), filter)
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter()
            .map(|row| {
                Ok(EventSeverityCount {
                    node_id: row.try_get("node_id")?,
                    severity: row.try_get::<i16, _>("syslog_severity")?,
                    count: row.try_get::<i64, _>("n")?,
                })
            })
            .collect()
    }

    /// Top unmatched-event signatures (trap OID or syslog app-name) — the coverage gaps `rule_gap`
    /// surfaces. Unmatched rows only.
    pub async fn event_unmatched_signatures(
        &self,
        filter: &EventFilter,
        limit: i64,
    ) -> anyhow::Result<Vec<EventSignatureCount>> {
        let rows = bind_event_filter(sqlx::query(&agg_unmatched_signatures_sql()), filter)
            .bind(limit.clamp(1, 500))
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter()
            .map(|row| {
                Ok(EventSignatureCount {
                    kind: row.try_get("kind")?,
                    signature: row.try_get("sig")?,
                    count: row.try_get::<i64, _>("n")?,
                    sample_node: row.try_get("sample_node")?,
                })
            })
            .collect()
    }

    /// Authentication-signal volume grouped by source — the input to `auth_probe`
    /// (authenticationFailure traps + auth-failure syslog).
    pub async fn event_auth_sources(
        &self,
        filter: &EventFilter,
        limit: i64,
    ) -> anyhow::Result<Vec<EventAuthSource>> {
        let rows = bind_event_filter(sqlx::query(&agg_auth_sources_sql()), filter)
            .bind(limit.clamp(1, 500))
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter()
            .map(|row| {
                Ok(EventAuthSource {
                    source_ip: row.try_get("src")?,
                    node_id: row.try_get("node_id")?,
                    count: row.try_get::<i64, _>("n")?,
                })
            })
            .collect()
    }

    // ── Dashboard summary aggregates (`/events/stats`) ──
    // Fleet-wide counts honoring the full `EventFilter` (the same predicate as `list_events`), for
    // the dashboard passive-event widgets. When VictoriaLogs is enabled these run against the log
    // store instead (the API picks the path); the PG path is accurate within retention when it isn't.

    /// One categorical count aggregation over the events, honoring the full [`EventFilter`], ordered
    /// by count desc. `group` picks a fixed column (never interpolated raw). Binds mirror
    /// `list_events` ($1..=$8) plus the row cap ($9).
    pub async fn stats_grouped(
        &self,
        filter: &EventFilter,
        group: EventStatGroup,
        limit: i64,
    ) -> anyhow::Result<Vec<EventStatBucket>> {
        let sql = stats_grouped_sql(group);
        let rows = bind_event_filter(sqlx::query(&sql), filter)
            .bind(limit.clamp(1, 500))
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter()
            .map(|row| {
                let count = row.try_get::<i64, _>("n")?;
                Ok(match group {
                    EventStatGroup::Source => {
                        let node_id: Option<Uuid> = row.try_get("node_id")?;
                        let source_ip: Option<String> = row.try_get("source_ip")?;
                        // `key` is stable for the React key + the fallback display when the source
                        // maps to no node; the UI resolves `node_id` to a name when present.
                        let key = node_id
                            .map(|n| n.to_string())
                            .or_else(|| source_ip.clone())
                            .unwrap_or_default();
                        EventStatBucket {
                            key,
                            label: source_ip,
                            node_id,
                            count,
                        }
                    }
                    EventStatGroup::Trap => {
                        let key: String =
                            row.try_get::<Option<String>, _>("key")?.unwrap_or_default();
                        let label = trap_oid_name(&key).map(str::to_owned);
                        EventStatBucket {
                            key,
                            label,
                            node_id: None,
                            count,
                        }
                    }
                    // The two groupings whose SQL `key` is already the display value. Named rather
                    // than wildcarded: a fifth grouping needing a resolved `label` (the way `trap`
                    // needs its MIB name) or a `node_id` (the way `source` does) would land here
                    // and render as a bare key — a plausible-looking value, so nothing would look
                    // broken enough to investigate.
                    EventStatGroup::Kind | EventStatGroup::Action => EventStatBucket {
                        key: row.try_get("key")?,
                        label: None,
                        node_id: None,
                        count,
                    },
                })
            })
            .collect()
    }

    /// The event-volume time series: counts bucketed into `bucket_secs`-wide windows (on event time
    /// `at_unix_ms`), honoring the full [`EventFilter`]; `split_kind` adds the per-kind breakdown.
    pub async fn stats_series(
        &self,
        filter: &EventFilter,
        bucket_secs: i64,
        split_kind: bool,
    ) -> anyhow::Result<Vec<EventTimeBucket>> {
        let b = bucket_secs.clamp(1, 86_400);
        let sql = stats_series_sql(split_kind);
        let rows = bind_event_filter(sqlx::query(&sql), filter)
            .bind(b)
            .fetch_all(&self.pool)
            .await?;
        // Fold (bucket, kind) rows into one `EventTimeBucket` per bucket (BTreeMap keeps time order).
        let mut buckets: std::collections::BTreeMap<
            i64,
            (i64, std::collections::BTreeMap<String, i64>),
        > = std::collections::BTreeMap::new();
        for row in rows {
            let bucket_s: i64 = row.try_get("bucket")?;
            let n: i64 = row.try_get("n")?;
            let entry = buckets.entry(bucket_s).or_default();
            entry.0 += n;
            if split_kind {
                let kind: String = row.try_get("kind")?;
                *entry.1.entry(kind).or_default() += n;
            }
        }
        Ok(buckets
            .into_iter()
            .map(|(bucket_s, (count, by))| EventTimeBucket {
                ts_unix_ms: bucket_s.saturating_mul(1000),
                count,
                by_kind: split_kind.then_some(by),
            })
            .collect())
    }

    /// Asymmetric retention: matched events keep the alert-history window; unmatched rows are
    /// rule-authoring material only and get a shorter one. Both windows come from the caller
    /// (`crate::retention`, ADR-040) so the policy is declared in one place, not here.
    /// Returns (matched, unmatched) rows removed.
    pub async fn prune_old(
        &self,
        matched_secs: i64,
        unmatched_secs: i64,
    ) -> anyhow::Result<(u64, u64)> {
        let matched = sqlx::query(
            "DELETE FROM events WHERE matched_rule_id IS NOT NULL \
             AND recorded_at < now() - $1 * interval '1 second'",
        )
        .bind(matched_secs)
        .execute(&self.pool)
        .await?
        .rows_affected();
        let unmatched = sqlx::query(
            "DELETE FROM events WHERE matched_rule_id IS NULL \
             AND recorded_at < now() - $1 * interval '1 second'",
        )
        .bind(unmatched_secs)
        .execute(&self.pool)
        .await?
        .rows_affected();
        Ok((matched, unmatched))
    }

    /// Remember one persisted batch's senders that no node claimed (ADR-179 決定 3).
    ///
    /// `senders` must hold each `(address, kind)` once — [`super::ingest::unattributed_senders`]
    /// builds it that way — because one `INSERT … ON CONFLICT` cannot touch the same row twice. A
    /// hostname is kept when a later message carries none: a device that names itself in some
    /// messages and not others still has a name.
    pub async fn record_unattributed_senders(
        &self,
        senders: &[crate::arp::SenderObservation],
    ) -> anyhow::Result<u64> {
        if senders.is_empty() {
            return Ok(0);
        }
        let ips: Vec<String> = senders.iter().map(|s| s.ip.to_string()).collect();
        let kinds: Vec<&str> = senders.iter().map(|s| s.kind.as_str()).collect();
        let hosts: Vec<Option<String>> = senders.iter().map(|s| s.hostname.clone()).collect();
        let res = sqlx::query(
            "INSERT INTO event_senders (ip, kind, hostname, first_seen, last_seen) \
             SELECT u.ip::INET, u.kind, u.host, now(), now() \
             FROM UNNEST($1::TEXT[], $2::TEXT[], $3::TEXT[]) AS u(ip, kind, host) \
             ON CONFLICT (ip, kind) DO UPDATE SET \
                 hostname = COALESCE(EXCLUDED.hostname, event_senders.hostname), \
                 last_seen = now()",
        )
        .bind(&ips)
        .bind(&kinds)
        .bind(&hosts)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected())
    }

    /// The `limit` most recently heard senders — the discovery sweep's fourth input.
    ///
    /// Bounded here and not only by [`Self::prune_senders`]: the sweep reads before it prunes,
    /// and between two sweeps a flood of forged source addresses can grow the table without limit
    /// (ADR-179 増分 5). The sweep could never keep more than `limit` of them anyway.
    pub async fn unattributed_senders(
        &self,
        limit: usize,
    ) -> anyhow::Result<Vec<crate::arp::SenderObservation>> {
        let rows = sqlx::query(
            "SELECT host(ip) AS ip, kind, hostname FROM event_senders \
             ORDER BY last_seen DESC, ip, kind LIMIT $1",
        )
        .bind(i64::try_from(limit).unwrap_or(i64::MAX))
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .filter_map(|row| {
                // A row that will not parse is skipped, not fatal: one bad row must not stop
                // discovery for the whole fleet (the same rule `known_addresses` follows).
                let ip = row.try_get::<String, _>("ip").ok()?.parse().ok()?;
                let kind =
                    crate::arp::SenderKind::from_token(&row.try_get::<String, _>("kind").ok()?)?;
                let hostname = row.try_get::<Option<String>, _>("hostname").ok()?;
                Some(crate::arp::SenderObservation { ip, kind, hostname })
            })
            .collect())
    }

    /// The newest sighting of any sender, or `None` when nothing unattributed has ever arrived —
    /// one of the four marks the sweep triggers on.
    pub async fn senders_watermark(&self) -> anyhow::Result<Option<chrono::DateTime<chrono::Utc>>> {
        let row = sqlx::query("SELECT max(last_seen) AS w FROM event_senders")
            .fetch_one(&self.pool)
            .await?;
        Ok(row.try_get("w")?)
    }

    /// Drop senders not heard from inside the retention window, then keep the newest `cap`.
    pub async fn prune_senders(&self, retention_secs: i64, cap: usize) -> anyhow::Result<u64> {
        let aged = sqlx::query(
            "DELETE FROM event_senders WHERE last_seen < now() - make_interval(secs => $1)",
        )
        .bind(retention_secs as f64)
        .execute(&self.pool)
        .await?;
        let over = sqlx::query(
            "DELETE FROM event_senders s USING ( \
                 SELECT ip, kind, row_number() OVER (ORDER BY last_seen DESC, ip, kind) AS rn \
                 FROM event_senders \
             ) r WHERE s.ip = r.ip AND s.kind = r.kind AND r.rn > $1",
        )
        .bind(i64::try_from(cap).unwrap_or(i64::MAX))
        .execute(&self.pool)
        .await?;
        Ok(aged.rows_affected() + over.rows_affected())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::testkit;

    /// Sixty seconds of event time, as milliseconds, starting from a fixed instant.
    ///
    /// A literal rather than `now()`: the aggregates below bucket on **event time**, and a fixture
    /// anchored to the wall clock would put two records either side of a bucket boundary on some
    /// runs and not others.
    const T0: i64 = 1_760_000_000_000;

    /// One record, with every field a query below actually reads spelled out.
    ///
    /// Built here rather than in `events/testkit.rs` because nothing outside this module needs it:
    /// the testkit's own doc says a helper used by one module stays in that module.
    fn record(
        msg: EventMsg,
        node_id: Option<Uuid>,
        rule: Option<Uuid>,
        action: EventAction,
    ) -> PersistRecord {
        PersistRecord {
            signature: signature_of(&msg),
            msg,
            node_id,
            source_id: None,
            matched_rule_id: rule,
            action,
        }
    }

    fn syslog_at(message: &str, at_ms: i64) -> EventMsg {
        let mut m = testkit::syslog_msg(message);
        m.at_unix_ms = at_ms;
        m
    }

    /// 🚨 **A record naming a node or rule deleted since it was queued is kept, and does not lose
    /// the rest of the batch** (ADR-141).
    ///
    /// `ON DELETE SET NULL` reaches only the rows that already exist when the parent goes. A record
    /// queued before the delete still carries the old id, and one violating row fails the whole
    /// multi-row INSERT — every other event in the flush with it.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_record_naming_a_deleted_node_or_rule_does_not_lose_the_batch(pool: sqlx::PgPool) {
        let kept = crate::pgtest::node(&pool, "rtr-1", 11, None).await;
        let gone = crate::pgtest::node(&pool, "rtr-2", 12, None).await;
        let repo = EventRepo::new(pool.clone());
        let rule = repo
            .create_rule(&rule_params("link down", "LINK-3-UPDOWN"))
            .await
            .unwrap();
        let gone_rule = repo
            .create_rule(&rule_params("retired", "RETIRED"))
            .await
            .unwrap();
        assert!(crate::pgtest::repo(pool.clone())
            .delete_node(gone)
            .await
            .unwrap());
        assert!(repo.delete_rule(gone_rule).await.unwrap());

        let a = record(
            syslog_at("from the kept node", T0),
            Some(kept),
            Some(rule),
            EventAction::Fired,
        );
        let b = record(
            syslog_at("from the deleted node", T0 + 1_000),
            Some(gone),
            None,
            EventAction::None,
        );
        let c = record(
            syslog_at("matched the deleted rule", T0 + 2_000),
            Some(kept),
            Some(gone_rule),
            EventAction::Fired,
        );

        assert_eq!(
            repo.insert_events_batch(&[&a, &b, &c])
                .await
                .expect("a batch naming a deleted node must still be written"),
            3
        );
        let all = repo.list_events(&EventFilter::default(), 50).await.unwrap();
        let row = |message: &str| {
            all.iter()
                .find(|e| e.message == message)
                .unwrap_or_else(|| panic!("{message:?} was not stored"))
        };
        assert_eq!(row("from the kept node").node_id, Some(kept));
        assert_eq!(row("from the kept node").matched_rule_id, Some(rule));
        assert_eq!(
            row("from the deleted node").node_id,
            None,
            "the id of a node that no longer exists"
        );
        assert_eq!(row("matched the deleted rule").node_id, Some(kept));
        assert_eq!(row("matched the deleted rule").matched_rule_id, None);
    }

    fn rule_params<'a>(name: &'a str, pattern: &'a str) -> RuleParams<'a> {
        RuleParams {
            name,
            enabled: true,
            source_kind: Some("syslog"),
            source_id: None,
            node_id: None,
            match_kind: "substring",
            pattern,
            clear_pattern: Some("link up"),
            severity: "warning",
            ttl_secs: 1800,
            min_count: 1,
            window_secs: 60,
        }
    }

    /// A webhook source through its whole life: created, listed, renamed, deleted.
    ///
    /// `create_source` is the only writer that mints a token, and the plaintext is returned exactly
    /// once — so this is also the only place the stored digest can be checked against the value the
    /// operator was shown.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_webhook_source_is_created_listed_renamed_and_deleted(pool: sqlx::PgPool) {
        let node = crate::pgtest::node(&pool, "rtr-1", 11, None).await;
        let repo = EventRepo::new(pool.clone());

        let (id, token) = repo.create_source("branch", Some(node)).await.unwrap();
        assert!(!token.is_empty());
        let listed = repo.list_sources().await.unwrap();
        let made = listed.iter().find(|s| s.id == id).expect("source listed");
        assert_eq!(made.name, "branch");
        assert!(made.enabled);
        assert_eq!(made.node_id, Some(node));

        assert!(repo
            .update_source(id, "branch-2", false, None)
            .await
            .unwrap());
        let after = repo.list_sources().await.unwrap();
        let made = after
            .iter()
            .find(|s| s.id == id)
            .expect("source still listed");
        assert_eq!(made.name, "branch-2");
        assert!(!made.enabled);
        assert_eq!(made.node_id, None);

        // A row that is not there reports false rather than erroring — the API edge turns that
        // into a 404, so the distinction is load-bearing.
        assert!(!repo
            .update_source(Uuid::new_v4(), "ghost", true, None)
            .await
            .unwrap());
        assert!(repo.delete_source(id).await.unwrap());
        assert!(!repo.delete_source(id).await.unwrap());
        assert!(repo
            .list_sources()
            .await
            .unwrap()
            .iter()
            .all(|s| s.id != id));
    }

    /// Rotating a token invalidates the previous one, and a disabled source is indistinguishable
    /// from one that does not exist.
    ///
    /// 🚨 The last assertion is a security property, not a tidiness one: `UnknownOrDisabled` is one
    /// variant on purpose, so a caller probing ids cannot tell a real source from a fabricated one.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_rotated_token_replaces_the_one_before_it(pool: sqlx::PgPool) {
        let repo = EventRepo::new(pool.clone());
        let (id, first) = repo.create_source("branch", None).await.unwrap();
        assert_eq!(
            repo.verify_token(id, &first).await.unwrap(),
            TokenVerify::Ok { node_id: None }
        );
        assert_eq!(
            repo.verify_token(id, "not-the-token").await.unwrap(),
            TokenVerify::BadToken
        );

        let second = repo.rotate_token(id).await.unwrap().expect("rotated");
        assert_ne!(first, second);
        assert_eq!(
            repo.verify_token(id, &second).await.unwrap(),
            TokenVerify::Ok { node_id: None }
        );
        assert_eq!(
            repo.verify_token(id, &first).await.unwrap(),
            TokenVerify::BadToken
        );

        assert!(repo.update_source(id, "branch", false, None).await.unwrap());
        assert_eq!(
            repo.verify_token(id, &second).await.unwrap(),
            TokenVerify::UnknownOrDisabled
        );
        assert_eq!(
            repo.verify_token(Uuid::new_v4(), &second).await.unwrap(),
            TokenVerify::UnknownOrDisabled
        );
        assert!(repo.rotate_token(Uuid::new_v4()).await.unwrap().is_none());
    }

    /// A rule through its whole life, including the fields the engine compiles from.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn an_event_rule_is_created_listed_edited_and_deleted(pool: sqlx::PgPool) {
        let repo = EventRepo::new(pool.clone());
        let id = repo
            .create_rule(&rule_params("link down", "LINK-3-UPDOWN"))
            .await
            .unwrap();

        let rules = repo.list_rules().await.unwrap();
        let made = rules.iter().find(|r| r.id == id).expect("rule listed");
        assert_eq!(made.pattern, "LINK-3-UPDOWN");
        assert_eq!(made.match_kind, EventMatchKind::Substring);
        assert_eq!(made.clear_pattern.as_deref(), Some("link up"));
        assert_eq!(made.ttl_secs, 1800);

        let mut edited = rule_params("link down", "%LINK-3-UPDOWN:");
        edited.enabled = false;
        edited.match_kind = "regex";
        edited.severity = "critical";
        assert!(repo.update_rule(id, &edited).await.unwrap());
        let rules = repo.list_rules().await.unwrap();
        let made = rules
            .iter()
            .find(|r| r.id == id)
            .expect("rule still listed");
        assert!(!made.enabled);
        assert_eq!(made.match_kind, EventMatchKind::Regex);
        assert_eq!(made.pattern, "%LINK-3-UPDOWN:");

        assert!(!repo.update_rule(Uuid::new_v4(), &edited).await.unwrap());
        assert!(repo.delete_rule(id).await.unwrap());
        assert!(!repo.delete_rule(id).await.unwrap());
    }

    /// A batch is written, and the filter reads back exactly the rows it names.
    ///
    /// ⚠️ `EVENT_FILTER_WHERE` has three implementations (here, `logstore.rs`, `matchRanges.ts`).
    /// A mirror test already pins them to each other; this is the first time the PostgreSQL one is
    /// **evaluated by PostgreSQL**.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_batch_is_written_and_the_filter_reads_back_what_it_names(pool: sqlx::PgPool) {
        let node = crate::pgtest::node(&pool, "rtr-1", 11, None).await;
        let other = crate::pgtest::node(&pool, "rtr-2", 12, None).await;
        let repo = EventRepo::new(pool.clone());
        let rule = repo
            .create_rule(&rule_params("link down", "LINK-3-UPDOWN"))
            .await
            .unwrap();

        let a = record(
            syslog_at("%LINK-3-UPDOWN: GE0/0/1 down", T0),
            Some(node),
            Some(rule),
            EventAction::Fired,
        );
        let b = record(
            syslog_at("%LINK-3-UPDOWN: GE0/0/1 up", T0 + 1_000),
            Some(node),
            Some(rule),
            EventAction::Cleared,
        );
        let c = record(
            syslog_at("routine housekeeping", T0 + 2_000),
            Some(other),
            None,
            EventAction::None,
        );
        let mut trap = testkit::trap_msg("1.3.6.1.6.3.1.1.5.3");
        trap.at_unix_ms = T0 + 3_000;
        let d = record(trap, Some(other), None, EventAction::None);

        assert_eq!(
            repo.insert_events_batch(&[&a, &b, &c, &d]).await.unwrap(),
            4
        );
        // An empty batch is not a degenerate INSERT — it returns before building one.
        assert_eq!(repo.insert_events_batch(&[]).await.unwrap(), 0);

        let all = repo.list_events(&EventFilter::default(), 50).await.unwrap();
        assert_eq!(all.len(), 4);

        let one_node = EventFilter {
            node_id: Some(node),
            ..EventFilter::default()
        };
        assert_eq!(repo.list_events(&one_node, 50).await.unwrap().len(), 2);

        let matched = EventFilter {
            matched: Some(true),
            ..EventFilter::default()
        };
        assert_eq!(repo.list_events(&matched, 50).await.unwrap().len(), 2);

        let traps = EventFilter {
            kinds: vec!["trap".to_owned()],
            ..EventFilter::default()
        };
        assert_eq!(repo.list_events(&traps, 50).await.unwrap().len(), 1);

        // Empty means every kind, which is the one spelling of "unfiltered" the type allows.
        let every_kind = EventFilter {
            kinds: Vec::new(),
            ..EventFilter::default()
        };
        assert_eq!(repo.list_events(&every_kind, 50).await.unwrap().len(), 4);

        let window = EventFilter {
            since: DateTime::from_timestamp_millis(T0 + 2_000),
            ..EventFilter::default()
        };
        assert_eq!(repo.list_events(&window, 50).await.unwrap().len(), 2);

        // The scope restriction is subtractive, and an empty visible set means nothing is visible.
        let scoped = EventFilter {
            visible_node_ids: Some(Vec::new()),
            ..EventFilter::default()
        };
        assert!(repo.list_events(&scoped, 50).await.unwrap().is_empty());
    }

    /// Every aggregate counts what the batch actually contains.
    ///
    /// One test rather than seven because they share a fixture whose shape is the whole point: the
    /// auth phrase, the unmatched signature, the syslog severity and the fire/clear pair each exist
    /// to make exactly one of these queries return a row. Split apart, six of them would be
    /// asserting emptiness — which every one of them also returns when its SQL is wrong
    /// (`rejection-only-tests-pass-when-everything-rejects`).
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn every_aggregate_counts_what_the_batch_contains(pool: sqlx::PgPool) {
        let node = crate::pgtest::node(&pool, "rtr-1", 11, None).await;
        let repo = EventRepo::new(pool.clone());
        let rule = repo
            .create_rule(&rule_params("link down", "LINK-3-UPDOWN"))
            .await
            .unwrap();

        let mut fired = syslog_at("%LINK-3-UPDOWN: GE0/0/1 down", T0);
        fired.syslog_severity = Some(3);
        let mut cleared = syslog_at("%LINK-3-UPDOWN: GE0/0/1 up", T0 + 1_000);
        cleared.syslog_severity = Some(3);
        // Unmatched, and carrying a signature: `COALESCE(trap_oid, signature, app_name)` falls to
        // the app name, which is the tier that clusters plain syslog.
        let mut noisy = syslog_at("disk usage nominal", T0 + 2_000);
        noisy.app_name = Some("housekeeper".to_owned());
        noisy.syslog_severity = Some(6);
        // Unmatched, and an authentication signal by phrase rather than by trap OID.
        let mut auth = syslog_at("Failed password for invalid user root", T0 + 3_000);
        auth.syslog_severity = Some(4);

        let rows = [
            record(fired, Some(node), Some(rule), EventAction::Fired),
            record(cleared, Some(node), Some(rule), EventAction::Cleared),
            record(noisy, Some(node), None, EventAction::None),
            record(auth, Some(node), None, EventAction::None),
        ];
        let refs: Vec<&PersistRecord> = rows.iter().collect();
        assert_eq!(repo.insert_events_batch(&refs).await.unwrap(), 4);

        let f = EventFilter::default();

        let buckets = repo.event_counts_by_bucket(&f, 60).await.unwrap();
        assert_eq!(buckets.iter().map(|b| b.count).sum::<i64>(), 4);

        let flap = repo.event_flap_stats(T0 - 1, T0 + 10_000).await.unwrap();
        let stat = flap
            .iter()
            .find(|s| s.rule_id == rule)
            .expect("the fired/cleared pair is one flap row");
        assert_eq!(stat.fires, 1);
        assert_eq!(stat.clears, 1);
        assert_eq!(stat.rule_name, "link down");
        // The window is on event time, so a window that excludes them finds nothing.
        assert!(repo
            .event_flap_stats(T0 - 10_000, T0 - 1)
            .await
            .unwrap()
            .is_empty());

        let sev = repo.event_severity_counts(&f).await.unwrap();
        assert_eq!(
            sev.iter().find(|s| s.severity == 3).map(|s| s.count),
            Some(2)
        );
        assert_eq!(
            sev.iter().find(|s| s.severity == 6).map(|s| s.count),
            Some(1)
        );

        let sigs = repo.event_unmatched_signatures(&f, 10).await.unwrap();
        assert!(
            sigs.iter().any(|s| s.signature == "housekeeper"),
            "unmatched signatures: {sigs:?}"
        );
        // Matched rows are excluded, so the rule's own pattern never appears here.
        assert!(sigs.iter().all(|s| s.count == 1));

        let auth_rows = repo.event_auth_sources(&f, 10).await.unwrap();
        assert_eq!(auth_rows.len(), 1, "auth sources: {auth_rows:?}");
        assert_eq!(auth_rows[0].count, 1);
        assert_eq!(auth_rows[0].node_id, Some(node));

        let by_kind = repo
            .stats_grouped(&f, EventStatGroup::Kind, 10)
            .await
            .unwrap();
        assert_eq!(by_kind.iter().map(|b| b.count).sum::<i64>(), 4);
        let by_action = repo
            .stats_grouped(&f, EventStatGroup::Action, 10)
            .await
            .unwrap();
        assert_eq!(by_action.len(), 3, "fired / cleared / none: {by_action:?}");

        let series = repo.stats_series(&f, 60, false).await.unwrap();
        assert_eq!(series.iter().map(|b| b.count).sum::<i64>(), 4);
        let split = repo.stats_series(&f, 60, true).await.unwrap();
        assert_eq!(split.iter().map(|b| b.count).sum::<i64>(), 4);
    }

    /// Retention deletes by age and keeps what is inside the window — both directions, and the two
    /// windows are independent.
    ///
    /// 🚨 The ages are set with a direct `UPDATE`, which is the one thing this module's tests do by
    /// hand. `recorded_at` defaults to `now()` on insert and nothing in the product ever writes it,
    /// so there is no production writer to go through; the alternative is passing a negative window
    /// so that "older than" is satisfied by everything, which would prove the statement runs and
    /// nothing about whether it selects the right rows.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn retention_deletes_by_age_and_keeps_what_is_inside_the_window(pool: sqlx::PgPool) {
        let node = crate::pgtest::node(&pool, "rtr-1", 11, None).await;
        let repo = EventRepo::new(pool.clone());
        let rule = repo
            .create_rule(&rule_params("link down", "LINK-3-UPDOWN"))
            .await
            .unwrap();

        let old_matched = record(
            syslog_at("%LINK-3-UPDOWN: old", T0),
            Some(node),
            Some(rule),
            EventAction::Fired,
        );
        let new_matched = record(
            syslog_at("%LINK-3-UPDOWN: new", T0 + 1_000),
            Some(node),
            Some(rule),
            EventAction::Fired,
        );
        let old_plain = record(
            syslog_at("old chatter", T0),
            Some(node),
            None,
            EventAction::None,
        );
        let new_plain = record(
            syslog_at("new chatter", T0 + 1_000),
            Some(node),
            None,
            EventAction::None,
        );
        repo.insert_events_batch(&[&old_matched, &new_matched, &old_plain, &new_plain])
            .await
            .unwrap();

        sqlx::query("UPDATE events SET recorded_at = now() - interval '2 days' WHERE id = ANY($1)")
            .bind(vec![old_matched.msg.event_id, old_plain.msg.event_id])
            .execute(&pool)
            .await
            .unwrap();

        // Nothing is old enough for a three-day window.
        assert_eq!(repo.prune_old(259_200, 259_200).await.unwrap(), (0, 0));
        // The unmatched window alone takes the aged unmatched row, and leaves the matched one.
        assert_eq!(repo.prune_old(259_200, 86_400).await.unwrap(), (0, 1));
        assert_eq!(repo.prune_old(86_400, 86_400).await.unwrap(), (1, 0));
        assert_eq!(crate::pgtest::rows(&pool, "events").await, 2);
    }

    /// The senders table keeps one row per (address, kind), keeps a hostname a later batch left
    /// out, moves the watermark, and prunes by age then by the ceiling (ADR-179 決定 3).
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn unattributed_senders_are_remembered_merged_and_pruned(pool: sqlx::PgPool) {
        use crate::arp::{SenderKind, SenderObservation};
        let repo = EventRepo::new(pool.clone());
        assert_eq!(repo.senders_watermark().await.unwrap(), None);
        let sender = |addr: &str, kind, host: Option<&str>| SenderObservation {
            ip: addr.parse().unwrap(),
            kind,
            hostname: host.map(str::to_owned),
        };
        assert_eq!(
            repo.record_unattributed_senders(&[
                sender("192.0.2.5", SenderKind::Syslog, Some("fw-01")),
                sender("192.0.2.5", SenderKind::Trap, None),
                sender("2001:db8::5", SenderKind::Syslog, None),
            ])
            .await
            .unwrap(),
            3
        );
        repo.record_unattributed_senders(&[sender("192.0.2.5", SenderKind::Syslog, None)])
            .await
            .unwrap();
        assert!(repo.senders_watermark().await.unwrap().is_some());

        let mut got = repo.unattributed_senders(100).await.unwrap();
        got.sort_by_key(|s| (s.ip, s.kind));
        assert_eq!(
            got,
            vec![
                sender("192.0.2.5", SenderKind::Syslog, Some("fw-01")),
                sender("192.0.2.5", SenderKind::Trap, None),
                sender("2001:db8::5", SenderKind::Syslog, None),
            ],
            "a later message with no hostname must not erase the name, and v6 must read back"
        );

        sqlx::query(
            "UPDATE event_senders SET last_seen = now() - interval '10 days' WHERE kind = 'trap'",
        )
        .execute(&pool)
        .await
        .unwrap();
        let newest = repo.unattributed_senders(2).await.unwrap();
        assert_eq!(
            newest.len(),
            2,
            "the read is bounded, whatever the table holds"
        );
        assert!(
            newest.iter().all(|s| s.kind == SenderKind::Syslog),
            "and it keeps the most recently heard: the aged trap is the one left out"
        );
        assert_eq!(
            repo.prune_senders(7 * 86_400, 10).await.unwrap(),
            1,
            "the aged row"
        );
        assert_eq!(
            repo.prune_senders(7 * 86_400, 1).await.unwrap(),
            1,
            "the ceiling"
        );
        assert_eq!(crate::pgtest::rows(&pool, "event_senders").await, 1);
    }
}
