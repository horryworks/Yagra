// SPDX-License-Identifier: AGPL-3.0-only
//! yagra-hoststats — the host self-metrics collector (self-observability, monitoring-conventions).
//!
//! Produces a [`yagra_common::HostSample`] (CPU %, load average, memory, per-watched-filesystem
//! usage, and network interface traffic) for the process's own host. Both the core and every poller
//! sample themselves; a poller ships its sample on the heartbeat so remote pollers behind NAT/FW
//! still report host health, and core is the single writer of the resulting `yagra_host_*` series
//! to the TSDB.
//!
//! `sysinfo` is used for CPU/load/memory (cross-platform, so `cargo run` works on the Windows dev
//! box). Filesystem capacity uses `statvfs(2)` directly on each watched path (unix only) — this is
//! reliable for bind-mounted data volumes and overlay rootfs inside containers, where sysinfo's
//! disk enumeration is unreliable. On non-unix hosts disk usage is simply empty.
//!
//! Network traffic is read from `/sys/class/net` directly rather than through sysinfo's `network`
//! feature (ADR-137): deciding *which* interfaces to count needs `/sys/devices/virtual/net` anyway,
//! so one tree answers both. Where that tree does not exist (the Windows dev box) the counters stay
//! at 0. The sample's `bus_*` fields are not this crate's to fill — they describe one process rather
//! than the host, and the two callers set them from the bus they hold.

use std::collections::HashMap;
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

/// Where the kernel publishes its device tree. [`HostCollector`] holds it as a field so a test can
/// point the network reader at a tree it built.
const SYS_ROOT: &str = "/sys";

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

/// One network interface as the kernel reports it at this instant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NicReading {
    pub(crate) name: String,
    /// The kernel's own counter: bytes since the interface came up in this network namespace.
    pub(crate) rx_bytes: u64,
    pub(crate) tx_bytes: u64,
    /// Listed under `/sys/devices/virtual/net` — loopback, veth pairs, bridges, VPN tunnels, bonds.
    pub(crate) is_virtual: bool,
}

/// Every interface under `<root>/class/net`, sorted by name. Empty where the tree does not exist.
///
/// An interface whose counters cannot be read is left out rather than read as zero: a zero would be
/// a reading, and the next real one would count every byte it had ever carried as fresh traffic.
pub(crate) fn read_nics(root: &Path) -> Vec<NicReading> {
    let Ok(entries) = std::fs::read_dir(root.join("class").join("net")) else {
        return Vec::new();
    };
    let virtual_dir = root.join("devices").join("virtual").join("net");
    let mut out: Vec<NicReading> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            let stats = entry.path().join("statistics");
            let counter = |file: &str| {
                std::fs::read_to_string(stats.join(file))
                    .ok()
                    .and_then(|s| s.trim().parse::<u64>().ok())
            };
            Some(NicReading {
                rx_bytes: counter("rx_bytes")?,
                tx_bytes: counter("tx_bytes")?,
                is_virtual: virtual_dir.join(&name).exists(),
                name,
            })
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// The interfaces whose traffic counts: the physical ones when there are any, otherwise every one
/// but loopback.
///
/// On a host (a remote poller runs with `network_mode: host`) the physical rule is what stops double
/// counting: the same bytes cross a container's veth, `docker0`, a WireGuard tunnel or a bond **and**
/// the physical port under it, so a name-based exclusion list is always one tunnel type short.
///
/// ⚠️ Inside a bridge-networked container the container's own `eth0` *is* a veth, so nothing is
/// physical and the fallback counts `eth0` — the container's traffic, not the host's. That is the
/// right answer for core, and the page says so rather than pretending otherwise.
pub(crate) fn counted_nics(readings: &[NicReading]) -> Vec<&NicReading> {
    if readings.iter().any(|r| !r.is_virtual) {
        readings.iter().filter(|r| !r.is_virtual).collect()
    } else {
        readings.iter().filter(|r| r.name != "lo").collect()
    }
}

/// Bytes moved on the counted interfaces since this collector was built, accumulated from positive
/// per-interface deltas.
///
/// 🚨 **Never the sum of the kernel's counters.** That sum drops when an interface disappears and
/// jumps when one appears, and the TSDB reads a counter that dropped as a reset — so `increase()`
/// would count the whole remaining total as one step's traffic (500 GB → 3 GB draws +3 GB in fifteen
/// seconds), and a VPN coming up would add everything its tunnel had ever carried. Accumulating what
/// each interface grew by makes this monotone by construction, so the only reset left is a process
/// restart — the same moment the bus counter resets, which is what lets the page subtract one from
/// the other (ADR-137).
#[derive(Debug, Default)]
pub(crate) struct NetCounter {
    last: HashMap<String, (u64, u64)>,
    rx: u64,
    tx: u64,
}

impl NetCounter {
    /// Fold one reading in and return the running `(rx, tx)`.
    ///
    /// An interface seen for the first time contributes nothing — its counter holds history from
    /// before this process was watching. One whose counter went backwards (recreated under the same
    /// name, or wrapped) also contributes nothing and restarts from its new value.
    pub(crate) fn observe(&mut self, readings: &[NicReading]) -> (u64, u64) {
        let mut next = HashMap::new();
        for nic in counted_nics(readings) {
            if let Some(&(rx, tx)) = self.last.get(&nic.name) {
                self.rx = self.rx.saturating_add(nic.rx_bytes.saturating_sub(rx));
                self.tx = self.tx.saturating_add(nic.tx_bytes.saturating_sub(tx));
            }
            next.insert(nic.name.clone(), (nic.rx_bytes, nic.tx_bytes));
        }
        self.last = next;
        (self.rx, self.tx)
    }
}

/// Samples the host's CPU / load / memory / disk / network on demand.
///
/// Holds a live `sysinfo::System` so CPU usage is a delta over the interval between [`sample`]
/// calls (the first sample reads ~0), and the per-interface network readings so traffic is a delta
/// too (the first sample reads 0). Cheap to keep around; sampling is O(watched paths + interfaces)
/// plus a CPU/memory refresh.
///
/// [`sample`]: HostCollector::sample
pub struct HostCollector {
    sys: Mutex<System>,
    watch: Vec<WatchPath>,
    net: Mutex<NetCounter>,
    sys_root: PathBuf,
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
            net: Mutex::new(NetCounter::default()),
            sys_root: PathBuf::from(SYS_ROOT),
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
    ///
    /// `bus_rx_bytes` / `bus_tx_bytes` come back `0`: they belong to the process holding the bus,
    /// which fills them in before the sample leaves.
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
        let (net_rx_bytes, net_tx_bytes) = self
            .net
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .observe(&read_nics(&self.sys_root));
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
            net_rx_bytes,
            net_tx_bytes,
            bus_rx_bytes: 0,
            bus_tx_bytes: 0,
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
        // Traffic is a counter that starts with the collector, whatever the interfaces already hold.
        assert_eq!((s.net_rx_bytes, s.net_tx_bytes), (0, 0));
        // And the bus share is never this crate's to fill.
        assert_eq!((s.bus_rx_bytes, s.bus_tx_bytes), (0, 0));
    }

    fn nic(name: &str, rx: u64, tx: u64, is_virtual: bool) -> NicReading {
        NicReading {
            name: name.to_owned(),
            rx_bytes: rx,
            tx_bytes: tx,
            is_virtual,
        }
    }

    fn names(readings: &[NicReading]) -> Vec<&str> {
        counted_nics(readings)
            .into_iter()
            .map(|r| r.name.as_str())
            .collect()
    }

    #[test]
    fn counts_the_physical_interfaces_when_there_are_any() {
        // A host-networked poller: the physical port carries every byte the virtual ones do.
        let host = [
            nic("bond0", 0, 0, true),
            nic("docker0", 0, 0, true),
            nic("eno1", 0, 0, false),
            nic("eno2", 0, 0, false),
            nic("lo", 0, 0, true),
            nic("veth1a2b", 0, 0, true),
            nic("wg0", 0, 0, true),
        ];
        assert_eq!(names(&host), ["eno1", "eno2"]);
    }

    #[test]
    fn counts_everything_but_loopback_inside_a_container() {
        // A bridge-networked container's `eth0` is itself a veth, so nothing is physical — and the
        // accept side matters: a rule that counted nothing here would zero core's card forever.
        let container = [nic("eth0", 0, 0, true), nic("lo", 0, 0, true)];
        assert_eq!(names(&container), ["eth0"]);
        assert!(names(&[nic("lo", 0, 0, true)]).is_empty());
    }

    #[test]
    fn traffic_starts_at_zero_and_accumulates_what_each_interface_grew_by() {
        let mut c = NetCounter::default();
        assert_eq!(
            c.observe(&[nic("eno1", 5_000, 900, false)]),
            (0, 0),
            "history from before the collector is not traffic"
        );
        assert_eq!(c.observe(&[nic("eno1", 5_300, 950, false)]), (300, 50));
        assert_eq!(c.observe(&[nic("eno1", 5_400, 950, false)]), (400, 50));
    }

    /// 🚨 The case the accumulator exists for. A sum of the kernel's counters falls from 1,100 to
    /// 150 when `eno2` goes away, and the TSDB reads a counter that fell as a reset — the next
    /// step's `increase()` would be 150 bytes of traffic that never happened, and at production
    /// sizes that is gigabytes in fifteen seconds.
    #[test]
    fn an_interface_leaving_or_arriving_never_moves_the_total_backwards_or_jumps_it() {
        let mut c = NetCounter::default();
        c.observe(&[nic("eno1", 100, 0, false), nic("eno2", 1_000, 0, false)]);
        assert_eq!(c.observe(&[nic("eno1", 150, 0, false)]).0, 50, "eno2 left");
        assert_eq!(
            c.observe(&[nic("eno1", 170, 0, false), nic("eno2", 9_000, 0, false)])
                .0,
            70,
            "eno2 came back holding 9,000 bytes of history, none of which is this step's"
        );
        assert_eq!(
            c.observe(&[nic("eno1", 170, 0, false), nic("eno2", 9_010, 0, false)])
                .0,
            80
        );
    }

    #[test]
    fn a_counter_that_goes_backwards_adds_nothing_and_restarts_from_there() {
        let mut c = NetCounter::default();
        c.observe(&[nic("eno1", 1_000, 1_000, false)]);
        assert_eq!(c.observe(&[nic("eno1", 10, 10, false)]), (0, 0));
        assert_eq!(c.observe(&[nic("eno1", 30, 15, false)]), (20, 5));
    }

    /// A throwaway `/sys`-shaped tree. The workspace has no `tempfile`, and a directory under the
    /// system temp dir named for this process and test is all this needs.
    struct FakeSys(PathBuf);

    impl FakeSys {
        fn new(tag: &str) -> Self {
            let root =
                std::env::temp_dir().join(format!("yagra-hoststats-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            Self(root)
        }

        fn add(&self, name: &str, rx: &str, tx: &str, is_virtual: bool) {
            let stats = self
                .0
                .join("class")
                .join("net")
                .join(name)
                .join("statistics");
            std::fs::create_dir_all(&stats).expect("create statistics dir");
            std::fs::write(stats.join("rx_bytes"), rx).expect("write rx_bytes");
            std::fs::write(stats.join("tx_bytes"), tx).expect("write tx_bytes");
            if is_virtual {
                std::fs::create_dir_all(
                    self.0
                        .join("devices")
                        .join("virtual")
                        .join("net")
                        .join(name),
                )
                .expect("create virtual marker");
            }
        }
    }

    impl Drop for FakeSys {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn reads_the_kernel_tree_and_tells_virtual_interfaces_apart() {
        let sys = FakeSys::new("read");
        sys.add("eth0", "1234\n", "567\n", false);
        sys.add("lo", "10\n", "10\n", true);
        // A counter that is not a number is left out, not read as 0.
        sys.add("broken0", "n/a\n", "1\n", false);
        assert_eq!(
            read_nics(&sys.0),
            vec![nic("eth0", 1234, 567, false), nic("lo", 10, 10, true)]
        );
    }

    #[test]
    fn a_missing_tree_reads_as_no_interfaces() {
        assert!(read_nics(Path::new("/definitely/not/a/sys/tree")).is_empty());
    }
}
