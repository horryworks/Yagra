// SPDX-License-Identifier: AGPL-3.0-only
//! Yagra-forward — the pure half of passive-data forwarding ("tee", ADR-034).
//!
//! Yagra receives syslog, SNMP traps and flow exports; this crate decides **which** of them a
//! given destination should get ([`filter`]) and **what bytes** to put on the wire ([`render`]).
//! It holds no sockets, no clients and no async — exactly the split `yagra-ingest` uses for
//! reception, so both halves stay unit-testable against byte fixtures. The sending side lives in
//! `yagra-core`'s forwarder.
//!
//! Two output *relay* fidelities exist, and the difference is the reason this crate has renderers at
//! all (a BigQuery destination is neither — see [`bqrow`]):
//!
//! * **Verbatim** — the original datagram, carried on [`yagra_bus::EventMsg::raw`] (syslog/traps) or
//!   [`yagra_bus::RawFlowDatagram`] (flow exports). Byte-exact.
//! * **Rendered** — rebuilt from the parsed fields by [`render`]. Necessary when the producer
//!   attached no raw payload (an N-1 poller, or an inbound webhook that never was a datagram),
//!   and useful when a collector wants normalized RFC 5424 rather than whatever the device emits.
//!   It is lossy: see [`render`] for exactly what is dropped. Flow has **no** rendered form — a
//!   NetFlow/IPFIX export is a template-bound binary bundle, so re-encoding decoded records would
//!   produce a different datagram rather than a rendering of the same one.
//!
//! Flow also filters differently, and the difference is visible to operators: a syslog line is one
//! message, but a v9/IPFIX datagram is a bundle of a template plus many records. Records cannot be
//! removed from a bundle without re-encoding it, so a flow filter is an **any-record** test — if any
//! record in the datagram matches, the whole datagram is relayed, non-matching records included.

pub mod bqrow;
pub mod filter;
pub mod render;

pub use bqrow::{
    event_row, event_schema, flow_row, flow_schema, FlowRow, EVENT_CLUSTERING,
    EVENT_PARTITION_FIELD, FLOW_CLUSTERING, FLOW_PARTITION_FIELD,
};
pub use filter::{
    compile, CompiledFilter, Condition, DestKind, FilterError, FilterExpr, FilterField, FilterMode,
    FilterOp, FilterView, FlowFields, SourceKind, MAX_CONDITIONS, MAX_PATTERN_CHARS,
};
pub use render::{render_syslog_5424, render_trap_v2c, DEFAULT_TRAP_COMMUNITY};
