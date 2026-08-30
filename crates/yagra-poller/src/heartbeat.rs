// SPDX-License-Identifier: AGPL-3.0-only
//! This poller saying it is alive, and what it can do (ADR-009).
//!
//! One beat every [`HEARTBEAT_SECS`], carrying liveness, the working set's epoch/last_seq so core
//! can spot a stale or gapped poller, node/spec/inflight/result counts, which listeners bound, a
//! host-resource sample, and this host's management addresses. Never logs and never carries a
//! secret.
//!
//! ⚠️ **This loop is deliberately not cancellable.** Its last act is to publish a `leaving` beat so
//! core reassigns this poller's nodes immediately instead of waiting three missed heartbeats; being
//! aborted mid-publish would put every graceful restart back on the timeout path. It observes the
//! shutdown token instead, and `main` joins it with a bounded timeout — that join is what makes the
//! guarantee real, since otherwise the beat races the runtime being dropped.
//!
//! **The capability list is the part that fails silently.** Five claims are unconditional and two
//! are earned; a claim this poller does not make is work core withholds — authenticated URL checks,
//! byte-exact forwarding, a support-log request — with no error anywhere and every test green.
//! `guards.rs` makes an unclaimed capability a build failure by deriving the vocabulary from
//! `yagra-bus`, which is the only place that list is written down.

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::task::JoinHandle;
use uuid::Uuid;
use yagra_bus::{HeartbeatMsg, NatsBus, SyncBus, HEARTBEAT_SECS};
use yagra_telemetry::CancellationToken;

use crate::pool::PoolState;
use crate::working_set::WorkingSet;
use crate::{location, PollerIdentity};

/// How long the `leaving` beat itself waits for the bus to confirm it left the process.
const LEAVE_FLUSH_TIMEOUT: Duration = Duration::from_secs(1);

/// Start the heartbeat loop, returning its handle so `main` can join the `leaving` beat.
///
/// ⚠️ **`tokio::spawn`, not `spawn_cancellable`** — see the module doc. The handle is the other half
/// of that decision: without joining it the final beat is lost with the runtime.
//
// Eight arguments because a beat reports on eight independent things, each already a shared handle
// its owner also uses elsewhere: the bus, who this poller is, which pool it now serves (ADR-107
// Inc.2), the working set, two counters, the listener labels and the shutdown token. Bundling them
// would produce a struct whose only member in common is "the heartbeat reads it", built at one call
// site and destructured at the next — a type that exists to satisfy a lint rather than to name
// something. The loop below carries the same allow for the same reason.
#[allow(clippy::too_many_arguments)]
pub(crate) fn start(
    bus: &Arc<NatsBus>,
    identity: &PollerIdentity,
    pool: &Arc<PoolState>,
    working_set: &Arc<Mutex<WorkingSet>>,
    results_total: &Arc<AtomicU64>,
    inflight: &Arc<AtomicU64>,
    listeners: Vec<String>,
    shutdown: &CancellationToken,
) -> JoinHandle<()> {
    // The host collector rides the beat so this poller's CPU/load/mem/disk reach core even across
    // NAT/FW (self-observability).
    let host_collector = Arc::new(yagra_hoststats::HostCollector::from_env());
    tokio::spawn(run_heartbeat_loop(
        bus.clone(),
        identity.id.clone(),
        pool.clone(),
        identity.incarnation,
        identity.version,
        working_set.clone(),
        results_total.clone(),
        inflight.clone(),
        listeners,
        host_collector,
        // Read once at startup rather than per beat: enumerating interfaces is a syscall, and an
        // address change on a poller host is a restart-level event in every deployment shape we
        // support (a container gets a new address by being recreated).
        location::local_mgmt_addrs(),
        shutdown.clone(),
    ))
}

/// Publish a liveness + telemetry heartbeat every [`HEARTBEAT_SECS`] (ADR-009). Echoes the working
/// set's epoch/last_seq so core can spot a stale/gapped poller, plus node/spec/inflight/result
/// counts, the bound listeners, and a host-resource sample (CPU/load/mem/disk). Never logs or
/// carries a secret.
#[allow(clippy::too_many_arguments)]
async fn run_heartbeat_loop<B>(
    bus: Arc<B>,
    poller_id: String,
    pool: Arc<PoolState>,
    incarnation: Uuid,
    version: &'static str,
    working_set: Arc<Mutex<WorkingSet>>,
    results_total: Arc<AtomicU64>,
    inflight: Arc<AtomicU64>,
    listeners: Vec<String>,
    host_collector: Arc<yagra_hoststats::HostCollector>,
    mgmt_addrs: Vec<std::net::IpAddr>,
    shutdown: CancellationToken,
) where
    B: SyncBus + 'static,
{
    let mut tick = tokio::time::interval(Duration::from_secs(HEARTBEAT_SECS));
    loop {
        // A shutdown must not wait out the next tick: the point of the final beat is that it
        // arrives before the process is gone, so core can hand this poller's nodes over
        // immediately instead of waiting three missed beats.
        let leaving = tokio::select! {
            () = shutdown.cancelled() => true,
            _ = tick.tick() => false,
        };
        let (nodes, specs, epoch, last_seq) = {
            let ws = working_set.lock().expect("working set mutex poisoned");
            let (nodes, specs) = ws.stats();
            let (epoch, last_seq) = ws.sync_state();
            (nodes, specs, epoch, last_seq)
        };
        metrics::gauge!("yagra_working_set_specs").set(f64::from(specs));
        let hb = HeartbeatMsg {
            poller_id: poller_id.clone(),
            // Read per beat, not captured once: core moves this poller by telling it a new pool
            // (ADR-107 Inc.2), and the beat is how core sees that the move landed. A captured
            // value would report the boot-time pool forever, and the Pollers page would show a
            // poller in the pool it used to serve while it polled the one it does.
            pool: pool.current(),
            incarnation,
            version: version.to_owned(),
            epoch,
            last_seq,
            working_set_nodes: nodes,
            working_set_specs: specs,
            inflight: u32::try_from(inflight.load(Ordering::Relaxed)).unwrap_or(u32::MAX),
            results_total: results_total.load(Ordering::Relaxed),
            listeners: listeners.clone(),
            // This build attaches the original datagram to passive events (ADR-034), so core may
            // promise byte-exact forwarding for anything this poller received. An N-1 poller sends
            // no caps, and core degrades that poller's traffic to re-rendered output + a warning.
            caps: vec![
                yagra_bus::CAP_RAW_CAPTURE.to_owned(),
                yagra_bus::CAP_FLOW_RELAY.to_owned(),
                // This build understands `HttpCheck::auth`. Without the claim core withholds every
                // authenticated URL check from this poller rather than let it probe anonymously and
                // report the resulting 401 as an outage.
                yagra_bus::CAP_HTTP_AUTH.to_owned(),
                // This build reads a URL check's response body and applies `HttpCheck::body_match`.
                // Without the claim core withholds every content-checked monitor rather than let
                // this poller report `http_up = 1` for a page it never looked at.
                yagra_bus::CAP_HTTP_BODY.to_owned(),
                // This build honours a `DiscoveryCancel` (ADR-068 Inc.2). Unconditional, like the
                // four above, because the subscription is unconditional.
                //
                // ⚠️ Core cannot use this to withhold anything — a sweep is queue-delivered, so it
                // does not know which poller will run one. The claim only lets the UI tell the
                // operator, before they press Stop, whether every poller that might be running the
                // sweep understands the command.
                yagra_bus::CAP_DISCOVERY_CANCEL.to_owned(),
                // This build puts `upgrade_report()` on the beat (ADR-051 Inc.5). Unconditional
                // like the five above, because it is a claim about the build and not about the
                // site: whether there is anything to report is what `upgrade` itself answers.
                // Core reads its absence as "no report is coming" and stops waiting for one, which
                // is the whole point — until this existed, a build that could report and a build
                // that could not were indistinguishable on the bus, so core waited out its full
                // budget on both.
                yagra_bus::CAP_UPGRADE_REPORT.to_owned(),
                // This build follows a pool change core sends on the working-set snapshot: it
                // re-points the three pool-derived subjects and reconnects the bus, without
                // restarting (ADR-107 Inc.2). Unconditional, like the six above, because it is a
                // claim about the build.
                //
                // 🚨 Core **refuses** to move a poller that does not claim this, and the refusal is
                // the whole point: a move without it half-works — the working set arrives on the
                // id-keyed subject and every screen reads correct, while "poll now" and discovery
                // keep going to the old pool's subject where nothing is listening.
                yagra_bus::CAP_POOL_FOLLOW.to_owned(),
            ]
            .into_iter()
            // Unlike the seven above, these two are conditional and come from the same read: the
            // site updater beside this poller is alive (`self-upgrade`), and it has vouched for
            // itself (`site-prepared`, ADR-051 Inc.7). Claiming the first unconditionally would
            // make core send commands into sites that cannot act on them and report every such
            // site as "will upgrade" when it will not; the second is a claim about a *neighbouring
            // container*, so its absence has to cover a stopped sidecar, one a release behind, and
            // one that never heard of the field — all three want the same warning before a press.
            .chain(updater_caps())
            // Conditional for the same shape of reason (ADR-045 Inc.4): this poller can answer a
            // support-log request only if it has a file layer to read. Without the claim core does
            // not ask, and the bundle names the site as unrepresented — which is the true statement.
            // Claiming it unconditionally would turn "there is nothing to send" into an empty reply
            // core cannot distinguish from a deliberate one.
            .chain(log_ship_cap())
            .collect(),
            host: Some(host_collector.sample()),
            leaving,
            // Where this poller sits, so core can root the derived dependency graph (ADR-043).
            mgmt_addrs: mgmt_addrs.clone(),
            // What the site updater last said about an upgrade (ADR-051 Inc.4). Sent on every beat
            // rather than only while one is running: core keys on the run id, so a stale report is
            // inert, and tracking "is this still current" here would be a second answer to a
            // question the run id already settles.
            upgrade: upgrade_report(),
        };
        if let Err(e) = bus.publish_heartbeat(hb).await {
            tracing::warn!(error = %e, "failed to publish heartbeat");
        }
        if leaving {
            // `publish` only queues into the client's writer, and this process is about to stop
            // existing — so without the flush the beat that makes a hand-over prompt is exactly the
            // beat most likely to be lost, degrading every graceful restart to the 30s timeout path
            // it was written to avoid (ADR-051). Bounded: a broker that cannot take it in a second
            // will not take it at all, and we are on the way out either way.
            match tokio::time::timeout(LEAVE_FLUSH_TIMEOUT, bus.flush()).await {
                Ok(Ok(())) => tracing::info!(
                    "published leaving heartbeat — core can reassign this poller's nodes"
                ),
                Ok(Err(e)) => {
                    tracing::warn!(error = %e, "leaving heartbeat may not have reached the bus");
                }
                Err(_) => tracing::warn!(
                    "timed out flushing the leaving heartbeat — core will fall back to timeout detection"
                ),
            }
            return;
        }
    }
}

/// The capabilities the site updater beside this poller earns it (ADR-051).
///
/// One function and one file read per beat, for two claims that are both answers about the *same
/// container* at the *same instant*. Splitting them would read `current.json` twice and give the
/// freshness rule two homes — and that rule is the load-bearing half of both, since it is the only
/// thing separating a running sidecar from one that was commented out, crashed, or wired to the
/// wrong path.
///
/// Impure only in its first three lines; everything decided lives in [`updater_caps_from`], where a
/// test can reach it without a filesystem or a clock.
fn updater_caps() -> Vec<String> {
    let Some(dir) = crate::env_nonempty("YAGRA_UPGRADE_DIR") else {
        return Vec::new();
    };
    let Ok(raw) = std::fs::read_to_string(Path::new(&dir).join("current.json")) else {
        return Vec::new();
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_secs()).ok());
    now.map_or_else(Vec::new, |now| updater_caps_from(&raw, now))
}

/// What a site updater's `current.json` claims, given its text and the clock.
///
/// * [`yagra_bus::CAP_SELF_UPGRADE`] — a sidecar is deployed and has been seen alive, so core may
///   hand it a release. Claiming this on the environment variable alone would let core report a
///   site as "will upgrade with core" and then send it a command nothing reads: the version skew
///   would still be there and the page would say it had been dealt with, which is worse than
///   saying nothing.
/// * [`yagra_bus::CAP_SITE_PREPARED`] — that sidecar's own word that an apply will not damage this
///   site (Inc.7). **Relayed, never decided here**: the hazard is that `docker compose`, run by
///   that sibling container from a container-local working directory, resolves this poller's
///   relative certificate bind against the wrong root and is handed an empty directory Docker
///   created for it. Nothing visible from inside this process tells that apart from a healthy site
///   until the replacement has already started and failed, so the container running the command is
///   the one that answers.
///
/// **Absence is the warning, and that is the whole design.** No field is written by a sidecar that
/// predates the fix, by one running an older release, or by none at all — and all three want the
/// same thing said about them. The only error this may not make is the reassuring one.
///
/// ⚠️ **Do not re-derive preparedness from a version or from another capability.** The obvious
/// shortcut is [`yagra_bus::CAP_UPGRADE_REPORT`], which shipped in the same commit as the fix — but
/// that is a claim about this *binary*, while the hazard lives in the site's *composition*. A
/// `docker compose up -d` at a site replaces the binary (floating tag, `pull_policy: always`) and
/// leaves the updater alone, because its definition did not change. The shortcut reports that site
/// as safe.
fn updater_caps_from(raw: &str, now: i64) -> Vec<String> {
    let Ok(beat) = serde_json::from_str::<serde_json::Value>(raw) else {
        return Vec::new();
    };
    let Some(written_at) = beat.get("written_at").and_then(serde_json::Value::as_i64) else {
        return Vec::new();
    };
    // The sidecar beats every few seconds; a minute of slack absorbs clock skew between two
    // containers on the same host without ever calling a dead updater alive. A beat from the future
    // is skew, not staleness — it was clearly written.
    if now.saturating_sub(written_at) > 60 {
        return Vec::new();
    }
    let mut caps = vec![yagra_bus::CAP_SELF_UPGRADE.to_owned()];
    if beat
        .get(yagra_bus::SITE_PREPARED_FIELD)
        .and_then(serde_json::Value::as_bool)
        == Some(true)
    {
        caps.push(yagra_bus::CAP_SITE_PREPARED.to_owned());
    }
    caps
}

/// What the site updater last wrote about an upgrade, for core to read off the beat (ADR-051 Inc.4).
///
/// **This is what turns a fifteen-minute sleep into a wait.** Core splits an upgrade into a prefetch
/// (the site pulls the image, nothing stops) and an apply (the container is recreated, which *is*
/// the outage), and until now had no way to learn the prefetch had finished — the updater writes
/// `status.json` to a volume core cannot reach. So it slept out the whole budget before starting the
/// first apply of every pool. Carrying the file on the beat costs one small read every ten seconds.
///
/// Deliberately **not** gated on `self_upgrade_cap`: those answer different questions. The capability
/// asks whether a sidecar is alive *now*, and a report is worth carrying from one that has since
/// stopped — that is exactly the case where core is waiting to hear how it went. Reading the same
/// env var is the shared part; the freshness test is not.
///
/// Any failure is `None`: no directory, no file yet, a half-written one, a shape this build cannot
/// read. All of them mean the same thing to core — nothing to report — and none of them is worth
/// failing a heartbeat over.
fn upgrade_report() -> Option<yagra_bus::UpgradeReport> {
    let dir = crate::env_nonempty("YAGRA_UPGRADE_DIR")?;
    let raw = std::fs::read_to_string(Path::new(&dir).join("status.json")).ok()?;
    serde_json::from_str(&raw).ok()
}

/// [`yagra_bus::CAP_LOG_SHIP`], but only when there is a log file to ship (ADR-045 Inc.4).
///
/// One condition, and it is the same one the subscribe above is gated on, deliberately read from the
/// same place: `yagra_telemetry::log_dir()`. If these two ever disagreed the failure would be
/// silent in the worse direction — core would ask a poller that is not listening and wait out the
/// whole deadline, then record the site as unresponsive when it was merely never subscribed.
fn log_ship_cap() -> Option<String> {
    yagra_telemetry::log_dir().map(|_| yagra_bus::CAP_LOG_SHIP.to_owned())
}

#[cfg(test)]
mod tests {
    /// The `leaving` beat is the one publish in the process with no successor, so it is the one that
    /// must be flushed. Nothing in the type system says so — `publish` returning `Ok` reads like
    /// delivery — hence a test that reads the source and pins the call.
    ///
    /// ⚠️ **Read through `module_source`, never `include_str!`** (ADR-091/102). The raw text would
    /// carry this test module with it, and then both needles below — the one that cuts and the one
    /// that asserts — would match the lines they are written on, so the check could not fail. That
    /// was true of this test until ADR-103 moved it here.
    #[test]
    fn the_leaving_beat_is_flushed_before_the_loop_returns() {
        let src = crate::module_source::code("src", "heartbeat");
        let leave = src
            .split_once("if leaving {")
            .expect("the heartbeat loop's leaving arm")
            .1;
        let arm = &leave[..leave.find("\n        }").unwrap_or(leave.len())];
        assert!(
            arm.contains("bus.flush()"),
            "the leaving arm must flush: a queued publish dies with the runtime"
        );
    }

    /// What a site updater's beat earns it, in both directions.
    ///
    /// 🚨 **The accept cases are here for a reason.** A suite of "X is not claimed" assertions is
    /// satisfied by a function that claims nothing at all, which is exactly the regression that
    /// matters least and passes most easily. So every rejection below is paired with a beat that
    /// *is* accepted, and the third case pins the pair apart: a live sidecar that has not vouched
    /// for itself must still earn `self-upgrade`, or turning the warning on would stop core sending
    /// any site a release.
    ///
    /// Pure on purpose (ADR-051 Inc.7): the decision takes the text and the clock, so this needs no
    /// filesystem, no environment variable and no sleep — and `set_var` in a parallel test binary is
    /// how a check like this becomes flaky and then ignored.
    #[test]
    fn a_site_updater_earns_preparedness_only_by_declaring_it() {
        use yagra_bus::{CAP_SELF_UPGRADE, CAP_SITE_PREPARED};
        let now = 1_700_000_000;
        let beat = |extra: &str| format!(r#"{{"written_at":{now},"repo":"r"{extra}}}"#);
        let caps = |raw: &str| super::updater_caps_from(raw, now);

        assert_eq!(
            caps(&beat(r#","prepared":true"#)),
            vec![CAP_SELF_UPGRADE.to_owned(), CAP_SITE_PREPARED.to_owned()],
            "a sidecar that vouches for itself earns both"
        );
        assert_eq!(
            caps(&beat("")),
            vec![CAP_SELF_UPGRADE.to_owned()],
            "a sidecar that predates the field is alive and unvouched — it must still be sent \
             releases, and it must still be warned about"
        );
        assert_eq!(
            caps(&beat(r#","prepared":false"#)),
            vec![CAP_SELF_UPGRADE.to_owned()],
            "an explicit false is the same answer as saying nothing"
        );
        assert_eq!(
            caps(&beat(r#","prepared":"yes""#)),
            vec![CAP_SELF_UPGRADE.to_owned()],
            "only a real boolean counts; a truthy string must not read as a claim"
        );

        // Staleness and unreadability outrank everything: a sidecar that has stopped cannot vouch
        // for what an apply would do, whatever its last file said.
        assert!(
            caps(&format!(
                r#"{{"written_at":{},"repo":"r","prepared":true}}"#,
                now - 61
            ))
            .is_empty(),
            "a beat older than the freshness window earns nothing, prepared or not"
        );
        assert!(
            caps(r#"{"repo":"r","prepared":true}"#).is_empty(),
            "a beat with no timestamp cannot be dated, so it cannot be believed"
        );
        assert!(
            caps(r#"{"written_at":"#).is_empty(),
            "a half-written file is read at some point on every host"
        );
    }
}
