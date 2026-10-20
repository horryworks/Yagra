// SPDX-License-Identifier: AGPL-3.0-only
//! Discovery sweep execution (Phase C) — the poller side.
//!
//! Consumes [`DiscoveryJob`]s off the bus and probes each target for ICMP liveness + SNMP
//! identity (`sysDescr.0` / `sysName.0`), trying the job's candidate credentials (stored
//! v2c/v3, resolved by core) and ad-hoc communities. Runs on the poller because ICMP needs
//! the raw socket. Progress is published as **cumulative** partial results after every
//! [`PROGRESS_CHUNK`] finished targets, so core can show the sweep advancing; the final message
//! carries `done: true`.
//!
//! ## "We have it" comes before "we found something" (ADR-068 Increment 4)
//!
//! The first thing a sweep publishes is a zero-progress result, before it probes a single address.
//! A job can still wait before that: jobs run side by side since ADR-204 Inc.3, but at most
//! [`MAX_CONCURRENT_JOBS`] of them, and each target waits for one of the poller's
//! [`SWEEP_CONCURRENCY`] slots. Core used to mark a scan `Running` the moment it accepted it, so
//! "queued behind another sweep", "no poller ever took this" and "sweeping, and everything out
//! there is silent" were one sentence on the operator's screen. This message is what core turns
//! into `Running`; a scan nobody sends it for stays `Queued` and says so.
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
//! 2. before each target is started — the coarse-grained stop;
//! 3. before each credential attempt on a target — the fine-grained one.
//!
//! The second used to be "at each chunk boundary". Since ADR-204 there are no chunk boundaries to
//! wait at — targets run as one stream — so the check moved to the start of each target, which is
//! where a stop between chunks used to take effect anyway.
//!
//! **The third is not belt-and-braces.** A target costs one ICMP timeout plus one attempt per
//! credential it has to try — up to **every** credential the operator selected (ADR-161) — so with
//! five credentials a single target can take ~22s. Stopping only between targets would leave
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
//! `cancelling` until the process restarts — and the old chunk-boundary stop used to `break` out of
//! the loop *before* the publish inside it, so a stop that landed between chunks ended the sweep in
//! silence and left the operator watching "stopping…" indefinitely. Layers 1 and 3 each published
//! from their own exit and were fine, which is exactly why nothing looked wrong. The terminal
//! publish is **one statement after the loop**, and the loop itself never sends `done: true`, so
//! closing the scan is a property of the function rather than something each exit must remember.
//!
//! ## Which credential first, and how many targets at once (ADR-204)
//!
//! Devices on one subnet nearly always share a credential, and a wrong v2c community costs about
//! four seconds of silence. So the sweep keeps a **hit tally** — how many targets each candidate
//! has matched so far in this job — and a target picks its next candidate from it **at every
//! attempt**, not once up front: a target already one wrong guess in switches to the credential
//! another target has just matched. Ties keep the operator's order, and stored credentials always
//! come before typed-in communities ([`next_candidate`]).
//!
//! ⚠️ What this changes for the operator: on a device where **two** candidates both answer, the one
//! bound at import is now the one that has matched most often on this sweep, not the one listed
//! first. Within one target the attempts stay strictly sequential and spaced (security.md) — what
//! runs concurrently is targets, never credentials against one device. Firing every credential at a
//! device at once would make a device that locks out a failing source worse, not better.
//!
//! The targets themselves run as **one stream**, with a progress result after every
//! [`PROGRESS_CHUNK`] completions. They used to run in 32-target chunks that each waited for their
//! slowest member, so one device working through five wrong credentials held 31 finished ones and
//! every target after them.
//!
//! ## Jobs side by side, slots shared (ADR-204 Inc.3)
//!
//! Detect on an unregistered device is a one-address job of its own, and jobs used to run strictly
//! one after another: five Detects pressed together finished one at a time, and a Detect pressed
//! during a range sweep waited for the whole sweep. Jobs now run concurrently, up to
//! [`MAX_CONCURRENT_JOBS`], and the [`SWEEP_CONCURRENCY`] limit is **the poller's, not each job's**:
//! every target takes one slot from a shared, first-come semaphore before it is probed and returns
//! it when done. So a Detect pressed mid-sweep starts at the next free slot, and the poller never
//! has more than [`SWEEP_CONCURRENCY`] devices in hand however many jobs are running. What one
//! job sends one device is unchanged: one credential at a time, spaced. Two jobs that both hold an
//! address (a Detect pressed on a device inside a range being swept) each run their own spaced
//! sequence at it, side by side; SNMP has no account to lock out, so that costs one extra request
//! every couple of seconds and nothing more.
//!
//! ## The ICMP gate (ADR-068 Increment 3)
//!
//! By default a target that does not answer ICMP is **not** tried with SNMP. That is where a sweep's
//! time goes: on a /24 the overwhelming majority of addresses are unassigned, and each one used to
//! cost one ICMP timeout *plus* an SNMP attempt per selected credential, spaced
//! [`LimiterConfig::min_interval_ms`] apart. Measured
//! on the test network: 254 addresses, 8 devices, **5m21s**, essentially all of it spent asking
//! empty addresses for their `sysDescr`.
//!
//! ⚠️ **That measurement predates ADR-161 and was taken under a cap of three credentials.** The
//! gate is now the *only* thing bounding a sweep's cost — every selected credential is tried on
//! every target that reaches the loop — so with the box below ticked and five credentials
//! selected, expect **roughly 1.6× that figure: arithmetic off the old measurement, not a new
//! one** (three attempts per silent address became five). The gate off is the common case and is unaffected:
//! only real devices pay, and they pay once. What was removed was not a cost control that worked;
//! it was a cooldown that deleted the operator's 4th and 5th credentials while protecting nothing
//! (`LimiterConfig::snmp_sweep`'s doc has the argument).
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
use std::time::{Duration, Instant};

use futures::stream::{Stream, StreamExt};
use tokio::sync::Semaphore;
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
/// sysName.0 instance OID — the one the identity probe reads too.
const SYSNAME_OID: &str = yagra_discovery::os_version::OID_SYS_NAME;
/// Targets probed concurrently (ADR-204; 16 before it). Each target is still probed one credential
/// at a time — this bounds how many *devices* are being asked at once, never how many credentials
/// one device is sent. A job carries at most 1,024 targets (core's `JOB_TARGETS`).
const SWEEP_CONCURRENCY: usize = 64;
/// Discovery jobs run side by side, at most this many (ADR-204 Inc.3). A job beyond it waits for
/// one to finish. It bounds the jobs, not the devices — [`SWEEP_CONCURRENCY`] slots are shared by
/// all of them — so this only caps how many scans hold a hit tally and a progress loop at once.
const MAX_CONCURRENT_JOBS: usize = 16;
/// Completed targets per progress result: a cumulative partial result is published after every
/// this many, so the operator sees the sweep advance instead of one long silence. Since ADR-204 it
/// is only a reporting cadence — nothing waits for a group of this size to finish.
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

/// How many targets each candidate has matched so far in one sweep (ADR-204), indexed like the
/// sweep's candidate list. Shared by every target of the job and read before every attempt.
///
/// Lives for one job on one poller and nowhere else: a scan split into several jobs, or taken by
/// several pollers, learns separately in each. A plain `Mutex` because nothing awaits while it is
/// held — a read is a copy of a handful of integers.
struct HitTally {
    hits: Mutex<Vec<u32>>,
}

impl HitTally {
    fn new(candidates: usize) -> Self {
        Self {
            hits: Mutex::new(vec![0; candidates]),
        }
    }

    fn record(&self, candidate: usize) {
        let mut g = self.hits.lock().expect("hit tally mutex poisoned");
        if let Some(n) = g.get_mut(candidate) {
            *n = n.saturating_add(1);
        }
    }

    fn snapshot(&self) -> Vec<u32> {
        self.hits.lock().expect("hit tally mutex poisoned").clone()
    }
}

/// Which candidate a target should try next, or `None` when it has tried them all (ADR-204).
///
/// Of the untried candidates: stored credentials before typed-in communities, then the SNMP
/// version the operator listed first, then the one that has matched the most targets, then the
/// operator's order. The first rule is what `candidates_of` already promised — import binds a
/// stored credential by reference, so a typed-in community that happens to match more often must
/// never overtake one.
///
/// 🚨 **The tally reorders within one version, never across** (ADR-204 Inc.2). Ranked across, an
/// operator who put v3 first still had a v2c community sent in cleartext to every device after
/// enough others answered v2c, and a device that speaks both was imported on v2c. Worse, a host on
/// the swept subnet can answer ping and any community on unused addresses, so the hits it fakes
/// would push v2c ahead of v3 for the real devices beside it; it cannot answer v3 without the keys.
///
/// Pure, and asked again before every attempt rather than once per target, so a target part way
/// through its list follows a match that another target found meanwhile.
fn next_candidate(candidates: &[SnmpCandidate], hits: &[u32], tried: &[bool]) -> Option<usize> {
    // Where each version first appears in the operator's order: that position ranks the version.
    let first_of = |v3: bool| {
        candidates
            .iter()
            .position(|c| matches!(c, SnmpCandidate::V3 { .. }) == v3)
            .unwrap_or(usize::MAX)
    };
    let (first_v3, first_v2c) = (first_of(true), first_of(false));
    (0..candidates.len())
        .filter(|&i| !tried.get(i).copied().unwrap_or(false))
        .min_by_key(|&i| {
            let typed_in = candidates[i].cred_ref().is_none();
            let version = match candidates[i] {
                SnmpCandidate::V3 { .. } => first_v3,
                SnmpCandidate::V2c { .. } => first_v2c,
            };
            let matched = hits.get(i).copied().unwrap_or(0);
            (typed_in, version, std::cmp::Reverse(matched), i)
        })
}

/// Everything one sweep holds constant across its targets.
///
/// A struct rather than a parameter list because the list had reached seven and the ICMP gate made
/// it eight — which is clippy's `too_many_arguments` threshold and, per `coding-conventions.md`, the
/// signal that the parameters want to be a type rather than an `#[allow]`. It also removes a real
/// hazard: the old `sweep_chunk` and `probe_one` took the same seven values in the same order, so a
/// transposed pair would have compiled.
struct SweepCtx {
    /// Credentials to try on each target, in the operator's order; the first that answers wins.
    /// Which one a target tries next is [`next_candidate`]'s answer, not this order alone.
    candidates: Arc<Vec<SnmpCandidate>>,
    /// How often each candidate has matched in this sweep (ADR-204). `Arc` only so a test's fake
    /// transport can record a match "by another target" while one is being probed.
    hits: Arc<HitTally>,
    /// Per-probe timeout (ICMP and SNMP alike).
    timeout: Duration,
    /// Per-device credential-probe rate limit (security.md).
    rate: LimiterConfig,
    scan_id: Uuid,
    cancels: Arc<CancelSet>,
    /// Whether a target that did not answer ICMP is still tried with SNMP (ADR-068 Increment 3).
    snmp_when_unreachable: bool,
    /// The poller's [`SWEEP_CONCURRENCY`] slots, shared by every running job (ADR-204 Inc.3). A
    /// target holds one while it is probed.
    slots: Arc<Semaphore>,
    /// How many of this sweep's targets could not be probed at all — `probe_icmp` returned `Err`.
    ///
    /// Counted rather than logged per target: a poller that cannot send ICMP fails on *every*
    /// address, and a line each would be 254 of them for a /24 — which reads as noise and gets
    /// filtered out, which is how the condition stayed invisible. One line at the end says the same
    /// thing and says how big it was.
    icmp_errors: AtomicUsize,
}

/// Everything one sweep holds constant, built from the job that asked for it.
///
/// A function rather than a literal inside the sweep loop so a test can read the wiring back:
/// **which rate-limit profile a sweep probes with is a decision, and it was wrong for a year**
/// with nothing able to see it (ADR-161).
fn sweep_ctx(job: &DiscoveryJob, cancels: Arc<CancelSet>, slots: Arc<Semaphore>) -> SweepCtx {
    let candidates = candidates_of(job);
    SweepCtx {
        hits: Arc::new(HitTally::new(candidates.len())),
        candidates: Arc::new(candidates),
        timeout: Duration::from_millis(u64::from(job.timeout_ms)),
        // Per-device credential-probe rate limit (security.md): space attempts so the sweep
        // paces itself at one device. **No cooldown** — SNMP has no account to lock out, so a
        // consecutive-failure limit here would not protect the device, it would only stop
        // trying the operator's remaining credentials (ADR-161, and see
        // `LimiterConfig::snmp_sweep`'s doc for why that is not the same trade-off SSH/CLI
        // probing will face).
        rate: LimiterConfig::snmp_sweep(),
        scan_id: job.scan_id,
        cancels,
        snmp_when_unreachable: job.snmp_when_unreachable,
        slots,
        icmp_errors: AtomicUsize::new(0),
    }
}

impl SweepCtx {
    /// Whether this sweep has been told to stop, as of now.
    fn stopping(&self) -> bool {
        self.cancels.is_cancelled(self.scan_id, Instant::now())
    }
}

/// Drain discovery jobs off the bus, sweeping each and publishing cumulative progress
/// results. Returns when the stream ends.
///
/// Jobs run side by side, at most [`MAX_CONCURRENT_JOBS`], sharing [`SWEEP_CONCURRENCY`] target
/// slots (ADR-204 Inc.3) — see the module doc.
pub async fn run_discovery_stream<S>(
    jobs: S,
    bus: Arc<dyn DiscoveryBus>,
    transport: Arc<dyn Transport>,
    cancels: Arc<CancelSet>,
) where
    S: Stream<Item = DiscoveryJob> + Unpin,
{
    let slots = Arc::new(Semaphore::new(SWEEP_CONCURRENCY));
    run_discovery_jobs(jobs, bus, transport, cancels, slots).await;
}

/// [`run_discovery_stream`] with the slots handed in, so a test can make them few.
async fn run_discovery_jobs<S>(
    jobs: S,
    bus: Arc<dyn DiscoveryBus>,
    transport: Arc<dyn Transport>,
    cancels: Arc<CancelSet>,
    slots: Arc<Semaphore>,
) where
    S: Stream<Item = DiscoveryJob> + Unpin,
{
    jobs.for_each_concurrent(MAX_CONCURRENT_JOBS, |job| {
        run_one_job(
            job,
            bus.clone(),
            transport.clone(),
            cancels.clone(),
            slots.clone(),
        )
    })
    .await;
    tracing::warn!("discovery job stream ended");
}

/// Sweep one job to its terminal result.
async fn run_one_job(
    job: DiscoveryJob,
    bus: Arc<dyn DiscoveryBus>,
    transport: Arc<dyn Transport>,
    cancels: Arc<CancelSet>,
    slots: Arc<Semaphore>,
) {
    {
        let total = u32::try_from(job.targets.len()).unwrap_or(u32::MAX);
        // Layer 1: a sweep cancelled while it waited its turn must not start. A job can still wait
        // — behind MAX_CONCURRENT_JOBS others on the subscription — long enough for the operator to
        // give up on it before it has probed anything.
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
            return;
        }
        tracing::info!(
            scan = %job.scan_id,
            targets = job.targets.len(),
            // Every credential is tried on every reachable target (ADR-161), so this number is
            // the multiplier on how long a sweep takes. Logged because it is the operator's own
            // choice and the only thing that explains a slow sweep that is working correctly.
            credentials = job.credentials.len() + job.communities.len(),
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
        let ctx = sweep_ctx(&job, cancels.clone(), slots);
        let mut found: Vec<DiscoveredDevice> = Vec::new();
        let mut probed: u32 = 0;
        let mut cancelled = false;

        // Targets that have come back, whether probed or skipped by a stop. Counted separately from
        // `probed` because it is what decides whether there is anything left to report on: a
        // progress result is only worth sending while targets remain, or the last one would be
        // sent twice — once as progress and once as the terminal message below.
        let mut completed: usize = 0;
        let mut since_progress: usize = 0;
        // Boxed: the slot-taking `then` stage holds an async block, which is not `Unpin`. A box
        // rather than `pin!` so the `drop` below still ends the stream before the terminal publish.
        let mut outcomes = Box::pin(sweep_targets(&job.targets, &ctx, transport.clone()));
        while let Some(outcome) = outcomes.next().await {
            completed += 1;
            match outcome {
                // A target the stop reached before it began: nothing was sent to it, so it is not
                // counted. The stream keeps draining, because targets already in flight still
                // finish and still count.
                Probe::Skipped => cancelled = true,
                Probe::Probed(device) => {
                    // The number actually probed, never the number handed out: a sweep abandoned
                    // partway must not claim credit for the targets it skipped. `probed` is how
                    // core (and the operator) tells a sweep that stopped early from one that
                    // finished.
                    probed = probed.saturating_add(1);
                    found.extend(device);
                    since_progress += 1;
                }
            }
            if since_progress >= PROGRESS_CHUNK && completed < job.targets.len() && !cancelled {
                since_progress = 0;
                publish(
                    bus.as_ref(),
                    DiscoveryResult {
                        scan_id: job.scan_id,
                        found: found.clone(),
                        probed,
                        total,
                        done: false,
                        cancelled: false,
                    },
                )
                .await;
            }
        }
        drop(outcomes);
        // The single exit that closes the scan, whichever way the stream ended — every target
        // done, a stop part way, or no targets at all. 🚨 Not bookkeeping: core has no timeout, so
        // a sweep that ends without `done: true` leaves the scan `cancelling` for the life of the
        // process. It shipped that way once (the old chunk-boundary stop broke out before its
        // publish), which is why the loop above never sends `done` and this is the only place
        // that does.
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
            return;
        }
        tracing::info!(scan = %job.scan_id, found = found.len(), "discovery sweep done");
    }
}

async fn publish(bus: &dyn DiscoveryBus, result: DiscoveryResult) {
    let scan_id = result.scan_id;
    if let Err(e) = bus.publish_discovery_result(result).await {
        tracing::warn!(error = %e, scan = %scan_id, "publish discovery result failed");
    }
}

/// What became of one target of a sweep.
enum Probe {
    /// The stop reached it before it began; nothing was sent to it.
    Skipped,
    /// It was probed, and this is the device if it was a candidate.
    Probed(Option<DiscoveredDevice>),
}

/// Probe a job's targets with bounded concurrency, yielding each target's outcome **as it
/// finishes** (ADR-204). No target waits for any other: the slowest device holds up only itself.
///
/// The `Skipped` / `Probed` split is what keeps `probed` honest when a stop lands part way.
///
/// **"Probed" means an ICMP probe was addressed to the target. That is the definition, and it
/// is chosen rather than incidental.** Three cases sit either side of the line, and this doc used
/// to describe only the first — which left the other two reading as bugs:
///
/// * A target the stop reached **before its future began** is `Skipped`. Nothing was sent to
///   it, and counting it is exactly how a stopped sweep would claim to have finished the work it
///   abandoned.
/// * A target whose **credential loop ended early** — cut short by the stop (`break 'candidates`)
///   or by the per-device cooldown — **is** `Probed`. Its ICMP probe had already gone out and been
///   awaited before either check could run, so the address really was looked at; only its
///   identification is incomplete, and an incomplete identification is reported as a device with
///   fewer fields, not as an address nobody visited.
/// * A target the **ICMP gate refused** is `Probed` as well. The gate is a decision taken *about* a
///   probed target, not a reason to skip one; `probe_one` returning `None` there means "not a
///   candidate", never "not looked at".
///
/// So `probed` is "addresses this sweep put a packet at", never "addresses it finished with". The
/// property the operator actually reads is unaffected: `probed < total` on a cancelled sweep still
/// means the stop took effect, because the only thing that fails to increment the count is a target
/// that was never sent anything.
fn sweep_targets<'a>(
    targets: &'a [IpAddr],
    ctx: &'a SweepCtx,
    transport: Arc<dyn Transport>,
) -> impl Stream<Item = Probe> + 'a {
    futures::stream::iter(targets.iter().copied())
        // One of the poller's shared slots first (ADR-204 Inc.3), held until the target is done.
        // Taken by `then`, so ONE target of this job waits in the semaphore's queue at a time: a
        // job that queued all its targets at once would put a Detect arriving after it behind every
        // one of them, the queue being first come first served. The semaphore is never closed, so
        // a failed acquire cannot happen; were it to, the target goes ahead unslotted rather than
        // being dropped unprobed.
        //
        // A stopped sweep does not queue at all: asking for a slot only to skip the target would
        // make each of its remaining targets wait its turn behind every other running job, so a
        // stop would take as long to close the scan as the other sweeps take to finish.
        .then(move |target| async move {
            if ctx.stopping() {
                return (target, None, true);
            }
            (target, ctx.slots.clone().acquire_owned().await.ok(), false)
        })
        .map(move |(target, slot, stopped)| {
            let transport = transport.clone();
            async move {
                let _slot = slot;
                // Layer 2: a target not yet started is dropped whole. Asked again after the slot,
                // so a stop that lands while a target waits for one still reaches it.
                if stopped || ctx.stopping() {
                    return Probe::Skipped;
                }
                Probe::Probed(probe_one(target, ctx, transport.as_ref()).await)
            }
        })
        .buffer_unordered(SWEEP_CONCURRENCY)
}

/// Current Unix time in milliseconds (clock for the per-device probe limiter).
use yagra_common::clock::now_unix_ms as now_ms;

/// Probe one target: ICMP liveness + SNMP identity, trying candidates in the order
/// [`next_candidate`] picks (first that answers wins, and a match is added to the sweep's
/// [`HitTally`]). Candidate probes run **sequentially per device** and are spaced
/// apart by a per-device [`CredentialProbeLimiter`] ([`SweepCtx::rate`]), so the sweep paces
/// itself at one device (security.md). Attempted credentials are never logged. Returns a
/// device iff it answered ICMP or SNMP.
///
/// 🚨 **Every candidate is tried** (ADR-161). The sweep's profile carries no consecutive-failure
/// cooldown, and it must not gain one: SNMP has no account to lock out, and every non-answer
/// looks alike here — a dropped packet, an ACL on 161, an agent with no `sysDescr` and a wrong
/// community all arrive as silence and all used to spend the same budget of three. The operator's
/// 4th and 5th credentials were dropped without a packet or a log line, which reads from the
/// Discovery screen as "the later communities do not work". The cooldown could not have protected
/// anything either: the limiter below is built **per target** and dropped on return, so it never
/// spanned two targets, two sweeps, or one re-run a second later.
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
    // enough for what it now does: each target appears once per sweep, and the spacing only has
    // to span this target's sequential candidate attempts.
    //
    // ⚠️ That is *why* a cooldown does not belong here, and it is the thing to remember when
    // SSH/CLI probing arrives with a credential kind that really does lock accounts out: a limit
    // enforced on a limiter that lives for one target protects nothing, while looking protective.
    // Hoist it to the sweep (or to core) at that point.
    let mut limiter = CredentialProbeLimiter::<IpAddr>::new(ctx.rate);
    let mut identity = SnmpIdentity::default();
    let mut matched_credential = None;
    let mut tried = vec![false; ctx.candidates.len()];
    // The tally is read again before every attempt (ADR-204): a match another target finds while
    // this one is between attempts changes what this one tries next.
    'candidates: while let Some(i) = next_candidate(&ctx.candidates, &ctx.hits.snapshot(), &tried) {
        tried[i] = true;
        let cand = &ctx.candidates[i];
        // Layer 3 — the fine-grained stop, and the one that decides how long the button
        // appears to do nothing. The rate limiter spaces attempts 2s apart, so a target with five
        // credentials occupies this loop for ~20s; without a check here a stop would wait out every
        // in-progress target before the per-target check could see it.
        //
        // ⚠️ The ICMP probe above is *not* interrupted — it has already been awaited by the time
        // this runs. That is deliberate (a probe in flight is left to time out rather than having
        // its socket dropped), and it is why the worst case is one probe timeout rather than zero.
        if ctx.stopping() {
            break 'candidates;
        }
        // Honour per-device spacing before each attempt. `CoolingDown` cannot occur under the
        // sweep's own profile (`LimiterConfig::snmp_sweep`) and is handled rather than ignored
        // because the arm is exhaustive and the variant stays reachable for a future caller that
        // does pass a cooldown.
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
            ctx.hits.record(i);
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
                .snmp_walk_strings(
                    target,
                    community,
                    &bases,
                    yagra_transport::WalkLimits::per_round_trip(timeout),
                )
                .await
                .ok()?
                .rows;
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
    pool: &Arc<crate::pool::PoolState>,
    queue: &str,
    shutdown: &CancellationToken,
) -> anyhow::Result<()> {
    let legacy = Box::pin(bus.subscribe_discovery_jobs(queue).await?);
    // The pool-scoped half goes behind a relay (ADR-107 Inc.2): its subject carries the pool name,
    // so a move has to re-point it. The legacy subject does not and is subscribed directly.
    //
    // 🚨 Getting this wrong is not a lost sweep, it is a sweep run **from the wrong network**: a
    // discovery job is queue-delivered across a pool, so a poller still listening on its old pool's
    // subject can be handed a sweep meant for a site it is no longer at.
    let pooled = {
        let bus_c = bus.clone();
        let queue_c = queue.to_owned();
        Box::pin(tokio_stream::wrappers::ReceiverStream::new(
            crate::pool::relay("discovery-jobs", pool, shutdown, move |p| {
                let bus = bus_c.clone();
                let queue = queue_c.clone();
                async move { bus.subscribe_discovery_jobs_for_pool(&p, &queue).await }
            }),
        ))
    };
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
    // Relayed for the same reason as the sweeps it mirrors: a stop must reach the poller on
    // whichever route the job took, and after a move that route has a different name.
    let cancel_pooled = {
        let bus_c = bus.clone();
        Box::pin(tokio_stream::wrappers::ReceiverStream::new(
            crate::pool::relay("discovery-cancels", pool, shutdown, move |p| {
                let bus = bus_c.clone();
                async move { bus.subscribe_discovery_cancels(Some(&p)).await }
            }),
        ))
    };
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
        SnmpTableSample, SnmpTableString, TableWalk, TransportError, WalkLimits,
    };

    /// A table walk that returned nothing and was stopped by nothing — what these fakes answer for
    /// the walks discovery does not use.
    fn empty_walk<R>() -> TableWalk<R> {
        TableWalk {
            rows: Vec::new(),
            columns: Vec::new(),
            stopped: None,
        }
    }

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
                sent: 1,
                received: u8::from(up),
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
            _limits: WalkLimits,
        ) -> Result<TableWalk<SnmpTableSample>, TransportError> {
            Ok(empty_walk())
        }

        async fn snmp_walk_strings(
            &self,
            _target: IpAddr,
            community: &str,
            column_oids: &[String],
            _limits: WalkLimits,
        ) -> Result<TableWalk<SnmpTableString>, TransportError> {
            let rows = if Some(community) == self.good_community.as_deref() {
                column_oids
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
                    .collect()
            } else {
                Vec::new()
            };
            Ok(TableWalk {
                rows,
                ..empty_walk()
            })
        }

        async fn snmp_v3_walk(
            &self,
            _target: IpAddr,
            _params: &SnmpV3Params,
            _column_oids: &[String],
            _limits: WalkLimits,
        ) -> Result<TableWalk<SnmpTableSample>, TransportError> {
            Ok(empty_walk())
        }

        async fn snmp_v3_walk_strings(
            &self,
            _target: IpAddr,
            _params: &SnmpV3Params,
            _column_oids: &[String],
            _limits: WalkLimits,
        ) -> Result<TableWalk<SnmpTableString>, TransportError> {
            Ok(empty_walk())
        }

        async fn snmp_walk_instances(
            &self,
            _target: IpAddr,
            _community: &str,
            _column_oids: &[String],
            _timeout: Duration,
            _max_rows: usize,
        ) -> Result<yagra_transport::InstanceWalk, TransportError> {
            Ok(yagra_transport::InstanceWalk {
                rows: Vec::new(),
                every_column_answered: true,
            })
        }

        async fn snmp_v3_walk_instances(
            &self,
            _target: IpAddr,
            _params: &SnmpV3Params,
            _column_oids: &[String],
            _timeout: Duration,
            _max_rows: usize,
        ) -> Result<yagra_transport::InstanceWalk, TransportError> {
            Ok(yagra_transport::InstanceWalk {
                rows: Vec::new(),
                every_column_answered: true,
            })
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
        ) -> Result<yagra_transport::MerakiCollected, yagra_transport::MerakiFetchError> {
            Ok(yagra_transport::MerakiCollected::default())
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
            max_consecutive_failures: None,
            cooldown_ms: 0,
        }
    }

    /// The **shipped** SNMP profile with only the sleeping removed.
    ///
    /// `..LimiterConfig::snmp_sweep()` rather than a fresh literal, deliberately: the field under
    /// test is `max_consecutive_failures`, so it has to come from the value production passes or
    /// the test is about a config nobody ships — which is exactly how `Some(3)` went unexercised
    /// (every probe test used [`no_rate`], which overrode it). Only `min_interval_ms` is zeroed,
    /// because the 2 s spacing is real time that `now_ms` reads off the wall clock: `start_paused`
    /// would make `sleep` return without advancing it and the wait loop would spin forever.
    fn shipped_rate_without_the_sleep() -> LimiterConfig {
        LimiterConfig {
            min_interval_ms: 0,
            ..LimiterConfig::snmp_sweep()
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
            hits: Arc::new(HitTally::new(candidates.len())),
            candidates: Arc::new(candidates),
            timeout: Duration::from_millis(100),
            rate: no_rate(),
            scan_id: Uuid::nil(),
            cancels: no_cancels(),
            snmp_when_unreachable: true,
            slots: Arc::new(Semaphore::new(SWEEP_CONCURRENCY)),
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

    /// ADR-161, the wiring half: **which rate profile a sweep probes with.** Read off the value
    /// `run_discovery_stream` actually builds, because the defect was not in the limiter — that
    /// did exactly what it was configured to do — it was in the configuration handed to it.
    #[test]
    fn a_sweep_probes_with_the_no_cooldown_profile() {
        let ctx = sweep_ctx(
            &job(vec![], vec!["a".to_owned()]),
            no_cancels(),
            Arc::new(Semaphore::new(1)),
        );
        assert_eq!(
            ctx.rate.max_consecutive_failures, None,
            "a cooldown here cuts the candidate list short and protects nothing: SNMP has \
             no account to lock out, and the limiter is rebuilt per target, so a cooldown \
             set here could never span two sweeps anyway"
        );
        assert!(
            ctx.rate.min_interval_ms > 0,
            "spacing is the half that does pace the device (security.md) and must survive \
             the removal of the cooldown — a fix that deleted the limiter outright would \
             pass the assertion above"
        );
    }

    /// ADR-161, the behaviour half — and the user-reported bug stated as a test.
    ///
    /// Five credentials, the **fifth** correct. Under the shipped config this used to find
    /// nothing at all: three failures tripped a 15-minute cooldown and `break 'candidates` threw
    /// away candidates 4 and 5 without sending a packet or logging a line.
    #[tokio::test]
    async fn every_selected_credential_is_tried_under_the_shipped_profile() {
        let id = Uuid::from_u128(9);
        let cands = candidates_of(&job(
            vec![
                v2c_cred(Uuid::from_u128(5), "wrong-1"),
                v2c_cred(Uuid::from_u128(6), "wrong-2"),
                v2c_cred(Uuid::from_u128(7), "wrong-3"),
                v2c_cred(Uuid::from_u128(8), "wrong-4"),
                v2c_cred(id, "right"),
            ],
            vec![],
        ));
        let fake = SelectiveFake {
            ping: Ping::Answers,
            good_community: Some("right".to_owned()),
            good_v3_user: None,
        };
        let mut c = ctx(cands);
        c.rate = shipped_rate_without_the_sleep();
        let d = probe_one(target(), &c, &fake)
            .await
            .expect("the fifth credential answers, so the device is found");
        assert_eq!(
            d.matched_credential,
            Some(id),
            "and it is reported as the credential that actually matched"
        );
    }

    /// The same, one layer out: the ad-hoc communities appended after the stored credentials
    /// (`candidates_of`) are reachable too. With a cap of three they never were — three stored
    /// credentials was enough to guarantee no typed-in community was ever tried.
    #[tokio::test]
    async fn an_adhoc_community_behind_three_stored_credentials_is_still_tried() {
        let cands = candidates_of(&job(
            vec![
                v2c_cred(Uuid::from_u128(5), "wrong-1"),
                v2c_cred(Uuid::from_u128(6), "wrong-2"),
                v2c_cred(Uuid::from_u128(7), "wrong-3"),
            ],
            vec!["typed-in".to_owned()],
        ));
        let fake = SelectiveFake {
            ping: Ping::Answers,
            good_community: Some("typed-in".to_owned()),
            good_v3_user: None,
        };
        let mut c = ctx(cands);
        c.rate = shipped_rate_without_the_sleep();
        let d = probe_one(target(), &c, &fake)
            .await
            .expect("the ad-hoc community answers and is reached");
        assert_eq!(
            d.matched_credential, None,
            "an ad-hoc community has no store id, so the match is reported by value-less reference"
        );
        assert_eq!(d.sysdescr.as_deref(), Some("Cisco IOS Software"));
    }

    /// The cooldown still ends a device's candidate list **when a config asks for one** — the
    /// SSH/CLI case ADR-018 sized it for. Kept so that making SNMP exempt cannot turn the
    /// mechanism into dead code that nothing would notice rotting.
    ///
    /// ⚠️ This used to assert the opposite thing and passed: as
    /// `cooldown_backs_off_remaining_candidates` it pinned the truncation as correct, at an
    /// overridden limit of 2 rather than the shipped 3, so nobody reading it learned that five
    /// selected credentials meant three probes.
    #[tokio::test]
    async fn a_cooldown_config_backs_off_remaining_candidates() {
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
                    sent: 1,
                    received: 0,
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
                _l: WalkLimits,
            ) -> Result<TableWalk<SnmpTableSample>, TransportError> {
                Ok(empty_walk())
            }
            async fn snmp_walk_strings(
                &self,
                _t: IpAddr,
                _c: &str,
                _o: &[String],
                _l: WalkLimits,
            ) -> Result<TableWalk<SnmpTableString>, TransportError> {
                self.attempts.fetch_add(1, Ordering::SeqCst);
                Ok(empty_walk()) // empty ⇒ this candidate "failed"
            }
            async fn snmp_v3_walk(
                &self,
                _t: IpAddr,
                _p: &SnmpV3Params,
                _o: &[String],
                _l: WalkLimits,
            ) -> Result<TableWalk<SnmpTableSample>, TransportError> {
                Ok(empty_walk())
            }
            async fn snmp_v3_walk_strings(
                &self,
                _t: IpAddr,
                _p: &SnmpV3Params,
                _o: &[String],
                _l: WalkLimits,
            ) -> Result<TableWalk<SnmpTableString>, TransportError> {
                Ok(empty_walk())
            }
            async fn snmp_walk_instances(
                &self,
                _t: IpAddr,
                _c: &str,
                _o: &[String],
                _to: Duration,
                _max: usize,
            ) -> Result<yagra_transport::InstanceWalk, TransportError> {
                Ok(yagra_transport::InstanceWalk {
                    rows: Vec::new(),
                    every_column_answered: true,
                })
            }
            async fn snmp_v3_walk_instances(
                &self,
                _t: IpAddr,
                _p: &SnmpV3Params,
                _o: &[String],
                _to: Duration,
                _max: usize,
            ) -> Result<yagra_transport::InstanceWalk, TransportError> {
                Ok(yagra_transport::InstanceWalk {
                    rows: Vec::new(),
                    every_column_answered: true,
                })
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
            ) -> Result<yagra_transport::MerakiCollected, yagra_transport::MerakiFetchError>
            {
                Ok(yagra_transport::MerakiCollected::default())
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
            max_consecutive_failures: Some(2),
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
    /// result per `PROGRESS_CHUNK` finished targets, `probed` monotone, `found` cumulative, and
    /// exactly one terminal `done`.
    #[tokio::test]
    async fn a_sweep_publishes_one_cumulative_result_per_chunk_and_one_terminal_done() {
        use yagra_bus::InMemoryBus;

        let bus = Arc::new(InMemoryBus::new(64));
        let mut rx = bus.subscribe_discovery_results();

        // More than one progress step's worth of targets, all answering, so `found` must grow
        // between the progress result and the terminal one.
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
            "the 'we have it' message, one progress result, and the terminal one -- no duplicate \
             progress result for the last target"
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
        assert!(results[2].done, "the last target closes the scan");
        assert_eq!(results[2].probed, total);
        assert_eq!(results[2].total, total);
        // Cumulative, not per-step: core replaces its candidate list wholesale from each partial,
        // so a delta here would make the UI drop everything found before the final message.
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
    /// exactly between two targets. Everything else is delegated: this exists to control *when*
    /// the stop arrives, not to model a device.
    ///
    /// WHY `at = PROGRESS_CHUNK`: the targets after that probe have been handed to
    /// `buffer_unordered` but not yet polled, so the per-target check (layer 2) is the first thing
    /// to see the stop -- and the progress result for the first `PROGRESS_CHUNK` targets is due at
    /// the same moment, which is where the old chunk loop's exit used to skip the publish.
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
            l: WalkLimits,
        ) -> Result<TableWalk<SnmpTableSample>, TransportError> {
            self.inner.snmp_walk(t, c, o, l).await
        }
        async fn snmp_walk_strings(
            &self,
            t: IpAddr,
            c: &str,
            o: &[String],
            l: WalkLimits,
        ) -> Result<TableWalk<SnmpTableString>, TransportError> {
            self.inner.snmp_walk_strings(t, c, o, l).await
        }
        async fn snmp_v3_walk(
            &self,
            t: IpAddr,
            p: &SnmpV3Params,
            o: &[String],
            l: WalkLimits,
        ) -> Result<TableWalk<SnmpTableSample>, TransportError> {
            self.inner.snmp_v3_walk(t, p, o, l).await
        }
        async fn snmp_v3_walk_strings(
            &self,
            t: IpAddr,
            p: &SnmpV3Params,
            o: &[String],
            l: WalkLimits,
        ) -> Result<TableWalk<SnmpTableString>, TransportError> {
            self.inner.snmp_v3_walk_strings(t, p, o, l).await
        }
        async fn snmp_walk_instances(
            &self,
            t: IpAddr,
            c: &str,
            o: &[String],
            to: Duration,
            m: usize,
        ) -> Result<yagra_transport::InstanceWalk, TransportError> {
            self.inner.snmp_walk_instances(t, c, o, to, m).await
        }
        async fn snmp_v3_walk_instances(
            &self,
            t: IpAddr,
            p: &SnmpV3Params,
            o: &[String],
            to: Duration,
            m: usize,
        ) -> Result<yagra_transport::InstanceWalk, TransportError> {
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
        ) -> Result<yagra_transport::MerakiCollected, yagra_transport::MerakiFetchError> {
            self.inner.collect_meraki(spec, to).await
        }
    }

    /// The bug this file's own doc warned about and did not test, found on a real deployment: press
    /// Stop, the poller logs `discovery sweep cancelled probed=32 total=254`, and the screen sits on
    /// "stopping..." until the core process is restarted. The sweep stopped correctly and never said
    /// so -- the old chunk-boundary exit broke out before the publish.
    ///
    /// Kept through ADR-204, which removed the chunks: the stop now lands between targets, and what
    /// must still hold is the same -- the skipped targets mark the sweep cancelled, and exactly one
    /// terminal message goes out.
    #[tokio::test]
    async fn a_stop_caught_between_chunks_still_closes_the_scan() {
        let bus = Arc::new(yagra_bus::InMemoryBus::new(16));
        let mut rx = bus.subscribe_discovery_results();
        let scan = Uuid::from_u128(77);
        let cancels = Arc::new(CancelSet::new());

        // One progress step's worth, then eight targets the stop must skip.
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
            "the targets before the stop were probed and the rest never started -- `probed < total` \
             is the operator's only evidence that the stop took effect"
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
    /// probed, not from how many targets were handed out.
    #[tokio::test]
    async fn a_sweep_counts_what_it_probed_not_what_it_was_given() {
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
        let outcomes: Vec<Probe> = sweep_targets(&targets, &c, transport.clone())
            .collect()
            .await;
        let found = outcomes
            .iter()
            .filter(|o| matches!(o, Probe::Probed(Some(_))))
            .count();
        assert_eq!(
            found, 8,
            "an uninterrupted sweep probes everything it was given"
        );

        c.cancels = cancelled(scan);
        let outcomes: Vec<Probe> = sweep_targets(&targets, &c, transport).collect().await;
        assert_eq!(outcomes.len(), 8, "every target still reports back");
        assert!(
            outcomes.iter().all(|o| matches!(o, Probe::Skipped)),
            "a cancelled sweep probes nothing -- the caller counts `Probed`, so a target reported \
             as probed here is exactly how a stopped sweep would claim to have finished"
        );
    }

    // ── Which credential first, and how many at once (ADR-204) ───────────────

    /// Candidates for the ordering tests: three stored v2c credentials, then two typed-in
    /// communities — the shape `candidates_of` produces.
    fn five_candidates() -> Vec<SnmpCandidate> {
        candidates_of(&job(
            vec![
                v2c_cred(Uuid::from_u128(1), "s0"),
                v2c_cred(Uuid::from_u128(2), "s1"),
                v2c_cred(Uuid::from_u128(3), "s2"),
            ],
            vec!["t3".to_owned(), "t4".to_owned()],
        ))
    }

    /// The order a target with no other help would walk the list in.
    fn walk_order(cands: &[SnmpCandidate], hits: &[u32]) -> Vec<usize> {
        let mut tried = vec![false; cands.len()];
        let mut out = Vec::new();
        while let Some(i) = next_candidate(cands, hits, &tried) {
            assert!(!tried[i], "a candidate is never offered twice");
            tried[i] = true;
            out.push(i);
        }
        out
    }

    #[test]
    fn with_no_matches_yet_the_operators_order_stands() {
        let c = five_candidates();
        assert_eq!(walk_order(&c, &[0; 5]), vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn the_credential_that_has_matched_most_is_tried_first() {
        let c = five_candidates();
        assert_eq!(walk_order(&c, &[0, 1, 4, 0, 0]), vec![2, 1, 0, 3, 4]);
    }

    #[test]
    fn equal_matches_keep_the_operators_order() {
        let c = five_candidates();
        assert_eq!(walk_order(&c, &[0, 2, 2, 0, 0]), vec![1, 2, 0, 3, 4]);
    }

    #[test]
    fn a_typed_in_community_never_overtakes_a_stored_credential() {
        // Import binds a stored credential by reference, so this is not a tie-break: a typed-in
        // community that matched every target so far still waits for the stored ones.
        let c = five_candidates();
        assert_eq!(walk_order(&c, &[0, 0, 0, 0, 9]), vec![0, 1, 2, 4, 3]);
    }

    /// Stored v3, v2c, v3, v2c, then one typed-in community.
    fn mixed_versions(v3_first: bool) -> Vec<SnmpCandidate> {
        let v3 = |n: u128| v3_cred(Uuid::from_u128(n), "u");
        let v2c = |n: u128| v2c_cred(Uuid::from_u128(n), "c");
        let creds = if v3_first {
            vec![v3(1), v2c(2), v3(3), v2c(4)]
        } else {
            vec![v2c(1), v3(2), v2c(3), v3(4)]
        };
        candidates_of(&job(creds, vec!["t".to_owned()]))
    }

    // ADR-204 Inc.2: hits the v2c side has piled up must not send a community in cleartext ahead
    // of the v3 credential the operator put first - and a host faking v2c answers on unused
    // addresses must not be able to arrange that for the real devices beside it.
    #[test]
    fn v2c_matches_never_overtake_a_v3_credential_listed_first() {
        let c = mixed_versions(true); // v3(0), v2c(1), v3(2), v2c(3), typed-in(4)
        assert_eq!(walk_order(&c, &[0, 9, 0, 20, 0]), vec![0, 2, 3, 1, 4]);
        // Within v3 the tally still reorders.
        assert_eq!(walk_order(&c, &[0, 9, 5, 0, 0]), vec![2, 0, 1, 3, 4]);
    }

    #[test]
    fn with_v2c_listed_first_v2c_stays_ahead() {
        let c = mixed_versions(false); // v2c(0), v3(1), v2c(2), v3(3), typed-in(4)
        assert_eq!(walk_order(&c, &[0, 9, 0, 9, 0]), vec![0, 2, 1, 3, 4]);
        assert_eq!(walk_order(&c, &[0, 0, 4, 0, 0]), vec![2, 0, 1, 3, 4]);
    }

    #[test]
    fn a_target_that_has_tried_everything_gets_nothing_more() {
        let c = five_candidates();
        assert_eq!(next_candidate(&c, &[3, 0, 0, 0, 0], &[true; 5]), None);
        assert_eq!(next_candidate(&[], &[], &[]), None);
    }

    /// A device that answers one community, records the order it was asked in, and can be made to
    /// hold one target up or to report a match "by another target" during its first attempt.
    struct ScriptedFake {
        good_community: String,
        asked: Mutex<Vec<String>>,
        /// On the first SNMP attempt, record a match for this candidate in this tally — as if a
        /// target probed at the same time had just found it.
        match_elsewhere_on_first_ask: Option<(Arc<HitTally>, usize)>,
        /// Hold this target's ICMP probe for this long.
        slow: Option<(IpAddr, Duration)>,
        /// Hold every target's ICMP probe for this long, so targets overlap.
        hold_all: Option<Duration>,
        /// Targets inside `probe_icmp` right now, and the most there have ever been at once.
        in_flight: AtomicUsize,
        max_in_flight: AtomicUsize,
    }

    impl ScriptedFake {
        fn answering(community: &str) -> Self {
            Self {
                good_community: community.to_owned(),
                asked: Mutex::new(Vec::new()),
                match_elsewhere_on_first_ask: None,
                slow: None,
                hold_all: None,
                in_flight: AtomicUsize::new(0),
                max_in_flight: AtomicUsize::new(0),
            }
        }

        fn asked(&self) -> Vec<String> {
            self.asked.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl Transport for ScriptedFake {
        async fn probe_icmp(
            &self,
            target: IpAddr,
            _count: u8,
            _timeout: Duration,
        ) -> Result<IcmpProbe, TransportError> {
            let now = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_in_flight.fetch_max(now, Ordering::SeqCst);
            if let Some((slow, hold)) = self.slow {
                if slow == target {
                    tokio::time::sleep(hold).await;
                }
            }
            if let Some(hold) = self.hold_all {
                tokio::time::sleep(hold).await;
            }
            self.in_flight.fetch_sub(1, Ordering::SeqCst);
            Ok(IcmpProbe {
                reachable: true,
                rtt_ms: Some(1.0),
                loss_pct: 0.0,
                sent: 1,
                received: 1,
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
            _l: WalkLimits,
        ) -> Result<TableWalk<SnmpTableSample>, TransportError> {
            Ok(empty_walk())
        }
        async fn snmp_walk_strings(
            &self,
            _target: IpAddr,
            community: &str,
            column_oids: &[String],
            _limits: WalkLimits,
        ) -> Result<TableWalk<SnmpTableString>, TransportError> {
            let first = {
                let mut asked = self.asked.lock().unwrap();
                asked.push(community.to_owned());
                asked.len() == 1
            };
            if first {
                if let Some((tally, candidate)) = &self.match_elsewhere_on_first_ask {
                    tally.record(*candidate);
                }
            }
            let rows = if community == self.good_community {
                column_oids
                    .iter()
                    .map(|b| SnmpTableString {
                        oid_base: b.clone(),
                        ifindex: 0,
                        value: "answered".to_owned(),
                    })
                    .collect()
            } else {
                Vec::new()
            };
            Ok(TableWalk {
                rows,
                ..empty_walk()
            })
        }
        async fn snmp_v3_walk(
            &self,
            _t: IpAddr,
            _p: &SnmpV3Params,
            _o: &[String],
            _l: WalkLimits,
        ) -> Result<TableWalk<SnmpTableSample>, TransportError> {
            Ok(empty_walk())
        }
        async fn snmp_v3_walk_strings(
            &self,
            _t: IpAddr,
            _p: &SnmpV3Params,
            _o: &[String],
            _l: WalkLimits,
        ) -> Result<TableWalk<SnmpTableString>, TransportError> {
            Ok(empty_walk())
        }
        async fn snmp_walk_instances(
            &self,
            _t: IpAddr,
            _c: &str,
            _o: &[String],
            _to: Duration,
            _max: usize,
        ) -> Result<yagra_transport::InstanceWalk, TransportError> {
            Ok(yagra_transport::InstanceWalk {
                rows: Vec::new(),
                every_column_answered: true,
            })
        }
        async fn snmp_v3_walk_instances(
            &self,
            _t: IpAddr,
            _p: &SnmpV3Params,
            _o: &[String],
            _to: Duration,
            _max: usize,
        ) -> Result<yagra_transport::InstanceWalk, TransportError> {
            Ok(yagra_transport::InstanceWalk {
                rows: Vec::new(),
                every_column_answered: true,
            })
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
        ) -> Result<yagra_transport::MerakiCollected, yagra_transport::MerakiFetchError> {
            Ok(yagra_transport::MerakiCollected::default())
        }
    }

    /// The point of ADR-204, as one sweep sees it: the first target pays for the four wrong
    /// guesses, and the next one asks the credential that answered straight away.
    #[tokio::test]
    async fn a_later_target_asks_the_credential_an_earlier_one_matched_first() {
        let right = Uuid::from_u128(9);
        let c = ctx(candidates_of(&job(
            vec![
                v2c_cred(Uuid::from_u128(5), "wrong-1"),
                v2c_cred(Uuid::from_u128(6), "wrong-2"),
                v2c_cred(Uuid::from_u128(7), "wrong-3"),
                v2c_cred(Uuid::from_u128(8), "wrong-4"),
                v2c_cred(right, "right"),
            ],
            vec![],
        )));
        let fake = ScriptedFake::answering("right");

        let first = probe_one(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)), &c, &fake)
            .await
            .expect("the first target is identified");
        assert_eq!(first.matched_credential, Some(right));
        assert_eq!(
            fake.asked().len(),
            5,
            "the first target walks the whole list"
        );

        let second = probe_one(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 2)), &c, &fake)
            .await
            .expect("the second target is identified");
        assert_eq!(second.matched_credential, Some(right));
        assert_eq!(
            fake.asked()[5..],
            ["right".to_owned()],
            "the second target asks the credential that matched, and nothing else"
        );
    }

    /// The choice is made again before every attempt, not once per target: a target already one
    /// wrong guess in follows a match another target found meanwhile, rather than finishing the
    /// list it started with.
    #[tokio::test]
    async fn a_target_part_way_through_its_list_follows_a_match_found_meanwhile() {
        let c = ctx(candidates_of(&job(
            vec![
                v2c_cred(Uuid::from_u128(1), "wrong-1"),
                v2c_cred(Uuid::from_u128(2), "wrong-2"),
                v2c_cred(Uuid::from_u128(3), "right"),
            ],
            vec![],
        )));
        let mut fake = ScriptedFake::answering("right");
        // While this target is asking `wrong-1`, another target matches candidate 2 ("right").
        fake.match_elsewhere_on_first_ask = Some((c.hits.clone(), 2));

        let d = probe_one(target(), &c, &fake)
            .await
            .expect("the device is identified");
        assert_eq!(d.matched_credential, Some(Uuid::from_u128(3)));
        assert_eq!(
            fake.asked(),
            ["wrong-1".to_owned(), "right".to_owned()],
            "the second attempt goes to the credential that just matched, skipping `wrong-2`"
        );
    }

    /// The other half of ADR-204: one slow device holds up only itself. Under the old 32-target
    /// chunks, the eight targets after the first chunk could not start until its slowest member
    /// had finished.
    #[tokio::test(start_paused = true)]
    async fn a_slow_target_does_not_hold_back_the_others() {
        let slow = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));
        let count = u8::try_from(PROGRESS_CHUNK).unwrap() + 8;
        let targets: Vec<IpAddr> = (1..=count)
            .map(|i| IpAddr::V4(Ipv4Addr::new(10, 0, 0, i)))
            .collect();
        let mut fake = ScriptedFake::answering("public");
        fake.slow = Some((slow, Duration::from_secs(30)));
        let transport: Arc<dyn Transport> = Arc::new(fake);
        let c = ctx(candidates_of(&job(vec![], vec!["public".to_owned()])));

        let order: Vec<IpAddr> = sweep_targets(&targets, &c, transport)
            .filter_map(|o| async move {
                match o {
                    Probe::Probed(Some(d)) => Some(d.address),
                    Probe::Probed(None) | Probe::Skipped => None,
                }
            })
            .collect()
            .await;
        assert_eq!(order.len(), targets.len(), "every target is identified");
        assert_eq!(
            order.last(),
            Some(&slow),
            "the slow target finishes last -- so the eight targets beyond the first \
             {PROGRESS_CHUNK} finished while it was still out, which the old chunks did not allow"
        );
    }

    // ── Jobs side by side, slots shared (ADR-204 Inc.3) ──────────────────────

    /// A job of `targets` addresses from 10.0.<net>.1 on, with one community and SNMP everywhere.
    fn sweep_job(scan: u128, net: u8, targets: u8) -> DiscoveryJob {
        DiscoveryJob {
            scan_id: Uuid::from_u128(scan),
            targets: (1..=targets)
                .map(|i| IpAddr::V4(Ipv4Addr::new(10, 0, net, i)))
                .collect(),
            communities: vec!["public".to_owned()],
            credentials: Vec::new(),
            timeout_ms: 100,
            snmp_when_unreachable: true,
        }
    }

    /// Run `jobs` through the job loop with `slots` shared slots, and return the scan ids in the
    /// order their terminal (`done`) results were published.
    async fn finish_order(
        jobs: Vec<DiscoveryJob>,
        transport: Arc<dyn Transport>,
        slots: usize,
    ) -> Vec<Uuid> {
        use yagra_bus::InMemoryBus;
        let bus = Arc::new(InMemoryBus::new(256));
        let mut rx = bus.subscribe_discovery_results();
        run_discovery_jobs(
            futures::stream::iter(jobs),
            bus.clone() as Arc<dyn DiscoveryBus>,
            transport,
            no_cancels(),
            Arc::new(Semaphore::new(slots)),
        )
        .await;
        let mut done = Vec::new();
        while let Ok(r) = rx.try_recv() {
            if r.done {
                done.push(r.scan_id);
            }
        }
        done
    }

    /// The defect this increment fixes: Detect is a one-address job of its own, and a job that
    /// arrived behind another waited for the whole of it. Here the first job's only device takes
    /// thirty seconds and the second job's answers at once.
    #[tokio::test(start_paused = true)]
    async fn a_slow_job_does_not_hold_back_the_next_one() {
        let slow_job = sweep_job(1, 1, 1);
        let mut fake = ScriptedFake::answering("public");
        fake.slow = Some((slow_job.targets[0], Duration::from_secs(30)));
        let order = finish_order(
            vec![slow_job, sweep_job(2, 2, 1)],
            Arc::new(fake),
            SWEEP_CONCURRENCY,
        )
        .await;
        assert_eq!(
            order,
            vec![Uuid::from_u128(2), Uuid::from_u128(1)],
            "the job that arrived second closed first -- one at a time, it would have waited 30s"
        );
    }

    /// The slots are the poller's, not each job's: two jobs of four devices each, three slots, and
    /// never more than three devices in hand -- yet three at once, so the jobs did overlap.
    #[tokio::test(start_paused = true)]
    async fn every_job_shares_the_pollers_slots() {
        let mut fake = ScriptedFake::answering("public");
        fake.hold_all = Some(Duration::from_secs(1));
        let fake = Arc::new(fake);
        let order = finish_order(
            vec![sweep_job(1, 1, 4), sweep_job(2, 2, 4)],
            fake.clone(),
            3,
        )
        .await;
        assert_eq!(order.len(), 2, "both jobs closed");
        assert_eq!(
            fake.max_in_flight.load(Ordering::SeqCst),
            3,
            "the slots bound the devices in hand across both jobs, and both jobs used them"
        );
    }

    /// A Detect pressed during a range sweep starts at the next free slot. The slots go out in the
    /// order they were asked for, so the one-address job is served before the sweep's later
    /// targets, and it closes long before the sweep does.
    #[tokio::test(start_paused = true)]
    async fn a_detect_during_a_sweep_starts_at_the_next_free_slot() {
        let mut fake = ScriptedFake::answering("public");
        fake.hold_all = Some(Duration::from_secs(1));
        let order = finish_order(
            vec![sweep_job(1, 1, 10), sweep_job(2, 2, 1)],
            Arc::new(fake),
            2,
        )
        .await;
        assert_eq!(
            order,
            vec![Uuid::from_u128(2), Uuid::from_u128(1)],
            "the one-address job closed before the ten-address sweep it arrived behind"
        );
    }

    /// A stopped sweep closes at once even while another job holds every slot. Its remaining
    /// targets are skipped without queueing for one; were they to queue, each would wait for the
    /// other sweep's next ten-second device, and the scan would sit in `cancelling` until about
    /// t=50s instead of closing when its one already-waiting target is served at t=20s.
    #[tokio::test(start_paused = true)]
    async fn a_stopped_sweep_does_not_wait_for_slots_to_skip_its_targets() {
        use yagra_bus::InMemoryBus;
        let bus = Arc::new(InMemoryBus::new(256));
        let mut rx = bus.subscribe_discovery_results();
        let cancels = no_cancels();
        let mut fake = ScriptedFake::answering("public");
        fake.hold_all = Some(Duration::from_secs(10));
        let stopped = Uuid::from_u128(2);
        let started = tokio::time::Instant::now();

        let run = tokio::spawn(run_discovery_jobs(
            futures::stream::iter(vec![sweep_job(1, 1, 5), sweep_job(2, 2, 10)]),
            bus.clone() as Arc<dyn DiscoveryBus>,
            Arc::new(fake),
            cancels.clone(),
            Arc::new(Semaphore::new(1)),
        ));
        tokio::time::sleep(Duration::from_secs(1)).await;
        cancels.insert(stopped, Instant::now());

        let closed_at = loop {
            let r = rx
                .recv()
                .await
                .expect("the bus stays open while the jobs run");
            if r.scan_id == stopped && r.done {
                assert!(r.cancelled, "the stopped sweep says it was stopped");
                break started.elapsed();
            }
        };
        assert!(
            closed_at <= Duration::from_secs(25),
            "the stopped sweep took {closed_at:?} to close: its skipped targets queued for slots"
        );
        run.await.expect("the job loop ends");
    }
}
