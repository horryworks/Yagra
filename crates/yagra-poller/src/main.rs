// SPDX-License-Identifier: AGPL-3.0-only
//! Yagra-poller — stateless poller worker.
//!
//! Pulls polling work off the bus (Yagra-bus / NATS), executes it via the transport layer
//! (Yagra-transport / raw-socket ICMP), and ships metrics back. In the distributed-poller model
//! (ADR-009/020) core hands each poller a **working set** — the specs it owns — as a snapshot +
//! deltas, and the poller schedules them **locally**, so steady-state bus traffic is tiny and
//! polling survives a WAN blip. Legacy per-job dispatch (`poll_now`, discovery, Meraki, and any pool
//! with no live registered poller) still arrives on the pool's job subject and is merged into the
//! same execution loop. Pollers stay stateless beyond this rebuildable working set, so they scale
//! out and fail over freely (ADR-003/009).
//!
//! Without `YAGRA_BUS_URL` the binary stays idle (so a bare `cargo run` doesn't crash-loop or
//! require raw-socket privilege); the container always sets it.
//!
//! Task layout (all detached for the process lifetime, mirroring the passive listeners): a **sync**
//! loop folds working-set syncs into the shared [`WorkingSet`], a **local scheduler** ticks every
//! 500ms and feeds due jobs into a bounded channel, a **heartbeat** loop beats liveness+telemetry
//! every [`HEARTBEAT_SECS`], and the **worker** loop drains both the local channel and the pool's
//! legacy job subject. `main` blocks on the worker loop; the local scheduler keeps it running across
//! bus blips (that continuity is the point of the working-set model).

// Global allocator (default-on `mimalloc` feature; `--no-default-features` gives the system one
// back). Per-poll buffers churned across the worker pool for weeks is the adversarial case for a
// thread-arena allocator: measured on 50k nodes, the poller's resident set crept upward under
// glibc's and stayed flat under this one. The full comparison is in the workspace Cargo.toml.
#[cfg(feature = "mimalloc")]
#[global_allocator]
static GLOBAL_ALLOC: mimalloc::MiMalloc = mimalloc::MiMalloc;

mod arp;
mod discovery;
mod flow;
mod l3;
mod limiter;
mod listeners;
mod location;
mod mau;
mod neighbors;
mod optical;
mod routing;
mod store_forward;
mod support_logs;
mod worker;
mod working_set;

use limiter::PollLimiter;
use metrics_exporter_prometheus::PrometheusBuilder;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use uuid::Uuid;
use working_set::{ApplyOutcome, WorkingSet};
use yagra_bus::{
    subjects, HeartbeatMsg, NatsBus, PollJob, SyncBus, SyncMsg, SyncRequest, HEARTBEAT_SECS,
};
use yagra_telemetry::{shutdown_signal, spawn_cancellable, CancellationToken};

/// How long shutdown waits for probes already in flight to finish and publish (ADR-051).
///
/// Sized against the probe timeouts rather than the poll interval: a probe still outstanding after
/// this was on its way to timing out, and its result would have been a failure the next poll
/// re-establishes. Long enough to save the common case, short enough that `docker stop`'s own 10s
/// grace still ends in a clean exit rather than a SIGKILL.
const INFLIGHT_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);

/// How long shutdown waits for the heartbeat loop to publish and flush its `leaving` beat.
const LEAVE_BEAT_TIMEOUT: Duration = Duration::from_secs(3);

/// How long the `leaving` beat itself waits for the bus to confirm it left the process.
const LEAVE_FLUSH_TIMEOUT: Duration = Duration::from_secs(1);

/// How the poller identifies itself to core (ADR-009). `id` is already sanitized for use as a NATS
/// subject token; `pool` is the (defaulted) pool it serves.
struct PollerIdentity {
    /// Sanitized, subject-safe poller id — stable across restarts (from `YAGRA_POLLER_ID` else the
    /// hostname else a random fallback).
    id: String,
    /// Pool this poller serves (`YAGRA_POLLER_POOL` else `"default"`).
    pool: String,
    /// Fresh per boot — lets core detect a restart and force a resync.
    incarnation: Uuid,
    /// Build version (`CARGO_PKG_VERSION`).
    version: &'static str,
}

/// Resolve the poller's identity from the environment. Precedence for the id: `YAGRA_POLLER_ID`,
/// else the machine hostname, else `poller-{8-hex}`. The chosen id is sanitized once so every
/// heartbeat / sync-request / assignment-subject use is consistent (`sanitize_token` is idempotent,
/// so it still matches the subject core publishes to). Logs the id + pool only — never a secret.
fn resolve_identity() -> PollerIdentity {
    let raw_id = env_nonempty("YAGRA_POLLER_ID")
        .or_else(machine_hostname)
        .unwrap_or_else(|| {
            let uuid = Uuid::new_v4().simple().to_string();
            format!("poller-{}", &uuid[..8])
        });
    let id = subjects::sanitize_token(&raw_id);
    let pool = env_nonempty("YAGRA_POLLER_POOL").unwrap_or_else(|| "default".to_owned());
    PollerIdentity {
        id,
        pool,
        incarnation: Uuid::new_v4(),
        version: env!("CARGO_PKG_VERSION"),
    }
}

/// The machine hostname as a non-empty `String`, if resolvable (used as the default poller id).
fn machine_hostname() -> Option<String> {
    hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .filter(|s| !s.is_empty())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Resolved before telemetry because the id names this poller's **log file** (ADR-045 Inc.3):
    // a pool sharing one log directory would otherwise have every member appending to the same
    // hourly file. Nothing here logs, so there is no gap in the trace.
    let identity = resolve_identity();

    // Structured logs + optional OpenTelemetry span export (self-observability). The guard flushes
    // spans at shutdown, so keep it alive for the whole process (`main` blocks on the worker loop).
    let _telemetry = yagra_telemetry::init_instance("yagra-poller", Some(&identity.id));

    // Self-observability: expose Prometheus metrics on :9100/metrics (monitoring-conventions).
    if let Err(e) = PrometheusBuilder::new()
        .with_http_listener(([0, 0, 0, 0], 9100))
        .install()
    {
        tracing::warn!(error = %e, "failed to start metrics exporter");
    }

    tracing::info!(
        poller_id = %identity.id,
        pool = %identity.pool,
        version = identity.version,
        "poller identity resolved"
    );

    let Ok(bus_url) = std::env::var("YAGRA_BUS_URL") else {
        tracing::warn!("YAGRA_BUS_URL not set — poller idle (no bus configured)");
        std::future::pending::<()>().await;
        return Ok(());
    };

    // Raw-socket ICMP transport — needs CAP_NET_RAW (granted to this container only).
    let transport: Arc<dyn yagra_transport::Transport> =
        Arc::new(yagra_transport::SurgePingTransport::new()?);
    tracing::info!("ICMP transport ready (raw sockets)");

    // Remote pollers pin the server cert with a CA file (TLS mandatory across trust boundaries,
    // security.md / ADR-020); the single-node plaintext path leaves it unset.
    let ca_file = env_nonempty("YAGRA_BUS_CA_FILE").map(PathBuf::from);
    // Tolerate NATS coming up after the poller (compose has no health gate). The poller presents its
    // own id/pool so core's Auth Callout can scope its credentials (ADR-030); harmless on no-auth.
    let bus =
        Arc::new(connect_bus(&bus_url, ca_file.as_deref(), &identity.id, &identity.pool).await?);

    let queue =
        std::env::var("YAGRA_POLLER_QUEUE").unwrap_or_else(|_| yagra_bus::POLLER_QUEUE.to_owned());

    // Rate control: bound total concurrent probes + per-device single-flight (#4).
    let max_concurrent = std::env::var("YAGRA_MAX_CONCURRENT_POLLS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(64);
    let limiter = Arc::new(PollLimiter::new(max_concurrent));

    // Shared state across the tasks: the working set (source of truth for local scheduling), a
    // lifetime result counter, and an in-flight gauge — the latter two feed heartbeat telemetry.
    // Adoption rate (ADR-051): how fast this poller takes on specs handed over by another poller.
    // It sizes the window newcomers spread over, so a failover reaches them in about
    // `adopted / rate` seconds instead of up to a full poll interval. `0` restores the pre-v0.2.3
    // behaviour (spread across the interval); see `working_set`'s module docs.
    let adopt_rate = std::env::var("YAGRA_ADOPT_RATE_PER_SEC")
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(working_set::DEFAULT_ADOPT_RATE_PER_SEC);
    let working_set = Arc::new(Mutex::new(WorkingSet::with_adopt_rate(adopt_rate)));
    let results_total = Arc::new(AtomicU64::new(0));
    let inflight = Arc::new(AtomicU64::new(0));

    // Store-and-forward result sink (Phase 3): during a core↔poller partition it buffers results
    // locally (bounded in-memory ring + on-disk spill) and replays them on reconnect onto the
    // backfill subject, so a partition becomes a metrics gap that heals — not lost history. Default
    // ON; `YAGRA_STORE_FORWARD=off` makes it a byte-identical pass-through (publish live, drop on
    // failure). The drain task is spawned once the shutdown token exists (below).
    let sf_bus: Arc<dyn yagra_bus::Bus> = bus.clone();
    let sink = store_forward::StoreForwardSink::new(sf_bus, store_forward::SfConfig::from_env());
    tracing::info!(
        store_forward = sink.is_enabled(),
        "store-and-forward result sink ready"
    );

    // Graceful shutdown: SIGTERM/Ctrl-C cancels this token so the background loops stop and the
    // worker loop below returns, instead of the process being hard-killed mid-poll (ADR-017 rolling
    // upgrade). Install the signal handler once, up front.
    let shutdown = CancellationToken::new();
    {
        let shutdown = shutdown.clone();
        tokio::spawn(async move {
            shutdown_signal().await;
            tracing::info!("shutdown signal received — stopping poller");
            shutdown.cancel();
        });
    }

    // Store-and-forward replay loop: whenever the bus is connected and the buffer is non-empty, drain
    // it oldest-first onto the backfill subject. A no-op while empty or disconnected (Phase 3).
    spawn_cancellable(&shutdown, sink.clone().run_drain(shutdown.clone()));

    // Passive-event listeners (Phase 2): syslog / SNMP traps, enabled per site via env. They publish
    // EventMsgs on `yagra.events`; core does the rule matching. The returned labels advertise which
    // ones actually bound (heartbeat telemetry).
    let listener_labels =
        spawn_event_listeners(&bus, &shutdown, &identity.id, &identity.pool).await;

    // Discovery sweeps run alongside polling and need the same raw-socket ICMP + SNMP transport. A
    // new core publishes them pool-scoped; an old core (or poll-now path) uses the legacy subject —
    // merge both so either producer is served.
    {
        let legacy = Box::pin(bus.subscribe_discovery_jobs(&queue).await?);
        let pooled = Box::pin(
            bus.subscribe_discovery_jobs_for_pool(&identity.pool, &queue)
                .await?,
        );
        let merged = Box::pin(futures::stream::select(legacy, pooled));
        let bus = bus.clone();
        let transport = transport.clone();
        spawn_cancellable(
            &shutdown,
            discovery::run_discovery_stream(merged, bus, transport),
        );
    }

    // Working-set sync (ADR-020): subscribe FIRST, then request an initial snapshot so we can't miss
    // the reply (it arrives as chunks on our assignment subject, not a request-reply).
    {
        let sync_sub = Box::pin(bus.subscribe_sync(&identity.id).await?);
        let initial = SyncRequest {
            poller_id: identity.id.clone(),
            pool: identity.pool.clone(),
            incarnation: identity.incarnation,
        };
        if let Err(e) = bus.publish_sync_request(initial).await {
            tracing::warn!(error = %e, "failed to publish initial sync request");
        }
        spawn_cancellable(
            &shutdown,
            run_sync_loop(
                sync_sub,
                working_set.clone(),
                bus.clone(),
                identity.id.clone(),
                identity.pool.clone(),
                identity.incarnation,
            ),
        );
    }

    // Heartbeat (ADR-009): liveness + telemetry every HEARTBEAT_SECS. The host collector rides the
    // beat so this poller's CPU/load/mem/disk reach core even across NAT/FW (self-observability).
    let host_collector = Arc::new(yagra_hoststats::HostCollector::from_env());
    // Deliberately NOT `spawn_cancellable`: this loop must *observe* the shutdown token rather than
    // be aborted by it, because its last act is to publish a `leaving` beat so core reassigns this
    // poller's nodes immediately instead of waiting three missed heartbeats. Being cancelled
    // mid-publish would put us back to timeout detection. It exits on its own once that beat is out.
    let heartbeat_task = tokio::spawn(run_heartbeat_loop(
        bus.clone(),
        identity.id.clone(),
        identity.pool.clone(),
        identity.incarnation,
        identity.version,
        working_set.clone(),
        results_total.clone(),
        inflight.clone(),
        listener_labels,
        host_collector,
        // Read once at startup rather than per beat: enumerating interfaces is a syscall, and an
        // address change on a poller host is a restart-level event in every deployment shape we
        // support (a container gets a new address by being recreated).
        location::local_mgmt_addrs(),
        shutdown.clone(),
    ));

    // Upgrade commands (ADR-051): core tells this poller to replace itself, and this poller writes
    // the request into the hand-off directory its site updater watches. Subscribed only when the
    // hand-off directory is configured — with nowhere to write, receiving the command would be a
    // log line and a lie, since the page would then show the site as "will upgrade".
    if let Some(dir) = env_nonempty("YAGRA_UPGRADE_DIR") {
        let sub = Box::pin(bus.subscribe_poller_upgrades(&identity.id).await?);
        spawn_cancellable(
            &shutdown,
            run_upgrade_loop(sub, identity.id.clone(), PathBuf::from(dir)),
        );
    }

    // Support-log requests (ADR-045 Inc.4): core asks this poller for a window of its own on-disk
    // log so a support bundle can carry a remote site's evidence. Subscribed only when there is a
    // log directory to read — a poller with no file layer would answer every request with an empty
    // reply, which core cannot tell apart from "answered nothing on purpose". Absence of
    // `CAP_LOG_SHIP` is what makes core say so by name instead.
    if let Some(dir) = yagra_telemetry::log_dir() {
        let sub = Box::pin(bus.subscribe_poller_log_requests(&identity.id).await?);
        let log_bus: Arc<dyn yagra_bus::LogBus> = bus.clone();
        spawn_cancellable(
            &shutdown,
            support_logs::run_log_request_loop(
                sub,
                identity.id.clone(),
                dir,
                working_set.clone(),
                log_bus,
            ),
        );
    }

    // Local scheduler: every 500ms, drain due specs into a bounded channel feeding the worker loop.
    let (jobs_tx, jobs_rx) = mpsc::channel::<PollJob>(256);
    spawn_cancellable(&shutdown, run_local_scheduler(working_set.clone(), jobs_tx));

    // Legacy / pool-scoped jobs: consume only this poller's pool (no more `yagra.jobs.*` wildcard)
    // so work stays local (ADR-009). Merge with the locally-scheduled jobs into one stream driving
    // the shared worker loop (PollLimiter + execution + result publish are shared).
    let legacy_jobs = Box::pin(bus.subscribe_jobs_for_pool(&identity.pool, &queue).await?);
    let unified = Box::pin(futures::stream::select(
        legacy_jobs,
        ReceiverStream::new(jobs_rx),
    ));
    let poller_id: Arc<str> = Arc::from(identity.id.as_str());
    tracing::info!(
        pool = %identity.pool,
        %queue,
        max_concurrent,
        "Yagra-poller running (working-set + legacy jobs)"
    );

    // Blocks for the process lifetime: the local scheduler keeps feeding this stream (so a bus blip
    // doesn't stop polling), so it only returns if both sources end (shutdown).
    let inflight_gauge = inflight.clone();
    tokio::select! {
        _ = worker::run_stream(
            unified,
            sink,
            transport,
            limiter,
            Some(poller_id),
            results_total,
            inflight,
        ) => {
            tracing::warn!("job stream ended — poller shutting down");
        }
        _ = shutdown.cancelled() => {
            tracing::info!("shutdown signal — poller stopped");
        }
    }

    // Everything below runs *after* the select and before `main` returns, because returning drops
    // the runtime and every task on it. Two things are still owed at that moment, and both are
    // results that would otherwise silently go missing (ADR-023: a rolling upgrade must not lose
    // data). Dropping `run_stream` above already stopped accepting new jobs — the probes it spawned
    // are independent tasks and keep going.
    drain_inflight(&inflight_gauge, INFLIGHT_DRAIN_TIMEOUT).await;
    // The heartbeat loop is not cancellable precisely so it can publish the `leaving` beat; joining
    // it is what makes that guarantee real, since otherwise it races this function's return and the
    // beat is lost with the runtime. It exits on its own once the beat is flushed.
    match tokio::time::timeout(LEAVE_BEAT_TIMEOUT, heartbeat_task).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => tracing::warn!(error = %e, "heartbeat task ended abnormally"),
        Err(_) => tracing::warn!("timed out waiting for the leaving heartbeat"),
    }
    Ok(())
}

/// Turn upgrade commands into hand-off files for the site updater to act on (ADR-051).
///
/// **This loop executes nothing.** It validates, writes a file, and goes back to waiting — the same
/// division ADR-050 drew centrally, where core writes a request and a container with the Docker
/// socket reads it. The poller is the piece with a network connection, so it is the piece that must
/// not be able to run anything.
async fn run_upgrade_loop<S>(mut stream: S, poller_id: String, dir: PathBuf)
where
    S: futures::Stream<Item = yagra_bus::PollerUpgradeMsg> + Unpin,
{
    use futures::StreamExt;
    while let Some(msg) = stream.next().await {
        // Addressed to someone else: the subject already routed it, so this can only be a mistake
        // (or a probe). Drop it rather than act on a routing error — the site whose id is on the
        // message is not this one, and installing its release here would be silently wrong.
        if msg.poller_id != poller_id {
            tracing::warn!(intended = %msg.poller_id, "ignoring an upgrade command addressed elsewhere");
            continue;
        }
        // Validate here as well as in the updater. Neither check is redundant: this one keeps a
        // malformed value out of the shared volume at all, and the updater's keeps it out of the
        // `docker` invocation even if something else wrote the file.
        if !is_release_tag(&msg.tag) {
            tracing::warn!(tag = %msg.tag, "refusing an upgrade command with an invalid release tag");
            continue;
        }
        if !is_run_id(&msg.run_id) {
            tracing::warn!("refusing an upgrade command with an invalid run id");
            continue;
        }
        let command = match msg.step {
            yagra_bus::UpgradeStep::Prefetch => "prefetch",
            yagra_bus::UpgradeStep::Apply => "apply",
        };
        let body = format!(
            "schema=1\nid={}\ncommand={}\ntag={}\nrequested_by={}\nrequested_at={}\n",
            msg.run_id,
            command,
            msg.tag,
            sanitize_actor(&msg.requested_by),
            msg.requested_at,
        );
        // Temp-then-rename, so the updater never reads a partially written request (ADR-050).
        let tmp = dir.join("request.tmp");
        let write =
            std::fs::write(&tmp, body).and_then(|()| std::fs::rename(&tmp, dir.join("request")));
        match write {
            Ok(()) => {
                tracing::info!(tag = %msg.tag, %command, run = %msg.run_id, "handed an upgrade request to the site updater")
            }
            Err(e) => tracing::error!(error = %e, "failed to write the upgrade request"),
        }
    }
}

/// Is this a release tag this poller will pass on? Mirrors core's `upgrade::is_valid_tag` — `v`
/// plus a three-part semver with an optional short suffix, and nothing else.
///
/// A second copy of a rule is normally the thing to avoid, but not across a trust boundary: the
/// point is that the value is checked by *both* sides, so neither has to assume the other did. The
/// rule is small and stable enough to state twice, and each side's copy has a test.
fn is_release_tag(tag: &str) -> bool {
    let Some(rest) = tag.strip_prefix('v') else {
        return false;
    };
    if rest.is_empty() || rest.len() > 40 {
        return false;
    }
    let (core, suffix) = match rest.split_once('-') {
        Some((c, s)) => (c, Some(s)),
        None => (rest, None),
    };
    let parts: Vec<&str> = core.split('.').collect();
    if parts.len() != 3 {
        return false;
    }
    if !parts
        .iter()
        .all(|p| !p.is_empty() && p.len() <= 6 && p.bytes().all(|b| b.is_ascii_digit()))
    {
        return false;
    }
    match suffix {
        None => true,
        Some(s) => !s.is_empty() && s.len() <= 16 && s.bytes().all(|b| b.is_ascii_alphanumeric()),
    }
}

/// A run id is a UUID in hyphenated form, and nothing else — it becomes part of a filename.
fn is_run_id(id: &str) -> bool {
    id.len() == 36 && id.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-')
}

/// Reduce an actor to characters that cannot break out of the `key=value` line the updater parses.
fn sanitize_actor(who: &str) -> String {
    let clean: String = who
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '@' | '-'))
        .take(64)
        .collect();
    if clean.is_empty() {
        "unknown".to_owned()
    } else {
        clean
    }
}

/// [`yagra_bus::CAP_SELF_UPGRADE`], but only when a site updater is really there to act on it.
///
/// Two conditions, and both are needed. `YAGRA_UPGRADE_DIR` says an operator wired the hand-off
/// volume; a **fresh** `current.json` in it says the sidecar is running rather than commented out,
/// crashed, or wired to the wrong path. Claiming on the env var alone would let core report a site
/// as "will upgrade with core" and then send it a command nothing reads — the version skew would
/// still be there, and the page would say it had been dealt with, which is worse than saying
/// nothing (ADR-051).
fn self_upgrade_cap() -> Option<String> {
    let dir = env_nonempty("YAGRA_UPGRADE_DIR")?;
    let raw = std::fs::read_to_string(Path::new(&dir).join("current.json")).ok()?;
    let beat: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let written_at = beat.get("written_at")?.as_i64()?;
    let now = i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_secs(),
    )
    .ok()?;
    // The sidecar beats every few seconds; a minute of slack absorbs clock skew between two
    // containers on the same host without ever calling a dead updater alive. A beat from the future
    // is skew, not staleness — it was clearly written.
    (now.saturating_sub(written_at) <= 60).then(|| yagra_bus::CAP_SELF_UPGRADE.to_owned())
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

/// Wait for in-flight probes to finish, up to `budget`.
///
/// A probe that is mid-flight when the process exits is a poll that happened and was thrown away:
/// its result is never published, and store-and-forward cannot help because that only buffers what a
/// *disconnected* bus refused. Nothing needs to be cancelled — the worker stream is already gone, so
/// the count only falls. Bounded because a hung probe must not hold the shutdown open; a device that
/// has not answered in `budget` was going to time out anyway.
async fn drain_inflight(inflight: &AtomicU64, budget: Duration) {
    let started = tokio::time::Instant::now();
    loop {
        let n = inflight.load(Ordering::Relaxed);
        if n == 0 {
            return;
        }
        if started.elapsed() >= budget {
            tracing::warn!(
                inflight = n,
                "shutting down with probes still in flight — their results are lost"
            );
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Consume this poller's working-set syncs, folding each into the shared [`WorkingSet`]. On a gap /
/// epoch mismatch ([`ApplyOutcome::NeedSync`]) it asks core for a fresh snapshot; on a successful
/// apply it advances the sync metrics and the specs gauge. Runs until the stream ends.
async fn run_sync_loop<B, S>(
    mut sync: S,
    working_set: Arc<Mutex<WorkingSet>>,
    bus: Arc<B>,
    poller_id: String,
    pool: String,
    incarnation: Uuid,
) where
    B: SyncBus + 'static,
    S: futures::Stream<Item = SyncMsg> + Unpin,
{
    use futures::stream::StreamExt;
    use rand::Rng;
    // Zero-capture jitter source (Send across the await points), guarded so it never gets a 0 bound.
    let mut rng = |bound: u32| {
        if bound == 0 {
            0
        } else {
            rand::thread_rng().gen_range(0..bound)
        }
    };
    while let Some(msg) = sync.next().await {
        let is_snapshot = matches!(msg, SyncMsg::SnapshotChunk(_));
        let outcome = {
            let mut ws = working_set.lock().expect("working set mutex poisoned");
            ws.apply(msg, Instant::now(), &mut rng)
        };
        match outcome {
            ApplyOutcome::Applied => {
                if is_snapshot {
                    metrics::counter!("yagra_sync_snapshots_total").increment(1);
                } else {
                    metrics::counter!("yagra_sync_deltas_total").increment(1);
                }
                let (_, specs) = working_set
                    .lock()
                    .expect("working set mutex poisoned")
                    .stats();
                metrics::gauge!("yagra_working_set_specs").set(f64::from(specs));
            }
            ApplyOutcome::NeedSync => {
                metrics::counter!("yagra_sync_gaps_total").increment(1);
                tracing::info!("working-set gap/epoch mismatch — requesting a fresh snapshot");
                let req = SyncRequest {
                    poller_id: poller_id.clone(),
                    pool: pool.clone(),
                    incarnation,
                };
                if let Err(e) = bus.publish_sync_request(req).await {
                    tracing::warn!(error = %e, "failed to publish sync request");
                }
            }
            ApplyOutcome::Ignored => {}
        }
    }
    tracing::warn!("working-set sync stream ended");
}

/// Tick every [`working_set::SCHEDULER_TICK`], popping the due specs from the working set and
/// forwarding each as a job to the worker loop over a bounded channel (backpressure: a full channel
/// awaits, it never drops). Stops when the channel closes (worker gone).
///
/// The period is the working set's constant, not a literal here: it spaces a node's specs by whole
/// ticks so they cannot collide in the worker's per-device single-flight guard, which only holds
/// while this timer runs at that period.
async fn run_local_scheduler(working_set: Arc<Mutex<WorkingSet>>, jobs_tx: mpsc::Sender<PollJob>) {
    let mut tick = tokio::time::interval(working_set::SCHEDULER_TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        let due = {
            let mut ws = working_set.lock().expect("working set mutex poisoned");
            ws.due(Instant::now())
        };
        for job in due {
            if jobs_tx.send(job).await.is_err() {
                tracing::warn!("worker channel closed — stopping local scheduler");
                return;
            }
        }
    }
}

/// Publish a liveness + telemetry heartbeat every [`HEARTBEAT_SECS`] (ADR-009). Echoes the working
/// set's epoch/last_seq so core can spot a stale/gapped poller, plus node/spec/inflight/result
/// counts, the bound listeners, and a host-resource sample (CPU/load/mem/disk). Never logs or
/// carries a secret.
#[allow(clippy::too_many_arguments)]
async fn run_heartbeat_loop<B>(
    bus: Arc<B>,
    poller_id: String,
    pool: String,
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
            pool: pool.clone(),
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
            ]
            .into_iter()
            // Unlike the four above, this one is conditional: it says a site updater is deployed
            // beside this poller and has been seen alive, so core may hand it a release to install
            // (ADR-051). Claiming it unconditionally would make core send commands into sites that
            // cannot act on them, and report every such site as "will upgrade" when it will not.
            .chain(self_upgrade_cap())
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

/// Spawn the syslog / SNMP-trap listeners for every bind address configured via env, returning a
/// label per listener that actually bound (for the heartbeat's `listeners` telemetry). Unset (or
/// empty) env = listener disabled. Both share one rate limiter so the global budget covers all
/// passive intake on this poller.
///
/// Pool tag: the event's `pool` stays the **raw** `YAGRA_POLLER_POOL` env option (unset ⇒ `None` ⇒
/// stored NULL core-side), unchanged from before — only job subscription uses the defaulted pool.
async fn spawn_event_listeners(
    bus: &Arc<yagra_bus::NatsBus>,
    shutdown: &CancellationToken,
    poller_id: &str,
    pool_defaulted: &str,
) -> Vec<String> {
    let syslog_bind = env_nonempty("YAGRA_SYSLOG_BIND");
    let trap_bind = env_nonempty("YAGRA_TRAP_BIND");
    // Flow collector (Phase 3, ADR-031) — NetFlow v5/v9 / IPFIX on `YAGRA_FLOW_BIND` (:2055-style),
    // sFlow v5 on `YAGRA_SFLOW_BIND` (:6343). Both off if unset; both feed the same aggregator.
    let flow_bind = env_nonempty("YAGRA_FLOW_BIND");
    let sflow_bind = env_nonempty("YAGRA_SFLOW_BIND");
    if syslog_bind.is_none() && trap_bind.is_none() && flow_bind.is_none() && sflow_bind.is_none() {
        return Vec::new();
    }

    // Edge intake caps (S8). Raised from the original 50/500 after the 2026-07-11 load test showed
    // the core matcher + single NATS event subscriber sustain ≥27k msg/s with zero NATS drop (the
    // real ceiling is the async persist writer, which sheds best-effort past it) — so the old
    // defaults dropped 75-97% of a realistic multi-device / chassis-storm flow for no protective
    // benefit. A chassis router's syslog burst now fits per-source; the global cap stays well under
    // the measured drain limit. Both remain env-tunable per deployment.
    let per_source = env_f64("YAGRA_EVENT_RATE_PER_SOURCE", 200.0);
    let global = env_f64("YAGRA_EVENT_RATE_GLOBAL", 5000.0);
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
    // One shared limiter behind a `std::sync::Mutex` (S22): its critical section is a few
    // arithmetic ops and is never held across an await, so all readers share the exact global
    // budget without async-lock overhead. Sharing (not sharding) keeps the global rate correct.
    let limiter = Arc::new(std::sync::Mutex::new(yagra_ingest::SourceLimiter::new(
        per_source, global, now_ms,
    )));
    let pool = env_nonempty("YAGRA_POLLER_POOL");
    // Parallel receive (S9): N reader sockets per protocol via SO_REUSEPORT, each with an enlarged
    // SO_RCVBUF. Default worker count tracks CPUs (capped) so a single poller drains the kernel in
    // parallel; both knobs are env-tunable per deployment.
    let workers = env_listener_workers();
    let rcvbuf = env_usize("YAGRA_LISTENER_RCVBUF_BYTES", 4 * 1024 * 1024);
    let mut labels = Vec::new();

    if let Some(bind) = syslog_bind {
        match listeners::bind_reuseport(&bind, workers, rcvbuf).await {
            Ok(socks) => {
                let n = socks.len();
                tracing::info!(%bind, workers = n, rcvbuf, per_source, global, "syslog listener enabled");
                labels.push(format!("syslog:{bind}"));
                for sock in socks {
                    spawn_cancellable(
                        shutdown,
                        listeners::run_syslog_listener(
                            sock,
                            bus.clone(),
                            limiter.clone(),
                            pool.clone(),
                        ),
                    );
                }
            }
            Err(e) => tracing::error!(%bind, error = %e, "failed to bind syslog listener"),
        }
    }

    if let Some(bind) = trap_bind {
        // Optional community filter — value must never be logged.
        let community = env_nonempty("YAGRA_TRAP_COMMUNITY");
        match listeners::bind_reuseport(&bind, workers, rcvbuf).await {
            Ok(socks) => {
                let n = socks.len();
                tracing::info!(%bind, workers = n, rcvbuf, community_filter = community.is_some(), "trap listener enabled (v1/v2c; v3 traps out of scope)");
                labels.push(format!("trap:{bind}"));
                for sock in socks {
                    spawn_cancellable(
                        shutdown,
                        listeners::run_trap_listener(
                            sock,
                            bus.clone(),
                            limiter.clone(),
                            community.clone(),
                            pool.clone(),
                        ),
                    );
                }
            }
            Err(e) => tracing::error!(%bind, error = %e, "failed to bind trap listener"),
        }
    }

    if flow_bind.is_some() || sflow_bind.is_some() {
        // Flow gets its own rate limiter (own env knobs): a flow datagram carries many records, so
        // it must not be counted against the syslog/trap event budget or starve it. Caps are on
        // datagrams/second per source, not records. Edge top-N aggregation (ADR-031) is the real
        // cardinality control; this is just the storm front door. NetFlow and sFlow share one
        // limiter, one aggregator `state`, and one flush ticker — a device exporting both merges
        // per-exporter, and there is one bucket cadence per poller.
        let flow_per_source = env_f64("YAGRA_FLOW_RATE_PER_SOURCE", 1000.0);
        let flow_global = env_f64("YAGRA_FLOW_RATE_GLOBAL", 20_000.0);
        let flow_now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
        let flow_limiter = Arc::new(std::sync::Mutex::new(yagra_ingest::SourceLimiter::new(
            flow_per_source,
            flow_global,
            flow_now_ms,
        )));
        let top_n = env_usize("YAGRA_FLOW_TOP_N", yagra_ingest::DEFAULT_FLOW_TOP_N);
        let bucket_secs = u32::try_from(env_usize("YAGRA_FLOW_BUCKET_SECS", 60)).unwrap_or(60);
        let state = Arc::new(std::sync::Mutex::new(flow::FlowState::new(top_n)));
        // Verbatim relay for forwarding (ADR-034 Increment 2). Unconditional: the aggregate above is
        // irreversible (bucketed, top-N, folded), so without the original datagrams a flow
        // forwarding destination could never be honoured — and making it a toggle would turn
        // fidelity into a configuration question. The tee is shared by both protocol readers.
        let (raw_tee, raw_relay) = flow::raw_flow_tee(poller_id.to_owned(), pool.clone());
        let mut any_bound = false;

        if let Some(bind) = flow_bind {
            match listeners::bind_reuseport(&bind, workers, rcvbuf).await {
                Ok(socks) => {
                    let n = socks.len();
                    tracing::info!(%bind, workers = n, rcvbuf, top_n, bucket_secs, "flow listener enabled (NetFlow v5/v9 / IPFIX)");
                    labels.push(format!("flow:{bind}"));
                    any_bound = true;
                    for sock in socks {
                        spawn_cancellable(
                            shutdown,
                            flow::run_flow_listener(
                                sock,
                                state.clone(),
                                flow_limiter.clone(),
                                flow::FlowProto::Netflow,
                                Some(raw_tee.clone()),
                            ),
                        );
                    }
                }
                Err(e) => tracing::error!(%bind, error = %e, "failed to bind flow listener"),
            }
        }

        if let Some(bind) = sflow_bind {
            match listeners::bind_reuseport(&bind, workers, rcvbuf).await {
                Ok(socks) => {
                    let n = socks.len();
                    tracing::info!(%bind, workers = n, rcvbuf, top_n, bucket_secs, "sflow listener enabled (sFlow v5)");
                    labels.push(format!("sflow:{bind}"));
                    any_bound = true;
                    for sock in socks {
                        spawn_cancellable(
                            shutdown,
                            flow::run_flow_listener(
                                sock,
                                state.clone(),
                                flow_limiter.clone(),
                                flow::FlowProto::Sflow,
                                Some(raw_tee.clone()),
                            ),
                        );
                    }
                }
                Err(e) => tracing::error!(%bind, error = %e, "failed to bind sflow listener"),
            }
        }

        // One flush ticker publishes per-exporter FlowBatches every bucket, and one relay task
        // publishes the verbatim datagrams — both spawned only if at least one protocol socket
        // bound (nothing to flush or relay otherwise).
        if any_bound {
            spawn_cancellable(
                shutdown,
                flow::run_flow_flusher(
                    bus.clone(),
                    state.clone(),
                    poller_id.to_owned(),
                    pool_defaulted.to_owned(),
                    bucket_secs,
                ),
            );
            spawn_cancellable(shutdown, flow::run_raw_flow_relay(bus.clone(), raw_relay));
        }
    }

    labels
}

/// Number of parallel `recv_from` readers per edge listener (S9). Defaults to the host's parallelism
/// capped at 4 (a single poller sustains tens of thousands of msg/s well before this matters); env
/// `YAGRA_LISTENER_WORKERS` overrides. Non-Unix collapses to one socket in `bind_reuseport` anyway.
fn env_listener_workers() -> usize {
    let default = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        .clamp(1, 4);
    env_usize("YAGRA_LISTENER_WORKERS", default).max(1)
}

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(default)
}

fn env_nonempty(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.is_empty())
}

fn env_f64(key: &str, default: f64) -> f64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .filter(|v| *v > 0.0)
        .unwrap_or(default)
}

/// Connect to NATS, retrying with a fixed backoff so startup ordering doesn't matter. `ca_file`
/// pins the server certificate for the remote-poller TLS path (`None` = plaintext single-node).
/// `poller_id`/`pool` are presented to core's Auth Callout for per-poller credential scoping (ADR-030).
async fn connect_bus(
    url: &str,
    ca_file: Option<&Path>,
    poller_id: &str,
    pool: &str,
) -> anyhow::Result<NatsBus> {
    const MAX_ATTEMPTS: u32 = 30;
    let mut attempt = 0;
    loop {
        match NatsBus::connect_opts_identified(url, ca_file, poller_id, pool).await {
            Ok(bus) => return Ok(bus),
            Err(e) if attempt < MAX_ATTEMPTS => {
                attempt += 1;
                tracing::warn!(error = %e, attempt, "NATS not ready; retrying in 2s");
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
            Err(e) => anyhow::bail!("NATS connect failed after {MAX_ATTEMPTS} attempts: {e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Identity resolution: sanitized id, defaulted pool, per-boot incarnation. Runs without env
    /// (the CI/dev default) so it doesn't race other tests over process-global env vars.
    #[test]
    fn identity_defaults_are_sane_and_sanitized() {
        let id = resolve_identity();
        // Pool defaults to "default" when YAGRA_POLLER_POOL is unset.
        assert_eq!(id.pool, "default");
        // The id is a legal single NATS subject token (sanitized): only [A-Za-z0-9_-].
        assert!(!id.id.is_empty());
        assert!(id
            .id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'));
        // Sanitizing again is a no-op (idempotent) → subscribe/publish subjects always match.
        assert_eq!(subjects::sanitize_token(&id.id), id.id);
        assert_eq!(id.version, env!("CARGO_PKG_VERSION"));
    }

    /// The assignment subject the poller subscribes to must equal the one core publishes to, given
    /// the same id — both funnel through `assignment_for` / `sanitize_token`.
    #[test]
    fn identity_id_round_trips_to_a_stable_assignment_subject() {
        let id = resolve_identity();
        assert_eq!(
            subjects::assignment_for(&id.id),
            subjects::assignment_for(&subjects::sanitize_token(&id.id))
        );
    }

    #[tokio::test(start_paused = true)]
    async fn draining_returns_at_once_when_nothing_is_in_flight() {
        // The common case, and the one that must not add latency: an idle poller exits immediately.
        let idle = AtomicU64::new(0);
        let started = tokio::time::Instant::now();
        drain_inflight(&idle, INFLIGHT_DRAIN_TIMEOUT).await;
        assert_eq!(started.elapsed(), Duration::ZERO);
    }

    #[tokio::test(start_paused = true)]
    async fn draining_waits_for_a_probe_then_stops_waiting_for_a_stuck_one() {
        // A probe that finishes releases the shutdown as soon as the count reaches zero...
        let busy = Arc::new(AtomicU64::new(1));
        let finisher = busy.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(200)).await;
            finisher.store(0, Ordering::Relaxed);
        });
        let started = tokio::time::Instant::now();
        drain_inflight(&busy, INFLIGHT_DRAIN_TIMEOUT).await;
        let waited = started.elapsed();
        assert!(waited >= Duration::from_millis(200), "waited {waited:?}");
        assert!(waited < INFLIGHT_DRAIN_TIMEOUT, "waited {waited:?}");

        // ...and one that never does cannot hold the process open past the budget, or `docker stop`
        // escalates to SIGKILL and we lose the `leaving` beat as well as the probe.
        let stuck = AtomicU64::new(3);
        let started = tokio::time::Instant::now();
        drain_inflight(&stuck, INFLIGHT_DRAIN_TIMEOUT).await;
        assert!(started.elapsed() >= INFLIGHT_DRAIN_TIMEOUT);
    }

    /// The `leaving` beat is the one publish in the process with no successor, so it is the one that
    /// must be flushed. Nothing in the type system says so — `publish` returning `Ok` reads like
    /// delivery — hence a test that reads the source and pins the call.
    #[test]
    fn the_leaving_beat_is_flushed_before_the_loop_returns() {
        let src = include_str!("main.rs");
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
}
