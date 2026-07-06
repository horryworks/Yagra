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

pub use bus::{Bus, BusError, InMemoryBus, SyncBus};
pub use messages::{
    CheckOutcome, CheckSpec, DiscoveredDevice, DiscoveredInterface, DiscoveryCredential,
    DiscoveryJob, DiscoveryResult, DiscoveryV3, EventKind, EventMsg, HeartbeatMsg, HttpCheck,
    IcmpCheck, JobSpec, MerakiCollectCheck, MerakiDeviceRef, NodeJobs, PollJob, PollResult, Sample,
    SnmpCheck, SnmpColumn, SnmpMetaColumn, SnmpTableCheck, SnmpV3Check, SyncMsg, SyncRequest,
    TraceContext, WorkingSetDelta, WorkingSetSnapshot, BUS_SCHEMA_VERSION, HEARTBEAT_SECS,
    OFFLINE_AFTER_SECS, SNAPSHOT_CHUNK_NODES,
};
#[cfg(feature = "nats")]
pub use nats::{NatsBus, DEFAULT_POOL, POLLER_QUEUE};
