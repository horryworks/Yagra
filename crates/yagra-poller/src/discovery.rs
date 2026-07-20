// SPDX-License-Identifier: AGPL-3.0-only
//! Discovery sweep execution (Phase C) — the poller side.
//!
//! Consumes [`DiscoveryJob`]s off the bus and probes each target for ICMP liveness + SNMP
//! identity (`sysDescr.0` / `sysName.0`), trying the job's candidate credentials (stored
//! v2c/v3, resolved by core) and ad-hoc communities. Runs on the poller because ICMP needs
//! the raw socket. Progress is published as **cumulative** partial results after each chunk
//! of targets, so core can show the sweep advancing; the final message carries `done: true`.

use std::net::IpAddr;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures::stream::{Stream, StreamExt};
use uuid::Uuid;
use yagra_bus::{DiscoveredDevice, DiscoveryJob, DiscoveryResult, NatsBus, BUS_SCHEMA_VERSION};
use yagra_discovery::{AttemptDecision, CredentialProbeLimiter, LimiterConfig};
use yagra_transport::{SnmpV3Params, Transport};

/// sysDescr column base — walking it yields the `.0` scalar instance (v2c path).
const SYSDESCR_BASE: &str = "1.3.6.1.2.1.1.1";
/// sysObjectID column base — the vendor-assigned enterprise OID identifying device type.
const SYSOBJECTID_BASE: &str = "1.3.6.1.2.1.1.2";
/// sysName column base.
const SYSNAME_BASE: &str = "1.3.6.1.2.1.1.5";
/// sysDescr.0 instance OID (scalar GET form, v3 path).
const SYSDESCR_OID: &str = "1.3.6.1.2.1.1.1.0";
/// sysObjectID.0 instance OID.
const SYSOBJECTID_OID: &str = "1.3.6.1.2.1.1.2.0";
/// sysName.0 instance OID.
const SYSNAME_OID: &str = "1.3.6.1.2.1.1.5.0";
/// Targets probed concurrently within a chunk.
const SWEEP_CONCURRENCY: usize = 16;
/// Targets per progress chunk: a cumulative partial result is published after each chunk
/// so the operator sees the sweep advance instead of one long silence.
const PROGRESS_CHUNK: usize = 32;

/// One SNMP credential to try on each target, in order. Stored credentials carry their
/// store id so a match is reported **by reference** (never the value — security.md);
/// ad-hoc communities (free-text from the scan form) have none.
enum SnmpCandidate {
    V2c {
        cred_ref: Option<Uuid>,
        community: String,
    },
    V3 {
        cred_ref: Uuid,
        params: SnmpV3Params,
    },
}

impl SnmpCandidate {
    fn cred_ref(&self) -> Option<Uuid> {
        match self {
            Self::V2c { cred_ref, .. } => *cred_ref,
            Self::V3 { cred_ref, .. } => Some(*cred_ref),
        }
    }
}

/// Flatten a job's stored credentials + ad-hoc communities into one ordered candidate
/// list. Stored credentials are tried first (they're the operator's registered secrets).
fn candidates_of(job: &DiscoveryJob) -> Vec<SnmpCandidate> {
    let mut out = Vec::with_capacity(job.credentials.len() + job.communities.len());
    for c in &job.credentials {
        if let Some(v3) = &c.v3 {
            out.push(SnmpCandidate::V3 {
                cred_ref: c.cred_ref,
                params: SnmpV3Params {
                    user: v3.user.clone(),
                    security_level: v3.security_level.clone(),
                    auth_protocol: v3.auth_protocol.clone(),
                    auth_key: v3.auth_key.clone(),
                    priv_protocol: v3.priv_protocol.clone(),
                    priv_key: v3.priv_key.clone(),
                },
            });
        } else if let Some(community) = &c.community {
            out.push(SnmpCandidate::V2c {
                cred_ref: Some(c.cred_ref),
                community: community.clone(),
            });
        }
    }
    for community in &job.communities {
        out.push(SnmpCandidate::V2c {
            cred_ref: None,
            community: community.clone(),
        });
    }
    out
}

/// Drain discovery jobs off the bus, sweeping each and publishing cumulative progress
/// results. Returns when the stream ends.
pub async fn run_discovery_stream<S>(mut jobs: S, bus: Arc<NatsBus>, transport: Arc<dyn Transport>)
where
    S: Stream<Item = DiscoveryJob> + Unpin,
{
    while let Some(job) = jobs.next().await {
        tracing::info!(scan = %job.scan_id, targets = job.targets.len(), "discovery sweep starting");
        let candidates = Arc::new(candidates_of(&job));
        let timeout = Duration::from_millis(u64::from(job.timeout_ms));
        // Per-device credential-probe rate limit (security.md): space attempts and back off after
        // repeated failures so the sweep can never trip a device's account lockout. Conservative
        // defaults; tunable here as SSH/CLI credential probing (which actually locks out) lands.
        let rate = LimiterConfig::default();
        let total = u32::try_from(job.targets.len()).unwrap_or(u32::MAX);
        let mut found: Vec<DiscoveredDevice> = Vec::new();
        let mut probed: u32 = 0;

        let chunk_count = job.targets.chunks(PROGRESS_CHUNK).count();
        for (i, chunk) in job.targets.chunks(PROGRESS_CHUNK).enumerate() {
            found.extend(sweep_chunk(chunk, &candidates, timeout, transport.clone(), rate).await);
            probed = probed.saturating_add(u32::try_from(chunk.len()).unwrap_or(u32::MAX));
            publish(
                &bus,
                DiscoveryResult {
                    schema_version: BUS_SCHEMA_VERSION,
                    scan_id: job.scan_id,
                    found: found.clone(),
                    probed,
                    total,
                    done: i + 1 == chunk_count,
                },
            )
            .await;
        }
        if job.targets.is_empty() {
            // Degenerate sweep: still complete the scan so core doesn't wait forever.
            publish(
                &bus,
                DiscoveryResult {
                    schema_version: BUS_SCHEMA_VERSION,
                    scan_id: job.scan_id,
                    found: Vec::new(),
                    probed: 0,
                    total: 0,
                    done: true,
                },
            )
            .await;
        }
        tracing::info!(scan = %job.scan_id, found = found.len(), "discovery sweep done");
    }
    tracing::warn!("discovery job stream ended");
}

async fn publish(bus: &NatsBus, result: DiscoveryResult) {
    let scan_id = result.scan_id;
    if let Err(e) = bus.publish_discovery_result(result).await {
        tracing::warn!(error = %e, scan = %scan_id, "publish discovery result failed");
    }
}

/// Probe one chunk of targets with bounded concurrency; keep the devices that responded.
async fn sweep_chunk(
    targets: &[IpAddr],
    candidates: &Arc<Vec<SnmpCandidate>>,
    timeout: Duration,
    transport: Arc<dyn Transport>,
    rate: LimiterConfig,
) -> Vec<DiscoveredDevice> {
    futures::stream::iter(targets.iter().copied())
        .map(|target| {
            let transport = transport.clone();
            let candidates = candidates.clone();
            async move { probe_one(target, &candidates, timeout, transport.as_ref(), rate).await }
        })
        .buffer_unordered(SWEEP_CONCURRENCY)
        .filter_map(|d| async move { d })
        .collect()
        .await
}

/// Current Unix time in milliseconds (clock for the per-device probe limiter).
fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

/// Probe one target: ICMP liveness + SNMP identity, trying each candidate in order
/// (first that answers wins). Candidate probes run **sequentially per device** and are gated
/// by a per-device [`CredentialProbeLimiter`] (`rate`): attempts are spaced apart and, after
/// repeated failures, the device is backed off for the rest of the sweep so probing can never
/// trip an account lockout (security.md). Attempted credentials are never logged. Returns a
/// device iff it answered ICMP or SNMP.
async fn probe_one(
    target: IpAddr,
    candidates: &[SnmpCandidate],
    timeout: Duration,
    transport: &dyn Transport,
    rate: LimiterConfig,
) -> Option<DiscoveredDevice> {
    let reachable = (transport.probe_icmp(target, 1, timeout).await)
        .map(|p| p.reachable)
        .unwrap_or(false);

    // Keyed by target IP — the device has no NodeId during discovery. One limiter per probe is
    // enough: each target appears once per sweep, and spacing/cooldown only span this target's
    // sequential candidate attempts.
    let mut limiter = CredentialProbeLimiter::<IpAddr>::new(rate);
    let mut identity = SnmpIdentity::default();
    let mut matched_credential = None;
    'candidates: for cand in candidates {
        // Honour per-device spacing before each attempt; stop the device entirely on cooldown.
        loop {
            match limiter.begin_attempt(target, now_ms()) {
                AttemptDecision::Allow => break,
                AttemptDecision::TooSoon { wait_ms } => {
                    if wait_ms > 0 {
                        tokio::time::sleep(Duration::from_millis(wait_ms.unsigned_abs())).await;
                    }
                }
                AttemptDecision::CoolingDown { .. } => break 'candidates,
            }
        }
        if let Some(id) = try_candidate(target, cand, timeout, transport).await {
            limiter.record_success(target);
            identity = id;
            matched_credential = cand.cred_ref();
            break;
        }
        limiter.record_failure(target, now_ms());
    }

    if reachable
        || identity.sysdescr.is_some()
        || identity.sysobjectid.is_some()
        || identity.sysname.is_some()
    {
        Some(DiscoveredDevice {
            address: target,
            reachable,
            sysdescr: identity.sysdescr,
            sysname: identity.sysname,
            sysobjectid: identity.sysobjectid,
            matched_credential,
        })
    } else {
        None
    }
}

/// SNMP identity scalars a discovery probe collects from a device (each optional — a device
/// may answer some but not all). `sysObjectID` is the authoritative device-type signal;
/// `sysDescr`/`sysName` are free-form. All are device-supplied — treat as untrusted.
#[derive(Default)]
struct SnmpIdentity {
    sysdescr: Option<String>,
    sysobjectid: Option<String>,
    sysname: Option<String>,
}

/// Try one candidate credential against a target. `Some` (the identity scalars, possibly
/// partial) iff the device answered SNMP under this credential.
async fn try_candidate(
    target: IpAddr,
    cand: &SnmpCandidate,
    timeout: Duration,
    transport: &dyn Transport,
) -> Option<SnmpIdentity> {
    match cand {
        SnmpCandidate::V2c { community, .. } => {
            let bases = [
                SYSDESCR_BASE.to_owned(),
                SYSOBJECTID_BASE.to_owned(),
                SYSNAME_BASE.to_owned(),
            ];
            let rows = transport
                .snmp_walk_strings(target, community, &bases, timeout)
                .await
                .ok()?;
            if rows.is_empty() {
                return None;
            }
            let mut id = SnmpIdentity::default();
            for r in rows {
                match r.oid_base.as_str() {
                    SYSDESCR_BASE => id.sysdescr = Some(r.value),
                    SYSOBJECTID_BASE => id.sysobjectid = Some(r.value),
                    SYSNAME_BASE => id.sysname = Some(r.value),
                    _ => {}
                }
            }
            Some(id)
        }
        SnmpCandidate::V3 { params, .. } => {
            let oids = [
                SYSDESCR_OID.to_owned(),
                SYSOBJECTID_OID.to_owned(),
                SYSNAME_OID.to_owned(),
            ];
            let rows = transport
                .snmp_v3_get_strings(target, params, &oids, timeout)
                .await
                .ok()?;
            if rows.is_empty() {
                return None;
            }
            let mut id = SnmpIdentity::default();
            for r in rows {
                match r.oid.as_str() {
                    SYSDESCR_OID => id.sysdescr = Some(r.value),
                    SYSOBJECTID_OID => id.sysobjectid = Some(r.value),
                    SYSNAME_OID => id.sysname = Some(r.value),
                    _ => {}
                }
            }
            Some(id)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::net::Ipv4Addr;
    use yagra_bus::{DiscoveryCredential, DiscoveryV3};
    use yagra_transport::{
        HttpProbe, HttpProbeSpec, IcmpProbe, SnmpSample, SnmpStringSample, SnmpTableSample,
        SnmpTableString, TransportError,
    };

    /// Answers SNMP only for a specific v2c community and/or v3 user, so tests can assert
    /// which candidate matched.
    struct SelectiveFake {
        ping: bool,
        good_community: Option<String>,
        good_v3_user: Option<String>,
    }

    #[async_trait]
    impl Transport for SelectiveFake {
        async fn probe_icmp(
            &self,
            _target: IpAddr,
            _count: u8,
            _timeout: Duration,
        ) -> Result<IcmpProbe, TransportError> {
            Ok(IcmpProbe {
                reachable: self.ping,
                rtt_ms: self.ping.then_some(1.0),
                loss_pct: if self.ping { 0.0 } else { 100.0 },
            })
        }

        async fn snmp_get(
            &self,
            _target: IpAddr,
            _community: &str,
            _oids: &[String],
            _timeout: Duration,
        ) -> Result<Vec<SnmpSample>, TransportError> {
            Ok(Vec::new())
        }

        async fn snmp_v3_get(
            &self,
            _target: IpAddr,
            _params: &SnmpV3Params,
            _oids: &[String],
            _timeout: Duration,
        ) -> Result<Vec<SnmpSample>, TransportError> {
            Ok(Vec::new())
        }

        async fn snmp_v3_get_strings(
            &self,
            _target: IpAddr,
            params: &SnmpV3Params,
            oids: &[String],
            _timeout: Duration,
        ) -> Result<Vec<SnmpStringSample>, TransportError> {
            if Some(params.user.as_str()) == self.good_v3_user.as_deref() {
                Ok(oids
                    .iter()
                    .map(|o| SnmpStringSample {
                        oid: o.clone(),
                        value: match o.as_str() {
                            SYSDESCR_OID => "Huawei USG6000".to_owned(),
                            SYSOBJECTID_OID => "1.3.6.1.4.1.2011.2.1".to_owned(),
                            _ => "fw01".to_owned(),
                        },
                    })
                    .collect())
            } else {
                Err(TransportError::Io("authentication failure".to_owned()))
            }
        }

        async fn snmp_walk(
            &self,
            _target: IpAddr,
            _community: &str,
            _column_oids: &[String],
            _timeout: Duration,
        ) -> Result<Vec<SnmpTableSample>, TransportError> {
            Ok(Vec::new())
        }

        async fn snmp_walk_strings(
            &self,
            _target: IpAddr,
            community: &str,
            column_oids: &[String],
            _timeout: Duration,
        ) -> Result<Vec<SnmpTableString>, TransportError> {
            if Some(community) == self.good_community.as_deref() {
                Ok(column_oids
                    .iter()
                    .map(|b| SnmpTableString {
                        oid_base: b.clone(),
                        ifindex: 0,
                        value: match b.as_str() {
                            SYSDESCR_BASE => "Cisco IOS Software".to_owned(),
                            SYSOBJECTID_BASE => "1.3.6.1.4.1.9.1.516".to_owned(),
                            _ => "sw01".to_owned(),
                        },
                    })
                    .collect())
            } else {
                Ok(Vec::new())
            }
        }

        async fn snmp_v3_walk(
            &self,
            _target: IpAddr,
            _params: &SnmpV3Params,
            _column_oids: &[String],
            _timeout: Duration,
        ) -> Result<Vec<SnmpTableSample>, TransportError> {
            Ok(Vec::new())
        }

        async fn snmp_v3_walk_strings(
            &self,
            _target: IpAddr,
            _params: &SnmpV3Params,
            _column_oids: &[String],
            _timeout: Duration,
        ) -> Result<Vec<SnmpTableString>, TransportError> {
            Ok(Vec::new())
        }

        async fn probe_http(
            &self,
            _spec: &HttpProbeSpec,
            _timeout: Duration,
        ) -> Result<HttpProbe, TransportError> {
            Ok(HttpProbe {
                reachable: false,
                status_code: None,
                response_time_ms: 0.0,
                cert_days_to_expiry: None,
            })
        }

        async fn collect_meraki(
            &self,
            _spec: &yagra_transport::MerakiCollectSpec,
            _timeout: Duration,
        ) -> Result<Vec<yagra_transport::MerakiObservation>, TransportError> {
            Ok(Vec::new())
        }
    }

    fn target() -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1))
    }

    fn v2c_cred(id: Uuid, community: &str) -> DiscoveryCredential {
        DiscoveryCredential {
            cred_ref: id,
            community: Some(community.to_owned()),
            v3: None,
        }
    }

    fn v3_cred(id: Uuid, user: &str) -> DiscoveryCredential {
        DiscoveryCredential {
            cred_ref: id,
            community: None,
            v3: Some(DiscoveryV3 {
                user: user.to_owned(),
                security_level: "auth".to_owned(),
                auth_protocol: Some("sha256".to_owned()),
                auth_key: Some("a-pass".to_owned()),
                priv_protocol: None,
                priv_key: None,
            }),
        }
    }

    /// Rate config that disables spacing and cooldown so the probe tests never actually sleep
    /// (the limiter wiring itself is unit-tested in `yagra-discovery::credential_finder`).
    fn no_rate() -> LimiterConfig {
        LimiterConfig {
            min_interval_ms: 0,
            max_consecutive_failures: u32::MAX,
            cooldown_ms: 0,
        }
    }

    fn job(credentials: Vec<DiscoveryCredential>, communities: Vec<String>) -> DiscoveryJob {
        DiscoveryJob {
            schema_version: BUS_SCHEMA_VERSION,
            scan_id: Uuid::nil(),
            targets: vec![target()],
            communities,
            credentials,
            timeout_ms: 100,
        }
    }

    #[tokio::test]
    async fn stored_v2c_credential_match_is_reported_by_reference() {
        let id = Uuid::from_u128(1);
        let cands = candidates_of(&job(vec![v2c_cred(id, "secret")], vec![]));
        let fake = SelectiveFake {
            ping: true,
            good_community: Some("secret".to_owned()),
            good_v3_user: None,
        };
        let d = probe_one(
            target(),
            &cands,
            Duration::from_millis(100),
            &fake,
            no_rate(),
        )
        .await
        .expect("device answers");
        assert_eq!(d.matched_credential, Some(id));
        assert_eq!(d.sysdescr.as_deref(), Some("Cisco IOS Software"));
        assert_eq!(d.sysname.as_deref(), Some("sw01"));
        // sysObjectID (OID-typed) is rendered dotted and carried for classification.
        assert_eq!(d.sysobjectid.as_deref(), Some("1.3.6.1.4.1.9.1.516"));
    }

    #[tokio::test]
    async fn v3_credential_match_is_reported_by_reference() {
        let v2 = Uuid::from_u128(1);
        let v3 = Uuid::from_u128(2);
        // The v2c candidate is tried first but doesn't answer; the v3 one does.
        let cands = candidates_of(&job(
            vec![v2c_cred(v2, "wrong"), v3_cred(v3, "monitor")],
            vec![],
        ));
        let fake = SelectiveFake {
            ping: false,
            good_community: None,
            good_v3_user: Some("monitor".to_owned()),
        };
        let d = probe_one(
            target(),
            &cands,
            Duration::from_millis(100),
            &fake,
            no_rate(),
        )
        .await
        .expect("device answers v3");
        assert_eq!(d.matched_credential, Some(v3));
        assert_eq!(d.sysdescr.as_deref(), Some("Huawei USG6000"));
        assert_eq!(d.sysobjectid.as_deref(), Some("1.3.6.1.4.1.2011.2.1"));
        assert!(!d.reachable, "SNMP-only answer still reports the device");
    }

    #[tokio::test]
    async fn adhoc_community_match_has_no_credential_ref() {
        let cands = candidates_of(&job(vec![], vec!["public".to_owned()]));
        let fake = SelectiveFake {
            ping: true,
            good_community: Some("public".to_owned()),
            good_v3_user: None,
        };
        let d = probe_one(
            target(),
            &cands,
            Duration::from_millis(100),
            &fake,
            no_rate(),
        )
        .await
        .expect("device answers");
        assert_eq!(d.matched_credential, None);
        assert!(d.sysdescr.is_some());
    }

    #[tokio::test]
    async fn stored_credentials_are_tried_before_adhoc_communities() {
        // Both the stored credential and the ad-hoc community would answer (same value):
        // the stored one must win so import binds the registered secret by reference.
        let id = Uuid::from_u128(7);
        let cands = candidates_of(&job(
            vec![v2c_cred(id, "shared")],
            vec!["shared".to_owned()],
        ));
        let fake = SelectiveFake {
            ping: true,
            good_community: Some("shared".to_owned()),
            good_v3_user: None,
        };
        let d = probe_one(
            target(),
            &cands,
            Duration::from_millis(100),
            &fake,
            no_rate(),
        )
        .await
        .expect("device answers");
        assert_eq!(d.matched_credential, Some(id));
    }

    #[tokio::test]
    async fn silent_target_yields_nothing() {
        let cands = candidates_of(&job(vec![], vec!["public".to_owned()]));
        let fake = SelectiveFake {
            ping: false,
            good_community: None,
            good_v3_user: None,
        };
        assert!(probe_one(
            target(),
            &cands,
            Duration::from_millis(100),
            &fake,
            no_rate()
        )
        .await
        .is_none());
    }

    /// Wiring check: once a device hits the consecutive-failure limit it is backed off, so the
    /// remaining candidates are never tried (lockout protection, security.md).
    #[tokio::test]
    async fn cooldown_backs_off_remaining_candidates() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        // Never answers; counts how many credential attempts actually reached the transport.
        struct CountingFake {
            attempts: Arc<AtomicUsize>,
        }
        #[async_trait]
        impl Transport for CountingFake {
            async fn probe_icmp(
                &self,
                _t: IpAddr,
                _c: u8,
                _to: Duration,
            ) -> Result<IcmpProbe, TransportError> {
                Ok(IcmpProbe {
                    reachable: false,
                    rtt_ms: None,
                    loss_pct: 100.0,
                })
            }
            async fn snmp_get(
                &self,
                _t: IpAddr,
                _c: &str,
                _o: &[String],
                _to: Duration,
            ) -> Result<Vec<SnmpSample>, TransportError> {
                Ok(Vec::new())
            }
            async fn snmp_v3_get(
                &self,
                _t: IpAddr,
                _p: &SnmpV3Params,
                _o: &[String],
                _to: Duration,
            ) -> Result<Vec<SnmpSample>, TransportError> {
                Ok(Vec::new())
            }
            async fn snmp_v3_get_strings(
                &self,
                _t: IpAddr,
                _p: &SnmpV3Params,
                _o: &[String],
                _to: Duration,
            ) -> Result<Vec<SnmpStringSample>, TransportError> {
                Ok(Vec::new())
            }
            async fn snmp_walk(
                &self,
                _t: IpAddr,
                _c: &str,
                _o: &[String],
                _to: Duration,
            ) -> Result<Vec<SnmpTableSample>, TransportError> {
                Ok(Vec::new())
            }
            async fn snmp_walk_strings(
                &self,
                _t: IpAddr,
                _c: &str,
                _o: &[String],
                _to: Duration,
            ) -> Result<Vec<SnmpTableString>, TransportError> {
                self.attempts.fetch_add(1, Ordering::SeqCst);
                Ok(Vec::new()) // empty ⇒ this candidate "failed"
            }
            async fn snmp_v3_walk(
                &self,
                _t: IpAddr,
                _p: &SnmpV3Params,
                _o: &[String],
                _to: Duration,
            ) -> Result<Vec<SnmpTableSample>, TransportError> {
                Ok(Vec::new())
            }
            async fn snmp_v3_walk_strings(
                &self,
                _t: IpAddr,
                _p: &SnmpV3Params,
                _o: &[String],
                _to: Duration,
            ) -> Result<Vec<SnmpTableString>, TransportError> {
                Ok(Vec::new())
            }
            async fn probe_http(
                &self,
                _spec: &HttpProbeSpec,
                _to: Duration,
            ) -> Result<HttpProbe, TransportError> {
                Ok(HttpProbe {
                    reachable: false,
                    status_code: None,
                    response_time_ms: 0.0,
                    cert_days_to_expiry: None,
                })
            }

            async fn collect_meraki(
                &self,
                _spec: &yagra_transport::MerakiCollectSpec,
                _to: Duration,
            ) -> Result<Vec<yagra_transport::MerakiObservation>, TransportError> {
                Ok(Vec::new())
            }
        }

        let attempts = Arc::new(AtomicUsize::new(0));
        let fake = CountingFake {
            attempts: attempts.clone(),
        };
        // Five candidates that all fail; cooldown trips after 2 consecutive failures.
        let cands = candidates_of(&job(
            vec![],
            vec![
                "a".to_owned(),
                "b".to_owned(),
                "c".to_owned(),
                "d".to_owned(),
                "e".to_owned(),
            ],
        ));
        let rate = LimiterConfig {
            min_interval_ms: 0,
            max_consecutive_failures: 2,
            cooldown_ms: 60_000,
        };
        let d = probe_one(target(), &cands, Duration::from_millis(100), &fake, rate).await;
        assert!(d.is_none(), "no device answered");
        assert_eq!(
            attempts.load(Ordering::SeqCst),
            2,
            "device is backed off after the failure limit instead of trying all five"
        );
    }
}
