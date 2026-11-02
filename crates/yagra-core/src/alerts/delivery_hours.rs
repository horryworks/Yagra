// SPDX-License-Identifier: AGPL-3.0-only
//! Notifications reach people only inside the hours a site keeps (ADR-226).
//!
//! Three things live here, and only the third touches a store:
//!
//! - **What a schedule says** ([`DeliverySchedule`], [`Window`]): weekdays plus a wall-clock span,
//!   read in the local time of the node an alert is about. A schedule carries no zone of its own,
//!   so one "weekdays 08:30-17:20" serves a site in Tokyo and one in New York (decision 3).
//! - **Which nodes it applies to, and in which zone** ([`build_policy`], [`HoursPolicy`]): a node is
//!   covered by a schedule naming a folder above it or a label it carries (decision 4), and its
//!   zone is the nearest folder's, else the deployment's default (decision 5). A node no schedule
//!   covers is always open, so a deployment with no schedule behaves exactly as before.
//! - **The notifications held while a node is closed** ([`DeliveryHoursRepo`] as a [`HeldStore`]),
//!   kept in PostgreSQL so a restart does not lose the morning's deliveries (decision 7).
//!
//! The alert itself is never touched: state, screens and history are what they would be without
//! this module. Only the notifier asks it anything (decision 1).

use chrono::{DateTime, Datelike, Duration, Timelike, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Row};
use std::collections::{BTreeSet, HashMap, HashSet};
use uuid::Uuid;
use yagra_alert::{Alert, Subject};
use yagra_base::wall_clock::Tz;
use yagra_common::{CheckId, NodeId};

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
    /// Labels it applies to, inherited from folders as every label is (ADR-135).
    pub labels: Vec<String>,
}

/// What [`DeliveryHoursRepo::create`] and [`DeliveryHoursRepo::update`] write: a schedule without
/// its id. Validated by the API edge before it gets here.
#[derive(Debug, Clone)]
pub struct ScheduleFields<'a> {
    pub name: &'a str,
    pub enabled: bool,
    pub days: &'a [u8],
    pub start_minute: u16,
    pub end_minute: u16,
    pub group_ids: &'a [Uuid],
    pub labels: &'a [String],
}

/// A schedule's span, ready to be asked about one local time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    /// Bit `d - 1` is ISO weekday `d`.
    days: u8,
    start: u16,
    end: u16,
}

impl Window {
    #[must_use]
    pub fn of(schedule: &DeliverySchedule) -> Self {
        let days = schedule
            .days
            .iter()
            .filter(|d| (1..=7).contains(*d))
            .fold(0u8, |mask, d| mask | (1 << (d - 1)));
        Self {
            days,
            start: schedule.start_minute.min(1439),
            end: schedule.end_minute.min(1439),
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

/// The hours one node keeps: its zone and every window a schedule gives it.
#[derive(Debug, Clone, PartialEq)]
pub struct NodeHours {
    pub zone: Tz,
    pub windows: Vec<Window>,
}

/// How far ahead [`NodeHours::next_open`] looks. Every window opens on at least one weekday, so a
/// week and a day always finds one.
const LOOKAHEAD_MINUTES: i64 = 8 * 24 * 60;

impl NodeHours {
    /// Whether any window is open at `now`, read in this node's zone. Several schedules on one
    /// node are a union: one saying "open" is enough (decision 4).
    #[must_use]
    pub fn is_open(&self, now: DateTime<Utc>) -> bool {
        let local = now.with_timezone(&self.zone);
        // `number_from_monday` is 1..=7 and the minute of the day is below 1440, so both fit.
        let day = u8::try_from(local.weekday().number_from_monday()).unwrap_or(1);
        let minute = u16::try_from(local.hour() * 60 + local.minute()).unwrap_or(0);
        self.windows.iter().any(|w| w.open_at(day, minute))
    }

    /// The first minute at or after `now` this node is open, or `None` if it never is.
    ///
    /// A minute at a time, through the zone, so a daylight-saving change is honoured without a
    /// rule of its own. At most [`LOOKAHEAD_MINUTES`] steps, and only for a held notification.
    #[must_use]
    pub fn next_open(&self, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
        if self.is_open(now) {
            return Some(now);
        }
        let start = now
            .with_second(0)
            .and_then(|t| t.with_nanosecond(0))
            .unwrap_or(now);
        (1..=LOOKAHEAD_MINUTES)
            .map(|m| start + Duration::minutes(m))
            .find(|t| self.is_open(*t))
    }
}

/// Every node some schedule covers, with the hours it keeps. A node that is absent is open.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HoursPolicy {
    nodes: HashMap<NodeId, NodeHours>,
}

impl HoursPolicy {
    /// Whether nothing is covered, which is every deployment with no enabled schedule.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Whether a notification about `node` may be delivered at `now`. An alert with no node (a
    /// Meraki organization, Yagra itself) and a node no schedule covers are always open
    /// (decision 13).
    #[must_use]
    pub fn is_open(&self, node: Option<NodeId>, now: DateTime<Utc>) -> bool {
        node.and_then(|n| self.nodes.get(&n))
            .is_none_or(|h| h.is_open(now))
    }

    /// When a notification about `node` held at `now` is expected to go out — display only.
    #[must_use]
    pub fn next_open(&self, node: Option<NodeId>, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
        match node.and_then(|n| self.nodes.get(&n)) {
            Some(h) => h.next_open(now),
            None => Some(now),
        }
    }
}

/// One node as [`build_policy`] needs it: where it is filed and every label it effectively carries.
#[derive(Debug, Clone)]
pub struct NodeTarget {
    pub id: NodeId,
    pub group: Option<Uuid>,
    pub labels: BTreeSet<String>,
}

/// Which nodes each enabled schedule covers, and the zone each covered node keeps time in.
///
/// `folders` is `(group, parent, own zone)` for every folder (`GroupRepo::time_zone_rows`). A zone
/// name this build does not know falls to `default_zone` rather than leaving the node always open.
#[must_use]
pub fn build_policy(
    schedules: &[DeliverySchedule],
    nodes: &[NodeTarget],
    folders: &[(Uuid, Option<Uuid>, Option<String>)],
    default_zone: Tz,
) -> HoursPolicy {
    let enabled: Vec<&DeliverySchedule> = schedules
        .iter()
        .filter(|s| s.enabled && !s.days.is_empty())
        .collect();
    if enabled.is_empty() {
        return HoursPolicy::default();
    }
    let edges: Vec<(Uuid, Option<Uuid>)> = folders.iter().map(|(id, p, _)| (*id, *p)).collect();
    // Per schedule, every folder it reaches: the named ones and everything beneath them.
    let reach: Vec<(Window, HashSet<Uuid>, &[String])> = enabled
        .iter()
        .map(|s| {
            let folders: HashSet<Uuid> = s
                .group_ids
                .iter()
                .flat_map(|g| yagra_base::groups::group_subtree(&edges, *g))
                .collect();
            (Window::of(s), folders, s.labels.as_slice())
        })
        .collect();
    let zones = yagra_base::groups::resolve_nearest_ancestor(folders.iter().cloned());
    let mut out = HashMap::new();
    for node in nodes {
        let windows: Vec<Window> = reach
            .iter()
            .filter(|(_, folders, labels)| {
                node.group.is_some_and(|g| folders.contains(&g))
                    || labels.iter().any(|l| node.labels.contains(l))
            })
            .map(|(w, _, _)| *w)
            .collect();
        if windows.is_empty() {
            continue;
        }
        let zone = node
            .group
            .and_then(|g| zones.get(&g))
            .and_then(|(name, _)| yagra_base::wall_clock::parse_zone(name))
            .unwrap_or(default_zone);
        out.insert(node.id, NodeHours { zone, windows });
    }
    HoursPolicy { nodes: out }
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
            "SELECT id, name, enabled, days, start_minute, end_minute, group_ids, labels \
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
                    labels: row.try_get("labels")?,
                })
            })
            .collect()
    }

    /// Store a new schedule and return its id.
    pub async fn create(&self, f: &ScheduleFields<'_>) -> anyhow::Result<Uuid> {
        let id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO delivery_hours \
               (id, name, enabled, days, start_minute, end_minute, group_ids, labels) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(id)
        .bind(f.name)
        .bind(f.enabled)
        .bind(days_column(f.days))
        .bind(i16::try_from(f.start_minute)?)
        .bind(i16::try_from(f.end_minute)?)
        .bind(f.group_ids)
        .bind(f.labels)
        .execute(&self.pool)
        .await?;
        Ok(id)
    }

    /// Replace a schedule. Returns whether it existed.
    pub async fn update(&self, id: Uuid, f: &ScheduleFields<'_>) -> anyhow::Result<bool> {
        let res = sqlx::query(
            "UPDATE delivery_hours SET name = $2, enabled = $3, days = $4, start_minute = $5, \
                    end_minute = $6, group_ids = $7, labels = $8 \
             WHERE id = $1",
        )
        .bind(id)
        .bind(f.name)
        .bind(f.enabled)
        .bind(days_column(f.days))
        .bind(i16::try_from(f.start_minute)?)
        .bind(i16::try_from(f.end_minute)?)
        .bind(f.group_ids)
        .bind(f.labels)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected() > 0)
    }

    /// Delete a schedule. Returns whether it existed. What it was holding goes out on the next
    /// release pass, because the nodes it covered are open again.
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
fn days_column(days: &[u8]) -> Vec<i16> {
    days.iter()
        .copied()
        .collect::<BTreeSet<u8>>()
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
                tracing::warn!(id, "a held notification could not be read; dropping it");
                self.remove(id).await?;
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

    async fn remove(&self, id: i64) -> anyhow::Result<()> {
        sqlx::query("DELETE FROM held_notifications WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}

/// Leader-only loop: every 30 seconds, send what was held for every node whose hours have opened
/// (ADR-226 decision 7). Spawned by `LeaderTasks::spawn_refresh_loops`; runs until shutdown.
///
/// The acks are read only when something is held, and a pass whose ack read fails sends nothing:
/// paging an alert someone already acknowledged is the mistake decision 9 exists to avoid, and
/// the next pass is 30 seconds away.
pub(crate) async fn run_release_loop(
    notifier: std::sync::Arc<super::Notifier>,
    store: std::sync::Arc<DeliveryHoursRepo>,
    alerts: std::sync::Arc<super::AlertManager>,
    acks: std::sync::Arc<super::ack::AckRepo>,
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
        let acked = match acks.all().await {
            Ok(a) => a,
            Err(e) => {
                tracing::warn!(error = %e, "failed to read acks; holding notifications a while longer");
                continue;
            }
        };
        let is_acked = |a: &Alert| {
            a.node().is_some_and(|n| {
                acked.contains_key(&(
                    n.as_uuid(),
                    a.check.as_uuid(),
                    a.severity.as_str().to_owned(),
                ))
            })
        };
        let active = alerts.active_alerts();
        notifier.release(rows, &active, &is_acked, Utc::now()).await;
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
    use chrono::TimeZone;

    fn schedule(days: &[u8], start: u16, end: u16) -> DeliverySchedule {
        DeliverySchedule {
            id: Uuid::nil(),
            name: "office".to_owned(),
            enabled: true,
            days: days.to_vec(),
            start_minute: start,
            end_minute: end,
            group_ids: Vec::new(),
            labels: Vec::new(),
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

    fn tokyo() -> Tz {
        yagra_base::wall_clock::parse_zone("Asia/Tokyo").unwrap()
    }

    #[test]
    fn a_node_keeps_time_in_its_own_zone() {
        let hours = NodeHours {
            zone: tokyo(),
            windows: vec![Window::of(&schedule(WEEKDAYS, OPEN, CLOSE))],
        };
        // Monday 2026-11-02 00:00 UTC is 09:00 in Tokyo: open there, though closed in UTC.
        let monday_9_tokyo = Utc.with_ymd_and_hms(2026, 11, 2, 0, 0, 0).unwrap();
        assert!(hours.is_open(monday_9_tokyo));
        // 09:00 UTC is 18:00 in Tokyo: closed.
        let monday_18_tokyo = Utc.with_ymd_and_hms(2026, 11, 2, 9, 0, 0).unwrap();
        assert!(!hours.is_open(monday_18_tokyo));
        // The next opening is Tuesday 08:30 Tokyo = Monday 23:30 UTC.
        assert_eq!(
            hours.next_open(monday_18_tokyo),
            Some(Utc.with_ymd_and_hms(2026, 11, 2, 23, 30, 0).unwrap())
        );
    }

    #[test]
    fn a_friday_evening_waits_for_monday_morning() {
        let hours = NodeHours {
            zone: tokyo(),
            windows: vec![Window::of(&schedule(WEEKDAYS, OPEN, CLOSE))],
        };
        // Friday 2026-11-06 20:00 Tokyo = 11:00 UTC.
        let friday_night = Utc.with_ymd_and_hms(2026, 11, 6, 11, 0, 0).unwrap();
        // Monday 2026-11-09 08:30 Tokyo = Sunday 23:30 UTC.
        assert_eq!(
            hours.next_open(friday_night),
            Some(Utc.with_ymd_and_hms(2026, 11, 8, 23, 30, 0).unwrap())
        );
    }

    #[test]
    fn daylight_saving_moves_the_utc_instant_not_the_wall_time() {
        let ny = yagra_base::wall_clock::parse_zone("America/New_York").unwrap();
        let hours = NodeHours {
            zone: ny,
            windows: vec![Window::of(&schedule(WEEKDAYS, OPEN, CLOSE))],
        };
        // 08:30 New York is 12:30 UTC in summer (EDT) and 13:30 UTC in winter (EST).
        assert!(hours.is_open(Utc.with_ymd_and_hms(2026, 7, 6, 12, 30, 0).unwrap()));
        assert!(!hours.is_open(Utc.with_ymd_and_hms(2026, 12, 7, 12, 30, 0).unwrap()));
        assert!(hours.is_open(Utc.with_ymd_and_hms(2026, 12, 7, 13, 30, 0).unwrap()));
    }

    fn node(n: u128, group: Option<u128>, labels: &[&str]) -> NodeTarget {
        NodeTarget {
            id: NodeId::from(Uuid::from_u128(n)),
            group: group.map(Uuid::from_u128),
            labels: labels.iter().map(|l| (*l).to_owned()).collect(),
        }
    }

    #[test]
    fn a_schedule_covers_its_folders_subtree_and_its_labels_and_nothing_else() {
        let site = Uuid::from_u128(10);
        let floor = Uuid::from_u128(11);
        let other = Uuid::from_u128(12);
        let folders = vec![
            (site, None, Some("Asia/Tokyo".to_owned())),
            (floor, Some(site), None),
            (other, None, None),
        ];
        let mut by_folder = schedule(WEEKDAYS, OPEN, CLOSE);
        by_folder.group_ids = vec![site];
        let mut by_label = schedule(WEEKDAYS, OPEN, CLOSE);
        by_label.labels = vec!["office-hours".to_owned()];
        let nodes = vec![
            node(1, Some(10), &[]),
            node(2, Some(11), &[]),
            node(3, Some(12), &["office-hours"]),
            node(4, Some(12), &[]),
        ];
        let policy = build_policy(&[by_folder, by_label], &nodes, &folders, Tz::UTC);
        let covered = |n: u128| policy.nodes.get(&NodeId::from(Uuid::from_u128(n)));
        assert_eq!(covered(1).map(|h| h.zone), Some(tokyo()));
        assert_eq!(
            covered(2).map(|h| h.zone),
            Some(tokyo()),
            "the floor inherits the site's zone"
        );
        assert_eq!(
            covered(3).map(|h| h.zone),
            Some(Tz::UTC),
            "no zoned folder: the default"
        );
        assert!(
            covered(4).is_none(),
            "nothing names node 4, so it is always open"
        );
        let anytime = Utc.with_ymd_and_hms(2026, 11, 7, 3, 0, 0).unwrap();
        assert!(policy.is_open(Some(NodeId::from(Uuid::from_u128(4))), anytime));
        assert!(
            policy.is_open(None, anytime),
            "an alert about no node is never held"
        );
    }

    #[test]
    fn two_schedules_on_one_node_open_it_when_either_is_open() {
        let mut day = schedule(WEEKDAYS, OPEN, CLOSE);
        day.labels = vec!["a".to_owned()];
        let mut weekend = schedule(&[6, 7], 0, 0);
        weekend.labels = vec!["b".to_owned()];
        let nodes = vec![node(1, None, &["a", "b"])];
        let policy = build_policy(&[day, weekend], &nodes, &[], Tz::UTC);
        let saturday_3am = Utc.with_ymd_and_hms(2026, 11, 7, 3, 0, 0).unwrap();
        assert!(policy.is_open(Some(NodeId::from(Uuid::from_u128(1))), saturday_3am));
        let monday_3am = Utc.with_ymd_and_hms(2026, 11, 9, 3, 0, 0).unwrap();
        assert!(!policy.is_open(Some(NodeId::from(Uuid::from_u128(1))), monday_3am));
    }

    #[test]
    fn a_switched_off_schedule_covers_nothing() {
        let mut s = schedule(WEEKDAYS, OPEN, CLOSE);
        s.enabled = false;
        s.labels = vec!["a".to_owned()];
        let policy = build_policy(&[s], &[node(1, None, &["a"])], &[], Tz::UTC);
        assert!(policy.is_empty());
    }

    #[test]
    fn every_event_round_trips_through_its_token() {
        for e in [HeldEvent::Fire, HeldEvent::Resolve, HeldEvent::Suppress] {
            assert_eq!(HeldEvent::from_stored(e.as_str()), e);
        }
    }
}
