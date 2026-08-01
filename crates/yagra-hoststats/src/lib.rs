// SPDX-License-Identifier: AGPL-3.0-only
//! yagra-hoststats — the host self-metrics collector (self-observability, monitoring-conventions).
//!
//! Produces a [`yagra_common::HostSample`] (CPU %, load average, memory, and per-watched-filesystem
//! usage) for the process's own host. Both the core and every poller sample themselves; a poller
//! ships its sample on the heartbeat so remote pollers behind NAT/FW still report host health, and
//! core is the single writer of the resulting `yagra_host_*` series to the TSDB.
//!
//! `sysinfo` is used for CPU/load/memory (cross-platform, so `cargo run` works on the Windows dev
//! box). Filesystem capacity uses `statvfs(2)` directly on each watched path (unix only) — this is
//! reliable for bind-mounted data volumes and overlay rootfs inside containers, where sysinfo's
//! disk enumeration is unreliable. On non-unix hosts disk usage is simply empty.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use sysinfo::{MemoryRefreshKind, RefreshKind, System};
use yagra_common::{DiskUsage, HostSample};

// The watch-path cluster is how `from_env` reads its configuration, not a surface for callers:
// both binaries construct the collector with `HostCollector::from_env()` and never name a path.

/// Environment variable naming the filesystems to watch. Comma-separated entries, each either a
/// bare `path` or `path=alias` (the alias is the low-cardinality TSDB `mount` label). Unset ⇒
/// [`DEFAULT_WATCH_SPEC`].
pub(crate) const ENV_DISK_WATCH_PATHS: &str = "YAGRA_DISK_WATCH_PATHS";

/// Default when [`ENV_DISK_WATCH_PATHS`] is unset: the root filesystem, labelled `root`.
pub(crate) const DEFAULT_WATCH_SPEC: &str = "/=root";

/// One filesystem to watch: a path to `statvfs` and the friendly `mount` label reported for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WatchPath {
    /// Filesystem path to measure.
    pub(crate) path: PathBuf,
    /// Low-cardinality label used as the TSDB `mount` value (e.g. `root`, `metrics`).
    pub(crate) alias: String,
}

/// Parse a [`ENV_DISK_WATCH_PATHS`]-style spec into watch paths. Blank entries are skipped; an entry
/// without `=alias` derives its alias from the last path segment (`/` ⇒ `root`).
#[must_use]
pub(crate) fn parse_watch_paths(spec: &str) -> Vec<WatchPath> {
    spec.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|entry| {
            let (path, alias) = match entry.split_once('=') {
                Some((p, a)) => (p.trim(), a.trim().to_string()),
                None => (entry, String::new()),
            };
            let alias = if alias.is_empty() {
                derive_alias(path)
            } else {
                alias
            };
            WatchPath {
                path: PathBuf::from(path),
                alias,
            }
        })
        .collect()
}

/// Friendly label for a path when the spec gives none: last non-empty segment, or `root` for `/`.
fn derive_alias(path: &str) -> String {
    Path::new(path)
        .file_name()
        .and_then(|s| s.to_str())
        .map_or_else(|| "root".to_string(), ToString::to_string)
}

/// Samples the host's CPU / load / memory / disk on demand.
///
/// Holds a live `sysinfo::System` so CPU usage is a delta over the interval between [`sample`]
/// calls (the first sample reads ~0). Cheap to keep around; sampling is O(watched paths) plus a
/// CPU/memory refresh.
///
/// [`sample`]: HostCollector::sample
pub struct HostCollector {
    sys: Mutex<System>,
    watch: Vec<WatchPath>,
}

impl HostCollector {
    /// Build a collector for the given watch paths. Crate-internal — callers use [`Self::from_env`].
    #[must_use]
    pub(crate) fn new(watch: Vec<WatchPath>) -> Self {
        let sys = System::new_with_specifics(
            RefreshKind::nothing()
                .with_cpu(sysinfo::CpuRefreshKind::nothing().with_cpu_usage())
                .with_memory(MemoryRefreshKind::nothing().with_ram().with_swap()),
        );
        Self {
            sys: Mutex::new(sys),
            watch,
        }
    }

    /// Build a collector from the environment ([`ENV_DISK_WATCH_PATHS`], else [`DEFAULT_WATCH_SPEC`]).
    #[must_use]
    pub fn from_env() -> Self {
        let spec =
            std::env::var(ENV_DISK_WATCH_PATHS).unwrap_or_else(|_| DEFAULT_WATCH_SPEC.into());
        Self::new(parse_watch_paths(&spec))
    }

    /// Take one host-resource sample. CPU % is the average over the interval since the previous
    /// call. Never panics on a stuck path — an unreadable filesystem is simply omitted.
    #[must_use]
    pub fn sample(&self) -> HostSample {
        let (cpu_pct, mem_used_bytes, mem_total_bytes, swap_used_bytes, swap_total_bytes) = {
            let mut sys = self
                .sys
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            sys.refresh_cpu_usage();
            sys.refresh_memory();
            (
                f64::from(sys.global_cpu_usage()),
                sys.used_memory(),
                sys.total_memory(),
                sys.used_swap(),
                sys.total_swap(),
            )
        };
        let load = System::load_average();
        let disks = self
            .watch
            .iter()
            .filter_map(|w| {
                statvfs_usage(&w.path).map(|(used_bytes, size_bytes)| DiskUsage {
                    mount: w.alias.clone(),
                    used_bytes,
                    size_bytes,
                })
            })
            .collect();
        HostSample {
            cpu_pct,
            load1: load.one,
            load5: load.five,
            load15: load.fifteen,
            mem_used_bytes,
            mem_total_bytes,
            swap_used_bytes,
            swap_total_bytes,
            disks,
        }
    }
}

/// `(used_bytes, size_bytes)` for the filesystem containing `path`, via `statvfs(2)`. `None` if the
/// path can't be measured (missing, permission denied, or zero-size fs). Unix only.
#[cfg(unix)]
// The `as u64` casts below are widening (or no-ops) depending on the platform's `c_ulong` /
// `fsblkcnt_t` widths; keep them for portability across 32/64-bit unix even though they look
// redundant on 64-bit glibc.
#[allow(clippy::unnecessary_cast)]
fn statvfs_usage(path: &Path) -> Option<(u64, u64)> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let cpath = CString::new(path.as_os_str().as_bytes()).ok()?;
    // SAFETY: `stat` is a plain-old-data struct we fully hand to the kernel to populate; `cpath`
    // is a valid NUL-terminated C string that outlives the call. `statvfs` writes only into
    // `stat` and returns a status code — no aliasing or lifetime concerns. We read `stat` only
    // when the call reports success (rc == 0).
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::statvfs(cpath.as_ptr(), &mut stat) };
    if rc != 0 {
        tracing::debug!(path = %path.display(), "statvfs failed for watched filesystem");
        return None;
    }
    let frsize = stat.f_frsize as u64;
    let size = (stat.f_blocks as u64).checked_mul(frsize)?;
    let free = (stat.f_bfree as u64).saturating_mul(frsize);
    if size == 0 {
        return None;
    }
    Some((size.saturating_sub(free), size))
}

/// No filesystem capacity available off-unix (the Windows dev box); the container target is Linux.
#[cfg(not(unix))]
fn statvfs_usage(_path: &Path) -> Option<(u64, u64)> {
    None
}

/// Bytes currently available to an unprivileged process on the filesystem containing `path`, via
/// `statvfs(2)` (`f_bavail`, which excludes root-reserved blocks — the right measure for the
/// non-root poller). `None` if the path can't be measured (missing / permission denied / non-unix).
///
/// The poller's store-and-forward spill (Phase 3) uses this as a host-disk safety floor: it stops
/// spilling to disk when free space drops below a threshold, so the buffer can never fill the shared
/// host disk (the remote poller runs with `network_mode: host`).
#[cfg(unix)]
#[allow(clippy::unnecessary_cast)]
#[must_use]
pub fn available_bytes(path: &Path) -> Option<u64> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let cpath = CString::new(path.as_os_str().as_bytes()).ok()?;
    // SAFETY: identical contract to `statvfs_usage` — `stat` is a POD struct the kernel fully
    // populates, `cpath` is a valid NUL-terminated C string that outlives the call, and we read
    // `stat` only on success (rc == 0).
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::statvfs(cpath.as_ptr(), &mut stat) };
    if rc != 0 {
        return None;
    }
    Some((stat.f_bavail as u64).saturating_mul(stat.f_frsize as u64))
}

/// No filesystem capacity available off-unix; store-and-forward then skips the free-space floor
/// (byte-cap + write-error degradation still bound disk usage).
#[cfg(not(unix))]
#[must_use]
pub fn available_bytes(_path: &Path) -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_aliases_and_derives_missing_ones() {
        let w = parse_watch_paths("/=root, /hostfs/vm=metrics ,/var/log");
        assert_eq!(w.len(), 3);
        assert_eq!(w[0].alias, "root");
        assert_eq!(w[0].path, PathBuf::from("/"));
        assert_eq!(w[1].alias, "metrics");
        assert_eq!(w[1].path, PathBuf::from("/hostfs/vm"));
        // No explicit alias → derived from the last segment.
        assert_eq!(w[2].alias, "log");
    }

    #[test]
    fn skips_blank_entries() {
        assert!(parse_watch_paths("  , ,").is_empty());
        assert_eq!(parse_watch_paths("/data").len(), 1);
    }

    #[test]
    fn sample_reports_plausible_memory() {
        // A real sample on the test host: total memory must be positive and used ≤ total.
        let c = HostCollector::new(parse_watch_paths(DEFAULT_WATCH_SPEC));
        let s = c.sample();
        assert!(s.mem_total_bytes > 0, "total memory should be discoverable");
        assert!(s.mem_used_bytes <= s.mem_total_bytes);
        // Global CPU usage is an average across cores in 0..=100.
        assert!(s.cpu_pct >= 0.0 && s.cpu_pct.is_finite());
        assert!(s.load1 >= 0.0);
    }
}
