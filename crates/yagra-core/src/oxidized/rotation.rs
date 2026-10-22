// SPDX-License-Identifier: AGPL-3.0-only
//! Trying a node's CLI logins one after another and remembering the one that works, without
//! locking anything out (ADR-210 stage 3, decisions 17-23).
//!
//! The judgement is [`next_step`], which is pure and takes the time as an argument: given a node's
//! candidates, where it is in its pass through them and every recent attempt, it says which login
//! to try now, or until when to wait. The leader loop at the bottom ([`run_rotation_loop`]) acts on
//! it and waits; `POST …/config-backup/fetch` asks it which login is next and tries that one at
//! once ([`login_to_force`]) — a person's press is not held back by the limits (decision 25), and
//! what it spends counts against the loop's.
//!
//! The limits are a node's scan mode's ([`limits_of`], the user's three speeds, decision 27): how
//! long after a failure the next try waits, how many failed logins one device may see in its
//! window, how many failures of one login the whole fleet may spend in an hour — that one protects
//! an account a TACACS+ or RADIUS server shares between many devices, so it is the strictest mode
//! among the nodes that try the login ([`login_caps`]) — and how long a device that does not answer
//! is dialled before the node gives up. A candidate a limit holds back is waited for, never
//! skipped: skipping it would spend another failure at the same device on a login that is further
//! down the list.
//!
//! Only Yagra starts a login with a candidate nobody has confirmed: such a node is in oxidized's
//! node list only while a fetch Yagra asked for is outstanding (`source::source_nodes`), so
//! oxidized's own daily schedule cannot spend an attempt the limits did not allow.
//!
//! 🚨 **oxidized's node list is never re-read while a fetch is out** ([`may_reload`]). oxidized
//! 0.37 rebuilds every node object on a reload and does not carry the running mark across
//! (`Nodes#update_nodes` copies `last` and `stats` only), and `/node/next` has already set `last` to
//! nothing — so a node being fetched reads as never fetched and a second job starts with whatever
//! login the list now holds. Measured on a lab deployment: one fetch-now, two logins, and the
//! second one's result recorded against the wrong login. So every reload goes through
//! [`may_reload`], and this loop starts its batch only when nothing is out.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use uuid::Uuid;
use yagra_base::config_backup::{Attempt, AttemptOutcome, ConfigBackupRepo, Rotation, ScanMode};
use yagra_base::secrets::{CredentialStore, KIND_CLI_LOGIN};

use super::client::OxidizedClient;
use super::source::{readiness, Readiness};

/// The window the fleet-wide limit on one login counts over, in every mode.
pub(crate) const WINDOW: chrono::TimeDelta = chrono::TimeDelta::hours(1);

/// One scan mode's limits (decision 27).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Limits {
    /// The least time between a failed login and the next try at the same device.
    pub gap: chrono::TimeDelta,
    /// Failed logins one device may see in [`Limits::node_window`].
    pub node_cap: usize,
    pub node_window: chrono::TimeDelta,
    /// Failures of one login across the fleet in [`WINDOW`].
    pub login_cap: usize,
    /// How long a device that did not answer is left before the same login is tried again. No
    /// login was attempted, so nothing is counted; this only keeps a dead device from being dialled
    /// all the time.
    pub unreachable_retry: chrono::TimeDelta,
    /// How long a device may go on not answering before the node stops trying.
    pub give_up: chrono::TimeDelta,
}

/// The limits of a scan mode. A mode this core does not know is held to the most careful one.
#[must_use]
pub(crate) const fn limits_of(mode: ScanMode) -> Limits {
    use chrono::TimeDelta as D;
    match mode {
        ScanMode::Rapid => Limits {
            gap: D::minutes(1),
            node_cap: 10,
            node_window: D::hours(1),
            login_cap: 30,
            unreachable_retry: D::minutes(10),
            give_up: D::hours(6),
        },
        ScanMode::Normal => Limits {
            gap: D::minutes(10),
            node_cap: 3,
            node_window: D::hours(1),
            login_cap: 5,
            unreachable_retry: D::hours(1),
            give_up: D::hours(24),
        },
        ScanMode::Slow | ScanMode::Unknown => Limits {
            gap: D::hours(1),
            node_cap: 3,
            node_window: D::hours(24),
            login_cap: 2,
            unreachable_retry: D::hours(6),
            give_up: D::hours(72),
        },
    }
}

/// The fleet-wide cap on each login: the smallest of the caps of every node that has it among its
/// candidates, so a login shared by a rapid node and a slow one is held to the slow one's.
#[must_use]
pub(crate) fn login_caps(nodes: &[(Vec<Uuid>, ScanMode)]) -> HashMap<Uuid, usize> {
    let mut caps: HashMap<Uuid, usize> = HashMap::new();
    for (cands, mode) in nodes {
        let cap = limits_of(*mode).login_cap;
        for c in cands {
            caps.entry(*c)
                .and_modify(|v| *v = (*v).min(cap))
                .or_insert(cap);
        }
    }
    caps
}
/// How long a requested fetch may stay unanswered before it is counted as [`AttemptOutcome::Lost`].
/// oxidized's own time limit for one fetch is five minutes (`config::render`'s `timelimit`).
pub(crate) const REQUEST_TIMEOUT: chrono::TimeDelta = chrono::TimeDelta::minutes(10);

/// How often the leader looks for a node whose next login is due.
const TICK: Duration = Duration::from_secs(60);
/// Ticks between two prunes of old attempts.
const PRUNE_EVERY: u32 = 60;

/// The logins a node tries, in order: its own order when a person saved one (a login since deleted
/// or no longer a CLI login dropped), otherwise every CLI login in the try order.
#[must_use]
pub(crate) fn candidates(order: Option<&[Uuid]>, cli_logins: &[Uuid]) -> Vec<Uuid> {
    match order {
        Some(order) => {
            let mut seen = HashSet::new();
            order
                .iter()
                .copied()
                .filter(|id| cli_logins.contains(id) && seen.insert(*id))
                .collect()
        }
        None => cli_logins.to_vec(),
    }
}

/// What a node should do now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Step {
    /// Hand this login to oxidized and fetch.
    Fetch(Uuid),
    /// Try this login, but not before then.
    Wait {
        credential: Uuid,
        until: DateTime<Utc>,
    },
    /// Every candidate failed in this pass. Nothing is tried until the pass is restarted.
    Exhausted,
    /// There is no CLI login to try.
    NoCandidate,
}

/// When `failures` (ascending by time, all within `window` of now) next leave room under `limit`:
/// the moment the oldest one that keeps it full leaves the window.
fn room_at(
    failures: &[DateTime<Utc>],
    limit: usize,
    window: chrono::TimeDelta,
) -> Option<DateTime<Utc>> {
    if failures.len() < limit {
        return None;
    }
    Some(failures[failures.len() - limit] + window)
}

/// The earliest moment `credential` may be tried at this node, under the node's `limits` and the
/// login's fleet-wide `login_cap`.
fn allowed_at(
    credential: Uuid,
    limits: &Limits,
    login_cap: usize,
    node_attempts: &[Attempt],
    fleet_recent: &[Attempt],
    now: DateTime<Utc>,
) -> DateTime<Utc> {
    let node_since = now - limits.node_window;
    let mut at_node: Vec<DateTime<Utc>> = node_attempts
        .iter()
        .filter(|a| a.outcome.is_login_failure() && a.at > node_since)
        .map(|a| a.at)
        .collect();
    at_node.sort();
    let since = now - WINDOW;
    let mut of_login: Vec<DateTime<Utc>> = fleet_recent
        .iter()
        .filter(|a| {
            a.credential_id == Some(credential) && a.outcome.is_login_failure() && a.at > since
        })
        .map(|a| a.at)
        .collect();
    of_login.sort();
    [
        Some(now),
        at_node.last().map(|t| *t + limits.gap),
        room_at(&at_node, limits.node_cap, limits.node_window),
        room_at(&of_login, login_cap, WINDOW),
    ]
    .into_iter()
    .flatten()
    .max()
    .unwrap_or(now)
}

/// Which login a node tries next, and when.
///
/// - `candidates`: from [`candidates`].
/// - `rotation`: the node's row.
/// - `node_attempts`: the node's own attempts, newest first, reaching back at least to the start of
///   its pass.
/// - `fleet_recent`: every attempt in the fleet in the last [`WINDOW`], the node's included.
/// - `limits`: the node's scan mode's; `caps`: each login's fleet-wide cap ([`login_caps`]), the
///   node's own `login_cap` for a login missing from it.
///
/// A login remembered as working is tried again as long as it is still a candidate. Otherwise the
/// first candidate that has not failed since the pass began is tried — after a device that did not
/// answer, the same one again after `limits.unreachable_retry`, until it has not answered for
/// `limits.give_up`, when the node stops as if every login had failed.
#[must_use]
pub(crate) fn next_step(
    candidates: &[Uuid],
    rotation: &Rotation,
    node_attempts: &[Attempt],
    fleet_recent: &[Attempt],
    limits: &Limits,
    caps: &HashMap<Uuid, usize>,
    now: DateTime<Utc>,
) -> Step {
    if candidates.is_empty() {
        return Step::NoCandidate;
    }
    let decide = |credential: Uuid, not_before: Option<DateTime<Utc>>| {
        let cap = caps.get(&credential).copied().unwrap_or(limits.login_cap);
        let until = allowed_at(credential, limits, cap, node_attempts, fleet_recent, now)
            .max(not_before.unwrap_or(now));
        if until <= now {
            Step::Fetch(credential)
        } else {
            Step::Wait { credential, until }
        }
    };
    if rotation.login_confirmed {
        if let Some(c) = rotation.credential_id.filter(|c| candidates.contains(c)) {
            return decide(c, None);
        }
    }
    let round = rotation.round_started_at;
    let in_round: Vec<&Attempt> = node_attempts
        .iter()
        .filter(|a| round.is_none_or(|r| a.at >= r))
        .collect();
    if let Some(last) = in_round.first() {
        if last.outcome == AttemptOutcome::Unreachable {
            // The oldest of the run of "did not answer" this pass ends with.
            let since = in_round
                .iter()
                .take_while(|a| a.outcome == AttemptOutcome::Unreachable)
                .last()
                .map_or(last.at, |a| a.at);
            if now - since >= limits.give_up {
                return Step::Exhausted;
            }
            if let Some(c) = last.credential_id.filter(|c| candidates.contains(c)) {
                return decide(c, Some(last.at + limits.unreachable_retry));
            }
        }
    }
    let tried: HashSet<Uuid> = in_round
        .iter()
        .filter(|a| a.outcome.is_login_failure())
        .filter_map(|a| a.credential_id)
        .collect();
    match candidates.iter().find(|c| !tried.contains(c)) {
        Some(c) => decide(*c, None),
        None => Step::Exhausted,
    }
}

/// The login a person's "fetch now" tries: the one [`next_step`] would try, now, whatever the
/// limits say (stage 3 decision 25, the user's: "try at once, forced"). The limits hold back only
/// the leader's automatic rotation; the forced attempt is recorded like any other and counts
/// against it. `None` when there is nothing to try.
#[must_use]
pub(crate) const fn login_to_force(step: Step) -> Option<Uuid> {
    match step {
        Step::Fetch(credential) | Step::Wait { credential, .. } => Some(credential),
        Step::Exhausted | Step::NoCandidate => None,
    }
}

/// How an oxidized hook event ended, from its `OX_EVENT` and `OX_ERR_TYPE`. `None` for an event
/// that is not the end of a fetch.
///
/// The error types were measured on a lab deployment against an SSH server and a telnet server
/// with a wrong password, an address nothing answers on, and a closed port:
/// `Net::SSH::AuthenticationFailed`, `Oxidized::PromptUndetect` (telnet: the device only asks for
/// the login again), `Net::SSH::ConnectionTimeout` and `Errno::ECONNREFUSED`.
#[must_use]
pub(crate) fn classify(event: &str, err_type: &str) -> Option<AttemptOutcome> {
    match event {
        "node_success" => Some(AttemptOutcome::Success),
        "node_fail" => Some(match err_type.trim() {
            t if t.ends_with("AuthenticationFailed") => AttemptOutcome::AuthFailed,
            "Net::SSH::ConnectionTimeout"
            | "Errno::ECONNREFUSED"
            | "Errno::EHOSTUNREACH"
            | "Errno::ENETUNREACH" => AttemptOutcome::Unreachable,
            _ => AttemptOutcome::Other,
        }),
        _ => None,
    }
}

/// The CLI logins, in the try order of Credentials & secrets.
pub(crate) async fn cli_logins(creds: &CredentialStore) -> anyhow::Result<Vec<Uuid>> {
    Ok(creds
        .list()
        .await?
        .into_iter()
        .filter(|c| c.kind == KIND_CLI_LOGIN)
        .map(|c| c.id)
        .collect())
}

/// Whether a fetch Yagra asked for at `requested_at` is still outstanding.
#[must_use]
pub(crate) fn in_flight(requested_at: Option<DateTime<Utc>>, now: DateTime<Utc>) -> bool {
    requested_at.is_some_and(|t| t > now - REQUEST_TIMEOUT)
}

/// What one node's next step is, read from the stores. `None` when the node is not configured or
/// not ready.
pub(crate) async fn step_of(
    repo: &ConfigBackupRepo,
    creds: &CredentialStore,
    node: Uuid,
    now: DateTime<Utc>,
) -> anyhow::Result<Option<Step>> {
    let Some((cfg, rotation)) = repo.get_with_rotation(node).await? else {
        return Ok(None);
    };
    let logins = cli_logins(creds).await?;
    let cands = candidates(cfg.login_order.as_deref(), &logins);
    let profile = repo.profile_name(node).await?;
    let moved = repo.address_changed(node).await?;
    if readiness(Some(&cfg), profile.as_deref(), !cands.is_empty(), moved) != Readiness::Ready {
        return Ok(None);
    }
    let node_attempts = repo.attempts_of(node, ATTEMPTS_PER_NODE).await?;
    let fleet = fleet_recent(repo, now).await?;
    let caps = login_caps(&candidate_sets(&repo.targets().await?, &logins));
    Ok(Some(next_step(
        &cands,
        &rotation,
        &node_attempts,
        &fleet,
        &limits_of(cfg.scan_mode),
        &caps,
        now,
    )))
}

/// Every configured node's candidates and scan mode, for [`login_caps`].
fn candidate_sets(
    targets: &[yagra_base::config_backup::BackupTarget],
    logins: &[Uuid],
) -> Vec<(Vec<Uuid>, ScanMode)> {
    targets
        .iter()
        .map(|t| {
            (
                candidates(t.config.login_order.as_deref(), logins),
                t.config.scan_mode,
            )
        })
        .collect()
}

/// Every attempt in the last [`WINDOW`] plus every fetch still out, which the limits count as if it
/// will fail.
pub(crate) async fn fleet_recent(
    repo: &ConfigBackupRepo,
    now: DateTime<Utc>,
) -> anyhow::Result<Vec<Attempt>> {
    let mut all = repo.attempts_since(now - WINDOW).await?;
    all.extend(repo.outstanding(now - REQUEST_TIMEOUT).await?);
    Ok(all)
}

/// Whether oxidized's node list may be read again now: only while no fetch is out anywhere. See the
/// module doc for what a reload during a fetch does.
pub(crate) async fn may_reload(
    repo: &ConfigBackupRepo,
    now: DateTime<Utc>,
) -> anyhow::Result<bool> {
    Ok(!repo.any_outstanding(now - REQUEST_TIMEOUT).await?)
}

/// How many of a node's own attempts are read to find what its pass has tried and what its window
/// holds. More than a day of the fastest mode's ten failures an hour.
const ATTEMPTS_PER_NODE: i64 = 300;

/// One tick of the leader loop: count outstanding fetches nobody answered, then start the next
/// login of every node whose turn it is.
async fn tick(client: &OxidizedClient, repo: &ConfigBackupRepo, creds: &CredentialStore) {
    let now = Utc::now();
    let (targets, logins, mut fleet) =
        match tokio::try_join!(repo.targets(), cli_logins(creds), fleet_recent(repo, now)) {
            Ok(read) => read,
            Err(e) => {
                tracing::warn!(error = %e, "config backup: the rotation could not read its state");
                return;
            }
        };
    // A batch starts only when the last one has answered (`may_reload`). Lost fetches are still
    // counted below, and they are what frees a batch whose answer never came.
    let busy = targets
        .iter()
        .any(|t| in_flight(t.rotation.requested_at, now));
    let caps = login_caps(&candidate_sets(&targets, &logins));
    let mut started = Vec::new();
    for t in targets {
        if t.rotation.requested_at.is_some() && !in_flight(t.rotation.requested_at, now) {
            match repo
                .record_result(t.node_id, AttemptOutcome::Lost, None)
                .await
            {
                Ok(Some(a)) => fleet.push(a),
                Ok(None) => continue,
                Err(e) => {
                    tracing::warn!(node = %t.node_id, error = %e, "config backup: could not record a lost fetch");
                    continue;
                }
            }
        } else if in_flight(t.rotation.requested_at, now) {
            continue;
        }
        // A working login is fetched on oxidized's own schedule.
        if busy || (t.rotation.login_confirmed && t.rotation.requested_at.is_none()) {
            continue;
        }
        let cands = candidates(t.config.login_order.as_deref(), &logins);
        if readiness(
            Some(&t.config),
            t.profile_name.as_deref(),
            !cands.is_empty(),
            t.address_changed,
        ) != Readiness::Ready
        {
            continue;
        }
        let node_attempts = match repo.attempts_of(t.node_id, ATTEMPTS_PER_NODE).await {
            Ok(a) => a,
            Err(e) => {
                tracing::warn!(node = %t.node_id, error = %e, "config backup: could not read attempts");
                continue;
            }
        };
        // Re-read: a lost fetch recorded above changed the rotation.
        let rotation = match repo.get_with_rotation(t.node_id).await {
            Ok(Some((_, r))) => r,
            _ => continue,
        };
        if rotation.login_confirmed {
            continue;
        }
        let step = next_step(
            &cands,
            &rotation,
            &node_attempts,
            &fleet,
            &limits_of(t.config.scan_mode),
            &caps,
            now,
        );
        let result = match step {
            Step::Fetch(credential) => {
                // Counted as if it will fail, so two nodes due in the same tick cannot both spend
                // the last failure a shared login has left.
                fleet.push(Attempt {
                    node_id: t.node_id,
                    credential_id: Some(credential),
                    outcome: AttemptOutcome::Lost,
                    new_version: None,
                    at: now,
                });
                match repo
                    .dispatch(t.node_id, credential, now, now - REQUEST_TIMEOUT)
                    .await
                {
                    Ok(true) => {
                        started.push(t.node_id);
                        Ok(())
                    }
                    // A person's "fetch now" got there first.
                    Ok(false) => Ok(()),
                    Err(e) => Err(e),
                }
            }
            Step::Wait { until, .. } => {
                if rotation.next_try_at == Some(until) {
                    Ok(())
                } else {
                    repo.set_next_try(t.node_id, Some(until)).await
                }
            }
            Step::Exhausted | Step::NoCandidate => {
                if rotation.next_try_at.is_none() {
                    Ok(())
                } else {
                    repo.set_next_try(t.node_id, None).await
                }
            }
        };
        if let Err(e) = result {
            tracing::warn!(node = %t.node_id, error = %e, "config backup: could not move the rotation on");
        }
    }
    if started.is_empty() {
        return;
    }
    for node in start_fetches(client, &started).await {
        if let Err(e) = repo.clear_request(node).await {
            tracing::warn!(%node, error = %e, "config backup: could not withdraw a fetch request");
        }
    }
}

/// Ask oxidized to re-read its node list (where the nodes just handed a login now appear) and to
/// fetch each of them now. Returns the nodes oxidized was not asked about — the caller withdraws
/// their requests, so an oxidized that did not answer is not later counted as a refused login.
pub(crate) async fn start_fetches(client: &OxidizedClient, nodes: &[Uuid]) -> Vec<Uuid> {
    if let Err(e) = client.reload().await {
        tracing::warn!(error = %e, "config backup: oxidized did not reload for a new login");
        metrics::counter!("yagra_oxidized_errors_total", "op" => "reload").increment(1);
        return nodes.to_vec();
    }
    let mut failed = Vec::new();
    for node in nodes {
        if let Err(e) = client.fetch_now(*node).await {
            tracing::warn!(%node, error = %e, "config backup: oxidized did not queue a fetch");
            metrics::counter!("yagra_oxidized_errors_total", "op" => "next").increment(1);
            failed.push(*node);
        }
    }
    failed
}

/// Runs until cancelled. Leader-only, for the reason `status.rs` gives: two cores would each start
/// the same node's next login, spending two attempts where the limits allowed one.
pub(crate) async fn run_rotation_loop(
    client: Arc<OxidizedClient>,
    repo: Arc<ConfigBackupRepo>,
    creds: Arc<CredentialStore>,
) {
    let mut every = tokio::time::interval(TICK);
    every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut ticks = 0u32;
    loop {
        every.tick().await;
        tick(&client, &repo, &creds).await;
        ticks = ticks.wrapping_add(1);
        if ticks.is_multiple_of(PRUNE_EVERY) {
            if let Err(e) = repo.prune_attempts().await {
                tracing::warn!(error = %e, "config backup: old attempts were not pruned");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t0() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-10-22T10:00:00Z")
            .expect("time")
            .with_timezone(&Utc)
    }

    fn minutes(m: i64) -> chrono::TimeDelta {
        chrono::TimeDelta::minutes(m)
    }

    const NODE: Uuid = Uuid::from_u128(1);
    const A: Uuid = Uuid::from_u128(0xa);
    const B: Uuid = Uuid::from_u128(0xb);
    const C: Uuid = Uuid::from_u128(0xc);

    fn attempt(node: Uuid, cred: Uuid, outcome: AttemptOutcome, at: DateTime<Utc>) -> Attempt {
        Attempt {
            node_id: node,
            credential_id: Some(cred),
            outcome,
            new_version: None,
            at,
        }
    }

    /// [`next_step`] for a node in Normal mode whose logins nobody else limits.
    fn step(
        cands: &[Uuid],
        rot: &Rotation,
        node: &[Attempt],
        fleet: &[Attempt],
        now: DateTime<Utc>,
    ) -> Step {
        next_step(
            cands,
            rot,
            node,
            fleet,
            &limits_of(ScanMode::Normal),
            &HashMap::new(),
            now,
        )
    }

    fn fresh(round: DateTime<Utc>) -> Rotation {
        Rotation {
            round_started_at: Some(round),
            ..Rotation::default()
        }
    }

    /// Newest first, as `attempts_of` returns them.
    fn newest_first(mut v: Vec<Attempt>) -> Vec<Attempt> {
        v.sort_by_key(|a| std::cmp::Reverse(a.at));
        v
    }

    #[test]
    fn a_node_order_keeps_only_cli_logins_in_its_own_order() {
        let logins = [A, B, C];
        assert_eq!(candidates(None, &logins), vec![A, B, C]);
        let gone = Uuid::from_u128(0xdead);
        assert_eq!(candidates(Some(&[C, gone, A, C]), &logins), vec![C, A]);
        assert!(candidates(Some(&[]), &logins).is_empty());
    }

    /// Wrong, wrong, right: ten minutes apart, then the third is fetched — and once it is
    /// remembered, it is the one tried.
    #[test]
    fn the_candidates_are_tried_in_turn_ten_minutes_apart() {
        let cands = [A, B, C];
        let start = t0();
        let rot = fresh(start);
        assert_eq!(step(&cands, &rot, &[], &[], start), Step::Fetch(A));

        let one = vec![attempt(NODE, A, AttemptOutcome::AuthFailed, start)];
        assert_eq!(
            step(&cands, &rot, &one, &one, start + minutes(1)),
            Step::Wait {
                credential: B,
                until: start + minutes(10)
            }
        );
        let later = start + minutes(10);
        assert_eq!(step(&cands, &rot, &one, &one, later), Step::Fetch(B));

        let two = newest_first(vec![
            one[0].clone(),
            attempt(NODE, B, AttemptOutcome::Other, later),
        ]);
        assert_eq!(
            step(&cands, &rot, &two, &two, later + minutes(10)),
            Step::Fetch(C)
        );

        let worked = Rotation {
            credential_id: Some(C),
            login_confirmed: true,
            ..rot
        };
        assert_eq!(
            step(&cands, &worked, &two, &two, later + minutes(30)),
            Step::Fetch(C)
        );
    }

    #[test]
    fn three_failures_in_an_hour_hold_the_device_until_the_first_leaves_the_window() {
        let cands = [A, B, C, Uuid::from_u128(0xd)];
        let start = t0();
        let fails = newest_first(vec![
            attempt(NODE, A, AttemptOutcome::AuthFailed, start),
            attempt(NODE, B, AttemptOutcome::AuthFailed, start + minutes(10)),
            attempt(NODE, C, AttemptOutcome::AuthFailed, start + minutes(20)),
        ]);
        let rot = fresh(start);
        let d = Uuid::from_u128(0xd);
        assert_eq!(
            step(&cands, &rot, &fails, &fails, start + minutes(30)),
            Step::Wait {
                credential: d,
                until: start + minutes(60)
            }
        );
        assert_eq!(
            step(&cands, &rot, &fails, &fails, start + minutes(61)),
            Step::Fetch(d)
        );
    }

    /// A shared login that failed five times elsewhere is waited for, not skipped.
    #[test]
    fn a_login_that_failed_across_the_fleet_is_waited_for_not_skipped() {
        let cands = [A, B];
        let start = t0();
        let fleet: Vec<Attempt> = (0..5)
            .map(|i| {
                attempt(
                    Uuid::from_u128(100 + i),
                    A,
                    AttemptOutcome::AuthFailed,
                    start + minutes(i64::try_from(i).expect("small")),
                )
            })
            .collect();
        let rot = fresh(start);
        assert_eq!(
            step(&cands, &rot, &[], &fleet, start + minutes(5)),
            Step::Wait {
                credential: A,
                until: start + minutes(60)
            }
        );
        assert_eq!(
            step(&cands, &rot, &[], &fleet, start + minutes(60)),
            Step::Fetch(A)
        );
    }

    #[test]
    fn a_device_that_did_not_answer_is_not_counted_and_keeps_its_login() {
        let cands = [A, B];
        let start = t0();
        let silent = vec![attempt(NODE, A, AttemptOutcome::Unreachable, start)];
        let rot = fresh(start);
        assert_eq!(
            step(&cands, &rot, &silent, &silent, start + minutes(5)),
            Step::Wait {
                credential: A,
                until: start + limits_of(ScanMode::Normal).unreachable_retry
            }
        );
        assert_eq!(
            step(
                &cands,
                &rot,
                &silent,
                &silent,
                start + limits_of(ScanMode::Normal).unreachable_retry
            ),
            Step::Fetch(A)
        );
    }

    #[test]
    fn a_pass_with_every_login_failed_is_over_until_it_restarts() {
        let cands = [A, B];
        let start = t0();
        let fails = newest_first(vec![
            attempt(NODE, A, AttemptOutcome::AuthFailed, start),
            attempt(NODE, B, AttemptOutcome::AuthFailed, start + minutes(10)),
        ]);
        let rot = fresh(start);
        assert_eq!(
            step(&cands, &rot, &fails, &fails, start + minutes(90)),
            Step::Exhausted
        );
        // Restarted: the pass begins after those failures, so A is due again.
        let restarted = fresh(start + minutes(91));
        assert_eq!(
            step(&cands, &restarted, &fails, &fails, start + minutes(91)),
            Step::Fetch(A)
        );
        assert_eq!(step(&[], &restarted, &[], &[], start), Step::NoCandidate);
    }

    /// A working login refused later (its password changed) starts a pass at that failure, so the
    /// next candidate is tried rather than the same one again.
    #[test]
    fn a_working_login_refused_later_moves_to_the_next_one() {
        let cands = [A, B];
        let start = t0();
        let refused = vec![attempt(NODE, A, AttemptOutcome::AuthFailed, start)];
        let rot = Rotation {
            credential_id: Some(A),
            login_confirmed: false,
            round_started_at: Some(start),
            ..Rotation::default()
        };
        assert_eq!(
            step(&cands, &rot, &refused, &refused, start + minutes(10)),
            Step::Fetch(B)
        );
    }

    /// A person's press tries the login a limit is holding back, at once; with nothing left to try
    /// it tries nothing.
    #[test]
    fn fetch_now_forces_the_held_back_login() {
        let until = t0() + minutes(10);
        assert_eq!(login_to_force(Step::Fetch(A)), Some(A));
        assert_eq!(
            login_to_force(Step::Wait {
                credential: B,
                until
            }),
            Some(B)
        );
        assert_eq!(login_to_force(Step::Exhausted), None);
        assert_eq!(login_to_force(Step::NoCandidate), None);
    }

    /// Rapid tries a minute after a failure where Normal waits ten and Slow an hour, and Slow's
    /// three failures hold a device for a day.
    #[test]
    fn the_three_modes_wait_their_own_gaps() {
        let cands = [A, B, C, Uuid::from_u128(0xd)];
        let start = t0();
        let one = vec![attempt(NODE, A, AttemptOutcome::AuthFailed, start)];
        let rot = fresh(start);
        let none = HashMap::new();
        let wait = |mode| match next_step(
            &cands,
            &rot,
            &one,
            &one,
            &limits_of(mode),
            &none,
            start + minutes(1),
        ) {
            Step::Fetch(_) => start + minutes(1),
            Step::Wait { until, .. } => until,
            other => panic!("{other:?}"),
        };
        assert_eq!(wait(ScanMode::Rapid), start + minutes(1));
        assert_eq!(wait(ScanMode::Normal), start + minutes(10));
        assert_eq!(wait(ScanMode::Slow), start + minutes(60));

        let three = newest_first(vec![
            attempt(NODE, A, AttemptOutcome::AuthFailed, start),
            attempt(NODE, B, AttemptOutcome::AuthFailed, start + minutes(60)),
            attempt(NODE, C, AttemptOutcome::AuthFailed, start + minutes(120)),
        ]);
        let slow = next_step(
            &cands,
            &rot,
            &three,
            &three,
            &limits_of(ScanMode::Slow),
            &none,
            start + minutes(180),
        );
        assert_eq!(
            slow,
            Step::Wait {
                credential: Uuid::from_u128(0xd),
                until: start + chrono::TimeDelta::hours(24)
            }
        );
        // A mode this core does not know is held to the most careful one.
        assert_eq!(limits_of(ScanMode::Unknown), limits_of(ScanMode::Slow));
    }

    /// A login shared by a rapid node and a slow one is held to the slow one's fleet-wide cap.
    #[test]
    fn a_shared_login_takes_the_strictest_cap() {
        let caps = login_caps(&[
            (vec![A, B], ScanMode::Rapid),
            (vec![A], ScanMode::Slow),
            (vec![C], ScanMode::Normal),
        ]);
        assert_eq!(caps[&A], limits_of(ScanMode::Slow).login_cap);
        assert_eq!(caps[&B], limits_of(ScanMode::Rapid).login_cap);
        assert_eq!(caps[&C], limits_of(ScanMode::Normal).login_cap);
    }

    /// A device that goes on not answering is given up on after the mode's time, as if every login
    /// had failed.
    #[test]
    fn a_device_silent_for_too_long_is_given_up_on() {
        let cands = [A, B];
        let start = t0();
        let silent = newest_first(vec![
            attempt(NODE, A, AttemptOutcome::Unreachable, start),
            attempt(NODE, A, AttemptOutcome::Unreachable, start + minutes(60)),
        ]);
        let rot = fresh(start);
        assert!(matches!(
            step(&cands, &rot, &silent, &silent, start + minutes(120)),
            Step::Fetch(A)
        ));
        assert_eq!(
            step(
                &cands,
                &rot,
                &silent,
                &silent,
                start + chrono::TimeDelta::hours(24)
            ),
            Step::Exhausted
        );
    }

    #[test]
    fn the_measured_error_types_are_classified() {
        assert_eq!(classify("node_success", ""), Some(AttemptOutcome::Success));
        assert_eq!(
            classify("node_fail", "Net::SSH::AuthenticationFailed"),
            Some(AttemptOutcome::AuthFailed)
        );
        assert_eq!(
            classify("node_fail", "Oxidized::PromptUndetect"),
            Some(AttemptOutcome::Other)
        );
        for silent in ["Net::SSH::ConnectionTimeout", "Errno::ECONNREFUSED"] {
            assert_eq!(
                classify("node_fail", silent),
                Some(AttemptOutcome::Unreachable)
            );
        }
        assert_eq!(
            classify("node_fail", "Timeout::Error"),
            Some(AttemptOutcome::Other)
        );
        assert_eq!(classify("post_store", ""), None);
    }

    #[test]
    fn a_fetch_is_outstanding_for_ten_minutes() {
        let now = t0();
        assert!(!in_flight(None, now));
        assert!(in_flight(Some(now - minutes(9)), now));
        assert!(!in_flight(Some(now - minutes(11)), now));
    }
}
