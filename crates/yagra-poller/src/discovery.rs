//! Discovery sweep execution (Phase C) — the poller side.
//!
//! Consumes [`DiscoveryJob`]s off the bus and probes each target for ICMP liveness + SNMP
//! identity (`sysDescr.0` / `sysName.0`), trying the job's candidate communities. Runs on the
//! poller because ICMP needs the raw socket. Results fan back to core on the discovery-results
//! subject; core correlates them by `scan_id` and classifies the devices.

use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use futures::stream::{Stream, StreamExt};
use yagra_bus::{DiscoveredDevice, DiscoveryJob, DiscoveryResult, NatsBus, BUS_SCHEMA_VERSION};
use yagra_transport::Transport;

/// sysDescr column base — walking it yields the `.0` scalar instance.
const SYSDESCR_BASE: &str = "1.3.6.1.2.1.1.1";
/// sysName column base.
const SYSNAME_BASE: &str = "1.3.6.1.2.1.1.5";
/// Targets probed concurrently within a sweep.
const SWEEP_CONCURRENCY: usize = 16;

/// Drain discovery jobs off the bus, sweeping each and publishing the result. Returns when the
/// stream ends.
pub async fn run_discovery_stream<S>(mut jobs: S, bus: Arc<NatsBus>, transport: Arc<dyn Transport>)
where
    S: Stream<Item = DiscoveryJob> + Unpin,
{
    while let Some(job) = jobs.next().await {
        tracing::info!(scan = %job.scan_id, targets = job.targets.len(), "discovery sweep starting");
        let found = sweep(&job, transport.clone()).await;
        tracing::info!(scan = %job.scan_id, found = found.len(), "discovery sweep done");
        let result = DiscoveryResult {
            schema_version: BUS_SCHEMA_VERSION,
            scan_id: job.scan_id,
            found,
        };
        if let Err(e) = bus.publish_discovery_result(result).await {
            tracing::warn!(error = %e, scan = %job.scan_id, "publish discovery result failed");
        }
    }
    tracing::warn!("discovery job stream ended");
}

/// Probe every target in the job with bounded concurrency; keep the devices that responded.
async fn sweep(job: &DiscoveryJob, transport: Arc<dyn Transport>) -> Vec<DiscoveredDevice> {
    let timeout = Duration::from_millis(u64::from(job.timeout_ms));
    let communities = Arc::new(job.communities.clone());
    futures::stream::iter(job.targets.iter().copied())
        .map(|target| {
            let transport = transport.clone();
            let communities = communities.clone();
            async move { probe_one(target, &communities, timeout, transport.as_ref()).await }
        })
        .buffer_unordered(SWEEP_CONCURRENCY)
        .filter_map(|d| async move { d })
        .collect()
        .await
}

/// Probe one target: ICMP liveness + SNMP identity (first community that answers). Returns a
/// device iff it answered ICMP or SNMP.
async fn probe_one(
    target: IpAddr,
    communities: &[String],
    timeout: Duration,
    transport: &dyn Transport,
) -> Option<DiscoveredDevice> {
    let reachable = (transport.probe_icmp(target, 1, timeout).await)
        .map(|p| p.reachable)
        .unwrap_or(false);

    let mut sysdescr = None;
    let mut sysname = None;
    for community in communities {
        let bases = [SYSDESCR_BASE.to_owned(), SYSNAME_BASE.to_owned()];
        if let Ok(rows) = transport
            .snmp_walk_strings(target, community, &bases, timeout)
            .await
        {
            if !rows.is_empty() {
                for r in rows {
                    if r.oid_base == SYSDESCR_BASE {
                        sysdescr = Some(r.value);
                    } else if r.oid_base == SYSNAME_BASE {
                        sysname = Some(r.value);
                    }
                }
                break; // this community worked
            }
        }
    }

    if reachable || sysdescr.is_some() {
        Some(DiscoveredDevice {
            address: target,
            reachable,
            sysdescr,
            sysname,
        })
    } else {
        None
    }
}
