// SPDX-License-Identifier: AGPL-3.0-only
//! Discovery orchestration (Phase C) — core side.
//!
//! Turns a scan request into a [`DiscoveryJob`] on the bus (the poller does the actual ICMP /
//! SNMP sweep), correlates the [`DiscoveryResult`]s back by `scan_id`, and classifies each
//! found device into a suggested device profile (via the core [`Classifier`](crate::classification)
//! — authoritative `sysObjectID` rules; vendor/model are pre-filled from `yagra_discovery::identify`).
//! The poller
//! publishes **cumulative** partial results as it sweeps, so a scan's status carries live
//! progress (probed/total + the address currently being probed). Scan state is held **in
//! memory** — scans are short-lived and core is single-instance today (Redis-backed state is
//! a future scale-out concern). The operator reviews candidates and imports the ones they
//! want as real nodes (reusing the create-node path); nothing is added automatically.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};

use futures::stream::{Stream, StreamExt};
use serde::Serialize;
use uuid::Uuid;
use yagra_bus::{DiscoveryBus, DiscoveryCredential, DiscoveryJob, DiscoveryResult};

use crate::classification::Classifier;

/// Per-probe timeout pushed to the poller (ms).
const SCAN_TIMEOUT_MS: u32 = 2000;

/// One device a scan found, with a suggested profile for the operator to confirm on import.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct Candidate {
    pub address: String,
    pub reachable: bool,
    pub sysdescr: Option<String>,
    pub sysname: Option<String>,
    /// `sysObjectID` (dotted) if it answered SNMP — the authoritative device-type signal the
    /// classifier prefers. Shown to the operator and useful for authoring rules.
    pub sysobjectid: Option<String>,
    /// Suggested device profile, resolved **server-side** via the classification rules (by
    /// sysObjectID prefix, else sysDescr regex, else "Generic SNMP" when SNMP answered). An id,
    /// not a name, so the UI binds it robustly even if the profile was renamed.
    pub suggested_profile_id: Option<Uuid>,
    /// Maker / model best-effort parsed from sysDescr (editable on import) — pre-fills the
    /// node's descriptive metadata so the imported node displays "name (addr) (vendor) (model)".
    pub vendor: Option<String>,
    pub model: Option<String>,
    /// The stored credential that answered SNMP, by reference (never the value) — the UI
    /// preselects it on import so the working secret is bound automatically.
    pub matched_credential_id: Option<Uuid>,
}

/// A scan's current status returned by the API.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct ScanStatus {
    pub scan_id: Uuid,
    pub done: bool,
    /// Targets probed so far / total targets in the sweep.
    pub probed: u32,
    pub total: u32,
    /// The address the sweep is currently at (the next unprobed target), while running.
    pub scanning: Option<String>,
    pub candidates: Vec<Candidate>,
}

struct ScanState {
    targets: Vec<IpAddr>,
    done: bool,
    probed: u32,
    candidates: Vec<Candidate>,
}

impl ScanState {
    fn new(targets: Vec<IpAddr>) -> Self {
        Self {
            targets,
            done: false,
            probed: 0,
            candidates: Vec::new(),
        }
    }

    /// Fold a (possibly partial) poller result in. Each message carries the cumulative
    /// found list, so candidates are replaced, not appended. Progress never regresses
    /// (guards against out-of-order delivery). The classifier resolves each device's
    /// suggested profile server-side from its sysObjectID / sysDescr.
    fn apply(&mut self, result: DiscoveryResult, classifier: &Classifier) {
        if result.probed < self.probed && !result.done {
            return;
        }
        self.candidates = result
            .found
            .into_iter()
            .map(|d| {
                // Authoritative profile suggestion from the classification rules (sysObjectID
                // first, then sysDescr, then the Generic-SNMP fallback when SNMP answered).
                let matched = classifier.classify(d.sysobjectid.as_deref(), d.sysdescr.as_deref());
                // Maker/model: a matching rule may pin them; otherwise fall back to the
                // best-effort sysDescr parse (editable by the operator on import).
                let parsed = d
                    .sysdescr
                    .as_deref()
                    .map(yagra_discovery::identify)
                    .unwrap_or_default();
                let vendor = matched
                    .as_ref()
                    .and_then(|m| m.vendor.clone())
                    .or(parsed.vendor);
                let model = matched
                    .as_ref()
                    .and_then(|m| m.model.clone())
                    .or(parsed.model);
                Candidate {
                    address: d.address.to_string(),
                    reachable: d.reachable,
                    sysdescr: d.sysdescr,
                    sysname: d.sysname,
                    sysobjectid: d.sysobjectid,
                    suggested_profile_id: matched.map(|m| m.profile_id),
                    vendor,
                    model,
                    matched_credential_id: d.matched_credential,
                }
            })
            .collect();
        self.probed = self.probed.max(result.probed);
        self.done = self.done || result.done;
    }

    fn status(&self, scan_id: Uuid) -> ScanStatus {
        let total = u32::try_from(self.targets.len()).unwrap_or(u32::MAX);
        let scanning = (!self.done)
            .then(|| {
                self.targets
                    .get(self.probed as usize)
                    .map(IpAddr::to_string)
            })
            .flatten();
        ScanStatus {
            scan_id,
            done: self.done,
            probed: self.probed.min(total),
            total,
            scanning,
            candidates: self.candidates.clone(),
        }
    }
}

/// Orchestrates discovery scans: publishes jobs, accumulates results, exposes status.
pub struct DiscoveryRunner {
    /// The publish seam, not the NATS type: this held `Arc<NatsBus>` and so could only ever be
    /// driven by a running broker, which is why the routing decision in [`Self::start`] had no test.
    bus: Arc<dyn DiscoveryBus>,
    classifier: Arc<Classifier>,
    scans: Mutex<HashMap<Uuid, ScanState>>,
}

impl DiscoveryRunner {
    #[must_use]
    pub fn new(bus: Arc<dyn DiscoveryBus>, classifier: Arc<Classifier>) -> Self {
        Self {
            bus,
            classifier,
            scans: Mutex::new(HashMap::new()),
        }
    }

    /// Start a scan: register it and publish the sweep job. `credentials` are the stored
    /// secrets to try (already resolved/decrypted by the caller — ADR-018/020); they are
    /// passed through to the poller and never logged. Returns the scan id to poll.
    ///
    /// `pool` routes the sweep (ADR-009/020): `Some(p)` publishes to that pool's own discovery
    /// subject (the caller has already confirmed a live poller serves it), so a remote-site poller
    /// runs the scan on its network; `None` publishes to the legacy global discovery subject (the
    /// compatibility path, absorbed by an old wildcard poller).
    pub async fn start(
        &self,
        targets: Vec<IpAddr>,
        communities: Vec<String>,
        credentials: Vec<DiscoveryCredential>,
        pool: Option<&str>,
    ) -> anyhow::Result<Uuid> {
        let scan_id = Uuid::new_v4();
        {
            let mut g = self.scans.lock().expect("scans mutex poisoned");
            g.insert(scan_id, ScanState::new(targets.clone()));
        }
        let job = DiscoveryJob {
            scan_id,
            targets,
            communities,
            credentials,
            timeout_ms: SCAN_TIMEOUT_MS,
        };
        match pool {
            Some(p) => self.bus.publish_discovery_job_for_pool(p, job).await?,
            None => self.bus.publish_discovery_job(job).await?,
        }
        Ok(scan_id)
    }

    /// Current status of a scan (progress + candidates so far + whether it completed).
    #[must_use]
    pub fn get(&self, scan_id: Uuid) -> Option<ScanStatus> {
        let g = self.scans.lock().expect("scans mutex poisoned");
        g.get(&scan_id).map(|s| s.status(scan_id))
    }

    /// Recent discovered candidates across all in-memory scans, deduped by address (first seen
    /// wins), capped at `limit`. Backs the dashboard "discovery queue" widget — a standing view of
    /// unclassified finds without needing a scan id. In-memory only (scans are short-lived).
    #[must_use]
    pub fn recent_candidates(&self, limit: usize) -> Vec<Candidate> {
        let g = self.scans.lock().expect("scans mutex poisoned");
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        for scan in g.values() {
            for c in &scan.candidates {
                if seen.insert(c.address.clone()) {
                    out.push(c.clone());
                    if out.len() >= limit {
                        return out;
                    }
                }
            }
        }
        out
    }

    /// Consume discovery results off the bus, folding each into its scan. Runs until the
    /// stream ends.
    pub async fn run_consumer<S>(self: Arc<Self>, mut results: S)
    where
        S: Stream<Item = DiscoveryResult> + Unpin,
    {
        while let Some(r) = results.next().await {
            tracing::debug!(
                scan = %r.scan_id,
                found = r.found.len(),
                probed = r.probed,
                done = r.done,
                "discovery result received"
            );
            let mut g = self.scans.lock().expect("scans mutex poisoned");
            if let Some(s) = g.get_mut(&r.scan_id) {
                s.apply(r, &self.classifier);
            }
        }
        tracing::warn!("discovery result stream ended");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;
    use yagra_bus::DiscoveredDevice;
    use yagra_common::ClassificationRule;

    /// Stable test profile id the Huawei rule maps to.
    const HUAWEI_PROFILE: u128 = 0x4A1;
    const GENERIC_PROFILE: u128 = 0x9E2E61C;

    /// A classifier with one Huawei sysObjectID-prefix rule + a Generic-SNMP fallback, so the
    /// fixture devices (Huawei sysObjectID) classify deterministically without a database.
    fn classifier() -> Classifier {
        let huawei = ClassificationRule {
            id: Uuid::from_u128(1),
            priority: 100,
            sysobjectid_prefix: Some("1.3.6.1.4.1.2011.".to_owned()),
            sysdescr_regex: None,
            profile_id: Uuid::from_u128(HUAWEI_PROFILE).into(),
            vendor: None,
            model: None,
            enabled: true,
        };
        Classifier::from_rules(vec![huawei], Some(Uuid::from_u128(GENERIC_PROFILE)))
    }

    fn targets(n: u8) -> Vec<IpAddr> {
        (1..=n)
            .map(|i| IpAddr::V4(Ipv4Addr::new(10, 0, 0, i)))
            .collect()
    }

    fn partial(probed: u32, done: bool, found: Vec<DiscoveredDevice>) -> DiscoveryResult {
        DiscoveryResult {
            scan_id: Uuid::nil(),
            found,
            probed,
            total: 4,
            done,
        }
    }

    fn device(last_octet: u8, cred: Option<Uuid>) -> DiscoveredDevice {
        DiscoveredDevice {
            address: IpAddr::V4(Ipv4Addr::new(10, 0, 0, last_octet)),
            reachable: true,
            sysdescr: Some("Huawei Versatile Routing Platform USG6000".to_owned()),
            sysname: Some(format!("dev{last_octet}")),
            sysobjectid: Some("1.3.6.1.4.1.2011.2.1".to_owned()),
            matched_credential: cred,
        }
    }

    #[test]
    fn progress_advances_and_reports_current_address() {
        let c = classifier();
        let mut s = ScanState::new(targets(4));
        let st = s.status(Uuid::nil());
        assert_eq!((st.probed, st.total), (0, 4));
        assert_eq!(st.scanning.as_deref(), Some("10.0.0.1"));

        s.apply(partial(2, false, vec![device(1, None)]), &c);
        let st = s.status(Uuid::nil());
        assert!(!st.done);
        assert_eq!(st.probed, 2);
        assert_eq!(st.scanning.as_deref(), Some("10.0.0.3"));
        assert_eq!(st.candidates.len(), 1);

        s.apply(partial(4, true, vec![device(1, None), device(3, None)]), &c);
        let st = s.status(Uuid::nil());
        assert!(st.done);
        assert_eq!(st.probed, 4);
        assert_eq!(st.scanning, None, "no current address once done");
        assert_eq!(st.candidates.len(), 2);
    }

    #[test]
    fn matched_credential_is_carried_into_the_candidate() {
        let cred = Uuid::from_u128(42);
        let mut s = ScanState::new(targets(1));
        s.apply(partial(1, true, vec![device(1, Some(cred))]), &classifier());
        let st = s.status(Uuid::nil());
        assert_eq!(st.candidates[0].matched_credential_id, Some(cred));
        // The device's sysObjectID resolves (server-side) to the Huawei profile by id, and the
        // maker/model are parsed from sysDescr to pre-fill the node's descriptive metadata.
        assert_eq!(
            st.candidates[0].suggested_profile_id,
            Some(Uuid::from_u128(HUAWEI_PROFILE))
        );
        assert_eq!(
            st.candidates[0].sysobjectid.as_deref(),
            Some("1.3.6.1.4.1.2011.2.1")
        );
        assert_eq!(st.candidates[0].vendor.as_deref(), Some("Huawei"));
        assert_eq!(st.candidates[0].model.as_deref(), Some("USG6000"));
    }

    #[test]
    fn snmp_device_with_no_rule_match_falls_back_to_generic_profile() {
        // No sysObjectID, sysDescr matches no rule → the Generic-SNMP fallback id (by id).
        let mut s = ScanState::new(targets(1));
        let dev = DiscoveredDevice {
            address: IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
            reachable: true,
            sysdescr: Some("Linux server 5.10 net-snmp".to_owned()),
            sysname: Some("srv01".to_owned()),
            sysobjectid: None,
            matched_credential: None,
        };
        s.apply(partial(1, true, vec![dev]), &classifier());
        let st = s.status(Uuid::nil());
        assert_eq!(
            st.candidates[0].suggested_profile_id,
            Some(Uuid::from_u128(GENERIC_PROFILE))
        );
    }

    #[test]
    fn stale_or_reordered_partials_never_regress_progress() {
        let c = classifier();
        let mut s = ScanState::new(targets(4));
        s.apply(
            partial(4, false, vec![device(1, None), device(2, None)]),
            &c,
        );
        // A late, out-of-order partial with lower progress must be ignored.
        s.apply(partial(2, false, vec![device(1, None)]), &c);
        let st = s.status(Uuid::nil());
        assert_eq!(st.probed, 4);
        assert_eq!(st.candidates.len(), 2);
    }

    #[test]
    fn old_poller_single_result_completes_the_scan() {
        // N-1 (ADR-017): an older poller sends one final message with default progress
        // fields (probed 0, done true) and no sysObjectID — the scan must still complete and
        // classify via the sysDescr fallback.
        let mut s = ScanState::new(targets(2));
        s.apply(
            DiscoveryResult {
                scan_id: Uuid::nil(),
                found: vec![DiscoveredDevice {
                    address: IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
                    reachable: true,
                    sysdescr: Some("Huawei VRP USG6000".to_owned()),
                    sysname: Some("dev1".to_owned()),
                    sysobjectid: None,
                    matched_credential: None,
                }],
                probed: 0,
                total: 0,
                done: true,
            },
            &classifier(),
        );
        let st = s.status(Uuid::nil());
        assert!(st.done);
        assert_eq!(st.candidates.len(), 1);
        // Old poller sent no sysObjectID, so it falls back to the Generic-SNMP profile (the
        // Huawei rule here is sysObjectID-only). The classifier still resolves a suggestion.
        assert_eq!(
            st.candidates[0].suggested_profile_id,
            Some(Uuid::from_u128(GENERIC_PROFILE))
        );
    }

    /// `start`'s routing choice, which had no test while the runner held the NATS type. Getting it
    /// wrong is invisible locally and wrong remotely: a sweep meant for a remote site published on
    /// the global subject is absorbed by whichever poller answers first, so it scans the *wrong*
    /// network and reports nothing found.
    #[tokio::test]
    async fn a_pool_scoped_scan_is_published_only_to_that_pool() {
        let bus = Arc::new(yagra_bus::InMemoryBus::new(8));
        let mut pooled = bus.subscribe_pool_discovery_jobs();
        let mut global = bus.subscribe_discovery_jobs();
        let runner = DiscoveryRunner::new(bus.clone(), Arc::new(classifier()));

        let scan = runner
            .start(
                targets(2),
                vec!["public".to_owned()],
                Vec::new(),
                Some("tokyo"),
            )
            .await
            .expect("publish succeeds");

        let (pool, job) = pooled.recv().await.expect("job published");
        assert_eq!(pool, "tokyo");
        assert_eq!(job.scan_id, scan);
        assert_eq!(job.targets.len(), 2);
        assert!(global.try_recv().is_err(), "must not also hit the wildcard");
        // The scan is registered before publish, so status is available the moment start returns.
        assert!(runner.get(scan).is_some());
    }

    #[tokio::test]
    async fn a_scan_with_no_pool_falls_back_to_the_global_subject() {
        let bus = Arc::new(yagra_bus::InMemoryBus::new(8));
        let mut pooled = bus.subscribe_pool_discovery_jobs();
        let mut global = bus.subscribe_discovery_jobs();
        let runner = DiscoveryRunner::new(bus.clone(), Arc::new(classifier()));

        let scan = runner
            .start(targets(1), Vec::new(), Vec::new(), None)
            .await
            .expect("publish succeeds");

        assert_eq!(global.recv().await.expect("job published").scan_id, scan);
        assert!(pooled.try_recv().is_err());
    }

    /// Credentials are passed through to the poller verbatim (it needs the plaintext to probe) but
    /// must never be logged — this pins the pass-through half so a future "sanitize the job" change
    /// cannot silently empty it and make every stored-credential sweep find nothing.
    #[tokio::test]
    async fn stored_credentials_reach_the_poller_in_the_job() {
        let bus = Arc::new(yagra_bus::InMemoryBus::new(8));
        let mut global = bus.subscribe_discovery_jobs();
        let runner = DiscoveryRunner::new(bus.clone(), Arc::new(classifier()));

        let cred_ref = Uuid::from_u128(9);
        runner
            .start(
                targets(1),
                Vec::new(),
                vec![DiscoveryCredential {
                    cred_ref,
                    community: Some("s3cret".to_owned()),
                    v3: None,
                }],
                None,
            )
            .await
            .expect("publish succeeds");

        let job = global.recv().await.expect("job published");
        assert_eq!(job.credentials.len(), 1);
        assert_eq!(job.credentials[0].cred_ref, cred_ref);
    }
}
