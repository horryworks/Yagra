// SPDX-License-Identifier: AGPL-3.0-only
//! When config backups are fetched (ADR-210 stage 6, decisions 58-68): every day from one wall
//! time in one zone (ADR-221), so many nodes at a time, oldest backup first, until every node has
//! been tried once.
//!
//! oxidized fetches nothing on its own any more (`interval: 0`, decision 58): it fetches a node only
//! when asked through `/node/next`. Before, it fetched a node a day after its last fetch, and since
//! it does not remember its fetches across a restart, every upgrade fetched the whole fleet again at
//! whatever time the upgrade happened.
//!
//! The judgement is [`plan`], pure and given the time: from the schedule and every node switched on
//! it says whether a run starts, which nodes to ask for now, or that the run is over. The leader
//! loop at the bottom ([`run_schedule_loop`]) acts on it.
//!
//! A run asks only for a node whose login is known to work (decision 61). A node still looking for
//! one belongs to the rotation (`rotation.rs`), which starts nothing while a fetch is out anywhere —
//! so it waits for the run (decision 67). A person's "fetch now" does not wait.
//!
//! What a run keeps is its start (decision 66): a node with an attempt after it is done, whatever
//! the outcome (decision 64), so a restart or a failover carries on where the run was. A fetch
//! oxidized never reports on is recorded lost by the rotation after `REQUEST_TIMEOUT`, which is
//! also what moves the run past it (decision 63).

use std::cmp::Ordering;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, TimeDelta, Utc};
use uuid::Uuid;
use yagra_base::config_backup::{BackupSchedule, BackupTarget, ConfigBackupRepo, ScheduleNode};
use yagra_base::secrets::CredentialStore;
use yagra_base::wall_clock;

use super::client::OxidizedClient;
use super::relay::Relay;
use super::rotation::{candidates, cli_logins, in_flight, settled, REQUEST_TIMEOUT};
use super::source::{readiness, Readiness};

/// How often the loop looks while a run is going: a fetch ends in seconds, and the next one should
/// not wait a minute for it.
const RUN_TICK: Duration = Duration::from_secs(10);
/// How often it looks otherwise: a run starts within a minute of its time.
const IDLE_TICK: Duration = Duration::from_secs(60);

/// One node switched on, as a run sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Candidate {
    pub node_id: Uuid,
    /// The node may be asked for: its login is known to work and it is ready to hand over.
    pub eligible: bool,
    /// A fetch Yagra asked for is out, whoever asked.
    pub in_flight: bool,
    pub last_attempt_at: Option<DateTime<Utc>>,
    pub last_success_at: Option<DateTime<Utc>>,
}

/// What to do now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Step {
    /// No run is going; the next one starts at `next_start`.
    Idle { next_start: DateTime<Utc> },
    /// Start a run now.
    Start,
    /// A run is going: ask for these nodes (possibly none, while the slots are full).
    Fetch(Vec<Uuid>),
    /// Every node of the run has been tried: mark it finished.
    Finish,
}

/// Where a run stands, for the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Progress {
    /// Nodes of the run tried since it started.
    pub done: usize,
    /// Nodes the run covers.
    pub total: usize,
}

/// The start instants on the dates around `t` in the schedule's zone (ADR-221), earliest first.
/// Four dates, yesterday to the day after tomorrow: "today" in a zone far from UTC can sit on
/// either side of `t`'s UTC date, and `next_start` needs one beyond it;
/// a daylight-saving gap or overlap resolves the way `yagra_base::wall_clock::resolve` says.
fn starts_around(s: &BackupSchedule, t: DateTime<Utc>) -> [DateTime<Utc>; 4] {
    let today = wall_clock::local_date(s.time_zone, t);
    let at = |d: i64| {
        wall_clock::resolve(
            s.time_zone,
            today + TimeDelta::days(d),
            u32::from(s.at_hour),
            u32::from(s.at_minute),
        )
    };
    [at(-1), at(0), at(1), at(2)]
}

/// The latest start instant at or before `now`.
#[must_use]
pub(crate) fn latest_start(s: &BackupSchedule, now: DateTime<Utc>) -> DateTime<Utc> {
    let [before, today, tomorrow, _] = starts_around(s, now);
    [tomorrow, today, before]
        .into_iter()
        .find(|t| *t <= now)
        .unwrap_or(before - TimeDelta::days(1))
}

/// The first start instant after `t`.
#[must_use]
pub(crate) fn next_start(s: &BackupSchedule, t: DateTime<Utc>) -> DateTime<Utc> {
    let around = starts_around(s, t);
    around
        .into_iter()
        .find(|x| *x > t)
        .unwrap_or(around[3] + TimeDelta::days(1))
}

/// Whether a run is going.
#[must_use]
pub(crate) fn running(s: &BackupSchedule) -> bool {
    s.run_started_at.is_some() && s.run_finished_at.is_none()
}

/// The last moment a start instant must come after to start a run: the latest run's start and end,
/// and the last change to the time — so a time set for earlier today first runs tomorrow, and a run
/// that ran past the next start skips that one (decision 65).
fn gate(s: &BackupSchedule) -> DateTime<Utc> {
    [s.run_started_at, s.run_finished_at, Some(s.updated_at)]
        .into_iter()
        .flatten()
        .max()
        .unwrap_or(DateTime::<Utc>::UNIX_EPOCH)
}

/// When the next run starts, while none is going.
#[must_use]
pub(crate) fn next_run_at(s: &BackupSchedule, now: DateTime<Utc>) -> DateTime<Utc> {
    let latest = latest_start(s, now);
    if latest > gate(s) {
        // Due now; the loop starts it within a tick.
        latest
    } else {
        next_start(s, now)
    }
}

/// Oldest backup first, a node never backed up before every other (decision 62); the id breaks a
/// tie so the order is the same on every tick.
fn oldest_first(a: &Candidate, b: &Candidate) -> Ordering {
    match (a.last_success_at, b.last_success_at) {
        (None, Some(_)) => Ordering::Less,
        (Some(_), None) => Ordering::Greater,
        (x, y) => x.cmp(&y),
    }
    .then(a.node_id.cmp(&b.node_id))
}

/// Where the run that started at `started` stands.
#[must_use]
pub(crate) fn progress(started: DateTime<Utc>, nodes: &[Candidate]) -> Progress {
    let covered = nodes.iter().filter(|n| n.eligible);
    let total = covered.clone().count();
    let done = covered
        .filter(|n| n.last_attempt_at.is_some_and(|t| t >= started))
        .count();
    Progress { done, total }
}

/// The step for now.
#[must_use]
pub(crate) fn plan(s: &BackupSchedule, nodes: &[Candidate], now: DateTime<Utc>) -> Step {
    let Some(started) = s.run_started_at.filter(|_| running(s)) else {
        return if latest_start(s, now) > gate(s) {
            Step::Start
        } else {
            Step::Idle {
                next_start: next_start(s, now),
            }
        };
    };
    let not_done = |n: &&Candidate| n.eligible && n.last_attempt_at.is_none_or(|t| t < started);
    let mut waiting: Vec<&Candidate> = nodes
        .iter()
        .filter(not_done)
        .filter(|n| !n.in_flight)
        .collect();
    let out_for_run = nodes.iter().filter(not_done).any(|n| n.in_flight);
    if waiting.is_empty() && !out_for_run {
        return Step::Finish;
    }
    // Every fetch out counts against the slots, the rotation's and a person's too: the point is
    // how many logins are open at once.
    let out = nodes.iter().filter(|n| n.in_flight).count();
    let slots = usize::from(s.concurrency.max(1)).saturating_sub(out);
    waiting.sort_by(|a, b| oldest_first(a, b));
    Step::Fetch(waiting.into_iter().take(slots).map(|n| n.node_id).collect())
}

/// Every node switched on as a run sees it, read from what the loop and the screen both read.
#[must_use]
pub(crate) fn candidates_of(
    targets: &[BackupTarget],
    nodes: &[ScheduleNode],
    logins: &[Uuid],
    relay: &Relay,
    now: DateTime<Utc>,
) -> Vec<Candidate> {
    let last: HashMap<Uuid, &ScheduleNode> = nodes.iter().map(|n| (n.node_id, n)).collect();
    targets
        .iter()
        .map(|t| {
            let cands = candidates(t.config.login_order.as_deref(), logins);
            let ready = readiness(
                Some(&t.config),
                t.profile_name.as_deref(),
                !cands.is_empty(),
                t.address_changed,
                &relay.route(t.node_id),
            ) == Readiness::Ready;
            let seen = last.get(&t.node_id);
            Candidate {
                node_id: t.node_id,
                eligible: ready && settled(&t.rotation, &cands),
                in_flight: in_flight(t.rotation.requested_at, now),
                last_attempt_at: seen.and_then(|n| n.last_attempt_at),
                last_success_at: seen.and_then(|n| n.last_success_at),
            }
        })
        .collect()
}

/// The schedule and every node as a run sees it, read now.
pub(crate) async fn read(
    repo: &ConfigBackupRepo,
    creds: &CredentialStore,
    relay: &Relay,
    now: DateTime<Utc>,
) -> anyhow::Result<(BackupSchedule, Vec<Candidate>, Vec<BackupTarget>)> {
    let (schedule, targets, nodes, logins) = tokio::try_join!(
        repo.schedule(),
        repo.targets(),
        repo.schedule_nodes(),
        cli_logins(creds)
    )?;
    let cands = candidates_of(&targets, &nodes, &logins, relay, now);
    Ok((schedule, cands, targets))
}

/// One look: act on [`plan`]. Answers whether a run is going, which sets how soon to look again.
async fn tick(
    client: &OxidizedClient,
    repo: &ConfigBackupRepo,
    creds: &CredentialStore,
    relay: &Relay,
) -> bool {
    let now = Utc::now();
    // Most of the day no run is going and none is due: that is decided from the one schedule row,
    // so every node is read only when a run starts or is going.
    match repo.schedule().await {
        Ok(s) if !running(&s) && latest_start(&s, now) <= gate(&s) => return false,
        Ok(_) => {}
        Err(e) => {
            tracing::warn!(error = %e, "config backup: the schedule could not read its state");
            return false;
        }
    }
    let (schedule, cands, targets) = match read(repo, creds, relay, now).await {
        Ok(read) => read,
        Err(e) => {
            tracing::warn!(error = %e, "config backup: the schedule could not read its state");
            return false;
        }
    };
    match plan(&schedule, &cands, now) {
        Step::Idle { .. } => false,
        Step::Start => {
            match repo.start_run(now, schedule.run_started_at).await {
                Ok(true) => {
                    let total = cands.iter().filter(|c| c.eligible).count();
                    tracing::info!(nodes = total, "config backup: a scheduled run started");
                }
                Ok(false) => {}
                Err(e) => tracing::warn!(error = %e, "config backup: a run could not start"),
            }
            true
        }
        Step::Finish => {
            if let Some(started) = schedule.run_started_at {
                let p = progress(started, &cands);
                match repo.finish_run(started, now).await {
                    Ok(()) => {
                        tracing::info!(nodes = p.done, "config backup: the scheduled run finished")
                    }
                    Err(e) => tracing::warn!(error = %e, "config backup: a run could not finish"),
                }
            }
            false
        }
        Step::Fetch(nodes) => {
            let logins: HashMap<Uuid, Uuid> = targets
                .iter()
                .filter_map(|t| t.rotation.credential_id.map(|c| (t.node_id, c)))
                .collect();
            for node in nodes {
                let Some(&credential) = logins.get(&node) else {
                    continue;
                };
                match repo
                    .dispatch(node, credential, now, now - REQUEST_TIMEOUT)
                    .await
                {
                    // The login is already in oxidized's list (a working one always is), so there
                    // is nothing to reload: ask for the fetch (decision 68).
                    Ok(true) => {
                        if let Err(e) = client.fetch_now(node).await {
                            tracing::warn!(%node, error = %e, "config backup: oxidized did not queue a scheduled fetch");
                            metrics::counter!("yagra_oxidized_errors_total", "op" => "next")
                                .increment(1);
                            if let Err(e) = repo.clear_request(node).await {
                                tracing::warn!(%node, error = %e, "config backup: could not withdraw a fetch request");
                            }
                        } else {
                            metrics::counter!("yagra_config_backup_scheduled_fetches_total")
                                .increment(1);
                        }
                    }
                    // A person's "fetch now" got there first.
                    Ok(false) => {}
                    Err(e) => {
                        tracing::warn!(%node, error = %e, "config backup: could not ask for a scheduled fetch");
                    }
                }
            }
            true
        }
    }
}

/// Runs until cancelled. Leader-only, for the reason `rotation.rs` gives: two cores would each ask
/// for the same node.
pub(crate) async fn run_schedule_loop(
    client: Arc<OxidizedClient>,
    repo: Arc<ConfigBackupRepo>,
    creds: Arc<CredentialStore>,
    relay: Arc<Relay>,
) {
    loop {
        let going = tick(&client, &repo, &creds, &relay).await;
        tokio::time::sleep(if going { RUN_TICK } else { IDLE_TICK }).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s)
            .expect("time")
            .with_timezone(&Utc)
    }

    /// 02:00 UTC, `concurrency` at a time, changed long ago.
    fn sched(concurrency: u8) -> BackupSchedule {
        BackupSchedule {
            concurrency,
            updated_at: at("2026-10-01T00:00:00Z"),
            ..BackupSchedule::default()
        }
    }

    fn running_since(concurrency: u8, started: &str) -> BackupSchedule {
        BackupSchedule {
            run_started_at: Some(at(started)),
            ..sched(concurrency)
        }
    }

    fn node(n: u128, last_success: Option<&str>) -> Candidate {
        Candidate {
            node_id: Uuid::from_u128(n),
            eligible: true,
            in_flight: false,
            last_attempt_at: last_success.map(at),
            last_success_at: last_success.map(at),
        }
    }

    fn ids(v: &[u128]) -> Vec<Uuid> {
        v.iter().map(|n| Uuid::from_u128(*n)).collect()
    }

    #[test]
    fn nothing_starts_before_the_time_and_a_run_starts_once_it_has_passed() {
        let s = BackupSchedule {
            run_started_at: Some(at("2026-10-29T02:00:10Z")),
            run_finished_at: Some(at("2026-10-29T03:00:00Z")),
            ..sched(1)
        };
        assert_eq!(
            plan(&s, &[node(1, None)], at("2026-10-30T01:59:00Z")),
            Step::Idle {
                next_start: at("2026-10-30T02:00:00Z")
            }
        );
        assert_eq!(
            plan(&s, &[node(1, None)], at("2026-10-30T02:00:30Z")),
            Step::Start
        );
        // A core that was down at 02:00 starts the run when it comes back.
        assert_eq!(
            plan(&s, &[node(1, None)], at("2026-10-30T09:00:00Z")),
            Step::Start
        );
        assert_eq!(
            next_run_at(&s, at("2026-10-30T01:00:00Z")),
            at("2026-10-30T02:00:00Z")
        );
    }

    #[test]
    fn the_start_time_is_a_wall_time_in_the_schedules_zone() {
        // 02:00 in Tokyo is 17:00Z the day before.
        let s = BackupSchedule {
            time_zone: yagra_base::wall_clock::Tz::Asia__Tokyo,
            ..sched(1)
        };
        assert_eq!(
            next_start(&s, at("2026-10-30T12:00:00Z")),
            at("2026-10-30T17:00:00Z")
        );
        assert_eq!(
            latest_start(&s, at("2026-10-30T12:00:00Z")),
            at("2026-10-29T17:00:00Z")
        );
        assert_eq!(
            latest_start(&s, at("2026-10-30T17:00:00Z")),
            at("2026-10-30T17:00:00Z")
        );
    }

    #[test]
    fn a_time_set_for_earlier_today_first_runs_tomorrow() {
        let s = BackupSchedule {
            updated_at: at("2026-10-30T10:30:00Z"),
            at_hour: 10,
            ..sched(1)
        };
        assert_eq!(
            plan(&s, &[node(1, None)], at("2026-10-30T10:31:00Z")),
            Step::Idle {
                next_start: at("2026-10-31T10:00:00Z")
            }
        );
        let later = BackupSchedule { at_hour: 11, ..s };
        assert_eq!(
            plan(&later, &[node(1, None)], at("2026-10-30T10:31:00Z")),
            Step::Idle {
                next_start: at("2026-10-30T11:00:00Z")
            }
        );
        assert_eq!(
            plan(&later, &[node(1, None)], at("2026-10-30T11:00:00Z")),
            Step::Start
        );
    }

    #[test]
    fn one_at_a_time_waits_for_the_fetch_out_and_three_fill_three_slots() {
        let now = at("2026-10-30T02:05:00Z");
        let mut nodes = vec![node(1, None), node(2, None), node(3, None), node(4, None)];
        assert_eq!(
            plan(&running_since(1, "2026-10-30T02:00:00Z"), &nodes, now),
            Step::Fetch(ids(&[1]))
        );
        nodes[0].in_flight = true;
        assert_eq!(
            plan(&running_since(1, "2026-10-30T02:00:00Z"), &nodes, now),
            Step::Fetch(vec![]),
            "the slot is taken until node 1 reports"
        );
        assert_eq!(
            plan(&running_since(3, "2026-10-30T02:00:00Z"), &nodes, now),
            Step::Fetch(ids(&[2, 3]))
        );
    }

    #[test]
    fn a_fetch_the_run_did_not_ask_for_takes_a_slot_too() {
        let now = at("2026-10-30T02:05:00Z");
        let looking = Candidate {
            eligible: false,
            in_flight: true,
            ..node(9, None)
        };
        assert_eq!(
            plan(
                &running_since(1, "2026-10-30T02:00:00Z"),
                &[node(1, None), looking],
                now
            ),
            Step::Fetch(vec![])
        );
    }

    #[test]
    fn the_oldest_backup_goes_first_and_one_never_backed_up_before_all() {
        let now = at("2026-10-30T02:05:00Z");
        let nodes = [
            node(1, Some("2026-10-28T02:00:00Z")),
            node(2, Some("2026-10-27T02:00:00Z")),
            node(3, None),
            node(4, Some("2026-10-29T02:00:00Z")),
        ];
        assert_eq!(
            plan(&running_since(8, "2026-10-30T02:00:00Z"), &nodes, now),
            Step::Fetch(ids(&[3, 2, 1, 4]))
        );
    }

    #[test]
    fn a_node_tried_since_the_start_is_done_whatever_came_of_it() {
        let started = "2026-10-30T02:00:00Z";
        let now = at("2026-10-30T02:30:00Z");
        // Node 1 failed at 02:10: tried, so not asked for again in this run.
        let failed = Candidate {
            last_attempt_at: Some(at("2026-10-30T02:10:00Z")),
            ..node(1, Some("2026-10-29T02:00:00Z"))
        };
        let nodes = [failed.clone(), node(2, Some("2026-10-30T02:20:00Z"))];
        assert_eq!(plan(&running_since(1, started), &nodes, now), Step::Finish);
        assert_eq!(
            progress(at(started), &nodes),
            Progress { done: 2, total: 2 }
        );
        // After a restart the same rows answer the same: the run carries on, never starts over.
        let more = [failed, node(2, None), node(3, Some("2026-10-29T03:00:00Z"))];
        assert_eq!(
            plan(&running_since(1, started), &more, now),
            Step::Fetch(ids(&[2]))
        );
        assert_eq!(progress(at(started), &more), Progress { done: 1, total: 3 });
    }

    #[test]
    fn a_run_is_not_over_while_one_of_its_fetches_is_out() {
        let out = Candidate {
            in_flight: true,
            ..node(1, None)
        };
        assert_eq!(
            plan(
                &running_since(1, "2026-10-30T02:00:00Z"),
                &[out],
                at("2026-10-30T02:05:00Z")
            ),
            Step::Fetch(vec![])
        );
    }

    #[test]
    fn a_node_still_looking_for_a_login_is_not_in_the_run() {
        let looking = Candidate {
            eligible: false,
            ..node(1, None)
        };
        let s = running_since(1, "2026-10-30T02:00:00Z");
        assert_eq!(
            plan(
                &s,
                std::slice::from_ref(&looking),
                at("2026-10-30T02:05:00Z")
            ),
            Step::Finish
        );
        assert_eq!(
            progress(at("2026-10-30T02:00:00Z"), &[looking]),
            Progress { done: 0, total: 0 }
        );
    }

    #[test]
    fn a_run_that_ran_past_the_next_start_skips_it() {
        // Started yesterday 02:00, finished today 03:00: today's 02:00 is not run.
        let s = BackupSchedule {
            run_started_at: Some(at("2026-10-29T02:00:00Z")),
            run_finished_at: Some(at("2026-10-30T03:00:00Z")),
            ..sched(1)
        };
        assert_eq!(
            plan(&s, &[node(1, None)], at("2026-10-30T03:01:00Z")),
            Step::Idle {
                next_start: at("2026-10-31T02:00:00Z")
            }
        );
        // And while it is still going, the new start instant does not start another.
        let going = running_since(1, "2026-10-29T02:00:00Z");
        assert_eq!(
            plan(&going, &[node(1, None)], at("2026-10-30T02:30:00Z")),
            Step::Fetch(ids(&[1]))
        );
    }
}
