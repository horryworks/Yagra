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
    pub async fn insert_events_batch(&self, records: &[&PersistRecord]) -> anyhow::Result<u64> {
        if records.is_empty() {
            return Ok(0);
        }
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
            b.push_bind(r.node_id)
                .push_bind(r.source_id)
                .push_bind(m.pool.clone())
                .push_bind(m.facility.map(i16::from))
                .push_bind(m.syslog_severity.map(i16::from))
                .push_bind(m.hostname.clone())
                .push_bind(m.app_name.clone())
                .push_bind(m.trap_oid.clone())
                .push_bind(r.signature.clone())
                .push_bind(varbinds)
                .push_bind(m.message.clone())
                .push_bind(r.matched_rule_id)
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
}
