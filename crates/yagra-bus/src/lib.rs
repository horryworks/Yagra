// SPDX-License-Identifier: AGPL-3.0-only
//! Yagra-bus — task queue / message bus client.
//!
//! Abstracts job distribution from Yagra-core to Yagra-poller workers over NATS
//! (ADR-007) so the transport stays swappable behind the [`Bus`] trait. This is the
//! seam that makes distributed polling possible: pollers are stateless workers reached
//! only via the bus (ADR-003), and messages are version-tolerant for rolling upgrades
//! (ADR-017).

pub mod bus;
pub mod messages;
#[cfg(feature = "nats")]
pub mod nats;
pub mod subjects;

pub use bus::{Bus, BusError, DiscoveryBus, InMemoryBus, LogBus, PeerBus, SyncBus, UpgradeBus};
pub use messages::{
    encode_raw, AuthRevoke, CheckOutcome, CheckSpec, DiscoveredDevice, DiscoveredInterface,
    DiscoveryCancel, DiscoveryCredential, DiscoveryJob, DiscoveryResult, DiscoveryV3, DnsCheck,
    EventKind, EventMsg, FlowBatch, FlowRecord, HeartbeatMsg, HttpCheck, IcmpCheck, JobSpec,
    MerakiCollectCheck, MerakiDeviceRef, NodeJobs, OpticalProbe, PollJob, PollResult,
    PollerLogChunk, PollerLogRequest, PollerUpgradeMsg, RawFlowDatagram, RawFlowProto, Sample,
    SnmpArpCheck, SnmpArpColumn, SnmpCheck, SnmpColumn, SnmpL3Check, SnmpL3Column, SnmpMauCheck,
    SnmpMetaColumn, SnmpNeighborCheck, SnmpNeighborColumn, SnmpOpticalCheck, SnmpRouteProbe,
    SnmpRoutingCheck, SnmpRoutingColumn, SnmpTableCheck, SnmpV3ArpCheck, SnmpV3Check,
    SnmpV3L3Check, SnmpV3MauCheck, SnmpV3NeighborCheck, SnmpV3OpticalCheck, SnmpV3RoutingCheck,
    SnmpV3TableCheck, SyncMsg, SyncRequest, TraceContext, UpgradeReport, UpgradeReportCommand,
    UpgradeReportState, UpgradeStep, WorkingSetDelta, WorkingSetSnapshot, CAP_DISCOVERY_CANCEL,
    CAP_FLOW_RELAY, CAP_HTTP_AUTH, CAP_HTTP_BODY, CAP_LOG_SHIP, CAP_POOL_FOLLOW, CAP_RAW_CAPTURE,
    CAP_SELF_UPGRADE, CAP_SITE_PREPARED, CAP_UPGRADE_REPORT, HEARTBEAT_SECS, OFFLINE_AFTER_SECS,
    SITE_PREPARED_FIELD, SNAPSHOT_CHUNK_NODES,
};
#[cfg(feature = "nats")]
pub use nats::{
    install_tls_crypto_provider, redact_url, split_userinfo_password, NatsBus, DEFAULT_POOL,
    POLLER_QUEUE,
};
