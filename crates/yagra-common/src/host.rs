// SPDX-License-Identifier: AGPL-3.0-only
//! Host self-observability sample — the NMS's own CPU / load / memory / disk.
//!
//! This is the cross-cutting DTO for Yagra monitoring **itself** (self-observability,
//! monitoring-conventions). A `HostSample` is produced by the `yagra-hoststats` collector on
//! both the core and every poller, and rides the poller→core heartbeat ([`yagra-bus`
//! `HeartbeatMsg`]) so remote pollers behind NAT/FW — which cannot reach VictoriaMetrics — still
//! report their host health. Core is the single writer of the resulting `yagra_host_*` series to
//! the TSDB.
//!
//! Kept dependency-free (plain `serde`) so `yagra-bus` can carry it without pulling the heavy
//! `sysinfo` collector; the collector lives in `yagra-hoststats`.

use serde::{Deserialize, Serialize};

/// Usage of one watched filesystem (or a store-size proxy, e.g. the PostgreSQL database size which
/// core cannot `statvfs`). `size_bytes == 0` means "total capacity unknown" — the value is a bare
/// growing size (used only), not a fraction of a disk.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct DiskUsage {
    /// Friendly mount label (e.g. `root`, `metrics`, `database`) — a stable, low-cardinality name,
    /// never a raw device path, so it is safe as a TSDB label.
    pub mount: String,
    /// Bytes in use on the filesystem (or the store's size when `size_bytes == 0`).
    pub used_bytes: u64,
    /// Total filesystem capacity in bytes; `0` when unknown (capacity not measurable).
    pub size_bytes: u64,
}

impl DiskUsage {
    /// Percent used (0–100), or `None` when the total capacity is unknown (`size_bytes == 0`).
    #[must_use]
    pub fn used_pct(&self) -> Option<f64> {
        if self.size_bytes == 0 {
            None
        } else {
            #[allow(clippy::cast_precision_loss)]
            Some((self.used_bytes as f64 / self.size_bytes as f64) * 100.0)
        }
    }
}

/// One instantaneous host-resource sample for a Yagra process's host.
///
/// All fields default so the sample is forward/backward tolerant on the bus (ADR-017): an older
/// poller that sends no host block deserializes as absent, and new fields added here stay N/N-1
/// safe. Load averages are `0.0` on platforms without a load average (e.g. Windows dev boxes).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct HostSample {
    /// Overall CPU utilization across all cores, 0–100.
    #[serde(default)]
    pub cpu_pct: f64,
    /// 1-minute load average (`0.0` where unavailable).
    #[serde(default)]
    pub load1: f64,
    /// 5-minute load average.
    #[serde(default)]
    pub load5: f64,
    /// 15-minute load average.
    #[serde(default)]
    pub load15: f64,
    /// Memory in use, bytes.
    #[serde(default)]
    pub mem_used_bytes: u64,
    /// Total physical memory, bytes.
    #[serde(default)]
    pub mem_total_bytes: u64,
    /// Swap in use, bytes.
    #[serde(default)]
    pub swap_used_bytes: u64,
    /// Total swap, bytes.
    #[serde(default)]
    pub swap_total_bytes: u64,
    /// Per-watched-filesystem usage (plus any store-size proxies like `database`).
    #[serde(default)]
    pub disks: Vec<DiskUsage>,
}

impl HostSample {
    /// Percent of physical memory in use (0–100), or `None` when total memory is unknown.
    #[must_use]
    pub fn mem_used_pct(&self) -> Option<f64> {
        if self.mem_total_bytes == 0 {
            None
        } else {
            #[allow(clippy::cast_precision_loss)]
            Some((self.mem_used_bytes as f64 / self.mem_total_bytes as f64) * 100.0)
        }
    }

    /// The "primary" disk usage percent for at-a-glance columns: the highest used-% across the
    /// watched filesystems that report a capacity. `None` when no watched filesystem has a known
    /// total (e.g. a poller that watches nothing, or size-unknown proxies only).
    #[must_use]
    pub fn primary_disk_used_pct(&self) -> Option<f64> {
        self.disks
            .iter()
            .filter_map(DiskUsage::used_pct)
            .fold(None, |acc, p| Some(acc.map_or(p, |a: f64| a.max(p))))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn used_pct_guards_zero_size() {
        assert_eq!(
            DiskUsage {
                mount: "root".into(),
                used_bytes: 25,
                size_bytes: 100
            }
            .used_pct(),
            Some(25.0)
        );
        assert_eq!(
            DiskUsage {
                mount: "database".into(),
                used_bytes: 999,
                size_bytes: 0
            }
            .used_pct(),
            None
        );
    }

    #[test]
    fn mem_and_primary_disk_derivations() {
        let s = HostSample {
            mem_used_bytes: 2,
            mem_total_bytes: 8,
            disks: vec![
                DiskUsage {
                    mount: "root".into(),
                    used_bytes: 10,
                    size_bytes: 100,
                },
                DiskUsage {
                    mount: "metrics".into(),
                    used_bytes: 90,
                    size_bytes: 100,
                },
                DiskUsage {
                    mount: "database".into(),
                    used_bytes: 5,
                    size_bytes: 0,
                },
            ],
            ..Default::default()
        };
        assert_eq!(s.mem_used_pct(), Some(25.0));
        // primary = max(10%, 90%); the size-unknown "database" proxy is ignored.
        assert_eq!(s.primary_disk_used_pct(), Some(90.0));
    }

    #[test]
    fn default_sample_has_no_derivations() {
        let s = HostSample::default();
        assert_eq!(s.mem_used_pct(), None);
        assert_eq!(s.primary_disk_used_pct(), None);
    }
}
