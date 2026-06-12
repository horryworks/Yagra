//! Discovery orchestration (Phase C) — core side.
//!
//! Turns a scan request into a [`DiscoveryJob`] on the bus (the poller does the actual ICMP /
//! SNMP sweep), correlates the [`DiscoveryResult`] back by `scan_id`, and classifies each found
//! device into a suggested device profile ([`yagra_discovery::classify`]). Scan state is held
//! **in memory** — scans are short-lived and core is single-instance today (Redis-backed state
//! is a future scale-out concern). The operator reviews candidates and imports the ones they
//! want as real nodes (reusing the create-node path); nothing is added automatically.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};

use futures::stream::{Stream, StreamExt};
use serde::Serialize;
use uuid::Uuid;
use yagra_bus::{DiscoveryJob, DiscoveryResult, NatsBus, BUS_SCHEMA_VERSION};

/// Per-probe timeout pushed to the poller (ms).
const SCAN_TIMEOUT_MS: u32 = 2000;

/// One device a scan found, with a suggested profile for the operator to confirm on import.
#[derive(Debug, Clone, Serialize)]
pub struct Candidate {
    pub address: String,
    pub reachable: bool,
    pub sysdescr: Option<String>,
    pub sysname: Option<String>,
    /// Suggested built-in profile name (classified from sysDescr), if any.
    pub suggested_profile: Option<String>,
}

/// A scan's current status returned by the API.
#[derive(Debug, Clone, Serialize)]
pub struct ScanStatus {
    pub scan_id: Uuid,
    pub done: bool,
    pub candidates: Vec<Candidate>,
}

struct ScanState {
    done: bool,
    candidates: Vec<Candidate>,
}

/// Orchestrates discovery scans: publishes jobs, accumulates results, exposes status.
pub struct DiscoveryRunner {
    bus: Arc<NatsBus>,
    scans: Mutex<HashMap<Uuid, ScanState>>,
}

impl DiscoveryRunner {
    #[must_use]
    pub fn new(bus: Arc<NatsBus>) -> Self {
        Self {
            bus,
            scans: Mutex::new(HashMap::new()),
        }
    }

    /// Start a scan: register it and publish the sweep job. Returns the scan id to poll.
    pub async fn start(
        &self,
        targets: Vec<IpAddr>,
        communities: Vec<String>,
    ) -> anyhow::Result<Uuid> {
        let scan_id = Uuid::new_v4();
        {
            let mut g = self.scans.lock().expect("scans mutex poisoned");
            g.insert(
                scan_id,
                ScanState {
                    done: false,
                    candidates: Vec::new(),
                },
            );
        }
        let job = DiscoveryJob {
            schema_version: BUS_SCHEMA_VERSION,
            scan_id,
            targets,
            communities,
            timeout_ms: SCAN_TIMEOUT_MS,
        };
        self.bus.publish_discovery_job(job).await?;
        Ok(scan_id)
    }

    /// Current status of a scan (candidates so far + whether it has completed).
    #[must_use]
    pub fn get(&self, scan_id: Uuid) -> Option<ScanStatus> {
        let g = self.scans.lock().expect("scans mutex poisoned");
        g.get(&scan_id).map(|s| ScanStatus {
            scan_id,
            done: s.done,
            candidates: s.candidates.clone(),
        })
    }

    /// Fold a poller result into its scan: classify each device and mark the scan done.
    fn ingest(&self, result: DiscoveryResult) {
        let candidates = result
            .found
            .into_iter()
            .map(|d| {
                // Vendor match from sysDescr, else "Generic SNMP" if it answered SNMP at all.
                let suggested = d
                    .sysdescr
                    .as_deref()
                    .and_then(yagra_discovery::classify)
                    .map(str::to_owned)
                    .or_else(|| d.sysdescr.as_ref().map(|_| "Generic SNMP".to_owned()));
                Candidate {
                    address: d.address.to_string(),
                    reachable: d.reachable,
                    sysdescr: d.sysdescr,
                    sysname: d.sysname,
                    suggested_profile: suggested,
                }
            })
            .collect();
        let mut g = self.scans.lock().expect("scans mutex poisoned");
        if let Some(s) = g.get_mut(&result.scan_id) {
            s.candidates = candidates;
            s.done = true;
        }
    }

    /// Consume discovery results off the bus, folding each into its scan. Runs until the
    /// stream ends.
    pub async fn run_consumer<S>(self: Arc<Self>, mut results: S)
    where
        S: Stream<Item = DiscoveryResult> + Unpin,
    {
        while let Some(r) = results.next().await {
            tracing::info!(scan = %r.scan_id, found = r.found.len(), "discovery result received");
            self.ingest(r);
        }
        tracing::warn!("discovery result stream ended");
    }
}
