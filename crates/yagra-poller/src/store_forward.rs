// SPDX-License-Identifier: AGPL-3.0-only
//! Store-and-forward result buffer for a remote poller (Phase 3).
//!
//! When the core↔poller NATS link partitions, the poller keeps executing its working set locally
//! (ADR-020) but its [`PollResult`]s have nowhere to go — `Bus::publish_result` fails and the result
//! is lost (`worker.rs` used to log-and-drop). This module gives those results a bounded home: a
//! two-tier buffer (an in-memory ring + an on-disk NDJSON spill) that fills during a partition and
//! **replays on reconnect** onto the dedicated `yagra.results.backfill` subject. Core imports
//! backfill to the TSDB at each result's original `at_unix_ms` (so history fills at true time, with
//! no spike) and deliberately **skips alert evaluation** — replaying a burst of stale samples must
//! not re-fire resolved alerts (see [`yagra_bus::subjects::results_backfill`]).
//!
//! Invariants (security.md / ADR-003):
//! - **Bounded, drop-oldest.** The buffer never grows without limit; disk usage is capped and the
//!   *oldest* history is discarded first. Store-and-forward must never OOM the poller or fill the
//!   host disk. Dropping the oldest slice only shortens how far back metrics history is backfilled;
//!   alerts resume from "now" regardless, so there is no alert-correctness impact.
//! - **Never blocks the poll loop.** Every buffer op is best-effort; a spill/replay failure is
//!   counted and logged, never propagated into polling.
//! - **No secrets on disk.** A [`PollResult`] carries samples/outcomes only — credentials live in
//!   the `JobSpec`/`PollJob` (never echoed back), so the spill holds nothing sensitive. The spill
//!   directory is still created `0700`.

use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};

use yagra_bus::{Bus, PollResult};
use yagra_telemetry::CancellationToken;

/// Metric names (self-observability, monitoring-conventions §Self-observability).
const M_DEPTH: &str = "yagra_poller_buffer_depth";
const M_BYTES: &str = "yagra_poller_buffer_bytes";
const M_DROPPED: &str = "yagra_poller_buffer_dropped_total";
const M_BACKFILL: &str = "yagra_poller_backfill_results_total";
const M_CONNECTED: &str = "yagra_poller_connected";

/// Default disk-segment roll size. Sized so a replay read (which loads a whole segment into memory)
/// stays small even on a weak remote box, while being large enough that the byte cap is enforced by
/// dropping whole oldest segments (so `disk_max` should be a comfortable multiple of this).
const SEGMENT_MAX_BYTES: u64 = 16 * 1024 * 1024;

/// How often the drain task wakes to check for reconnection + a non-empty buffer.
const DRAIN_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

/// How many in-memory results to publish per replay batch before re-checking the connection.
const REPLAY_BATCH: usize = 256;

/// Rate-limit the "dropping oldest" warning to at most once per this interval (ms).
const DROP_WARN_EVERY_MS: i64 = 30_000;

/// Tunables for the store-and-forward buffer. All are env-overridable; defaults are conservative for
/// a small remote box.
#[derive(Debug, Clone)]
pub struct SfConfig {
    /// Master switch. When `false` the sink is a pure pass-through (byte-identical to the pre-feature
    /// behaviour: publish live, drop on failure).
    pub enabled: bool,
    /// Directory for the on-disk spill (created `0700`). Empty ⇒ memory-only.
    pub dir: PathBuf,
    /// Max results held in the in-memory ring before the oldest spill to disk.
    pub mem_max: usize,
    /// Max total bytes of on-disk spill segments before the oldest segment is dropped.
    pub disk_max_bytes: u64,
    /// Drop buffered results older than this at replay time (stale history not worth backfilling).
    pub max_age_ms: i64,
    /// Stop spilling to disk when the filesystem's available space drops below this — the host-disk
    /// safety floor (the remote poller shares the host, `network_mode: host`).
    pub free_floor_bytes: u64,
    /// Roll to a new spill segment once the current one exceeds this many bytes. The disk cap is
    /// enforced by dropping whole oldest segments, so this is the granularity of `disk_max_bytes`.
    pub segment_max_bytes: u64,
}

impl SfConfig {
    /// Build from `YAGRA_STORE_FORWARD*` env, applying defaults. `YAGRA_STORE_FORWARD=off` (or
    /// `false`/`0`) disables the feature.
    #[must_use]
    pub fn from_env() -> Self {
        fn var(name: &str) -> Option<String> {
            std::env::var(name).ok().filter(|v| !v.trim().is_empty())
        }
        let enabled = var("YAGRA_STORE_FORWARD")
            .map(|v| {
                !matches!(
                    v.trim().to_ascii_lowercase().as_str(),
                    "off" | "false" | "0" | "no"
                )
            })
            .unwrap_or(true); // default ON
        let dir = var("YAGRA_STORE_FORWARD_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/var/lib/yagra/buffer"));
        let mem_max = var("YAGRA_STORE_FORWARD_MEM_MAX")
            .and_then(|v| v.parse().ok())
            .unwrap_or(20_000usize)
            .max(1);
        let disk_max_bytes = var("YAGRA_STORE_FORWARD_DISK_MAX_MB")
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(512)
            .saturating_mul(1024 * 1024);
        let max_age_ms = var("YAGRA_STORE_FORWARD_MAX_AGE_SECS")
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(86_400)
            .saturating_mul(1000);
        let free_floor_bytes = var("YAGRA_STORE_FORWARD_DISK_FREE_FLOOR_MB")
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(1024)
            .saturating_mul(1024 * 1024);
        let segment_max_bytes = var("YAGRA_STORE_FORWARD_SEGMENT_MB")
            .and_then(|v| v.parse::<u64>().ok())
            .filter(|&mb| mb > 0)
            .map_or(SEGMENT_MAX_BYTES, |mb| mb.saturating_mul(1024 * 1024));
        Self {
            enabled,
            dir,
            mem_max,
            disk_max_bytes,
            max_age_ms,
            free_floor_bytes,
            segment_max_bytes,
        }
    }
}

/// One on-disk spill segment: an append-only NDJSON file of [`PollResult`]s. Segments are ordered by
/// `seq` (oldest → newest); drop-oldest deletes the lowest-`seq` segment.
struct Segment {
    seq: u64,
    path: PathBuf,
    bytes: u64,
    count: u64,
}

/// The mutable buffer state, guarded by a single `std::sync::Mutex` that is **never** held across an
/// `.await` (the hot path is synchronous; the drain task takes the lock only for quick structural
/// edits and does its I/O outside it).
struct BufState {
    /// Newest tier: the most-recent results, in arrival order (`push_back` = newest).
    mem: VecDeque<PollResult>,
    /// Older tier: spill segments, oldest → newest. Empty when disk is disabled/unavailable.
    segments: VecDeque<Segment>,
    /// Append target for the current (newest) segment.
    writer: Option<BufWriter<File>>,
    /// Byte size of the current segment (drives rotation).
    cur_bytes: u64,
    /// Total bytes across all segments (drives the disk cap).
    disk_bytes: u64,
    /// Next segment sequence number.
    next_seq: u64,
    /// `true` once a disk write fails (ENOSPC etc.) or the dir is unusable — degrade to memory-only.
    disk_broken: bool,
}

impl BufState {
    fn total_depth(&self) -> u64 {
        self.mem.len() as u64 + self.segments.iter().map(|s| s.count).sum::<u64>()
    }
}

/// The store-and-forward result sink the poll loop publishes through.
pub struct StoreForwardSink {
    bus: Arc<dyn Bus>,
    cfg: SfConfig,
    state: Mutex<BufState>,
    last_drop_warn_ms: AtomicI64,
}

impl StoreForwardSink {
    /// A pass-through sink (store-and-forward disabled): `submit` publishes live and drops on
    /// failure, exactly as before the feature. A test convenience — the production off-switch is
    /// `YAGRA_STORE_FORWARD=off`, which yields the same behaviour via [`SfConfig::from_env`].
    #[cfg(test)]
    #[must_use]
    pub fn passthrough(bus: Arc<dyn Bus>) -> Arc<Self> {
        let mut cfg = SfConfig::from_env();
        cfg.enabled = false;
        Self::new(bus, cfg)
    }

    /// Build a sink from `cfg`. Never fails: if the spill directory can't be created/scanned, the
    /// sink logs a warning and runs memory-only (degrade, don't crash — ADR-017 spirit).
    #[must_use]
    pub fn new(bus: Arc<dyn Bus>, cfg: SfConfig) -> Arc<Self> {
        let mut state = BufState {
            mem: VecDeque::new(),
            segments: VecDeque::new(),
            writer: None,
            cur_bytes: 0,
            disk_bytes: 0,
            next_seq: 0,
            disk_broken: false,
        };
        if cfg.enabled && !cfg.dir.as_os_str().is_empty() {
            match Self::prepare_dir(&cfg.dir) {
                Ok(loaded) => {
                    let (segments, disk_bytes, next_seq) = loaded;
                    state.segments = segments;
                    state.disk_bytes = disk_bytes;
                    state.next_seq = next_seq;
                    if next_seq > 0 {
                        tracing::info!(
                            dir = %cfg.dir.display(),
                            segments = state.segments.len(),
                            depth = state.total_depth(),
                            "store-and-forward: recovered spilled results from a previous run"
                        );
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        dir = %cfg.dir.display(),
                        error = %e,
                        "store-and-forward: spill directory unusable — running memory-only"
                    );
                    state.disk_broken = true;
                }
            }
        }
        let sink = Arc::new(Self {
            bus,
            cfg,
            state: Mutex::new(state),
            last_drop_warn_ms: AtomicI64::new(0),
        });
        if let Ok(st) = sink.state.lock() {
            sink.record_gauges(&st);
        }
        sink
    }

    /// Whether the buffer is enabled (real store-and-forward vs. pass-through).
    #[must_use]
    pub fn is_enabled(&self) -> bool {
        self.cfg.enabled
    }

    /// Submit a freshly produced poll result. **Infallible** by contract — the poll loop must never
    /// block or error on the return path. When connected, publishes live; when the bus is down (or
    /// store-and-forward is off and the publish fails) it buffers or drops per policy.
    pub async fn submit(&self, result: PollResult) {
        if self.cfg.enabled && !self.bus.is_connected() {
            // Known-disconnected: buffer without even attempting a publish that would sit in
            // async-nats's pending buffer and be lost on a longer outage.
            self.enqueue(result);
            return;
        }
        // Connected (or pass-through): publish live. A publish error while "connected" is rare (a
        // dropped link surfaces as `is_connected() == false` on the next result), so we count-and-drop
        // rather than clone every steady-state result just to guard that edge.
        if let Err(e) = self.bus.publish_result(result).await {
            metrics::counter!(M_DROPPED, "tier" => "publish_error").increment(1);
            tracing::warn!(error = %e, "store-and-forward: live publish failed; result dropped");
        }
    }

    /// Buffer a result (disconnected path). Synchronous and best-effort: pushes to the in-memory
    /// ring, spills the overflow to disk, and enforces the disk cap by dropping the oldest segment.
    fn enqueue(&self, result: PollResult) {
        let Ok(mut st) = self.state.lock() else {
            // A poisoned lock must not take down polling.
            metrics::counter!(M_DROPPED, "tier" => "lock").increment(1);
            return;
        };
        st.mem.push_back(result);
        // Spill the oldest in-memory results once over the ring cap.
        while st.mem.len() > self.cfg.mem_max {
            let Some(old) = st.mem.pop_front() else { break };
            self.spill_to_disk(&mut st, old);
        }
        self.record_gauges(&st);
    }

    /// Move one result from memory to the disk tier, enforcing the byte cap + free-space floor.
    fn spill_to_disk(&self, st: &mut BufState, result: PollResult) {
        if st.disk_broken || self.cfg.dir.as_os_str().is_empty() {
            self.count_drop("no_disk", 1);
            return;
        }
        // Host-disk safety floor: never keep spilling when free space is low.
        if let Some(avail) = yagra_hoststats::available_bytes(&self.cfg.dir) {
            if avail < self.cfg.free_floor_bytes {
                self.count_drop("disk_full", 1);
                return;
            }
        }
        let mut line = match serde_json::to_string(&result) {
            Ok(s) => s,
            Err(e) => {
                tracing::debug!(error = %e, "store-and-forward: failed to encode spill line");
                self.count_drop("encode", 1);
                return;
            }
        };
        line.push('\n');
        let bytes = line.len() as u64;
        if let Err(e) = self.write_spill_line(st, line.as_bytes()) {
            tracing::warn!(error = %e, "store-and-forward: spill write failed — degrading to memory-only");
            st.disk_broken = true;
            st.writer = None;
            self.count_drop("write_error", 1);
            return;
        }
        st.disk_bytes += bytes;
        if let Some(seg) = st.segments.back_mut() {
            seg.bytes += bytes;
            seg.count += 1;
        }
        // Enforce the disk cap: drop whole oldest segments until under budget.
        while st.disk_bytes > self.cfg.disk_max_bytes && st.segments.len() > 1 {
            self.drop_oldest_segment(st);
        }
    }

    /// Append raw bytes to the current segment, rotating to a new one when it grows past
    /// [`SEGMENT_MAX_BYTES`].
    fn write_spill_line(&self, st: &mut BufState, bytes: &[u8]) -> std::io::Result<()> {
        if st.writer.is_none() || st.cur_bytes >= self.cfg.segment_max_bytes {
            if let Some(mut w) = st.writer.take() {
                let _ = w.flush();
            }
            let seq = st.next_seq;
            st.next_seq += 1;
            let path = segment_path(&self.cfg.dir, seq);
            let file = OpenOptions::new().create(true).append(true).open(&path)?;
            st.writer = Some(BufWriter::new(file));
            st.cur_bytes = 0;
            st.segments.push_back(Segment {
                seq,
                path,
                bytes: 0,
                count: 0,
            });
        }
        let w = st.writer.as_mut().expect("writer set above");
        w.write_all(bytes)?;
        // Flush to the OS so the spill survives a poller *process* restart (not power loss); at a
        // remote poller's low result rate this is cheap.
        w.flush()?;
        st.cur_bytes += bytes.len() as u64;
        Ok(())
    }

    /// Delete the oldest segment file and update accounting (drop-oldest). Counts its results as
    /// dropped so truncation is never silent.
    fn drop_oldest_segment(&self, st: &mut BufState) {
        let Some(seg) = st.segments.pop_front() else {
            return;
        };
        st.disk_bytes = st.disk_bytes.saturating_sub(seg.bytes);
        let _ = std::fs::remove_file(&seg.path);
        self.count_drop("disk_oldest", seg.count);
    }

    /// The replay loop: while connected and non-empty, drain the buffer oldest-first onto the
    /// backfill subject. Runs until `shutdown` is cancelled.
    pub async fn run_drain(self: Arc<Self>, shutdown: CancellationToken) {
        loop {
            tokio::select! {
                () = shutdown.cancelled() => break,
                () = tokio::time::sleep(DRAIN_INTERVAL) => {}
            }
            metrics::gauge!(M_CONNECTED).set(u8::from(self.bus.is_connected()));
            if !self.cfg.enabled || !self.bus.is_connected() {
                continue;
            }
            self.drain_round(&shutdown).await;
        }
    }

    /// Replay everything currently buffered (oldest disk segments first, then the in-memory ring),
    /// stopping early on shutdown, disconnect, or a publish failure (the un-sent remainder stays
    /// buffered for the next round). Duplicate replays after a mid-segment failure are harmless — the
    /// TSDB dedups identical `(series, ts)` samples.
    async fn drain_round(&self, shutdown: &CancellationToken) {
        loop {
            if shutdown.is_cancelled() || !self.bus.is_connected() {
                return;
            }
            // Oldest disk segment first.
            let oldest = {
                let Ok(st) = self.state.lock() else { return };
                st.segments.front().map(|s| (s.seq, s.path.clone()))
            };
            if let Some((seq, path)) = oldest {
                if self.replay_segment(seq, &path, shutdown).await {
                    let Ok(mut st) = self.state.lock() else {
                        return;
                    };
                    // Only remove if it's still the oldest (an enqueue drop-oldest could have raced).
                    if st.segments.front().is_some_and(|s| s.seq == seq) {
                        let seg = st.segments.pop_front().expect("front checked");
                        st.disk_bytes = st.disk_bytes.saturating_sub(seg.bytes);
                        let _ = std::fs::remove_file(&seg.path);
                    }
                    self.record_gauges(&st);
                    continue; // more segments may remain
                }
                return; // publish failed / disconnected — keep the segment for next round
            }
            // No segments: drain the in-memory ring in batches.
            let batch = {
                let Ok(mut st) = self.state.lock() else {
                    return;
                };
                let n = st.mem.len().min(REPLAY_BATCH);
                st.mem.drain(..n).collect::<Vec<_>>()
            };
            if batch.is_empty() {
                return; // buffer drained
            }
            let now = now_unix_ms();
            for (i, result) in batch.iter().enumerate() {
                if now - result.at_unix_ms > self.cfg.max_age_ms {
                    self.count_drop("too_old", 1);
                    continue;
                }
                if self
                    .bus
                    .publish_result_backfill(result.clone())
                    .await
                    .is_err()
                    || shutdown.is_cancelled()
                {
                    // Re-buffer the un-sent tail (this one + the rest) at the front, in order.
                    if let Ok(mut st) = self.state.lock() {
                        for r in batch[i..].iter().rev() {
                            st.mem.push_front(r.clone());
                        }
                    }
                    return;
                }
                metrics::counter!(M_BACKFILL).increment(1);
            }
            if let Ok(st) = self.state.lock() {
                self.record_gauges(&st);
            }
        }
    }

    /// Replay one disk segment onto the backfill subject. Returns `true` iff every (non-stale) line
    /// was published — only then does the caller delete the segment. A corrupt line is skipped.
    async fn replay_segment(&self, seq: u64, path: &Path, shutdown: &CancellationToken) -> bool {
        // Read the whole segment (up to `segment_max_bytes`) off the async runtime — a large blocking
        // read on a Tokio worker would stall other tasks (including the poll scheduler) on a weak box.
        let path_owned = path.to_path_buf();
        let content =
            match tokio::task::spawn_blocking(move || std::fs::read_to_string(&path_owned)).await {
                Ok(Ok(c)) => c,
                _ => {
                    // Missing/unreadable: drop it so we don't loop forever on it.
                    tracing::warn!(
                        seq,
                        "store-and-forward: cannot read spill segment — dropping"
                    );
                    return true;
                }
            };
        let now = now_unix_ms();
        for line in content.lines() {
            if line.is_empty() {
                continue;
            }
            if shutdown.is_cancelled() || !self.bus.is_connected() {
                return false;
            }
            let result: PollResult = match serde_json::from_str(line) {
                Ok(r) => r,
                Err(_) => {
                    self.count_drop("corrupt", 1);
                    continue;
                }
            };
            if now - result.at_unix_ms > self.cfg.max_age_ms {
                self.count_drop("too_old", 1);
                continue;
            }
            if self.bus.publish_result_backfill(result).await.is_err() {
                return false;
            }
            metrics::counter!(M_BACKFILL).increment(1);
        }
        true
    }

    /// Increment the drop counter and emit a rate-limited warning (never a silent truncation —
    /// monitoring-conventions §Self-observability).
    fn count_drop(&self, tier: &'static str, n: u64) {
        if n == 0 {
            return;
        }
        metrics::counter!(M_DROPPED, "tier" => tier).increment(n);
        let now = now_unix_ms();
        let last = self.last_drop_warn_ms.load(Ordering::Relaxed);
        if now - last >= DROP_WARN_EVERY_MS
            && self
                .last_drop_warn_ms
                .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
        {
            tracing::warn!(
                tier,
                dropped = n,
                "store-and-forward: buffer over capacity — dropping oldest results"
            );
        }
    }

    fn record_gauges(&self, st: &BufState) {
        metrics::gauge!(M_DEPTH).set(st.total_depth() as f64);
        metrics::gauge!(M_BYTES).set(st.disk_bytes as f64);
    }

    /// Create the spill dir (`0700`) and load any segments left by a previous run.
    fn prepare_dir(dir: &Path) -> std::io::Result<(VecDeque<Segment>, u64, u64)> {
        std::fs::create_dir_all(dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
        }
        let mut found: Vec<(u64, PathBuf, u64)> = Vec::new();
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            let Some(seq) = path.file_name().and_then(parse_segment_seq) else {
                continue;
            };
            let bytes = entry.metadata().map(|m| m.len()).unwrap_or(0);
            found.push((seq, path, bytes));
        }
        found.sort_by_key(|(seq, _, _)| *seq);
        let mut segments = VecDeque::new();
        let mut disk_bytes = 0u64;
        let mut max_seq = 0u64;
        for (seq, path, bytes) in found {
            let count = count_lines(&path).unwrap_or(0);
            disk_bytes += bytes;
            max_seq = max_seq.max(seq);
            segments.push_back(Segment {
                seq,
                path,
                bytes,
                count,
            });
        }
        let next_seq = if segments.is_empty() { 0 } else { max_seq + 1 };
        Ok((segments, disk_bytes, next_seq))
    }
}

fn segment_path(dir: &Path, seq: u64) -> PathBuf {
    dir.join(format!("seg-{seq:012}.ndjson"))
}

/// Parse the `seq` out of a `seg-<seq>.ndjson` file name; `None` for anything else.
fn parse_segment_seq(name: &std::ffi::OsStr) -> Option<u64> {
    let name = name.to_str()?;
    let stem = name.strip_prefix("seg-")?.strip_suffix(".ndjson")?;
    stem.parse().ok()
}

/// Count the newline-delimited records in a segment file (one-time, at recovery).
fn count_lines(path: &Path) -> std::io::Result<u64> {
    let mut file = File::open(path)?;
    let mut buf = [0u8; 64 * 1024];
    let mut count = 0u64;
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        count += buf[..n].iter().filter(|&&b| b == b'\n').count() as u64;
    }
    Ok(count)
}

fn now_unix_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::sync::atomic::AtomicBool;
    use uuid::Uuid;
    use yagra_bus::{BusError, CheckOutcome, EventMsg, PollJob, Sample};
    use yagra_common::NodeId;

    /// A `Bus` whose connection state can be toggled and that records live vs. backfill publishes —
    /// the test seam for a partition.
    #[derive(Default)]
    struct TogglableBus {
        connected: AtomicBool,
        live: Mutex<Vec<PollResult>>,
        backfill: Mutex<Vec<PollResult>>,
        fail_backfill: AtomicBool,
    }
    impl TogglableBus {
        fn new(connected: bool) -> Arc<Self> {
            Arc::new(Self {
                connected: AtomicBool::new(connected),
                ..Default::default()
            })
        }
        fn set_connected(&self, v: bool) {
            self.connected.store(v, Ordering::SeqCst);
        }
    }
    #[async_trait]
    impl Bus for TogglableBus {
        async fn publish_job(&self, _job: PollJob) -> Result<(), BusError> {
            Ok(())
        }
        async fn publish_result(&self, result: PollResult) -> Result<(), BusError> {
            if !self.connected.load(Ordering::SeqCst) {
                return Err(BusError::Publish("disconnected".into()));
            }
            self.live.lock().unwrap().push(result);
            Ok(())
        }
        async fn publish_result_backfill(&self, result: PollResult) -> Result<(), BusError> {
            if self.fail_backfill.load(Ordering::SeqCst) {
                return Err(BusError::Publish("backfill down".into()));
            }
            self.backfill.lock().unwrap().push(result);
            Ok(())
        }
        async fn publish_event(&self, _event: EventMsg) -> Result<(), BusError> {
            Ok(())
        }
        fn is_connected(&self) -> bool {
            self.connected.load(Ordering::SeqCst)
        }
    }

    fn result(at_ms: i64) -> PollResult {
        PollResult {
            schema_version: 1,
            job_id: Uuid::nil(),
            node_id: NodeId::from(Uuid::nil()),
            at_unix_ms: at_ms,
            outcome: CheckOutcome::Reachable,
            samples: vec![Sample::gauge("icmp_rtt_ms", 1.0)],
            interfaces: Vec::new(),
            sys_descr: None,
            dns_chain: None,
            poller_id: Some("edge-1".into()),
            trace_context: Default::default(),
        }
    }

    struct TempDir(PathBuf);
    impl TempDir {
        fn new(tag: &str) -> Self {
            use std::sync::atomic::AtomicU64;
            static N: AtomicU64 = AtomicU64::new(0);
            let n = N.fetch_add(1, Ordering::SeqCst);
            let p = std::env::temp_dir().join(format!("yagra-sf-{tag}-{}-{n}", std::process::id()));
            let _ = std::fs::remove_dir_all(&p);
            Self(p)
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn cfg(dir: PathBuf, mem_max: usize) -> SfConfig {
        SfConfig {
            enabled: true,
            dir,
            mem_max,
            disk_max_bytes: 10 * 1024 * 1024,
            max_age_ms: i64::MAX / 2,
            free_floor_bytes: 0,
            segment_max_bytes: 1024 * 1024,
        }
    }

    fn now() -> i64 {
        now_unix_ms()
    }

    #[tokio::test]
    async fn connected_submits_publish_live_not_buffered() {
        let td = TempDir::new("live");
        let bus = TogglableBus::new(true);
        let sink = StoreForwardSink::new(bus.clone(), cfg(td.0.clone(), 100));
        sink.submit(result(now())).await;
        assert_eq!(bus.live.lock().unwrap().len(), 1);
        assert_eq!(bus.backfill.lock().unwrap().len(), 0);
        assert_eq!(sink.state.lock().unwrap().total_depth(), 0);
    }

    #[tokio::test]
    async fn disconnected_submits_buffer_then_replay_on_reconnect() {
        let td = TempDir::new("replay");
        let bus = TogglableBus::new(false);
        let sink = StoreForwardSink::new(bus.clone(), cfg(td.0.clone(), 100));

        for _ in 0..5 {
            sink.submit(result(now())).await;
        }
        assert_eq!(sink.state.lock().unwrap().total_depth(), 5, "all buffered");
        assert_eq!(bus.live.lock().unwrap().len(), 0);

        // Reconnect and drain.
        bus.set_connected(true);
        let shutdown = CancellationToken::new();
        sink.drain_round(&shutdown).await;

        assert_eq!(
            bus.backfill.lock().unwrap().len(),
            5,
            "all replayed as backfill"
        );
        assert_eq!(
            bus.live.lock().unwrap().len(),
            0,
            "replay never uses the live subject"
        );
        assert_eq!(
            sink.state.lock().unwrap().total_depth(),
            0,
            "buffer drained"
        );
    }

    #[tokio::test]
    async fn overflow_spills_to_disk_and_survives_reload() {
        let td = TempDir::new("spill");
        let bus = TogglableBus::new(false);
        // mem_max=3 → the 4th+ results spill to disk.
        let sink = StoreForwardSink::new(bus.clone(), cfg(td.0.clone(), 3));
        for i in 0..10 {
            sink.submit(result(now() + i)).await;
        }
        {
            let st = sink.state.lock().unwrap();
            assert_eq!(st.total_depth(), 10, "nothing lost");
            assert!(!st.segments.is_empty(), "overflow spilled to disk");
            assert!(st.mem.len() <= 3);
        }
        drop(sink);

        // A fresh sink over the same dir recovers the spilled results (survives restart).
        let bus2 = TogglableBus::new(true);
        let sink2 = StoreForwardSink::new(bus2.clone(), cfg(td.0.clone(), 3));
        assert!(
            sink2.state.lock().unwrap().total_depth() >= 7,
            "disk-tier recovered"
        );
        let shutdown = CancellationToken::new();
        sink2.drain_round(&shutdown).await;
        assert!(
            bus2.backfill.lock().unwrap().len() >= 7,
            "recovered results replay on next run"
        );
    }

    #[tokio::test]
    async fn disk_cap_drops_oldest_and_counts_it() {
        let td = TempDir::new("cap");
        let bus = TogglableBus::new(false);
        let mut c = cfg(td.0.clone(), 1);
        // Tiny segments + a small multiple as the cap → forces rotation and oldest-segment drops.
        c.segment_max_bytes = 256;
        c.disk_max_bytes = 1024;
        let sink = StoreForwardSink::new(bus.clone(), c);
        for i in 0..200 {
            sink.submit(result(now() + i)).await;
        }
        let st = sink.state.lock().unwrap();
        // Bounded: on-disk bytes stay near the cap (at most a segment's worth of slack), not unbounded.
        assert!(
            st.disk_bytes <= 1024 + 256,
            "disk usage stays bounded by the cap, got {}",
            st.disk_bytes
        );
        // Something was dropped (depth is far below the 200 submitted).
        assert!(
            st.total_depth() < 200,
            "oldest results were dropped under the cap"
        );
    }

    #[tokio::test]
    async fn free_floor_stops_disk_spill() {
        let td = TempDir::new("floor");
        let bus = TogglableBus::new(false);
        let mut c = cfg(td.0.clone(), 1);
        c.free_floor_bytes = u64::MAX; // no filesystem can satisfy → never spill
        let sink = StoreForwardSink::new(bus.clone(), c);
        for i in 0..5 {
            sink.submit(result(now() + i)).await;
        }
        let st = sink.state.lock().unwrap();
        // On unix the floor blocks spill (only the 1 mem slot survives); off-unix `available_bytes`
        // is None so spill proceeds — assert the buffer stayed bounded either way.
        assert!(st.total_depth() <= 5);
        if cfg!(unix) {
            assert!(
                st.segments.is_empty(),
                "floor guard prevented any disk spill"
            );
        }
    }

    #[tokio::test]
    async fn passthrough_publishes_live_and_never_buffers() {
        let bus = TogglableBus::new(true);
        let sink = StoreForwardSink::passthrough(bus.clone());
        assert!(!sink.is_enabled());
        sink.submit(result(now())).await;
        assert_eq!(bus.live.lock().unwrap().len(), 1);
        assert_eq!(sink.state.lock().unwrap().total_depth(), 0);
    }

    #[tokio::test]
    async fn replay_failure_keeps_results_buffered() {
        let td = TempDir::new("failreplay");
        let bus = TogglableBus::new(false);
        let sink = StoreForwardSink::new(bus.clone(), cfg(td.0.clone(), 100));
        for _ in 0..3 {
            sink.submit(result(now())).await;
        }
        bus.set_connected(true);
        bus.fail_backfill.store(true, Ordering::SeqCst); // backfill subject rejects
        let shutdown = CancellationToken::new();
        sink.drain_round(&shutdown).await;
        assert_eq!(bus.backfill.lock().unwrap().len(), 0);
        assert_eq!(
            sink.state.lock().unwrap().total_depth(),
            3,
            "un-sent results stay buffered for the next round"
        );
    }
}
