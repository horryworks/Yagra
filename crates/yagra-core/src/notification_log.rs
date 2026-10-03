// SPDX-License-Identifier: AGPL-3.0-only
//! The notification delivery log (ADR-195): one row per delivery, saying whether it arrived and,
//! when it did not, on whose side it failed.
//!
//! Before this nothing about an individual delivery was kept. Two Prometheus series counted
//! outcomes per channel, and the reason a page failed was matched as `Err(_)` and dropped inside
//! the dispatcher's retry loop - so an operator whose JSM never paged could not tell a wrong key
//! from a Yagra fault from a network that ate the request.
//!
//! Three parts, kept apart so the notifier never waits on PostgreSQL:
//! - [`DeliveryLog`], the sender the notifier holds. Recording is a `try_send`: a full queue drops
//!   the row and counts it, because a page must never wait on its own audit trail (ADR-104).
//! - [`start`], the writer task that drains that queue in batches through `batch_writer::run`.
//! - [`DeliveryLogRepo`], the table: the batch insert, the page read, the retention prune.

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Row};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use yagra_alert::{Attempt, FailureSide};
use yagra_common::{NotifyEvent, Severity};

use crate::notifications::ChannelKind;

// The three enums below derive `ToSchema`, so their doc comments are published to API clients.
// Each is both a column value and a JSON tag; `token_enum!` gives them one token list, and
// `tokens_and_serde_agree` pins that list to serde's spelling.

/// What a delivery was for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryEvent {
    /// An alert fired.
    Fire,
    /// An alert recovered; for PagerDuty and JSM this closes the incident.
    Resolve,
    /// An alert was rolled up under an upstream root cause; its incident is closed.
    Suppress,
    /// A test notification sent from the channel list.
    Test,
    /// The close that follows a PagerDuty or JSM test notification.
    TestClose,
    /// Written by a newer core with a kind this one does not know.
    Unknown,
}

crate::stored_enum::token_enum!(DeliveryEvent, Unknown, "notification_deliveries.event", [
    Fire => "fire",
    Resolve => "resolve",
    Suppress => "suppress",
    Test => "test",
    TestClose => "test_close",
    Unknown => "unknown",
]);

impl From<NotifyEvent> for DeliveryEvent {
    fn from(e: NotifyEvent) -> Self {
        match e {
            NotifyEvent::Fire => Self::Fire,
            NotifyEvent::Resolve => Self::Resolve,
            NotifyEvent::Suppress => Self::Suppress,
        }
    }
}

/// Whether a delivery arrived.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryResult {
    /// The receiving service accepted it, possibly after retries.
    Delivered,
    /// Every attempt failed.
    Failed,
    /// Written by a newer core with a result this one does not know.
    Unknown,
}

crate::stored_enum::token_enum!(DeliveryResult, Unknown, "notification_deliveries.result", [
    Delivered => "delivered",
    Failed => "failed",
    Unknown => "unknown",
]);

/// Where a failed delivery failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DeliverySide {
    /// Stopped inside Yagra before anything was sent: the target address was refused, or the
    /// request or the message could not be built.
    Yagra,
    /// Sent, but nothing answered: a timeout, a refused connection, DNS, TLS.
    Network,
    /// The receiving service answered and refused: an HTTP status outside 2xx, or an SMTP reply
    /// code. `status` and usually `response` say why.
    Remote,
    /// Written by a newer core with a side this one does not know.
    Unknown,
}

crate::stored_enum::token_enum!(DeliverySide, Unknown, "notification_deliveries.side", [
    Yagra => "yagra",
    Network => "network",
    Remote => "remote",
    Unknown => "unknown",
]);

impl From<FailureSide> for DeliverySide {
    fn from(s: FailureSide) -> Self {
        match s {
            FailureSide::Yagra => Self::Yagra,
            FailureSide::Network => Self::Network,
            FailureSide::Remote => Self::Remote,
        }
    }
}

/// One delivery, as the notifier hands it over. Not the API shape: see [`DeliveryRow`].
#[derive(Debug, Clone)]
pub(crate) struct DeliveryRecord {
    pub at: DateTime<Utc>,
    /// `None` for the environment default route (`YAGRA_WEBHOOK_URL` / `YAGRA_SMTP_*`).
    pub channel_id: Option<Uuid>,
    /// The channel's kind when the delivery was made, kept because the channel may be deleted.
    pub channel_kind: Option<ChannelKind>,
    pub event: DeliveryEvent,
    /// The alert's subject as its stable text (a node UUID, `pool:<name>`, ...).
    pub subject: String,
    /// The node, when the subject is one.
    pub node_id: Option<Uuid>,
    /// The subject's display name, when the notifier had resolved it.
    pub subject_name: Option<String>,
    pub severity: Option<Severity>,
    pub delivered: bool,
    /// Wall time for the whole delivery, retries and backoff included.
    pub duration: Duration,
    /// Every call made to the channel, oldest first.
    pub attempts: Vec<Attempt>,
}

/// One call to the channel inside a delivery.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct DeliveryAttempt {
    /// How long the channel took to answer, in milliseconds.
    pub duration_ms: i64,
    /// Where it failed; absent when this attempt succeeded.
    pub side: Option<DeliverySide>,
    /// The status the receiving service answered with, when it answered.
    pub status: Option<i32>,
    /// Why it failed, in one line.
    pub error: Option<String>,
}

/// One row of the delivery log (API shape).
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct DeliveryRow {
    /// Row id; the second half of the paging cursor.
    pub id: i64,
    /// When the delivery started (RFC 3339). The first half of the paging cursor.
    pub at: String,
    /// The channel, or absent for the environment default route.
    pub channel_id: Option<Uuid>,
    /// The channel's current name; absent for the default route and for a deleted channel.
    pub channel_name: Option<String>,
    /// The channel's kind when the delivery was made.
    pub channel_kind: Option<ChannelKind>,
    pub event: DeliveryEvent,
    pub result: DeliveryResult,
    /// Where it failed; absent for a delivered row.
    pub side: Option<DeliverySide>,
    /// The status of the last failed attempt, when the receiving service answered.
    pub status: Option<i32>,
    /// How many calls were made to the channel.
    pub attempts: i32,
    /// Wall time for the whole delivery, retries and backoff included, in milliseconds.
    pub duration_ms: i64,
    /// The alert's subject as stored: a node UUID, `pool:<name>`, `meraki_org:<id>`, or `test`.
    pub subject: String,
    /// The node the alert was about, when it was about one.
    pub node_id: Option<Uuid>,
    /// The subject's display name, when it was known at delivery time.
    pub subject_name: Option<String>,
    pub severity: Option<Severity>,
    /// Why the last attempt failed. Never contains the channel's URL or key.
    pub error: Option<String>,
    /// The start of what the receiving service answered (at most 512 characters), with the
    /// channel's URL, host and key replaced by `<redacted>`.
    pub response: Option<String>,
    /// Every call made to the channel, oldest first.
    pub attempt_log: Vec<DeliveryAttempt>,
}

/// Default / maximum page sizes.
pub const DEFAULT_LIMIT: i64 = 100;
pub const MAX_LIMIT: i64 = 500;

/// A validated page query. Built at the API edge by `api::notifications::delivery_page`.
#[derive(Debug, Default, Clone)]
pub struct DeliveryFilter {
    /// Keyset cursor: rows strictly before this `(at, id)`.
    pub before: Option<(DateTime<Utc>, i64)>,
    pub since: Option<DateTime<Utc>>,
    pub until: Option<DateTime<Utc>>,
    /// Only these channels. Empty with `default_route` false means every channel.
    pub channels: Vec<Uuid>,
    /// Include the environment default route in a channel filter.
    pub default_route: bool,
    pub results: Vec<DeliveryResult>,
    pub sides: Vec<DeliverySide>,
    pub events: Vec<DeliveryEvent>,
    /// Rows per page; clamped to `1..=MAX_LIMIT` by [`DeliveryLogRepo::list`].
    pub limit: i64,
}

/// The `WHERE` of a page. Every clause is always present with a `NULL` bind meaning "no filter",
/// the rule `audit.rs` states: a clause appended only when set is a branch that can be forgotten,
/// and forgetting one here fails open.
///
/// `$5`/`$6` are one filter: a channel set and whether the default route (no channel id) is in
/// it. `$5` is bound non-NULL whenever either half is set, so "default route only" is an empty
/// array plus `true`.
const DELIVERY_FILTER_WHERE: &str = "($1::timestamptz IS NULL OR (d.at, d.id) < ($1, $2::bigint)) \
     AND ($3::timestamptz IS NULL OR d.at >= $3) \
     AND ($4::timestamptz IS NULL OR d.at <= $4) \
     AND ($5::uuid[] IS NULL OR d.channel_id = ANY($5) OR ($6::boolean AND d.channel_id IS NULL)) \
     AND ($7::text[] IS NULL OR d.result = ANY($7)) \
     AND ($8::text[] IS NULL OR d.side = ANY($8)) \
     AND ($9::text[] IS NULL OR d.event = ANY($9))";

fn list_sql() -> String {
    format!(
        "SELECT d.id, d.at, d.channel_id, c.name AS channel_name, d.channel_kind, d.event, \
                d.result, d.side, d.status, d.attempts, d.duration_ms, d.subject, d.node_id, \
                d.subject_name, d.severity, d.error, d.response, d.attempt_log \
         FROM notification_deliveries d \
         LEFT JOIN notification_channels c ON c.id = d.channel_id \
         WHERE {DELIVERY_FILTER_WHERE} \
         ORDER BY d.at DESC, d.id DESC LIMIT $10"
    )
}

/// Bind `$1..=$9` of [`DELIVERY_FILTER_WHERE`], in the one order that matches it.
fn bind_filter<'q>(
    q: sqlx::query::Query<'q, sqlx::Postgres, sqlx::postgres::PgArguments>,
    f: &'q DeliveryFilter,
) -> sqlx::query::Query<'q, sqlx::Postgres, sqlx::postgres::PgArguments> {
    fn tokens<T: Copy>(set: &[T], token: impl Fn(T) -> &'static str) -> Option<Vec<&'static str>> {
        (!set.is_empty()).then(|| set.iter().map(|v| token(*v)).collect())
    }
    let channels = (!f.channels.is_empty() || f.default_route).then(|| f.channels.clone());
    q.bind(f.before.map(|(at, _)| at))
        .bind(f.before.map(|(_, id)| id))
        .bind(f.since)
        .bind(f.until)
        .bind(channels)
        .bind(f.default_route)
        .bind(tokens(&f.results, DeliveryResult::as_str))
        .bind(tokens(&f.sides, DeliverySide::as_str))
        .bind(tokens(&f.events, DeliveryEvent::as_str))
}

/// The columns derived from a record's attempts: the side, status, error and response of the
/// failure that decided it. `None`s for a delivered record - an earlier failed attempt stays in
/// the attempt log, but the row as a whole did not fail.
fn failure_columns(
    r: &DeliveryRecord,
) -> (
    Option<&'static str>,
    Option<i32>,
    Option<String>,
    Option<String>,
) {
    if r.delivered {
        return (None, None, None, None);
    }
    match r.attempts.iter().rev().find_map(|a| a.failure.as_ref()) {
        Some(f) => (
            Some(DeliverySide::from(f.side).as_str()),
            f.status.map(i32::from),
            Some(f.message.clone()),
            f.response.clone(),
        ),
        None => (None, None, None, None),
    }
}

fn millis(d: Duration) -> i64 {
    i64::try_from(d.as_millis()).unwrap_or(i64::MAX)
}

fn attempt_log(attempts: &[Attempt]) -> Vec<DeliveryAttempt> {
    attempts
        .iter()
        .map(|a| DeliveryAttempt {
            duration_ms: millis(a.duration),
            side: a.failure.as_ref().map(|f| f.side.into()),
            status: a.failure.as_ref().and_then(|f| f.status.map(i32::from)),
            error: a.failure.as_ref().map(|f| f.message.clone()),
        })
        .collect()
}

/// PostgreSQL-backed delivery log.
pub struct DeliveryLogRepo {
    pool: PgPool,
}

impl DeliveryLogRepo {
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Insert a batch. 15 columns × [`WRITE_BATCH_MAX`] stays far under PostgreSQL's
    /// 65,535-parameter ceiling.
    pub(crate) async fn insert(&self, records: &[DeliveryRecord]) -> anyhow::Result<u64> {
        if records.is_empty() {
            return Ok(0);
        }
        let mut qb = sqlx::QueryBuilder::new(
            "INSERT INTO notification_deliveries \
             (at, channel_id, channel_kind, event, result, side, status, attempts, duration_ms, \
              subject, node_id, subject_name, severity, error, response, attempt_log) ",
        );
        qb.push_values(records, |mut b, r| {
            let (side, status, error, response) = failure_columns(r);
            let result = if r.delivered {
                DeliveryResult::Delivered
            } else {
                DeliveryResult::Failed
            };
            let log = serde_json::to_value(attempt_log(&r.attempts))
                .unwrap_or_else(|_| serde_json::Value::Array(Vec::new()));
            b.push_bind(r.at)
                .push_bind(r.channel_id)
                .push_bind(r.channel_kind.map(ChannelKind::as_str))
                .push_bind(r.event.as_str())
                .push_bind(result.as_str())
                .push_bind(side)
                .push_bind(status)
                .push_bind(i32::try_from(r.attempts.len()).unwrap_or(i32::MAX))
                .push_bind(millis(r.duration))
                .push_bind(r.subject.clone())
                .push_bind(r.node_id)
                .push_bind(r.subject_name.clone())
                .push_bind(r.severity.map(|s| s.as_str()))
                .push_bind(error)
                .push_bind(response)
                .push_bind(log);
        });
        let res = qb.build().execute(&self.pool).await?;
        Ok(res.rows_affected())
    }

    /// One newest-first page matching `filter`.
    pub async fn list(&self, filter: &DeliveryFilter) -> anyhow::Result<Vec<DeliveryRow>> {
        let limit = filter.limit.clamp(1, MAX_LIMIT);
        let sql = list_sql();
        let rows = bind_filter(sqlx::query(&sql), filter)
            .bind(limit)
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter().map(|row| read_row(&row)).collect()
    }

    /// Delete rows older than the retention window (`retention::Subject::NotificationDeliveries`).
    pub async fn prune_old(&self, older_than_secs: i64) -> anyhow::Result<u64> {
        let res = sqlx::query(
            "DELETE FROM notification_deliveries \
             WHERE at < now() - ($1::double precision * interval '1 second')",
        )
        .bind(older_than_secs as f64)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected())
    }
}

fn read_row(row: &sqlx::postgres::PgRow) -> anyhow::Result<DeliveryRow> {
    let at: DateTime<Utc> = row.try_get("at")?;
    let kind: Option<String> = row.try_get("channel_kind")?;
    let side: Option<String> = row.try_get("side")?;
    let severity: Option<String> = row.try_get("severity")?;
    let log: serde_json::Value = row.try_get("attempt_log")?;
    Ok(DeliveryRow {
        id: row.try_get("id")?,
        at: at.to_rfc3339(),
        channel_id: row.try_get("channel_id")?,
        channel_name: row.try_get("channel_name")?,
        channel_kind: kind.as_deref().and_then(ChannelKind::from_token),
        event: DeliveryEvent::from_stored(&row.try_get::<String, _>("event")?),
        result: DeliveryResult::from_stored(&row.try_get::<String, _>("result")?),
        side: side.as_deref().map(DeliverySide::from_stored),
        status: row.try_get("status")?,
        attempts: row.try_get("attempts")?,
        duration_ms: row.try_get("duration_ms")?,
        subject: row.try_get("subject")?,
        node_id: row.try_get("node_id")?,
        subject_name: row.try_get("subject_name")?,
        severity: severity
            .as_deref()
            .and_then(|s| Severity::ALL.into_iter().find(|v| v.as_str() == s)),
        error: row.try_get("error")?,
        response: row.try_get("response")?,
        // A log this core cannot read (a newer shape) shows as empty rather than failing the page.
        attempt_log: serde_json::from_value(log).unwrap_or_default(),
    })
}

/// How many records may wait for the writer. A notification storm that outruns PostgreSQL loses
/// log rows past this, never notifications (ADR-195 decision 5).
pub(crate) const CHANNEL_CAP: usize = 4096;
/// Rows per insert.
pub(crate) const WRITE_BATCH_MAX: usize = 256;

/// Counter for a record that could not be queued - the log is missing a row the notifier made.
const M_DROPPED: &str = "yagra_notification_delivery_log_dropped_total";
/// Counter for a batch PostgreSQL refused - the log is missing that many rows.
const M_WRITE_FAILED: &str = "yagra_notification_delivery_log_write_failures_total";

/// The notifier's handle on the log. Cheap to clone; never waits.
#[derive(Clone)]
pub struct DeliveryLog {
    tx: tokio::sync::mpsc::Sender<DeliveryRecord>,
}

impl DeliveryLog {
    /// Queue one record. A full or closed queue drops it and counts it: the notifier calls this
    /// right after a delivery, and waiting here would put PostgreSQL in front of the next page.
    pub(crate) fn record(&self, r: DeliveryRecord) {
        if self.tx.try_send(r).is_err() {
            metrics::counter!(M_DROPPED).increment(1);
        }
    }

    /// A log whose records land in the returned receiver, for tests.
    #[cfg(test)]
    pub(crate) fn for_test() -> (Self, tokio::sync::mpsc::Receiver<DeliveryRecord>) {
        let (tx, rx) = tokio::sync::mpsc::channel(CHANNEL_CAP);
        (Self { tx }, rx)
    }
}

/// Start the writer and return the handle the notifier records through.
///
/// **Safe on every core**: it writes only what this core's own notifier delivers, and a standby
/// core delivers nothing, so its writer idles on an empty queue. On shutdown it writes what is
/// queued before returning (`batch_writer::run`).
pub(crate) fn start(repo: Arc<DeliveryLogRepo>, shutdown: &CancellationToken) -> DeliveryLog {
    let (tx, rx) = tokio::sync::mpsc::channel(CHANNEL_CAP);
    tokio::spawn(crate::batch_writer::run(
        rx,
        WRITE_BATCH_MAX,
        crate::batch_writer::FlushPolicy::EveryDrain,
        "notification_deliveries",
        shutdown.clone(),
        move |buf: &mut Vec<DeliveryRecord>| -> futures::future::BoxFuture<'_, ()> {
            let repo = repo.clone();
            Box::pin(async move {
                if let Err(e) = repo.insert(buf).await {
                    metrics::counter!(M_WRITE_FAILED).increment(buf.len() as u64);
                    tracing::warn!(error = %e, rows = buf.len(), "writing the notification delivery log failed");
                }
                buf.clear();
            })
        },
    ));
    DeliveryLog { tx }
}

#[cfg(test)]
mod tests {
    use super::*;
    use yagra_alert::DeliveryFailure;

    fn attempt(ms: u64, failure: Option<DeliveryFailure>) -> Attempt {
        Attempt {
            duration: Duration::from_millis(ms),
            failure,
        }
    }

    fn record(delivered: bool, attempts: Vec<Attempt>) -> DeliveryRecord {
        DeliveryRecord {
            at: Utc::now(),
            channel_id: Some(Uuid::from_u128(7)),
            channel_kind: Some(ChannelKind::Jsm),
            event: DeliveryEvent::Fire,
            subject: Uuid::from_u128(9).to_string(),
            node_id: Some(Uuid::from_u128(9)),
            subject_name: Some("sw-01".to_owned()),
            severity: Some(Severity::Critical),
            delivered,
            duration: Duration::from_millis(1600),
            attempts,
        }
    }

    #[test]
    fn tokens_and_serde_agree() {
        for v in DeliveryEvent::ALL {
            assert_eq!(serde_json::to_value(v).unwrap(), v.as_str(), "{v:?}");
        }
        for v in DeliveryResult::ALL {
            assert_eq!(serde_json::to_value(v).unwrap(), v.as_str(), "{v:?}");
        }
        for v in DeliverySide::ALL {
            assert_eq!(serde_json::to_value(v).unwrap(), v.as_str(), "{v:?}");
        }
    }

    /// A failed row carries the failure that decided it: the last one.
    #[test]
    fn a_failed_row_says_where_the_last_attempt_failed() {
        let r = record(
            false,
            vec![
                attempt(10_000, Some(DeliveryFailure::network("timed out"))),
                attempt(
                    80,
                    Some(DeliveryFailure::remote(
                        Some(401),
                        "unexpected status 401 Unauthorized",
                        Some("{\"message\":\"Key format is not valid\"}".to_owned()),
                    )),
                ),
            ],
        );
        let (side, status, error, response) = failure_columns(&r);
        assert_eq!(side, Some("remote"));
        assert_eq!(status, Some(401));
        assert_eq!(error.as_deref(), Some("unexpected status 401 Unauthorized"));
        assert!(response.unwrap().contains("Key format"));
        let log = attempt_log(&r.attempts);
        assert_eq!(log[0].side, Some(DeliverySide::Network));
        assert_eq!(log[0].duration_ms, 10_000);
        assert_eq!(log[1].status, Some(401));
    }

    /// Delivered after a retry: the row did not fail, so it names no side - but the earlier
    /// failure stays in the attempt log, which is where a flaky endpoint shows.
    #[test]
    fn a_delivered_row_keeps_its_earlier_failures_only_in_the_attempt_log() {
        let r = record(
            true,
            vec![
                attempt(
                    5,
                    Some(DeliveryFailure::remote(
                        Some(503),
                        "unexpected status 503",
                        None,
                    )),
                ),
                attempt(40, None),
            ],
        );
        assert_eq!(failure_columns(&r), (None, None, None, None));
        let log = attempt_log(&r.attempts);
        assert_eq!(log[0].status, Some(503));
        assert_eq!(log[1].side, None);
    }

    #[test]
    fn a_full_queue_drops_rather_than_waits() {
        let (tx, _rx) = tokio::sync::mpsc::channel(1);
        let log = DeliveryLog { tx };
        log.record(record(true, Vec::new()));
        // The second does not fit; `record` must return rather than block.
        log.record(record(true, Vec::new()));
    }

    use crate::pgtest;

    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn rows_round_trip_and_page_newest_first(pool: PgPool) {
        let repo = DeliveryLogRepo::new(pool.clone());
        let base = Utc::now();
        let mut records = Vec::new();
        for i in 0..5_i64 {
            let mut r = record(
                i % 2 == 0,
                vec![attempt(
                    20,
                    (i % 2 == 1)
                        .then(|| DeliveryFailure::remote(Some(401), "unexpected status 401", None)),
                )],
            );
            r.at = base - chrono::Duration::seconds(i);
            records.push(r);
        }
        // One default-route row, to filter on.
        let mut dflt = record(true, vec![attempt(5, None)]);
        dflt.channel_id = None;
        dflt.channel_kind = None;
        dflt.at = base - chrono::Duration::seconds(10);
        records.push(dflt);
        assert_eq!(repo.insert(&records).await.unwrap(), 6);
        assert_eq!(pgtest::rows(&pool, "notification_deliveries").await, 6);

        let all = repo
            .list(&DeliveryFilter {
                limit: 100,
                ..DeliveryFilter::default()
            })
            .await
            .unwrap();
        assert_eq!(all.len(), 6);
        assert!(all.windows(2).all(|w| w[0].at >= w[1].at), "newest first");
        // No such channel row exists, so the join finds no name - the deleted-channel case.
        assert_eq!(all[0].channel_name, None);
        assert_eq!(all[0].channel_kind, Some(ChannelKind::Jsm));

        let failed = repo
            .list(&DeliveryFilter {
                results: vec![DeliveryResult::Failed],
                sides: vec![DeliverySide::Remote],
                limit: 100,
                ..DeliveryFilter::default()
            })
            .await
            .unwrap();
        assert_eq!(failed.len(), 2);
        assert!(failed.iter().all(|r| r.status == Some(401)));

        let only_default = repo
            .list(&DeliveryFilter {
                default_route: true,
                limit: 100,
                ..DeliveryFilter::default()
            })
            .await
            .unwrap();
        assert_eq!(only_default.len(), 1);
        assert_eq!(only_default[0].channel_id, None);

        // The cursor resumes strictly after the last row of a page.
        let page1 = repo
            .list(&DeliveryFilter {
                limit: 2,
                ..DeliveryFilter::default()
            })
            .await
            .unwrap();
        let last = page1.last().unwrap();
        let cursor = (
            DateTime::parse_from_rfc3339(&last.at)
                .unwrap()
                .with_timezone(&Utc),
            last.id,
        );
        let page2 = repo
            .list(&DeliveryFilter {
                before: Some(cursor),
                limit: 100,
                ..DeliveryFilter::default()
            })
            .await
            .unwrap();
        assert_eq!(page2.len(), 4);
        assert!(page2.iter().all(|r| !page1.iter().any(|p| p.id == r.id)));

        // Nothing is older than a day, so a day's window deletes nothing; zero deletes everything.
        assert_eq!(repo.prune_old(86_400).await.unwrap(), 0);
        assert_eq!(repo.prune_old(0).await.unwrap(), 6);
    }
}
