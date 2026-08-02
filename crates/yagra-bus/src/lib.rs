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

pub use bus::{Bus, BusError, DiscoveryBus, InMemoryBus, PeerBus, SyncBus};
pub use messages::{
    encode_raw, AuthRevoke, CheckOutcome, CheckSpec, DiscoveredDevice, DiscoveredInterface,
    DiscoveryCredential, DiscoveryJob, DiscoveryResult, DiscoveryV3, DnsCheck, EventKind, EventMsg,
    FlowBatch, FlowRecord, HeartbeatMsg, HttpCheck, IcmpCheck, JobSpec, MerakiCollectCheck,
    MerakiDeviceRef, NodeJobs, PollJob, PollResult, RawFlowDatagram, RawFlowProto, Sample,
    SnmpCheck, SnmpColumn, SnmpMetaColumn, SnmpTableCheck, SnmpV3Check, SnmpV3TableCheck, SyncMsg,
    SyncRequest, TraceContext, WorkingSetDelta, WorkingSetSnapshot, CAP_FLOW_RELAY, CAP_HTTP_AUTH,
    CAP_RAW_CAPTURE, HEARTBEAT_SECS, OFFLINE_AFTER_SECS, SNAPSHOT_CHUNK_NODES,
};
#[cfg(feature = "nats")]
pub use nats::{NatsBus, DEFAULT_POOL, POLLER_QUEUE};
