// SPDX-License-Identifier: AGPL-3.0-only
//! Discovery sweep execution (Phase C) — the poller side.
//!
//! Consumes [`DiscoveryJob`]s off the bus and probes each target for ICMP liveness + SNMP
//! identity (`sysDescr.0` / `sysName.0`), trying the job's candidate credentials (stored
//! v2c/v3, resolved by core) and ad-hoc communities. Runs on the poller because ICMP needs
//! the raw socket. Progress is published as **cumulative** partial results after each chunk
//! of targets, so core can show the sweep advancing; the final message carries `done: true`.
//!
//! ## "We have it" comes before "we found something" (ADR-068 Increment 4)
//!
//! The first thing a sweep publishes is a zero-progress result, before it probes a single address.
//! This loop is **strictly sequential** — one job at a time, start to finish — so a job can sit on
//! the subscription for the length of the sweep ahead of it, which with the ICMP gate off is
//! minutes. Core used to mark a scan `Running` the moment it accepted it, so "queued behind another
//! sweep", "no poller ever took this" and "sweeping, and everything out there is silent" were one
//! sentence on the operator's screen. This message is what core turns into `Running`; a scan nobody
//! sends it for stays `Queued` and says so.
//!
//! It is an ordinary partial result, so it needs no wire change and an N-1 core folds it in as a
//! zero-progress update.
//!
//! ## Stopping a sweep (ADR-068 Increment 2)
//!
//! A stop arrives as a [`yagra_bus::DiscoveryCancel`] **broadcast to every poller** — core cannot
//! address it, because sweep jobs are queue-delivered and the result carries no poller id. Each
//! poller keeps the ids it has been told to stop in a [`CancelSet`] and checks it in three places:
//!
//! 1. when a job is taken off the bus — a sweep cancelled while it queued must not start;
//! 2. at each chunk boundary — the coarse-grained stop;
//! 3. before each credential attempt on a target — the fine-grained one.
//!
//! **The third is not belt-and-braces.** A target costs one ICMP timeout plus a rate-limited
//! attempt per credential (2s apart, `security.md`), so with five credentials a single target can
//! take ~22s and a 32-target chunk two waves of that. Stopping only at chunk boundaries would leave
//! the operator watching an unresponsive button for the better part of a minute. What is *not*
//! interrupted is a probe already in flight: dropping a raw socket or an SNMP session mid-exchange
//! has consequences that are not worth discovering here, so the worst-case delay is one probe
//! timeout.
//!
//! ⚠️ A stopped sweep still publishes a terminal result, with `cancelled: true` and the count of
//! targets it **actually** probed. Both matter: without the message core waits forever, and without
//! the honest count `probed == total` would claim the sweep finished the work it just abandoned.
//!
//! 🚨 **"Forever" is literal, and it shipped.** Core has no timeout on a sweep — a scan sits in
//! `cancelling` until the process restarts — and the chunk-boundary stop (2) used to `break` out of
//! the loop *before* the publish inside it, so a stop that landed between chunks ended the sweep in
//! silence and left the operator watching "stopping…" indefinitely. Layers 1 and 3 each published
//! from their own exit and were fine, which is exactly why nothing looked wrong. The publish is now
//! **one statement after the loop**, guarded by whether a `done: true` has already gone out, so
//! closing the scan is a property of the function rather than something each exit must remember.
//!
//! ## The ICMP gate (ADR-068 Increment 3)
//!
//! By default a target that does not answer ICMP is **not** tried with SNMP. That is where a sweep's
//! time goes: on a /24 the overwhelming majority of addresses are unassigned, and each one used to
//! cost one ICMP timeout *plus* up to [`LimiterConfig::max_consecutive_failures`] SNMP attempts
//! spaced [`LimiterConfig::min_interval_ms`] apart before the limiter backed the device off. Measured
//! on the test network: 254 addresses, 8 devices, **5m21s**, essentially all of it spent asking
//! empty addresses for their `sysDescr`.
//!
//! ⚠️ **What this gives up is real and is why it is a choice, not a rule.** A device that filters
//! ICMP and answers SNMP — a firewall, a hardened host — is now found only when the operator ticks
//! the box (`DiscoveryJob::snmp_when_unreachable`). Worse, the ICMP probe is a *single* echo capped
//! at one timeout (`yagra-transport`'s S4 deadline breaks out of the loop as soon as a host has been
//! silent for one full timeout, so raising `count` buys nothing), which means one lost packet is
//! indistinguishable here from a silent address. Discovery has no dwell window to absorb that the
//! way liveness does.
//!
//! 🚨 **What the gate does *not* do is treat a failed probe as a silent host.** `probe_icmp`
//! returning `Err` — no raw socket, no route, an address family this poller cannot send on — is a
//! fault here and says nothing about the target, so the gate does not apply to it and the target is
//! probed with SNMP as it was before the gate existed. This shipped the other way round for one
//! release: both outcomes collapsed into `reachable == false`, so a poller that could not send ICMP
//! at all swept an entire subnet without asking a single address for its identity and reported it,
//! successfully, as empty. The module doc named that as a thing to watch for. Watching is not a fix.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use futures::stream::{Stream, StreamExt};
use uuid::Uuid;
use yagra_bus::{
    DiscoveredDevice, DiscoveryBus, DiscoveryCancel, DiscoveryJob, DiscoveryResult, NatsBus,
};
use yagra_discovery::{AttemptDecision, CredentialProbeLimiter, LimiterConfig};
use yagra_telemetry::{spawn_cancellable, CancellationToken};
use yagra_transport::{SnmpV3Params, Transport};

/// sysDescr column base — walking it yields the `.0` scalar instance (v2c path).
const SYSDESCR_BASE: &str = "1.3.6.1.2.1.1.1";
/// sysObjectID column base — the vendor-assigned enterprise OID identifying device type.
const SYSOBJECTID_BASE: &str = "1.3.6.1.2.1.1.2";
/// sysName column base.
const SYSNAME_BASE: &str = "1.3.6.1.2.1.1.5";
/// sysDescr.0 instance OID (scalar GET form, v3 path).
const SYSDESCR_OID: &str = "1.3.6.1.2.1.1.1.0";
/// sysObjectID.0 instance OID.
const SYSOBJECTID_OID: &str = "1.3.6.1.2.1.1.2.0";
/// sysName.0 instance OID.
const SYSNAME_OID: &str = "1.3.6.1.2.1.1.5.0";
/// Targets probed concurrently within a chunk.
const SWEEP_CONCURRENCY: usize = 16;
/// Targets per progress chunk: a cumulative partial result is published after each chunk
/// so the operator sees the sweep advance instead of one long silence.
const PROGRESS_CHUNK: usize = 32;

/// How long a cancelled scan id is remembered.
///
/// It exists for the race, not for the running sweep: a stop can arrive before the job does (both
/// travel over NATS, and a reconnect reorders nothing but delivers on its own schedule), so the id
/// must outlive the gap between the two. Ten minutes is far more than that gap and far less than a
/// leak.
const CANCEL_TTL: Duration = Duration::from_secs(600);

/// Upper bound on remembered ids, in case something publishes stops in a loop.
///
/// ⚠️ Both this and [`CANCEL_TTL`] exist because core's own scan map shipped with **neither** and
/// grew for the life of the process (ADR-068 Increment 1 fixed that). Repeating the mistake on the
/// poller — where nobody is watching a list — would be worse.
const MAX_CANCELS: usize = 1024;

/// The scan ids this poller has been told to stop.
///
/// Shared between the cancel-consuming task and the sweeping task, which is why it is a plain
/// `Mutex<…>` rather than a channel: the sweep must be able to *ask* at three different depths of
/// its loop, not be woken.
#[derive(Default)]
pub struct CancelSet {
    inner: Mutex<HashMap<Uuid, Instant>>,
}

impl CancelSet {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a stop, evicting anything expired (and, if it comes to it, the oldest).
    pub fn insert(&self, scan_id: Uuid, now: Instant) {
        let mut g = self.inner.lock().expect("cancel set mutex poisoned");
        g.retain(|_, at| now.duration_since(*at) < CANCEL_TTL);
        if g.len() >= MAX_CANCELS {
            if let Some((&oldest, _)) = g.iter().min_by_key(|(_, at)| **at) {
                g.remove(&oldest);
            }
        }
        g.insert(scan_id, now);
    }

    /// Whether this sweep has been cancelled.
    #[must_use]
    pub fn is_cancelled(&self, scan_id: Uuid, now: Instant) -> bool {
        let g = self.inner.lock().expect("cancel set mutex poisoned");
        g.get(&scan_id)
            .is_some_and(|at| now.duration_since(*at) < CANCEL_TTL)
    }
}

/// Drain stop commands into `cancels`. Runs until the stream ends.
pub async fn run_cancel_stream<S>(mut cancels: S, set: Arc<CancelSet>)
where
    S: Stream<Item = DiscoveryCancel> + Unpin,
{
    while let Some(c) = cancels.next().await {
        tracing::info!(scan = %c.scan_id, "discovery cancel received");
        // Recorded whether or not this poller is running that sweep. It costs one map entry, and it
        // is what makes the stop work when it overtakes the job it refers to.
        set.insert(c.scan_id, Instant::now());
    }
    tracing::warn!("discovery cancel stream ended");
}

/// One SNMP credential to try on each target, in order. Stored credentials carry their
/// store id so a match is reported **by reference** (never the value — security.md);
/// ad-hoc communities (free-text from the scan form) have none.
enum SnmpCandidate {
    V2c {
        cred_ref: Option<Uuid>,
        community: String,
    },
    V3 {
        cred_ref: Uuid,
        params: SnmpV3Params,
    },
}

impl SnmpCandidate {
    fn cred_ref(&self) -> Option<Uuid> {
        match self {
            Self::V2c { cred_ref, .. } => *cred_ref,
            Self::V3 { cred_ref, .. } => Some(*cred_ref),
        }
    }
}

/// Flatten a job's stored credentials + ad-hoc communities into one ordered candidate
/// list. Stored credentials are tried first (they're the operator's registered secrets).
fn candidates_of(job: &DiscoveryJob) -> Vec<SnmpCandidate> {
    let mut out = Vec::with_capacity(job.credentials.len() + job.communities.len());
    for c in &job.credentials {
        if let Some(v3) = &c.v3 {
            // One type since ADR-084: the job's `DiscoveryV3` and the walker's `SnmpV3Params` are
            // both `yagra_common::SnmpV3Auth`, so what used to be a six-field transcription is a
            // clone. It was the last place a USM field could be added on one side and dropped on
            // the other without the compiler noticing.
            out.push(SnmpCandidate::V3 {
                cred_ref: c.cred_ref,
                params: v3.clone(),
            });
        } else if let Some(community) = &c.community {
            out.push(SnmpCandidate::V2c {
                cred_ref: Some(c.cred_ref),
                community: community.clone(),
            });
        }
    }
    for community in &job.communities {
        out.push(SnmpCandidate::V2c {
            cred_ref: None,
            community: community.clone(),
        });
    }
    out
}

/// Everything one sweep holds constant across its targets.
///
/// A struct rather than a parameter list because the list had reached seven and the ICMP gate made
/// it eight — which is clippy's `too_many_arguments` threshold and, per `coding-conventions.md`, the
/// signal that the parameters want to be a type rather than an `#[allow]`. It also removes a real
/// hazard: `sweep_chunk` and `probe_one` took the same seven values in the same order, so a
/// transposed pair would have compiled.
struct SweepCtx {
    /// Credentials to try on each target, in order; the first that answers wins.
    candidates: Arc<Vec<SnmpCandidate>>,
    /// Per-probe timeout (ICMP and SNMP alike).
    timeout: Duration,
    /// Per-device credential-probe rate limit (security.md).
    rate: LimiterConfig,
    scan_id: Uuid,
    cancels: Arc<CancelSet>,
    /// Whether a target that did not answer ICMP is still tried with SNMP (ADR-068 Increment 3).
    snmp_when_unreachable: bool,
    /// How many of this sweep's targets could not be probed at all — `probe_icmp` returned `Err`.
    ///
    /// Counted rather than logged per target: a poller that cannot send ICMP fails on *every*
    /// address, and a line each would be 254 of them for a /24 — which reads as noise and gets
    /// filtered out, which is how the condition stayed invisible. One line at the end says the same
    /// thing and says how big it was.
    icmp_errors: AtomicUsize,
}

impl SweepCtx {
    /// Whether this sweep has been told to stop, as of now.
    fn stopping(&self) -> bool {
        self.cancels.is_cancelled(self.scan_id, Instant::now())
    }
}

/// Drain discovery jobs off the bus, sweeping each and publishing cumulative progress
/// results. Returns when the stream ends.
pub async fn run_discovery_stream<S>(
    mut jobs: S,
    bus: Arc<dyn DiscoveryBus>,
    transport: Arc<dyn Transport>,
    cancels: Arc<CancelSet>,
) where
    S: Stream<Item = DiscoveryJob> + Unpin,
{
    while let Some(job) = jobs.next().await {
        let total = u32::try_from(job.targets.len()).unwrap_or(u32::MAX);
        // Layer 1: a sweep cancelled while it waited its turn must not start. This loop is strictly
        // sequential, so a job can sit on the subscription for the length of the sweep ahead of it —
        // long enough for the operator to give up on it before it has probed anything.
        if cancels.is_cancelled(job.scan_id, Instant::now()) {
            tracing::info!(scan = %job.scan_id, "discovery sweep cancelled before it started");
            publish(
                bus.as_ref(),
                DiscoveryResult {
                    scan_id: job.scan_id,
                    found: Vec::new(),
                    probed: 0,
                    total,
                    done: true,
                    cancelled: true,
                },
            )
            .await;
            continue;
        }
        tracing::info!(
            scan = %job.scan_id,
            targets = job.targets.len(),
            snmp_when_unreachable = job.snmp_when_unreachable,
            "discovery sweep starting"
        );
        // Before the first probe: see the module doc. ⚠️ It carries an empty `found`, and core's
        // `ScanState::apply` *replaces* the candidate list rather than appending to it — so a
        // reordered or duplicated copy landing after real progress would, read literally, empty the
        // operator's table. What refuses it is the regression guard core already had
        // (`result.probed < self.probed`), which predates this message and is not obviously
        // connected to it; there is a test on the core side pinning that it keeps refusing.
        publish(
            bus.as_ref(),
            DiscoveryResult {
                scan_id: job.scan_id,
                found: Vec::new(),
                probed: 0,
                total,
                done: false,
                cancelled: false,
            },
        )
        .await;
        let ctx = SweepCtx {
            candidates: Arc::new(candidates_of(&job)),
            timeout: Duration::from_millis(u64::from(job.timeout_ms)),
            // Per-device credential-probe rate limit (security.md): space attempts and back off
            // after repeated failures so the sweep can never trip a device's account lockout.
            // Conservative defaults; tunable here as SSH/CLI credential probing (which actually
            // locks out) lands.
            rate: LimiterConfig::default(),
            scan_id: job.scan_id,
            cancels: cancels.clone(),
            snmp_when_unreachable: job.snmp_when_unreachable,
            icmp_errors: AtomicUsize::new(0),
        };
        let mut found: Vec<DiscoveredDevice> = Vec::new();
        let mut probed: u32 = 0;
        let mut cancelled = false;

        let chunk_count = job.targets.chunks(PROGRESS_CHUNK).count();
        // Whether a message carrying `done: true` has gone out. 🚨 This is not bookkeeping: core
        // has no timeout, so a sweep that ends without one leaves the scan `cancelling` for the
        // life of the process and the operator watching "stopping…" forever. It shipped that way —
        // the chunk-boundary stop below breaks out *before* the publish, which is the one exit from
        // this loop that used to reach neither the publish inside it nor the one after it.
        let mut terminal_sent = false;
        for (i, chunk) in job.targets.chunks(PROGRESS_CHUNK).enumerate() {
            // Layer 2: skip whole chunks not yet started.
            if ctx.stopping() {
                cancelled = true;
                break;
            }
            let (devices, swept) = sweep_chunk(chunk, &ctx, transport.clone()).await;
            found.extend(devices);
            // The number actually swept, never the chunk length: a chunk abandoned partway must not
            // claim credit for the targets it skipped. `probed` is how core (and the operator)
            // tells a sweep that stopped early from one that finished.
            probed = probed.saturating_add(swept);
            // A short chunk means the fine-grained check fired inside it.
            if swept < u32::try_from(chunk.len()).unwrap_or(u32::MAX) {
                cancelled = true;
            }
            let done = cancelled || i + 1 == chunk_count;
            publish(
                bus.as_ref(),
                DiscoveryResult {
                    scan_id: job.scan_id,
                    found: found.clone(),
                    probed,
                    total,
                    done,
                    cancelled,
                },
            )
            .await;
            terminal_sent = done;
            if cancelled {
                break;
            }
        }
        // The single exit that closes the scan, whichever way the loop ended. Three paths arrive
        // here without having said anything terminal, and they used to be handled by two ad-hoc
        // branches and one oversight: a stop caught at a chunk boundary (the oversight), a sweep
        // with no targets at all, and — in principle — a loop that ran no iterations for any other
        // reason. One statement is what makes "the poller always closes the scan" a property of the
        // function rather than of remembering to repeat it at each way out.
        if !terminal_sent {
            publish(
                bus.as_ref(),
                DiscoveryResult {
                    scan_id: job.scan_id,
                    found: found.clone(),
                    probed,
                    total,
                    done: true,
                    cancelled,
                },
            )
            .await;
        }
        // One line for the whole sweep — see `SweepCtx::icmp_errors`. `warn` rather than `info`
        // because the sweep quietly stopped being the fast one: every target it could not ping was
        // probed with SNMP regardless of the gate, so "the gate is on but the sweep took five
        // minutes" has its explanation here rather than looking like the gate not working.
        let icmp_errors = ctx.icmp_errors.load(Ordering::Relaxed);
        if icmp_errors > 0 {
            tracing::warn!(
                scan = %job.scan_id, icmp_errors, total,
                "discovery could not send ICMP to some targets; the ping gate did not apply to \
                 those, so they were probed with SNMP and this sweep was slower than usual"
            );
        }
        if cancelled {
            tracing::info!(
                scan = %job.scan_id, probed, total, found = found.len(),
                "discovery sweep cancelled"
            );
            continue;
        }
        tracing::info!(scan = %job.scan_id, found = found.len(), "discovery sweep done");
    }
    tracing::warn!("discovery job stream ended");
}

async fn publish(bus: &dyn DiscoveryBus, result: DiscoveryResult) {
    let scan_id = result.scan_id;
    if let Err(e) = bus.publish_discovery_result(result).await {
        tracing::warn!(error = %e, scan = %scan_id, "publish discovery result failed");
    }
}

/// Probe one chunk of targets with bounded concurrency.
///
/// Returns the devices that responded **and how many targets this chunk looked at** — the second
/// value is what keeps `probed` honest when a stop lands mid-chunk.
///
/// **"Looked at" means an ICMP probe was addressed to the target. That is the definition, and it
/// is chosen rather than incidental.** Three cases sit either side of the line, and this doc used
/// to describe only the first — which left the other two reading as bugs:
///
/// * A target the stop reached **before its future began** is *not* counted. Nothing was sent to
///   it, and counting it is exactly how a stopped sweep would claim to have finished the work it
///   abandoned.
/// * A target whose **credential loop ended early** — cut short by the stop (`break 'candidates`)
///   or by the per-device cooldown — **is** counted. Its ICMP probe had already gone out and been
///   awaited before either check could run, so the address really was looked at; only its
///   identification is incomplete, and an incomplete identification is reported as a device with
///   fewer fields, not as an address nobody visited.
/// * A target the **ICMP gate refused** is counted as well. The gate is a decision taken *about* a
///   probed target, not a reason to skip one; `probe_one` returning `None` there means "not a
///   candidate", never "not looked at".
///
/// So `probed` is "addresses this sweep put a packet at", never "addresses it finished with". The
/// property the operator actually reads is unaffected: `probed < total` on a cancelled sweep still
/// means the stop took effect, because the only thing that fails to increment the count is a target
/// that was never sent anything.
async fn sweep_chunk(
    targets: &[IpAddr],
    ctx: &SweepCtx,
    transport: Arc<dyn Transport>,
) -> (Vec<DiscoveredDevice>, u32) {
    let results: Vec<Option<Option<DiscoveredDevice>>> =
        futures::stream::iter(targets.iter().copied())
            .map(|target| {
                let transport = transport.clone();
                async move {
                    // Layer 3, part one: a target this wave has not reached yet is dropped whole.
                    // `buffer_unordered` starts at most SWEEP_CONCURRENCY at a time, so with a big
                    // chunk most of these futures have not begun when a stop arrives.
                    if ctx.stopping() {
                        return None;
                    }
                    Some(probe_one(target, ctx, transport.as_ref()).await)
                }
            })
            .buffer_unordered(SWEEP_CONCURRENCY)
            .collect()
            .await;
    let swept = u32::try_from(results.iter().filter(|r| r.is_some()).count()).unwrap_or(u32::MAX);
    let devices = results.into_iter().flatten().flatten().collect();
    (devices, swept)
}

/// Current Unix time in milliseconds (clock for the per-device probe limiter).
fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

/// Probe one target: ICMP liveness + SNMP identity, trying each candidate in order
/// (first that answers wins). Candidate probes run **sequentially per device** and are gated
/// by a per-device [`CredentialProbeLimiter`] ([`SweepCtx::rate`]): attempts are spaced apart and,
/// after repeated failures, the device is backed off for the rest of the sweep so probing can never
/// trip an account lockout (security.md). Attempted credentials are never logged. Returns a
/// device iff it answered ICMP or SNMP.
///
/// Unless [`SweepCtx::snmp_when_unreachable`] is set, a target that did not answer ICMP returns
/// here without a single SNMP packet — see the module doc for what that buys and what it costs.
async fn probe_one(
    target: IpAddr,
    ctx: &SweepCtx,
    transport: &dyn Transport,
) -> Option<DiscoveredDevice> {
    // Three outcomes, not two, and keeping them apart is the whole of this block. "The host was
    // silent" is what the gate is for. "The probe itself failed" is a fault on *this poller* — no
    // raw socket, no route, an address family it cannot send on — and says nothing whatever about
    // the target, so it must not be read as silence.
    let probe = transport.probe_icmp(target, 1, ctx.timeout).await;
    let could_not_probe = probe.is_err();
    if could_not_probe {
        ctx.icmp_errors.fetch_add(1, Ordering::Relaxed);
    }
    let reachable = probe.map(|p| p.reachable).unwrap_or(false);

    // The ICMP gate (ADR-068 Inc.3). `None` and not a partial device: a silent address that was
    // never asked for its identity is not a candidate, and reporting it as one — `reachable: false`
    // with no identity — would fill the import table with every unassigned address in the subnet.
    //
    // 🚨 `could_not_probe` is the half that shipped missing. Without it a poller whose ICMP is
    // broken reports every subnet as empty — successfully, with no error anywhere — because a failed
    // probe and a silent address were the same value. On a failure the gate simply does not apply:
    // the target is probed exactly as it was before the gate existed. That costs the sweep its
    // speed, which is the correct thing to lose, and the count is reported once at the end.
    if !reachable && !could_not_probe && !ctx.snmp_when_unreachable {
        return None;
    }

    // Keyed by target IP — the device has no NodeId during discovery. One limiter per probe is
    // enough: each target appears once per sweep, and spacing/cooldown only span this target's
    // sequential candidate attempts.
    let mut limiter = CredentialProbeLimiter::<IpAddr>::new(ctx.rate);
    let mut identity = SnmpIdentity::default();
    let mut matched_credential = None;
    'candidates: for cand in ctx.candidates.iter() {
        // Layer 3, part two — the fine-grained stop, and the one that decides how long the button
        // appears to do nothing. The rate limiter spaces attempts 2s apart, so a target with five
        // credentials occupies this loop for ~20s; without a check here a stop would wait out every
        // in-progress target before the chunk boundary could see it.
        //
        // ⚠️ The ICMP probe above is *not* interrupted — it has already been awaited by the time
        // this runs. That is deliberate (a probe in flight is left to time out rather than having
        // its socket dropped), and it is why the worst case is one probe timeout rather than zero.
        if ctx.stopping() {
            break 'candidates;
        }
        // Honour per-device spacing before each attempt; stop the device entirely on cooldown.
        loop {
            match limiter.begin_attempt(target, now_ms()) {
                AttemptDecision::Allow => break,
                AttemptDecision::TooSoon { wait_ms } => {
                    if wait_ms > 0 {
                        tokio::time::sleep(Duration::from_millis(wait_ms.unsigned_abs())).await;
                    }
                }
                AttemptDecision::CoolingDown { .. } => break 'candidates,
            }
        }
        if let Some(id) = try_candidate(target, cand, ctx.timeout, transport).await {
            limiter.record_success(target);
            identity = id;
            matched_credential = cand.cred_ref();
            break;
        }
        limiter.record_failure(target, now_ms());
    }

    if reachable
        || identity.sysdescr.is_some()
        || identity.sysobjectid.is_some()
        || identity.sysname.is_some()
    {
        Some(DiscoveredDevice {
            address: target,
            reachable,
            sysdescr: identity.sysdescr,
            sysname: identity.sysname,
            sysobjectid: identity.sysobjectid,
            matched_credential,
        })
    } else {
        None
    }
}

/// SNMP identity scalars a discovery probe collects from a device (each optional — a device
/// may answer some but not all). `sysObjectID` is the authoritative device-type signal;
/// `sysDescr`/`sysName` are free-form. All are device-supplied — treat as untrusted.
#[derive(Default)]
struct SnmpIdentity {
    sysdescr: Option<String>,
    sysobjectid: Option<String>,
    sysname: Option<String>,
}

/// Try one candidate credential against a target. `Some` (the identity scalars, possibly
/// partial) iff the device answered SNMP under this credential.
async fn try_candidate(
    target: IpAddr,
    cand: &SnmpCandidate,
    timeout: Duration,
    transport: &dyn Transport,
) -> Option<SnmpIdentity> {
    match cand {
        SnmpCandidate::V2c { community, .. } => {
            let bases = [
                SYSDESCR_BASE.to_owned(),
                SYSOBJECTID_BASE.to_owned(),
                SYSNAME_BASE.to_owned(),
            ];
            let rows = transport
                .snmp_walk_strings(target, community, &bases, timeout)
                .await
                .ok()?;
            if rows.is_empty() {
                return None;
            }
            let mut id = SnmpIdentity::default();
            for r in rows {
                match r.oid_base.as_str() {
                    SYSDESCR_BASE => id.sysdescr = Some(r.value),
                    SYSOBJECTID_BASE => id.sysobjectid = Some(r.value),
                    SYSNAME_BASE => id.sysname = Some(r.value),
                    _ => {}
                }
            }
            Some(id)
        }
        SnmpCandidate::V3 { params, .. } => {
            let oids = [
                SYSDESCR_OID.to_owned(),
                SYSOBJECTID_OID.to_owned(),
                SYSNAME_OID.to_owned(),
            ];
            let rows = transport
                .snmp_v3_get_strings(target, params, &oids, timeout)
                .await
                .ok()?;
            if rows.is_empty() {
                return None;
            }
            let mut id = SnmpIdentity::default();
            for r in rows {
                match r.oid.as_str() {
                    SYSDESCR_OID => id.sysdescr = Some(r.value),
                    SYSOBJECTID_OID => id.sysobjectid = Some(r.value),
                    SYSNAME_OID => id.sysname = Some(r.value),
                    _ => {}
                }
            }
            Some(id)
        }
    }
}

/// Subscribe to this pool's sweeps and their stop commands, and start both consumers.
///
/// A new core publishes sweeps pool-scoped; an old core (and the poll-now path) uses the legacy
/// subject — both are merged so either producer is served. The sweep and its cancel are consumed by
/// **separate** tasks on purpose: the sweep loop is strictly sequential, so a cancel riding the same
/// stream would queue behind the very sweep it exists to interrupt.
pub(crate) async fn start(
    bus: &Arc<NatsBus>,
    transport: &Arc<dyn Transport>,
    pool: &str,
    queue: &str,
    shutdown: &CancellationToken,
) -> anyhow::Result<()> {
    let legacy = Box::pin(bus.subscribe_discovery_jobs(queue).await?);
    let pooled = Box::pin(bus.subscribe_discovery_jobs_for_pool(pool, queue).await?);
    let merged = Box::pin(futures::stream::select(legacy, pooled));
    // Stop commands (ADR-068 Inc.2), on the two routes a sweep can arrive by and mirroring the
    // job subscriptions above. **No queue group**, unlike those: core cannot know which poller
    // took a queue-delivered job, so a stop goes to everyone and the `scan_id` decides.
    //
    // Consumed by a task of its own, which is what makes the stop arrive at all: the sweep loop
    // is strictly sequential, so a cancel riding the same stream would queue behind the very
    // sweep it is meant to interrupt.
    let cancels = Arc::new(CancelSet::new());
    let cancel_global = Box::pin(bus.subscribe_discovery_cancels(None).await?);
    let cancel_pooled = Box::pin(bus.subscribe_discovery_cancels(Some(pool)).await?);
    spawn_cancellable(
        shutdown,
        run_cancel_stream(
            Box::pin(futures::stream::select(cancel_global, cancel_pooled)),
            cancels.clone(),
        ),
    );
    let bus = bus.clone();
    let transport = transport.clone();
    spawn_cancellable(
        shutdown,
        run_discovery_stream(merged, bus, transport, cancels),
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::net::Ipv4Addr;
    use yagra_bus::{DiscoveryCredential, DiscoveryV3};
    use yagra_transport::{
        DnsChain, DnsProbeSpec, HttpProbe, HttpProbeSpec, IcmpProbe, SnmpSample, SnmpStringSample,
        SnmpTableSample, SnmpTableString, TransportError,
    };

    /// Discovery never resolves DNS, so both fakes below answer with an empty timed-out chain —
    /// enough to satisfy the trait without pretending to model a resolution.
    fn unused_dns_chain() -> DnsChain {
        DnsChain {
            query: String::new(),
            record_type: yagra_transport::DnsRecordType::A,
            resolver: "unused".to_owned(),
            hops: Vec::new(),
            failure: Some(yagra_common::DnsFailure::Timeout),
            resolve_ms: 0.0,
        }
    }

    /// Answers SNMP only for a specific v2c community and/or v3 user, so tests can assert
    /// which candidate matched.
    /// What the fake's ICMP probe does.
    ///
    /// Three outcomes, not two, because that is the distinction under test: a `bool` can say
    /// "answered" or "did not answer" and has no way to say "the probe never happened". The
    /// production code collapsed those last two for a release, so a test type that cannot express
    /// them apart would be unable to catch it coming back.
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Ping {
        /// Answered the echo.
        Answers,
        /// Reachable in principle; simply did not reply. What the gate exists to skip.
        Silent,
        /// `probe_icmp` itself returned `Err` — no raw socket, no route, wrong address family.
        /// Says nothing about the target.
        Errors,
    }

    struct SelectiveFake {
        ping: Ping,
        good_community: Option<String>,
        good_v3_user: Option<String>,
    }

    #[async_trait]
    impl Transport for SelectiveFake {
        async fn probe_icmp(
            &self,
            _target: IpAddr,
            _count: u8,
            _timeout: Duration,
        ) -> Result<IcmpProbe, TransportError> {
            if self.ping == Ping::Errors {
                return Err(TransportError::Io("no raw socket".to_owned()));
            }
            let up = self.ping == Ping::Answers;
            Ok(IcmpProbe {
                reachable: up,
                rtt_ms: up.then_some(1.0),
                loss_pct: if up { 0.0 } else { 100.0 },
            })
        }

        async fn snmp_get(
            &self,
            _target: IpAddr,
            _community: &str,
            _oids: &[String],
            _timeout: Duration,
        ) -> Result<Vec<SnmpSample>, TransportError> {
            Ok(Vec::new())
        }

        async fn snmp_v3_get(
            &self,
            _target: IpAddr,
            _params: &SnmpV3Params,
            _oids: &[String],
            _timeout: Duration,
        ) -> Result<Vec<SnmpSample>, TransportError> {
            Ok(Vec::new())
        }

        async fn snmp_v3_get_strings(
            &self,
            _target: IpAddr,
            params: &SnmpV3Params,
            oids: &[String],
            _timeout: Duration,
        ) -> Result<Vec<SnmpStringSample>, TransportError> {
            if Some(params.user.as_str()) == self.good_v3_user.as_deref() {
                Ok(oids
                    .iter()
                    .map(|o| SnmpStringSample {
                        oid: o.clone(),
                        value: match o.as_str() {
                            SYSDESCR_OID => "Huawei USG6000".to_owned(),
                            SYSOBJECTID_OID => "1.3.6.1.4.1.2011.2.1".to_owned(),
                            _ => "fw01".to_owned(),
                        },
                    })
                    .collect())
            } else {
                Err(TransportError::Io("authentication failure".to_owned()))
            }
        }

        async fn snmp_walk(
            &self,
            _target: IpAddr,
            _community: &str,
            _column_oids: &[String],
            _timeout: Duration,
        ) -> Result<Vec<SnmpTableSample>, TransportError> {
            Ok(Vec::new())
        }

        async fn snmp_walk_strings(
            &self,
            _target: IpAddr,
            community: &str,
            column_oids: &[String],
            _timeout: Duration,
        ) -> Result<Vec<SnmpTableString>, TransportError> {
            if Some(community) == self.good_community.as_deref() {
                Ok(column_oids
                    .iter()
                    .map(|b| SnmpTableString {
                        oid_base: b.clone(),
                        ifindex: 0,
                        value: match b.as_str() {
                            SYSDESCR_BASE => "Cisco IOS Software".to_owned(),
                            SYSOBJECTID_BASE => "1.3.6.1.4.1.9.1.516".to_owned(),
                            _ => "sw01".to_owned(),
                        },
                    })
                    .collect())
            } else {
                Ok(Vec::new())
            }
        }

        async fn snmp_v3_walk(
            &self,
            _target: IpAddr,
            _params: &SnmpV3Params,
            _column_oids: &[String],
            _timeout: Duration,
        ) -> Result<Vec<SnmpTableSample>, TransportError> {
            Ok(Vec::new())
        }

        async fn snmp_v3_walk_strings(
            &self,
            _target: IpAddr,
            _params: &SnmpV3Params,
            _column_oids: &[String],
            _timeout: Duration,
        ) -> Result<Vec<SnmpTableString>, TransportError> {
            Ok(Vec::new())
        }

        async fn snmp_walk_instances(
            &self,
            _target: IpAddr,
            _community: &str,
            _column_oids: &[String],
            _timeout: Duration,
            _max_rows: usize,
        ) -> Result<Vec<yagra_transport::SnmpInstanceRow>, TransportError> {
            Ok(Vec::new())
        }

        async fn snmp_v3_walk_instances(
            &self,
            _target: IpAddr,
            _params: &SnmpV3Params,
            _column_oids: &[String],
            _timeout: Duration,
            _max_rows: usize,
        ) -> Result<Vec<yagra_transport::SnmpInstanceRow>, TransportError> {
            Ok(Vec::new())
        }

        async fn probe_http(
            &self,
            _spec: &HttpProbeSpec,
            _timeout: Duration,
        ) -> Result<HttpProbe, TransportError> {
            Ok(HttpProbe {
                reachable: false,
                status_code: None,
                response_time_ms: 0.0,
                cert_days_to_expiry: None,
                body: None,
            })
        }

        async fn resolve_dns(
            &self,
            _spec: &DnsProbeSpec,
            _timeout: Duration,
        ) -> Result<DnsChain, TransportError> {
            Ok(unused_dns_chain())
        }

        async fn collect_meraki(
            &self,
            _spec: &yagra_transport::MerakiCollectSpec,
            _timeout: Duration,
        ) -> Result<Vec<yagra_transport::MerakiObservation>, TransportError> {
            Ok(Vec::new())
        }
    }

    fn target() -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1))
    }

    fn v2c_cred(id: Uuid, community: &str) -> DiscoveryCredential {
        DiscoveryCredential {
            cred_ref: id,
            community: Some(community.to_owned()),
            v3: None,
        }
    }

    fn v3_cred(id: Uuid, user: &str) -> DiscoveryCredential {
        DiscoveryCredential {
            cred_ref: id,
            community: None,
            v3: Some(DiscoveryV3 {
                user: user.to_owned(),
                security_level: "auth".to_owned(),
                auth_protocol: Some("sha256".to_owned()),
                auth_key: Some("a-pass".to_owned()),
                priv_protocol: None,
                priv_key: None,
            }),
        }
    }

    /// Rate config that disables spacing and cooldown so the probe tests never actually sleep
    /// (the limiter wiring itself is unit-tested in `yagra-discovery::credential_finder`).
    fn no_rate() -> LimiterConfig {
        LimiterConfig {
            min_interval_ms: 0,
            max_consecutive_failures: u32::MAX,
            cooldown_ms: 0,
        }
    }

    /// An empty cancel set — what every test that is not about stopping wants.
    fn no_cancels() -> Arc<CancelSet> {
        Arc::new(CancelSet::new())
    }

    /// A cancel set already holding `scan_id`, as if the stop had arrived first.
    fn cancelled(scan_id: Uuid) -> Arc<CancelSet> {
        let set = Arc::new(CancelSet::new());
        set.insert(scan_id, Instant::now());
        set
    }

    /// A sweep context for the probe tests: nothing sleeps, nothing is stopping, and SNMP is tried
    /// everywhere — the last of which is what every test written before the ICMP gate assumed, so
    /// keeping it the helper's default is what stops the gate from quietly rewriting their subject.
    /// Tests that are *about* the gate set the field themselves.
    fn ctx(candidates: Vec<SnmpCandidate>) -> SweepCtx {
        SweepCtx {
            candidates: Arc::new(candidates),
            timeout: Duration::from_millis(100),
            rate: no_rate(),
            scan_id: Uuid::nil(),
            cancels: no_cancels(),
            snmp_when_unreachable: true,
            icmp_errors: AtomicUsize::new(0),
        }
    }

    fn job(credentials: Vec<DiscoveryCredential>, communities: Vec<String>) -> DiscoveryJob {
        DiscoveryJob {
            scan_id: Uuid::nil(),
            targets: vec![target()],
            communities,
            credentials,
            timeout_ms: 100,
            snmp_when_unreachable: true,
        }
    }

    #[tokio::test]
    async fn stored_v2c_credential_match_is_reported_by_reference() {
        let id = Uuid::from_u128(1);
        let cands = candidates_of(&job(vec![v2c_cred(id, "secret")], vec![]));
        let fake = SelectiveFake {
            ping: Ping::Answers,
            good_community: Some("secret".to_owned()),
            good_v3_user: None,
        };
        let d = probe_one(target(), &ctx(cands), &fake)
            .await
            .expect("device answers");
        assert_eq!(d.matched_credential, Some(id));
        assert_eq!(d.sysdescr.as_deref(), Some("Cisco IOS Software"));
        assert_eq!(d.sysname.as_deref(), Some("sw01"));
        // sysObjectID (OID-typed) is rendered dotted and carried for classification.
        assert_eq!(d.sysobjectid.as_deref(), Some("1.3.6.1.4.1.9.1.516"));
    }

    #[tokio::test]
    async fn v3_credential_match_is_reported_by_reference() {
        let v2 = Uuid::from_u128(1);
        let v3 = Uuid::from_u128(2);
        // The v2c candidate is tried first but doesn't answer; the v3 one does.
        let cands = candidates_of(&job(
            vec![v2c_cred(v2, "wrong"), v3_cred(v3, "monitor")],
            vec![],
        ));
        let fake = SelectiveFake {
            ping: Ping::Silent,
            good_community: None,
            good_v3_user: Some("monitor".to_owned()),
        };
        let d = probe_one(target(), &ctx(cands), &fake)
            .await
            .expect("device answers v3");
        assert_eq!(d.matched_credential, Some(v3));
        assert_eq!(d.sysdescr.as_deref(), Some("Huawei USG6000"));
        assert_eq!(d.sysobjectid.as_deref(), Some("1.3.6.1.4.1.2011.2.1"));
        assert!(!d.reachable, "SNMP-only answer still reports the device");
    }

    #[tokio::test]
    async fn adhoc_community_match_has_no_credential_ref() {
        let cands = candidates_of(&job(vec![], vec!["public".to_owned()]));
        let fake = SelectiveFake {
            ping: Ping::Answers,
            good_community: Some("public".to_owned()),
            good_v3_user: None,
        };
        let d = probe_one(target(), &ctx(cands), &fake)
            .await
            .expect("device answers");
        assert_eq!(d.matched_credential, None);
        assert!(d.sysdescr.is_some());
    }

    #[tokio::test]
    async fn stored_credentials_are_tried_before_adhoc_communities() {
        // Both the stored credential and the ad-hoc community would answer (same value):
        // the stored one must win so import binds the registered secret by reference.
        let id = Uuid::from_u128(7);
        let cands = candidates_of(&job(
            vec![v2c_cred(id, "shared")],
            vec!["shared".to_owned()],
        ));
        let fake = SelectiveFake {
            ping: Ping::Answers,
            good_community: Some("shared".to_owned()),
            good_v3_user: None,
        };
        let d = probe_one(target(), &ctx(cands), &fake)
            .await
            .expect("device answers");
        assert_eq!(d.matched_credential, Some(id));
    }

    #[tokio::test]
    async fn silent_target_yields_nothing() {
        let cands = candidates_of(&job(vec![], vec!["public".to_owned()]));
        let fake = SelectiveFake {
            ping: Ping::Silent,
            good_community: None,
            good_v3_user: None,
        };
        assert!(probe_one(target(), &ctx(cands), &fake).await.is_none());
    }

    // ── The ICMP gate (ADR-068 Increment 3) ──────────────────────────────────
    //
    // Both directions, deliberately. A suite that only shows the gate refusing would pass just as
    // happily if the gate refused *everything* — the failure mode this repo has already shipped
    // once — so the accepting cases below are not padding.

    #[tokio::test]
    async fn a_silent_address_is_never_asked_for_snmp() {
        // The whole point: 246 of a /24's 254 addresses answer nothing, and each one used to cost
        // three rate-limited SNMP attempts on top of its ICMP timeout.
        //
        // The fake answers `public` on SNMP, so `None` here is not "SNMP was tried and failed" —
        // it is the only outcome consistent with SNMP never having been attempted at all.
        let cands = candidates_of(&job(vec![], vec!["public".to_owned()]));
        let fake = SelectiveFake {
            ping: Ping::Silent,
            good_community: Some("public".to_owned()),
            good_v3_user: None,
        };
        let mut c = ctx(cands);
        c.snmp_when_unreachable = false;
        assert!(
            probe_one(target(), &c, &fake).await.is_none(),
            "a target that did not answer ping must not be probed, and must not be reported as a \
             candidate either — every unassigned address in the subnet would otherwise land in the \
             import table"
        );
    }

    #[tokio::test]
    async fn the_gate_only_looks_at_silence() {
        // An address that answers ping is swept exactly as before. Stated separately from the
        // refusal above because "the gate is on" and "discovery still works" have to both be true
        // in one run for the default to be shippable.
        let id = Uuid::from_u128(3);
        let cands = candidates_of(&job(vec![v2c_cred(id, "secret")], vec![]));
        let fake = SelectiveFake {
            ping: Ping::Answers,
            good_community: Some("secret".to_owned()),
            good_v3_user: None,
        };
        let mut c = ctx(cands);
        c.snmp_when_unreachable = false;
        let d = probe_one(target(), &c, &fake)
            .await
            .expect("a reachable device is still identified with the gate on");
        assert_eq!(d.matched_credential, Some(id));
        assert_eq!(d.sysdescr.as_deref(), Some("Cisco IOS Software"));
    }

    #[tokio::test]
    async fn a_probe_that_could_not_be_sent_does_not_count_as_silence() {
        // 🚨 The gate's third case, and the one that shipped wrong. `probe_icmp` returning `Err`
        // is a fault on this poller — no raw socket, no route, an address family it cannot send on
        // — and it used to collapse into the same `reachable == false` a silent host produces. With
        // the gate on, that meant a poller whose ICMP was broken swept an entire subnet without
        // sending one SNMP packet and reported it as empty, with no error logged anywhere and the
        // scan finishing successfully. The whole failure is invisible: every layer says it worked.
        //
        // So: gate ON, ICMP broken, and the device must still be found.
        let cands = candidates_of(&job(vec![], vec!["public".to_owned()]));
        let fake = SelectiveFake {
            ping: Ping::Errors,
            good_community: Some("public".to_owned()),
            good_v3_user: None,
        };
        let mut c = ctx(cands);
        c.snmp_when_unreachable = false;
        let d = probe_one(target(), &c, &fake)
            .await
            .expect("a target whose ICMP probe could not be sent must still be asked over SNMP");
        assert_eq!(d.sysdescr.as_deref(), Some("Cisco IOS Software"));
        assert!(
            !d.reachable,
            "and is still reported honestly as not having answered ping"
        );
        assert_eq!(
            c.icmp_errors.load(Ordering::Relaxed),
            1,
            "and the sweep counts it, so the one warning at the end can say how many there were — \
             without that count the operator sees only that the sweep got slow"
        );
    }

    #[tokio::test]
    async fn opting_in_still_finds_a_device_that_blocks_icmp() {
        // The capability the gate would otherwise delete, and the reason it is a switch rather than
        // a rule: a firewall that drops echo requests and answers SNMP is a real device on a real
        // network, and this is the only way discovery reaches it.
        let cands = candidates_of(&job(vec![], vec!["public".to_owned()]));
        let fake = SelectiveFake {
            ping: Ping::Silent,
            good_community: Some("public".to_owned()),
            good_v3_user: None,
        };
        let d = probe_one(target(), &ctx(cands), &fake)
            .await
            .expect("with the box ticked, an ICMP-blocked device is still discovered");
        assert!(
            !d.reachable,
            "and is reported honestly as not answering ping"
        );
        assert_eq!(d.sysdescr.as_deref(), Some("Cisco IOS Software"));
    }

    /// Wiring check: once a device hits the consecutive-failure limit it is backed off, so the
    /// remaining candidates are never tried (lockout protection, security.md).
    #[tokio::test]
    async fn cooldown_backs_off_remaining_candidates() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        // Never answers; counts how many credential attempts actually reached the transport.
        struct CountingFake {
            attempts: Arc<AtomicUsize>,
        }
        #[async_trait]
        impl Transport for CountingFake {
            async fn probe_icmp(
                &self,
                _t: IpAddr,
                _c: u8,
                _to: Duration,
            ) -> Result<IcmpProbe, TransportError> {
                Ok(IcmpProbe {
                    reachable: false,
                    rtt_ms: None,
                    loss_pct: 100.0,
                })
            }
            async fn snmp_get(
                &self,
                _t: IpAddr,
                _c: &str,
                _o: &[String],
                _to: Duration,
            ) -> Result<Vec<SnmpSample>, TransportError> {
                Ok(Vec::new())
            }
            async fn snmp_v3_get(
                &self,
                _t: IpAddr,
                _p: &SnmpV3Params,
                _o: &[String],
                _to: Duration,
            ) -> Result<Vec<SnmpSample>, TransportError> {
                Ok(Vec::new())
            }
            async fn snmp_v3_get_strings(
                &self,
                _t: IpAddr,
                _p: &SnmpV3Params,
                _o: &[String],
                _to: Duration,
            ) -> Result<Vec<SnmpStringSample>, TransportError> {
                Ok(Vec::new())
            }
            async fn snmp_walk(
                &self,
                _t: IpAddr,
                _c: &str,
                _o: &[String],
                _to: Duration,
            ) -> Result<Vec<SnmpTableSample>, TransportError> {
                Ok(Vec::new())
            }
            async fn snmp_walk_strings(
                &self,
                _t: IpAddr,
                _c: &str,
                _o: &[String],
                _to: Duration,
            ) -> Result<Vec<SnmpTableString>, TransportError> {
                self.attempts.fetch_add(1, Ordering::SeqCst);
                Ok(Vec::new()) // empty ⇒ this candidate "failed"
            }
            async fn snmp_v3_walk(
                &self,
                _t: IpAddr,
                _p: &SnmpV3Params,
                _o: &[String],
                _to: Duration,
            ) -> Result<Vec<SnmpTableSample>, TransportError> {
                Ok(Vec::new())
            }
            async fn snmp_v3_walk_strings(
                &self,
                _t: IpAddr,
                _p: &SnmpV3Params,
                _o: &[String],
                _to: Duration,
            ) -> Result<Vec<SnmpTableString>, TransportError> {
                Ok(Vec::new())
            }
            async fn snmp_walk_instances(
                &self,
                _t: IpAddr,
                _c: &str,
                _o: &[String],
                _to: Duration,
                _max: usize,
            ) -> Result<Vec<yagra_transport::SnmpInstanceRow>, TransportError> {
                Ok(Vec::new())
            }
            async fn snmp_v3_walk_instances(
                &self,
                _t: IpAddr,
                _p: &SnmpV3Params,
                _o: &[String],
                _to: Duration,
                _max: usize,
            ) -> Result<Vec<yagra_transport::SnmpInstanceRow>, TransportError> {
                Ok(Vec::new())
            }
            async fn probe_http(
                &self,
                _spec: &HttpProbeSpec,
                _to: Duration,
            ) -> Result<HttpProbe, TransportError> {
                Ok(HttpProbe {
                    reachable: false,
                    status_code: None,
                    response_time_ms: 0.0,
                    cert_days_to_expiry: None,
                    body: None,
                })
            }

            async fn resolve_dns(
                &self,
                _spec: &DnsProbeSpec,
                _to: Duration,
            ) -> Result<DnsChain, TransportError> {
                Ok(unused_dns_chain())
            }

            async fn collect_meraki(
                &self,
                _spec: &yagra_transport::MerakiCollectSpec,
                _to: Duration,
            ) -> Result<Vec<yagra_transport::MerakiObservation>, TransportError> {
                Ok(Vec::new())
            }
        }

        let attempts = Arc::new(AtomicUsize::new(0));
        let fake = CountingFake {
            attempts: attempts.clone(),
        };
        // Five candidates that all fail; cooldown trips after 2 consecutive failures.
        let cands = candidates_of(&job(
            vec![],
            vec![
                "a".to_owned(),
                "b".to_owned(),
                "c".to_owned(),
                "d".to_owned(),
                "e".to_owned(),
            ],
        ));
        let mut c = ctx(cands);
        c.rate = LimiterConfig {
            min_interval_ms: 0,
            max_consecutive_failures: 2,
            cooldown_ms: 60_000,
        };
        let d = probe_one(target(), &c, &fake).await;
        assert!(d.is_none(), "no device answered");
        assert_eq!(
            attempts.load(Ordering::SeqCst),
            2,
            "device is backed off after the failure limit instead of trying all five"
        );
    }

    /// The sweep loop itself — untestable until the bus became a trait, because it published
    /// through the NATS type. What matters to core is the *shape* of the progress stream: one
    /// result per chunk, `probed` monotone, `found` cumulative, and exactly one terminal `done`.
    #[tokio::test]
    async fn a_sweep_publishes_one_cumulative_result_per_chunk_and_one_terminal_done() {
        use yagra_bus::InMemoryBus;

        let bus = Arc::new(InMemoryBus::new(64));
        let mut rx = bus.subscribe_discovery_results();

        // Two chunks' worth of targets, all answering, so `found` must grow across the boundary.
        let count = u8::try_from(PROGRESS_CHUNK).unwrap() + 5;
        let targets: Vec<IpAddr> = (1..=count)
            .map(|i| IpAddr::V4(Ipv4Addr::new(10, 0, 0, i)))
            .collect();
        let total = u32::from(count);
        let job = DiscoveryJob {
            scan_id: Uuid::from_u128(11),
            targets,
            communities: vec!["public".to_owned()],
            credentials: Vec::new(),
            timeout_ms: 100,
            snmp_when_unreachable: true,
        };
        let transport = Arc::new(SelectiveFake {
            ping: Ping::Answers,
            good_community: Some("public".to_owned()),
            good_v3_user: None,
        });

        run_discovery_stream(
            futures::stream::iter(vec![job]),
            bus.clone() as Arc<dyn DiscoveryBus>,
            transport,
            no_cancels(),
        )
        .await;

        let mut results = Vec::new();
        while let Ok(r) = rx.try_recv() {
            results.push(r);
        }
        assert_eq!(
            results.len(),
            3,
            "the 'we have it' message, then one progress result per chunk"
        );
        // The zero-progress message (ADR-068 Inc.4). It is what core turns into `Running`, so a
        // sweep that never sends it stays `Queued` — which is the whole point, and why this is
        // asserted rather than skipped past by bumping an index.
        assert!(!results[0].done);
        assert_eq!(
            (results[0].probed, results[0].total),
            (0, total),
            "it announces the size of the work and no progress at all"
        );
        assert!(results[0].found.is_empty());

        assert!(!results[1].done);
        assert_eq!(results[1].probed, u32::try_from(PROGRESS_CHUNK).unwrap());
        assert_eq!(results[1].found.len(), PROGRESS_CHUNK);
        assert!(results[2].done, "the last chunk closes the scan");
        assert_eq!(results[2].probed, total);
        assert_eq!(results[2].total, total);
        // Cumulative, not per-chunk: core replaces its candidate list wholesale from each partial,
        // so a delta here would make the UI drop everything found before the final chunk.
        assert_eq!(results[2].found.len(), usize::from(count));
    }

    /// A sweep with no targets still has to close, or core's scan sits at "scanning" forever.
    #[tokio::test]
    async fn an_empty_sweep_still_completes_the_scan() {
        use yagra_bus::InMemoryBus;

        let bus = Arc::new(InMemoryBus::new(8));
        let mut rx = bus.subscribe_discovery_results();
        let job = DiscoveryJob {
            scan_id: Uuid::from_u128(12),
            targets: Vec::new(),
            communities: Vec::new(),
            credentials: Vec::new(),
            timeout_ms: 100,
            snmp_when_unreachable: true,
        };
        let transport = Arc::new(SelectiveFake {
            ping: Ping::Silent,
            good_community: None,
            good_v3_user: None,
        });

        run_discovery_stream(
            futures::stream::iter(vec![job]),
            bus.clone() as Arc<dyn DiscoveryBus>,
            transport,
            no_cancels(),
        )
        .await;

        let first = rx
            .try_recv()
            .expect("even an empty sweep says it was picked up");
        assert!(!first.done, "being taken off the bus is not being finished");
        let r = rx.try_recv().expect("the degenerate sweep still reports");
        assert!(r.done);
        assert_eq!((r.probed, r.total), (0, 0));
        assert!(rx.try_recv().is_err(), "and nothing after the terminal one");
    }

    // ── Stopping a sweep (ADR-068 Increment 2) ───────────────────────────────

    #[test]
    fn the_cancel_set_forgets_and_is_bounded() {
        // Core's scan map shipped with neither a TTL nor a cap and grew for the life of the
        // process. This is the same structure on the poller, where nobody would ever look.
        let set = CancelSet::new();
        let now = Instant::now();
        let old = Uuid::from_u128(1);
        set.insert(old, now - CANCEL_TTL - Duration::from_secs(1));
        assert!(
            !set.is_cancelled(old, now),
            "an id past its TTL must not keep cancelling sweeps that reuse nothing"
        );

        let fresh = Uuid::from_u128(2);
        set.insert(fresh, now);
        assert!(set.is_cancelled(fresh, now));

        for i in 0..(MAX_CANCELS + 10) {
            set.insert(Uuid::from_u128(1000 + i as u128), now);
        }
        assert!(
            set.inner.lock().unwrap().len() <= MAX_CANCELS,
            "the set must stay bounded even if stops arrive in a loop"
        );
    }

    #[tokio::test]
    async fn a_sweep_cancelled_before_it_starts_probes_nothing_and_still_reports() {
        // The queue case. This loop is sequential, so a job can wait behind the sweep ahead of it
        // for minutes — long enough for the operator to give up before it has touched the network.
        // The terminal message is not optional: without it core waits for a sweep that will never
        // speak, and the UI sits on "stopping…" forever.
        let bus = Arc::new(yagra_bus::InMemoryBus::new(16));
        let mut rx = bus.subscribe_discovery_results();
        let scan = Uuid::from_u128(42);
        let mut job = job(vec![], vec!["public".to_owned()]);
        job.scan_id = scan;
        job.targets = (1..=64)
            .map(|i| IpAddr::V4(std::net::Ipv4Addr::new(10, 0, 0, i)))
            .collect();
        // Answers every target, so a single probe would show up as a candidate.
        let transport = Arc::new(SelectiveFake {
            ping: Ping::Answers,
            good_community: Some("public".to_owned()),
            good_v3_user: None,
        });

        run_discovery_stream(
            futures::stream::iter(vec![job]),
            bus.clone() as Arc<dyn DiscoveryBus>,
            transport,
            cancelled(scan),
        )
        .await;

        let r = rx.try_recv().expect("a cancelled sweep still reports");
        assert!(r.done);
        assert!(
            r.cancelled,
            "and says it was cancelled rather than finished"
        );
        assert_eq!(r.probed, 0);
        assert_eq!(r.total, 64, "the total is still what was asked for");
        assert!(rx.try_recv().is_err(), "exactly one message");
        assert!(
            r.found.is_empty(),
            "this fake answers every target, so a single candidate would mean a packet went to the \
             network for a sweep that was already stopped"
        );
    }

    /// Deterministically records a stop on the Nth ICMP probe, so a cancel can be made to land
    /// exactly at a chunk boundary. Everything else is delegated: this exists to control *when* the
    /// stop arrives, not to model a device.
    ///
    /// WHY `at = PROGRESS_CHUNK`: that probe is the last one `buffer_unordered` starts in the first
    /// chunk, so the chunk still completes whole (`swept == chunk.len()`, no short-chunk signal) and
    /// the next iteration's layer-2 check is the first thing to see the stop. Let the cancel land
    /// earlier and it takes the layer-3 exit, which was never broken -- so the test would pass
    /// against the bug it exists to catch.
    struct CancelAtProbe {
        inner: SelectiveFake,
        at: usize,
        seen: std::sync::atomic::AtomicUsize,
        set: Arc<CancelSet>,
        scan: Uuid,
    }

    #[async_trait]
    impl Transport for CancelAtProbe {
        async fn probe_icmp(
            &self,
            target: IpAddr,
            count: u8,
            timeout: Duration,
        ) -> Result<IcmpProbe, TransportError> {
            let n = self.seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
            if n == self.at {
                self.set.insert(self.scan, Instant::now());
            }
            self.inner.probe_icmp(target, count, timeout).await
        }
        async fn snmp_get(
            &self,
            t: IpAddr,
            c: &str,
            o: &[String],
            to: Duration,
        ) -> Result<Vec<SnmpSample>, TransportError> {
            self.inner.snmp_get(t, c, o, to).await
        }
        async fn snmp_v3_get(
            &self,
            t: IpAddr,
            p: &SnmpV3Params,
            o: &[String],
            to: Duration,
        ) -> Result<Vec<SnmpSample>, TransportError> {
            self.inner.snmp_v3_get(t, p, o, to).await
        }
        async fn snmp_v3_get_strings(
            &self,
            t: IpAddr,
            p: &SnmpV3Params,
            o: &[String],
            to: Duration,
        ) -> Result<Vec<SnmpStringSample>, TransportError> {
            self.inner.snmp_v3_get_strings(t, p, o, to).await
        }
        async fn snmp_walk(
            &self,
            t: IpAddr,
            c: &str,
            o: &[String],
            to: Duration,
        ) -> Result<Vec<SnmpTableSample>, TransportError> {
            self.inner.snmp_walk(t, c, o, to).await
        }
        async fn snmp_walk_strings(
            &self,
            t: IpAddr,
            c: &str,
            o: &[String],
            to: Duration,
        ) -> Result<Vec<SnmpTableString>, TransportError> {
            self.inner.snmp_walk_strings(t, c, o, to).await
        }
        async fn snmp_v3_walk(
            &self,
            t: IpAddr,
            p: &SnmpV3Params,
            o: &[String],
            to: Duration,
        ) -> Result<Vec<SnmpTableSample>, TransportError> {
            self.inner.snmp_v3_walk(t, p, o, to).await
        }
        async fn snmp_v3_walk_strings(
            &self,
            t: IpAddr,
            p: &SnmpV3Params,
            o: &[String],
            to: Duration,
        ) -> Result<Vec<SnmpTableString>, TransportError> {
            self.inner.snmp_v3_walk_strings(t, p, o, to).await
        }
        async fn snmp_walk_instances(
            &self,
            t: IpAddr,
            c: &str,
            o: &[String],
            to: Duration,
            m: usize,
        ) -> Result<Vec<yagra_transport::SnmpInstanceRow>, TransportError> {
            self.inner.snmp_walk_instances(t, c, o, to, m).await
        }
        async fn snmp_v3_walk_instances(
            &self,
            t: IpAddr,
            p: &SnmpV3Params,
            o: &[String],
            to: Duration,
            m: usize,
        ) -> Result<Vec<yagra_transport::SnmpInstanceRow>, TransportError> {
            self.inner.snmp_v3_walk_instances(t, p, o, to, m).await
        }
        async fn probe_http(
            &self,
            spec: &HttpProbeSpec,
            to: Duration,
        ) -> Result<HttpProbe, TransportError> {
            self.inner.probe_http(spec, to).await
        }
        async fn resolve_dns(
            &self,
            spec: &DnsProbeSpec,
            to: Duration,
        ) -> Result<DnsChain, TransportError> {
            self.inner.resolve_dns(spec, to).await
        }
        async fn collect_meraki(
            &self,
            spec: &yagra_transport::MerakiCollectSpec,
            to: Duration,
        ) -> Result<Vec<yagra_transport::MerakiObservation>, TransportError> {
            self.inner.collect_meraki(spec, to).await
        }
    }

    /// The bug this file's own doc warned about and did not test, found on a real deployment: press
    /// Stop, the poller logs `discovery sweep cancelled probed=32 total=254`, and the screen sits on
    /// "stopping..." until the core process is restarted. The sweep stopped correctly and never said
    /// so -- the chunk-boundary exit broke out before the publish.
    ///
    /// What the neighbouring test pins is that a short chunk *counts* honestly. What it left
    /// unproven, in writing, was "the wiring -- that the loop propagates a short chunk into
    /// `cancelled` and stops". This is that wiring, and the answer was no.
    #[tokio::test]
    async fn a_stop_caught_between_chunks_still_closes_the_scan() {
        let bus = Arc::new(yagra_bus::InMemoryBus::new(16));
        let mut rx = bus.subscribe_discovery_results();
        let scan = Uuid::from_u128(77);
        let cancels = Arc::new(CancelSet::new());

        // Two chunks: the first full, the second holding the remainder.
        let count = u8::try_from(PROGRESS_CHUNK).unwrap() + 8;
        let mut job = job(vec![], vec!["public".to_owned()]);
        job.scan_id = scan;
        job.targets = (1..=count)
            .map(|i| IpAddr::V4(Ipv4Addr::new(10, 0, 0, i)))
            .collect();

        let transport = Arc::new(CancelAtProbe {
            inner: SelectiveFake {
                ping: Ping::Answers,
                good_community: Some("public".to_owned()),
                good_v3_user: None,
            },
            at: PROGRESS_CHUNK,
            seen: std::sync::atomic::AtomicUsize::new(0),
            set: cancels.clone(),
            scan,
        });

        run_discovery_stream(
            futures::stream::iter(vec![job]),
            bus.clone() as Arc<dyn DiscoveryBus>,
            transport,
            cancels,
        )
        .await;

        let mut results = Vec::new();
        while let Ok(r) = rx.try_recv() {
            results.push(r);
        }
        let last = results.last().expect("the sweep said something");
        assert!(
            last.done,
            "core has no timeout: a sweep that ends without `done` leaves the scan `cancelling` for \
             the life of the process, which is exactly what the operator saw"
        );
        assert!(
            last.cancelled,
            "and it must say it was cancelled, or the scan resolves to `done` and the screen claims \
             a stopped sweep finished"
        );
        assert_eq!(
            last.probed,
            u32::try_from(PROGRESS_CHUNK).unwrap(),
            "one whole chunk was probed and the second never started -- `probed < total` is the \
             operator's only evidence that the stop took effect"
        );
        assert!(last.probed < last.total);
        assert_eq!(
            results.iter().filter(|r| r.done).count(),
            1,
            "exactly one terminal message: the guard must not let the post-loop publish duplicate a \
             `done` the loop already sent"
        );
    }

    /// The honest-count property, at the level where it is decided.
    ///
    /// `probed < total` on a cancelled sweep is the **only** evidence an operator has that the stop
    /// took effect: a sweep that abandoned 200 addresses while claiming to have probed them is
    /// indistinguishable from one that finished. So the count has to come from what was actually
    /// swept, not from the chunk's length.
    ///
    /// ⚠️ Tested here rather than through `run_discovery_stream`, and the reason is worth keeping:
    /// a test that cancels *while* the loop runs has to win a race against it, and against a fake
    /// transport that answers instantly there is no window to win. Such a test passes or fails on
    /// scheduling rather than on behaviour, which is worse than no test. **What this leaves
    /// unproven is the wiring** — that the loop propagates a short chunk into `cancelled` and stops.
    /// That is read, not tested, and it is the thing to watch on the real deployment.
    #[tokio::test]
    async fn a_chunk_counts_what_it_probed_not_what_it_was_given() {
        let scan = Uuid::from_u128(11);
        let transport: Arc<dyn Transport> = Arc::new(SelectiveFake {
            ping: Ping::Answers,
            good_community: Some("public".to_owned()),
            good_v3_user: None,
        });
        let targets: Vec<IpAddr> = (1..=8)
            .map(|i| IpAddr::V4(std::net::Ipv4Addr::new(10, 0, 0, i)))
            .collect();

        let mut c = ctx(candidates_of(&job(vec![], vec!["public".to_owned()])));
        c.scan_id = scan;
        let (devices, swept) = sweep_chunk(&targets, &c, transport.clone()).await;
        assert_eq!(
            swept, 8,
            "an uninterrupted chunk probes everything it was given"
        );
        assert_eq!(devices.len(), 8);

        c.cancels = cancelled(scan);
        let (devices, swept) = sweep_chunk(&targets, &c, transport).await;
        assert_eq!(
            swept, 0,
            "a cancelled chunk counts nothing — the caller adds this to `probed`, so counting \
             skipped targets here is exactly how a stopped sweep would claim to have finished"
        );
        assert!(
            devices.is_empty(),
            "and this fake answers every target, so a single device would mean one was probed"
        );
    }
}
