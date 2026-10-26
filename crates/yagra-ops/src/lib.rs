// SPDX-License-Identifier: AGPL-3.0-only
//! **Yagra-ops — how a deployment changes itself** (ADR-202 Inc.6).
//!
//! The upgrade read and request path and its hand-off to the updater sidecar (`upgrade`, ADR-050),
//! moving the remote-site pollers after it (`poller_upgrade`, ADR-051), moving a deployment to
//! another host (`relocation`, ADR-121), the support bundle (`support_bundle`, ADR-045), the
//! pollers' own logs (`poller_logs`), and the archive a remote site is brought up from
//! (`poller_bundle`, ADR-065).
//!
//! It names nothing in `yagra-core`; the compiler holds that. What it needs from core's running
//! state comes in through the three traits below, each implemented by core: the live poller
//! registry, the fleet-wide maintenance window an upgrade opens, and the durable poller inventory.
//! Each is the narrow slice this crate reads, not the core type behind it.
//!
//! ⚠️ Checks about the whole program are `yagra-core/src/program_guards.rs`, which reads this
//! crate's tree too — the support bundle's "every variable core reads is carried or excluded" is
//! one of them, because the variables are read all over the program.

use async_trait::async_trait;
use uuid::Uuid;

#[cfg(test)]
mod module_source;
pub mod poller_bundle;
pub mod poller_logs;
pub mod poller_upgrade;
pub mod relocation;
pub mod support_bundle;
/// Writing a tar archive, the half the bundles need (ADR-214 decision 1).
mod tar_writer;
pub mod upgrade;

/// One poller as the live registry last heard it — the part of core's `PollerView` an upgrade
/// reads.
#[derive(Debug, Clone)]
pub struct LivePoller {
    /// Sanitized poller id.
    pub id: String,
    /// Pool it serves.
    pub pool: String,
    /// Whether it is within the offline window.
    pub online: bool,
    /// Build version from its latest heartbeat.
    pub version: String,
    /// Per-process incarnation: a poller that came back on the same version is still told apart.
    pub incarnation: Uuid,
    /// Capabilities its build advertises (empty from an N-1 poller).
    pub caps: Vec<String>,
    /// What its site updater last reported about an upgrade; `None` when it has none.
    pub upgrade: Option<yagra_bus::UpgradeReport>,
}

/// The live poller registry (core's `Coordinator`), as an upgrade reads it.
pub trait PollerRegistry: Send + Sync {
    /// Every poller the registry holds right now, online or not.
    fn live_pollers(&self) -> Vec<LivePoller>;
}

/// The fleet-wide maintenance window an upgrade opens (ADR-050 decision 12), as settling a
/// finished run closes it. Implemented by core's `MaintenanceRepo`.
#[async_trait]
pub trait UpgradeWindows: Send + Sync {
    /// End every open upgrade window now. Returns how many were open.
    async fn end_upgrade_windows(&self) -> anyhow::Result<u64>;
}

/// The durable poller inventory (core's `PollerRepo`): the pool each known poller last served.
///
/// Read for a poller an upgrade chose that has not reconnected, because the live registry has not
/// heard of it since this process started (ADR-051 Inc.8).
#[async_trait]
pub trait PollerInventory: Send + Sync {
    /// `(id, pool)` for every poller the inventory holds.
    async fn pools_by_id(&self) -> anyhow::Result<Vec<(String, String)>>;
}
