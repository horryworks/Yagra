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
        // Stop commands (ADR-068 Inc.2), on the two routes a sweep can arrive by and mirroring the
        // job subscriptions above. **No queue group**, unlike those: core cannot know which poller
        // took a queue-delivered job, so a stop goes to everyone and the `scan_id` decides.
        //
        // Consumed by a task of its own, which is what makes the stop arrive at all: the sweep loop
        // is strictly sequential, so a cancel riding the same stream would queue behind the very
        // sweep it is meant to interrupt.
        let cancels = Arc::new(discovery::CancelSet::new());
        let cancel_global = Box::pin(bus.subscribe_discovery_cancels(None).await?);
        let cancel_pooled = Box::pin(
            bus.subscribe_discovery_cancels(Some(&identity.pool))
                .await?,
        );
        spawn_cancellable(
            &shutdown,
            discovery::run_cancel_stream(
                Box::pin(futures::stream::select(cancel_global, cancel_pooled)),
                cancels.clone(),
            ),
        );
        let bus = bus.clone();
        let transport = transport.clone();
        spawn_cancellable(
            &shutdown,
            discovery::run_discovery_stream(merged, bus, transport, cancels),
        );
    }

    // Working-set sync (ADR-020) and the local scheduler that drains it: two loops around one
    // set, started together because they are one program. Hands back the receiving half of the
    // job channel, which merges into the worker stream below.
    //
    // ⚠️ The scheduler now spawns a few subscribes earlier than it used to, and that cannot
    // matter: the working set is empty until the first snapshot lands, so `due()` has nothing
    // to hand over, and the channel it feeds is bounded and awaits rather than dropping.
    let jobs_rx = assignment::start(&bus, &identity, &working_set, &shutdown).await?;

    // Heartbeat (ADR-009): liveness + telemetry every HEARTBEAT_SECS, ending in the `leaving`
    // beat this function joins below. Deliberately not cancellable — see the module doc.
    let heartbeat_task = heartbeat::start(
        &bus,
        &identity,
        &working_set,
        &results_total,
        &inflight,
        listener_labels,
        &shutdown,
    );

    // Upgrade commands (ADR-051): core tells this poller to replace itself, and this poller
    // writes the request into the hand-off directory its site updater watches.
    upgrade::start(&bus, &identity, &shutdown).await?;

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
}
