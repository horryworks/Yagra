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
mod assignment;
mod discovery;
mod flow;
/// What ADR-103 bought, written as checks. Test-only, like `module_source`.
#[cfg(test)]
mod guards;
mod heartbeat;
mod l3;
mod limiter;
mod listeners;
mod location;
mod mau;
/// Reading a module's own source text (ADR-091/099). Test-only; the rule lives in
/// `yagra_common::srcread` and this is only where this crate is on disk.
#[cfg(test)]
mod module_source;
mod neighbors;
mod optical;
mod pool;
mod routing;
mod store_forward;
mod support_logs;
mod upgrade;
mod worker;
mod working_set;

use limiter::PollLimiter;
use metrics_exporter_prometheus::PrometheusBuilder;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio_stream::wrappers::ReceiverStream;
use uuid::Uuid;
use working_set::WorkingSet;
use yagra_bus::{subjects, NatsBus};
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

/// How the poller identifies itself to core (ADR-009). `id` is already sanitized for use as a NATS
/// subject token; `pool` is the (defaulted) pool it serves.
struct PollerIdentity {
    /// Sanitized, subject-safe poller id — stable across restarts (from `YAGRA_POLLER_ID` else the
    /// hostname else a random fallback).
    id: String,
    /// The pool this poller **starts** in (`YAGRA_POLLER_POOL` else `"default"`).
    ///
    /// ⚠️ **Not the pool it serves.** Core decides that (ADR-107 Inc.2) and sends it on the
    /// working-set snapshot; `pool::PoolState` holds the live answer and everything that needs one
    /// reads it there. This field is only the value core adopts when it has never seen this poller
    /// before — after the inventory row exists it is ignored, which is what lets a move survive a
    /// container restart.
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
    // Before anything that could reach a TLS library. Two crypto providers are enabled in this
    // dependency graph, so rustls installs no default and `async_nats` panics building its own
    // client config the moment the bus URL is `tls://` (ADR-065 Inc.5 bug 3).
    yagra_bus::install_tls_crypto_provider();

    // Resolved before telemetry because the id names this poller's **log file** (ADR-045 Inc.3):
    // a pool sharing one log directory would otherwise have every member appending to the same
    // hourly file. Nothing here logs, so there is no gap in the trace.
    let identity = resolve_identity();

    // Structured logs + optional OpenTelemetry span export (self-observability). The guard flushes
    // spans at shutdown, so keep it alive for the whole process (`main` blocks on the worker loop).
    let _telemetry = yagra_telemetry::init_instance("yagra-poller", Some(&identity.id));

    // Self-observability: expose Prometheus metrics on :9100/metrics (monitoring-conventions).
    //
    // `yagra_poll_phase_seconds` gets explicit buckets so it renders as a real histogram rather
    // than this exporter's default rolling summary: quantiles computed per poller cannot be added
    // up across a pool, and "how is the fleet's polling doing" is a question about the pool. The
    // range spans one network round trip (5 ms) to the single-flight ceiling (30 s+), because the
    // distribution this exists to show is bimodal — a probe that answers, and a probe that waits
    // out a 1 s timeout (ADR-109).
    let exporter = || PrometheusBuilder::new().with_http_listener(([0, 0, 0, 0], 9100));
    let builder = match exporter().set_buckets_for_metric(
        metrics_exporter_prometheus::Matcher::Full(worker::POLL_PHASE_METRIC.to_owned()),
        worker::POLL_PHASE_BUCKETS,
    ) {
        Ok(with_buckets) => with_buckets,
        Err(e) => {
            tracing::warn!(error = %e, "poll-phase buckets rejected; exporting a summary instead");
            exporter()
        }
    };
    if let Err(e) = builder.install() {
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

    // Rate control: bound total concurrent probes + per-device single-flight (#4). The default
    // lives beside the semaphore it sizes, with the measurement that chose it — never a literal
    // here, because four shipped files quote that number and a test pins them to the constant.
    let max_concurrent = env_usize(
        "YAGRA_MAX_CONCURRENT_POLLS",
        limiter::DEFAULT_MAX_CONCURRENT_POLLS,
    );
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

    // Passive-event listeners (Phase 2: syslog / SNMP traps) and the flow collector (ADR-031),
    // each enabled per site via env. They publish on `yagra.events` / `yagra.flows`; core does
    // the matching and the storing. The returned labels advertise which ones actually bound
    // (heartbeat telemetry), and the two share only how a UDP edge socket is opened.
    // Which pool this poller serves, from here on. `identity.pool` is only the starting value —
    // core owns the answer (ADR-107 Inc.2) and hands it over on the working-set snapshot, at which
    // point this state re-points the three pool-derived subscriptions and reconnects the bus.
    let pool = pool::PoolState::new(identity.pool.clone(), Some(bus.clone()));

    let tuning = listeners::EdgeTuning::from_env();
    let mut listener_labels = listeners::start(&bus, &shutdown, &tuning).await;
    listener_labels
        .extend(flow::start(&bus, &shutdown, &identity.id, &identity.pool, &tuning).await);

    // Discovery sweeps run alongside polling and need the same raw-socket ICMP + SNMP
    // transport this poller already holds.
    discovery::start(&bus, &transport, &pool, &queue, &shutdown).await?;

    // Working-set sync (ADR-020) and the local scheduler that drains it: two loops around one
    // set, started together because they are one program. Hands back the receiving half of the
    // job channel, which merges into the worker stream below.
    //
    // ⚠️ The scheduler now spawns a few subscribes earlier than it used to, and that cannot
    // matter: the working set is empty until the first snapshot lands, so `due()` has nothing
    // to hand over, and the channel it feeds is bounded and awaits rather than dropping.
    let jobs_rx = assignment::start(&bus, &identity, &pool, &working_set, &shutdown).await?;

    // Heartbeat (ADR-009): liveness + telemetry every HEARTBEAT_SECS, ending in the `leaving`
    // beat this function joins below. Deliberately not cancellable — see the module doc.
    let heartbeat_task = heartbeat::start(
        &bus,
        &identity,
        &pool,
        &working_set,
        &results_total,
        &inflight,
        listener_labels,
        &shutdown,
    );

    // Upgrade commands (ADR-051): core tells this poller to replace itself, and this poller
    // writes the request into the hand-off directory its site updater watches.
    upgrade::start(&bus, &identity, &shutdown).await?;

    // Support-log requests (ADR-045 Inc.4): core asks this poller for a window of its own
    // on-disk log so a support bundle can carry a remote site's evidence.
    support_logs::start(&bus, &identity.id, &working_set, &shutdown).await?;

    // Legacy / pool-scoped jobs: consume only this poller's pool (no more `yagra.jobs.*` wildcard)
    // so work stays local (ADR-009). Merge with the locally-scheduled jobs into one stream driving
    // the shared worker loop (PollLimiter + execution + result publish are shared).
    //
    // Behind a relay since ADR-107 Inc.2, because this subject's name contains the pool: the
    // stream below must survive a move, and what changes is the subscription feeding it. This is
    // the subject "poll now" arrives on, so getting it wrong is a control that does nothing.
    let legacy_jobs = {
        let bus = bus.clone();
        let queue = queue.clone();
        pool::relay("jobs", &pool, &shutdown, move |p| {
            let bus = bus.clone();
            let queue = queue.clone();
            async move { bus.subscribe_jobs_for_pool(&p, &queue).await }
        })
    };
    let unified = Box::pin(futures::stream::select(
        ReceiverStream::new(legacy_jobs),
        ReceiverStream::new(jobs_rx),
    ));
    let poller_id: Arc<str> = Arc::from(identity.id.as_str());
    tracing::info!(
        pool = %pool.current(),
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
/// Which username this poller presents to the broker — a **deployment** question, not an identity
/// one, and the two shipped bus configurations want opposite answers.
///
/// * **Auth Callout on (ADR-030)**, `YAGRA_BUS_AUTH_CALLOUT=1`: the poller's own id. Core's callout
///   scopes `yagra.poller.assign.{id}` to exactly that, and `nats-server.conf` deliberately keeps
///   the static `poller` account out of the way.
/// * **Auth Callout off — what ships**: the username written in the URL, which the ADR-065 switch
///   generates as the literal `poller`. That static account is the only thing authorizing anyone.
///
/// 🚨 The id was presented unconditionally until 2026-08-25, so on the default configuration the
/// poller offered its **container hostname** and NATS answered `authentication error - User
/// "4aeea2381430"` forever (measured on 192.168.1.211, ADR-065 Inc.5 bug 8). `nats-server.conf`
/// already said what should happen — "this static account remains the fallback when callout is
/// off" — and nothing on this side had ever been told which mode it was in. The knob is the
/// missing half of that sentence, and it defaults to the configuration that ships.
fn bus_username<'a>(url: &'a str, poller_id: &'a str) -> &'a str {
    if env_nonempty("YAGRA_BUS_AUTH_CALLOUT")
        .is_some_and(|v| v == "1" || v.eq_ignore_ascii_case("true"))
    {
        return poller_id;
    }
    // Leaked deliberately: the caller holds `url` for the whole connection attempt, and the
    // alternative is allocating a String per retry for a value that never changes.
    match url
        .split_once("://")
        .map_or(url, |(_, r)| r)
        .rsplit_once('@')
    {
        Some((userinfo, _)) => match userinfo.split_once(':') {
            Some((u, _)) if !u.is_empty() => u,
            _ => poller_id,
        },
        // No userinfo at all — the plaintext single-node bus, where nothing is presented anyway.
        None => poller_id,
    }
}

async fn connect_bus(
    url: &str,
    ca_file: Option<&Path>,
    poller_id: &str,
    pool: &str,
) -> anyhow::Result<NatsBus> {
    const MAX_ATTEMPTS: u32 = 30;
    let username = bus_username(url, poller_id);
    let mut attempt = 0;
    loop {
        match NatsBus::connect_opts_identified(url, ca_file, username, pool).await {
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

    /// Which name the poller offers the broker, decided from the URL rather than from its identity.
    ///
    /// 🚨 This is the fix for a poller that could never authenticate on the configuration that
    /// ships. It presented its own id — a container hostname — while `nats-server.conf`'s static
    /// account is called `poller`, so NATS answered `authentication error - User "4aeea2381430"`
    /// for as long as the deployment was up (192.168.1.211, 2026-08-25).
    ///
    /// Reads no environment, so it cannot race the other tests over process-global state; the
    /// callout branch is asserted through its own env var in a test that does set one.
    #[test]
    fn the_poller_offers_the_name_the_bus_configuration_expects() {
        // What the ADR-065 switch generates: the static account's name is in the URL.
        assert_eq!(
            bus_username("tls://poller:s3cretpassword@nats:4222", "4aeea2381430"),
            "poller"
        );
        // A password containing a colon must not shift the username.
        assert_eq!(bus_username("tls://poller:a:b@host:4222", "id"), "poller");
        // The plaintext single-node bus carries no userinfo and presents nothing, so the id is as
        // good an answer as any — and is what it has always been.
        assert_eq!(bus_username("nats://nats:4222", "aio-1"), "aio-1");
        // Userinfo with an empty username is not a username.
        assert_eq!(bus_username("tls://:pw@nats:4222", "aio-1"), "aio-1");
    }

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
}
