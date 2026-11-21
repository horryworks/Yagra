// SPDX-License-Identifier: AGPL-3.0-only
//! What is left of delivery hours (ADR-226) since a routing rule keeps its own (ADR-262).
//!
//! - **A weekly window** ([`Window`]): weekdays plus a wall-clock span, read in local time. A
//!   routing rule's `when` is one ([`crate::alerts::notifications::RuleWhen::window`]); which zone
//!   it is read in, and where an alert goes, is `rule_time.rs`.
//! - **The schedules of an earlier version** ([`DeliverySchedule`], [`DeliveryHoursRepo`]): still
//!   listed so an operator can rebuild them as rules and delete them, but read by nothing that
//!   decides a delivery (ADR-262 decision 6). The table stays because an older core reads it.
//! - **The deployment's default zone**, kept with them.
//! - **The notifications held while every rule that matches them is closed**
//!   ([`DeliveryHoursRepo`] as a [`HeldStore`]), kept in PostgreSQL so a restart does not lose the
//!   morning's deliveries (ADR-226 decision 7).
//!
//! The alert itself is never touched: state, screens and history are what they would be without
//! this module.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Row};

use uuid::Uuid;
use yagra_alert::{Alert, Subject};
use yagra_common::CheckId;

/// One named schedule, as stored and as the API returns it (ADR-226 decision 3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct DeliverySchedule {
    pub id: Uuid,
    pub name: String,
    /// Switched off, it covers nothing and every node it named is open again.
    pub enabled: bool,
    /// ISO weekdays the window opens on, 1 = Monday .. 7 = Sunday. Never empty.
    pub days: Vec<u8>,
    /// Minutes after local midnight. A start later than the end spans midnight, and the window
    /// belongs to the day it opens on; equal start and end means the whole of each listed day.
    pub start_minute: u16,
    pub end_minute: u16,
    /// Folders it applies to, each with everything beneath it.
    pub group_ids: Vec<Uuid>,
}

/// What [`DeliveryHoursRepo::create`] writes: a schedule without its id. Test-only since ADR-262:
/// only an earlier version created schedules, and a test stands in for it.
#[cfg(test)]
#[derive(Debug, Clone)]
pub struct ScheduleFields<'a> {
    pub name: &'a str,
    pub enabled: bool,
    pub days: &'a [u8],
    pub start_minute: u16,
    pub end_minute: u16,
    pub group_ids: &'a [Uuid],
}

/// A schedule's span, ready to be asked about one local time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Window {
    /// Bit `d - 1` is ISO weekday `d`.
    days: u8,
    start: u16,
    end: u16,
}

impl Window {
    #[cfg(test)]
    #[must_use]
    pub fn of(schedule: &DeliverySchedule) -> Self {
        Self::from_parts(&schedule.days, schedule.start_minute, schedule.end_minute)
    }

    /// A window from its parts: ISO weekdays and minutes after local midnight. A day outside
    /// `1..=7` is ignored and a minute past the day is clamped to its last minute. Shared with a
    /// routing rule's own window (ADR-262), so the two read a span the same way.
    #[must_use]
    pub fn from_parts(days: &[u8], start_minute: u16, end_minute: u16) -> Self {
        let days = days
            .iter()
            .filter(|d| (1..=7).contains(*d))
            .fold(0u8, |mask, d| mask | (1 << (d - 1)));
        Self {
            days,
            start: start_minute.min(1439),
            end: end_minute.min(1439),
        }
    }

    fn on(self, day: u8) -> bool {
        self.days & (1 << (day - 1)) != 0
    }

    /// Whether the window is open at `minute` of ISO weekday `day`, local time.
    #[must_use]
    pub fn open_at(self, day: u8, minute: u16) -> bool {
        let previous = if day == 1 { 7 } else { day - 1 };
        if self.start == self.end {
            self.on(day)
        } else if self.start < self.end {
            self.on(day) && minute >= self.start && minute < self.end
        } else {
            // Spans midnight: the evening of a listed day, or the small hours after one.
            (self.on(day) && minute >= self.start) || (self.on(previous) && minute < self.end)
        }
    }
}

/// What the notifier was handed when it held a notification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum HeldEvent {
    Fire,
    Resolve,
    Suppress,
}

// The stored `held_notifications.event` token. The fallback is over a `&str` from the database:
// a row an unknown future core wrote reads as a fire, which the release loop only sends if the
// alert is still open.
yagra_base::stored_enum::token_enum!(HeldEvent, Fire, "held_notifications.event", [
    Fire => "fire",
    Resolve => "resolve",
    Suppress => "suppress",
]);

/// One held notification, as the release loop reads it back.
#[derive(Debug, Clone, PartialEq)]
pub struct HeldRow {
    pub id: i64,
    pub event: HeldEvent,
    pub alert: Alert,
    pub held_at: DateTime<Utc>,
    pub release_at: Option<DateTime<Utc>>,
}

/// Where the notifier keeps what it holds. A trait so the notifier's hold-and-release logic is
/// tested without a database; [`DeliveryHoursRepo`] is the real one.
#[async_trait::async_trait]
pub trait HeldStore: Send + Sync {
    /// Keep one notification until its node's hours open.
    async fn hold(
        &self,
        event: HeldEvent,
        alert: &Alert,
        release_at: Option<DateTime<Utc>>,
    ) -> anyhow::Result<()>;
    /// Drop every held fire for this alert, returning how many there were. A resolve that finds
    /// one knows its fire never went out, so it sends nothing either (decision 8).
    async fn discard_fires(&self, subject: &Subject, check: CheckId) -> anyhow::Result<u64>;
    /// Everything held, oldest first.
    async fn list(&self) -> anyhow::Result<Vec<HeldRow>>;
    /// Forget one row once it has been sent or found unnecessary.
    async fn remove(&self, id: i64) -> anyhow::Result<()>;
}

/// The schedules, the deployment's default zone, and the held notifications, in PostgreSQL.
pub struct DeliveryHoursRepo {
    pool: PgPool,
}

impl DeliveryHoursRepo {
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Every schedule, by name.
    pub async fn list(&self) -> anyhow::Result<Vec<DeliverySchedule>> {
        let rows = sqlx::query(
            "SELECT id, name, enabled, days, start_minute, end_minute, group_ids \
             FROM delivery_hours ORDER BY name, id",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| {
                let days: Vec<i16> = row.try_get("days")?;
                let start: i16 = row.try_get("start_minute")?;
                let end: i16 = row.try_get("end_minute")?;
                Ok(DeliverySchedule {
                    id: row.try_get("id")?,
                    name: row.try_get("name")?,
                    enabled: row.try_get("enabled")?,
                    days: days
                        .into_iter()
                        .filter_map(|d| u8::try_from(d).ok())
                        .collect(),
                    start_minute: u16::try_from(start).unwrap_or(0),
                    end_minute: u16::try_from(end).unwrap_or(0),
                    group_ids: row.try_get("group_ids")?,
                })
            })
            .collect()
    }

    /// Store a new schedule and return its id, as an earlier version did.
    #[cfg(test)]
    pub async fn create(&self, f: &ScheduleFields<'_>) -> anyhow::Result<Uuid> {
        let id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO delivery_hours \
               (id, name, enabled, days, start_minute, end_minute, group_ids) \
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(id)
        .bind(f.name)
        .bind(f.enabled)
        .bind(days_column(f.days))
        .bind(i16::try_from(f.start_minute)?)
        .bind(i16::try_from(f.end_minute)?)
        .bind(f.group_ids)
        .execute(&self.pool)
        .await?;
        Ok(id)
    }

    /// Delete a schedule. Returns whether it existed. It holds nothing since ADR-262, so deleting
    /// it changes no delivery.
    pub async fn delete(&self, id: Uuid) -> anyhow::Result<bool> {
        let res = sqlx::query("DELETE FROM delivery_hours WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() > 0)
    }

    /// The zone a node with no zoned folder above it keeps time in, or `None` for UTC.
    pub async fn default_zone(&self) -> anyhow::Result<Option<String>> {
        let zone: Option<Option<String>> =
            sqlx::query_scalar("SELECT default_time_zone FROM app_settings WHERE id = TRUE")
                .fetch_optional(&self.pool)
                .await?;
        Ok(zone.flatten())
    }

    /// Set (or clear) the default zone. The caller has checked the name.
    pub async fn set_default_zone(&self, zone: Option<&str>) -> anyhow::Result<()> {
        sqlx::query("UPDATE app_settings SET default_time_zone = $1 WHERE id = TRUE")
            .bind(zone)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}

/// The `SMALLINT[]` a weekday list is stored as, sorted and without repeats.
#[cfg(test)]
fn days_column(days: &[u8]) -> Vec<i16> {
    days.iter()
        .copied()
        .collect::<std::collections::BTreeSet<u8>>()
        .into_iter()
        .map(i16::from)
        .collect()
}

#[async_trait::async_trait]
impl HeldStore for DeliveryHoursRepo {
    async fn hold(
        &self,
        event: HeldEvent,
        alert: &Alert,
        release_at: Option<DateTime<Utc>>,
    ) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO held_notifications (event, subject, check_id, node_id, alert, release_at) \
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(event.as_str())
        .bind(alert.subject.to_string())
        .bind(alert.check.as_uuid())
        .bind(alert.node().map(|n| n.as_uuid()))
        .bind(serde_json::to_value(alert)?)
        .bind(release_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn discard_fires(&self, subject: &Subject, check: CheckId) -> anyhow::Result<u64> {
        let res = sqlx::query(
            "DELETE FROM held_notifications \
             WHERE subject = $1 AND check_id = $2 AND event = $3",
        )
        .bind(subject.to_string())
        .bind(check.as_uuid())
        .bind(HeldEvent::Fire.as_str())
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected())
    }

    async fn list(&self) -> anyhow::Result<Vec<HeldRow>> {
        self.read_held(true).await
    }

    async fn remove(&self, id: i64) -> anyhow::Result<()> {
        sqlx::query("DELETE FROM held_notifications WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}

impl DeliveryHoursRepo {
    /// Everything held, oldest first, for a screen or a tool to show. Unlike the release loop's
    /// [`HeldStore::list`] it removes nothing: a row this core cannot read is skipped, not
    /// dropped, because a read must not decide what happens to a held page.
    pub async fn held_for_display(&self) -> anyhow::Result<Vec<HeldRow>> {
        self.read_held(false).await
    }

    async fn read_held(&self, drop_unreadable: bool) -> anyhow::Result<Vec<HeldRow>> {
        let rows = sqlx::query(
            "SELECT id, event, alert, held_at, release_at FROM held_notifications ORDER BY id",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let id: i64 = row.try_get("id")?;
            let alert: serde_json::Value = row.try_get("alert")?;
            // One row an older or newer core wrote in a shape this one cannot read costs that row,
            // not the whole release pass.
            let Ok(alert) = serde_json::from_value::<Alert>(alert) else {
                if drop_unreadable {
                    tracing::warn!(id, "a held notification could not be read; dropping it");
                    HeldStore::remove(self, id).await?;
                }
                continue;
            };
            out.push(HeldRow {
                id,
                event: HeldEvent::from_stored(&row.try_get::<String, _>("event")?),
                alert,
                held_at: row.try_get("held_at")?,
                release_at: row.try_get("release_at")?,
            });
        }
        Ok(out)
    }
}

/// Leader-only loop: every 30 seconds, send what was held for every node whose hours have opened
/// (ADR-226 decision 7). Spawned by `LeaderTasks::spawn_refresh_loops`; runs until shutdown.
///
pub(crate) async fn run_release_loop(
    notifier: std::sync::Arc<super::Notifier>,
    store: std::sync::Arc<DeliveryHoursRepo>,
    alerts: std::sync::Arc<super::AlertManager>,
) {
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(30)).await;
        // The held rows, not the schedules: the inherent `list` is the other one.
        let rows = match HeldStore::list(store.as_ref()).await {
            Ok(rows) if rows.is_empty() => continue,
            Ok(rows) => rows,
            Err(e) => {
                tracing::warn!(error = %e, "failed to read held notifications");
                continue;
            }
        };
        let active = alerts.active_alerts();
        notifier.release(rows, &active, Utc::now()).await;
    }
}

#[cfg(test)]
pub(crate) mod testkit {
    //! An in-memory [`HeldStore`] for the notifier's tests.
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    pub(crate) struct MemHeld {
        pub(crate) rows: Mutex<Vec<HeldRow>>,
        next: Mutex<i64>,
    }

    #[async_trait::async_trait]
    impl HeldStore for MemHeld {
        async fn hold(
            &self,
            event: HeldEvent,
            alert: &Alert,
            release_at: Option<DateTime<Utc>>,
        ) -> anyhow::Result<()> {
            let mut next = self.next.lock().unwrap();
            *next += 1;
            self.rows.lock().unwrap().push(HeldRow {
                id: *next,
                event,
                alert: alert.clone(),
                held_at: Utc::now(),
                release_at,
            });
            Ok(())
        }
        async fn discard_fires(&self, subject: &Subject, check: CheckId) -> anyhow::Result<u64> {
            let mut rows = self.rows.lock().unwrap();
            let before = rows.len();
            rows.retain(|r| {
                !(r.event == HeldEvent::Fire
                    && r.alert.subject == *subject
                    && r.alert.check == check)
            });
            Ok((before - rows.len()) as u64)
        }
        async fn list(&self) -> anyhow::Result<Vec<HeldRow>> {
            Ok(self.rows.lock().unwrap().clone())
        }
        async fn remove(&self, id: i64) -> anyhow::Result<()> {
            self.rows.lock().unwrap().retain(|r| r.id != id);
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schedule(days: &[u8], start: u16, end: u16) -> DeliverySchedule {
        DeliverySchedule {
            id: Uuid::nil(),
            name: "office".to_owned(),
            enabled: true,
            days: days.to_vec(),
            start_minute: start,
            end_minute: end,
            group_ids: Vec::new(),
        }
    }

    const WEEKDAYS: &[u8] = &[1, 2, 3, 4, 5];
    /// 08:30 and 17:20, the hours the ADR was written for.
    const OPEN: u16 = 8 * 60 + 30;
    const CLOSE: u16 = 17 * 60 + 20;

    #[test]
    fn a_day_window_opens_at_its_start_and_closes_at_its_end() {
        let w = Window::of(&schedule(WEEKDAYS, OPEN, CLOSE));
        assert!(!w.open_at(1, OPEN - 1));
        assert!(w.open_at(1, OPEN));
        assert!(w.open_at(5, CLOSE - 1));
        assert!(!w.open_at(5, CLOSE), "the end minute is already closed");
        assert!(!w.open_at(6, 12 * 60), "Saturday is not listed");
    }

    #[test]
    fn a_window_past_midnight_belongs_to_the_day_it_opens() {
        let w = Window::of(&schedule(&[5], 22 * 60, 6 * 60));
        assert!(w.open_at(5, 23 * 60), "Friday evening");
        assert!(
            w.open_at(6, 5 * 60),
            "the small hours of Saturday, after Friday's opening"
        );
        assert!(!w.open_at(6, 23 * 60), "Saturday evening was not listed");
        assert!(
            !w.open_at(5, 5 * 60),
            "Friday's small hours belong to Thursday, not listed"
        );
    }

    #[test]
    fn equal_start_and_end_is_the_whole_day() {
        let w = Window::of(&schedule(&[7], 0, 0));
        assert!(w.open_at(7, 0) && w.open_at(7, 1439));
        assert!(!w.open_at(1, 0));
    }

    #[test]
    fn every_event_round_trips_through_its_token() {
        for e in [HeldEvent::Fire, HeldEvent::Resolve, HeldEvent::Suppress] {
            assert_eq!(HeldEvent::from_stored(e.as_str()), e);
        }
    }
}
