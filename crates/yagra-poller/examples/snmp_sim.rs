// SPDX-License-Identifier: AGPL-3.0-only
//! Synthetic SNMP v2c agent farm — the ADR-110 supply-side rig.
//!
//! Answers SNMP GET / GETNEXT / GETBULK on **one UDP socket per fake device address**, serving one
//! generated MIB from every address. Built for the question ADR-109 could not ask: ICMP's ceiling is
//! `permits ÷ probe time`, but an SNMP table walk is ~40 sequential round trips instead of three
//! echoes, and the poller serialises every spec aimed at one IP. This is the only way to put 50,000
//! *distinct* SNMP devices in front of a poller.
//!
//! ## Three things about this file are load-bearing, not style
//!
//! 1. 🚨 **One socket per address, bound to that address. Never `0.0.0.0`.** `csnmp` does not
//!    connect its socket — it compares the datagram's source against the target it asked and
//!    **silently discards a mismatch**, then loops until the 2 s budget is gone
//!    (`csnmp-0.5.0/src/client.rs:468`). A wildcard listener replies from whatever address routing
//!    picks, so every poll times out and the failure reads as "the poller is slow", not "the rig is
//!    wrong". Same bind-per-source shape as `event_firehose.rs` / `trap_firehose.rs`, with a fixed
//!    port instead of an ephemeral one.
//! 2. 🚨 **The response `Buf` never enters a future.** `snmp2`'s `pdu::Buf` is a **65,507-byte
//!    array** (`heap_buffers` is off), and `Buf::default()` zeroes all of it. Holding one per task
//!    would cost 3.2 GB at 50,000 sockets, and building one per request would memset gigabytes a
//!    second at the rates this rig exists for. So the reply is built by a **synchronous** function
//!    reading a thread-local `Buf` that is only ever `reset()`.
//! 3. 🚨 **Counters advance with elapsed time.** A frozen counter makes `rate()` zero, which makes
//!    core's interface-utilisation watch track **no** ports — the single most expensive thing on the
//!    receiving side would not run, and the test would go green having skipped it.
//!
//! ## What the generated MIB covers
//!
//! Every table the shipped check kinds walk, so all of them come back non-empty at once:
//! system scalars, `ifTable` + `ifXTable` (the built-in catalog's 11 per-interface metrics and the
//! three metadata columns), `dot3StatsDuplexStatus`, `hrProcessorLoad`, LLDP local + remote, CDP
//! cache, `ipAddrTable` + `ipAddressTable`, `ipNetToMedia` + `ipNetToPhysical`, BGP/OSPF neighbour
//! state, `ifMauTable`, ENTITY-MIB (the MAU walk's fallback and its alias mapping), and the
//! **Juniper** optical dialect.
//!
//! ⚠️ Optical is Juniper's `jnxDomCurrentTable` because it is **ifIndex-keyed**, so a reading needs
//! no ENTITY-MIB correlation (`OpticalFlavor::is_ifindex_keyed`). That is one session and two
//! columns; the Cisco/ENTITY-SENSOR dialect is two sessions and six, and would cost more per poll.
//! If a run shows optical dominating, this is the knob that was chosen cheaply — say so.
//!
//! ⚠️ Not every OID a poll asks for exists here, deliberately: the routing check GETs a host route
//! per monitored target, and a switch does not carry one. An absent column costs the walker two
//! round trips and returns nothing, which is what a real device does too.
//!
//! ## Run
//!
//!   sudo ip route add local 10.0.0.0/8 dev lo    # AnyIP: makes every 10.x address bindable
//!   ulimit -n 200000                             # one fd per device
//!   SNMPSIM_TARGETS=50000 cargo run --release --example snmp_sim
//!
//! Optional 40 ms round trip, the same rig `icmp_bench.rs` uses:
//!   sudo tc qdisc add dev lo root netem delay 20ms limit 100000
//!
//! Env knobs (all optional):
//!   SNMPSIM_BASE        first device address           (default 10.0.0.1)
//!   SNMPSIM_TARGETS     how many addresses to bind     (default 50000)
//!   SNMPSIM_IFACES      interfaces per device          (default 24)
//!   SNMPSIM_COMMUNITY   accepted community             (default "public")
//!   SNMPSIM_PORT        agent port                     (default 161)
//!   SNMPSIM_REPORT_SECS stats line interval            (default 10)
//!
//! 🚨 **Read the bound count it prints before trusting any poller number.** A device that did not
//! bind is indistinguishable in the poller's counters from a device that is merely slow: `inflight`
//! sits below the cap, CPU stays low, and **nothing is counted as dropped** — the same signature the
//! lab produced when its simulators were stopped.

use std::cell::RefCell;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use snmp2::{pdu, snmp, MessageType, Oid, Value, Version};
use tokio::net::UdpSocket;

/// Largest request this rig will read. A walk sends one column per request, so real requests are
/// tens of bytes; this bound is what keeps 50,000 idle tasks at ~75 MB rather than 3.2 GB.
const RECV_BUF: usize = 1500;

/// Ceiling on repetitions honoured in one GETBULK reply, and on total varbinds emitted. The walker
/// asks for 20 (`WALK_MAX_REPETITIONS`); this only stops a hostile or mistaken request from asking
/// for a reply that will not fit the 65,507-byte buffer.
const MAX_REPETITIONS: u32 = 64;
const MAX_VARBINDS: usize = 256;

thread_local! {
    /// One reply buffer per worker thread. See the module doc: this must not live in a future.
    static REPLY_BUF: RefCell<pdu::Buf> = RefCell::new(pdu::Buf::default());
}

/// One MIB cell. Counters carry a per-second rate so `rate()` is non-zero downstream.
#[derive(Debug, Clone)]
enum Cell {
    Int(i64),
    Str(Vec<u8>),
    ObjId(Vec<u64>),
    Gauge(u32),
    Ip([u8; 4]),
    /// Elapsed time in hundredths of a second — `sysUpTime`.
    Uptime,
    Counter32 {
        base: u32,
        per_sec: u32,
    },
    Counter64 {
        base: u64,
        per_sec: u64,
    },
}

/// The generated agent: OID arcs in ascending order, with the value each names.
///
/// Sorted by arc vector, which **is** SNMP lexicographic order, so GETNEXT is the entry after the
/// insertion point and GETBULK is the slice that follows it.
struct Mib {
    entries: Vec<(Vec<u64>, Cell)>,
    /// Encoded once at build time, so a reply borrows rather than re-encoding per varbind.
    oids: Vec<Oid<'static>>,
}

impl Mib {
    fn new(mut entries: Vec<(Vec<u64>, Cell)>) -> Self {
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        entries.dedup_by(|a, b| a.0 == b.0);
        let oids = entries
            .iter()
            .map(|(arcs, _)| Oid::from(arcs).expect("generated OID encodes"))
            .collect();
        Self { entries, oids }
    }

    /// Index of the exact OID, if present.
    fn get(&self, oid: &[u64]) -> Option<usize> {
        self.entries
            .binary_search_by(|e| e.0.as_slice().cmp(oid))
            .ok()
    }

    /// Index of the first entry strictly greater than `oid` — GETNEXT.
    fn next(&self, oid: &[u64]) -> Option<usize> {
        let idx = match self.entries.binary_search_by(|e| e.0.as_slice().cmp(oid)) {
            Ok(i) => i + 1,
            Err(i) => i,
        };
        (idx < self.entries.len()).then_some(idx)
    }

    /// Resolve one cell to a wire value at `elapsed`.
    fn value_at(&self, idx: usize, elapsed: Duration) -> Value<'_> {
        let secs = elapsed.as_secs();
        match &self.entries[idx].1 {
            Cell::Int(n) => Value::Integer(*n),
            Cell::Str(b) => Value::OctetString(b.as_slice()),
            // Rebuilt per read: `Value::ObjectIdentifier` owns its `Oid`, and these are rare
            // (alias mapping, sysObjectID, ifMauType) rather than on the counter hot path.
            Cell::ObjId(arcs) => {
                Value::ObjectIdentifier(Oid::from(arcs).expect("generated OID encodes"))
            }
            Cell::Gauge(v) => Value::Unsigned32(*v),
            Cell::Ip(b) => Value::IpAddress(*b),
            Cell::Uptime => {
                Value::Timeticks(u32::try_from(elapsed.as_millis() / 10).unwrap_or(u32::MAX))
            }
            Cell::Counter32 { base, per_sec } => Value::Counter32(
                base.wrapping_add(per_sec.wrapping_mul(u32::try_from(secs).unwrap_or(u32::MAX))),
            ),
            Cell::Counter64 { base, per_sec } => {
                Value::Counter64(base.wrapping_add(per_sec.wrapping_mul(secs)))
            }
        }
    }
}

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|v| *v > 0)
        .unwrap_or(default)
}

fn env_string(key: &str, default: &str) -> String {
    std::env::var(key)
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| default.to_owned())
}

/// Arcs of a column base plus its trailing instance sub-identifiers.
fn oid(prefix: &[u64], tail: &[u64]) -> Vec<u64> {
    let mut v = Vec::with_capacity(prefix.len() + tail.len());
    v.extend_from_slice(prefix);
    v.extend_from_slice(tail);
    v
}

const IF_TABLE: &[u64] = &[1, 3, 6, 1, 2, 1, 2, 2, 1];
const IF_X_TABLE: &[u64] = &[1, 3, 6, 1, 2, 1, 31, 1, 1, 1];
const LLDP_LOC: &[u64] = &[1, 0, 8802, 1, 1, 2, 1, 3, 7, 1];
const LLDP_REM: &[u64] = &[1, 0, 8802, 1, 1, 2, 1, 4, 1, 1];
const CDP_CACHE: &[u64] = &[1, 3, 6, 1, 4, 1, 9, 9, 23, 1, 2, 1, 1];
const ENTITY: &[u64] = &[1, 3, 6, 1, 2, 1, 47, 1, 1, 1, 1];
const JNX_DOM: &[u64] = &[1, 3, 6, 1, 4, 1, 2636, 3, 18, 1, 1, 1];

/// Build the one MIB every address serves. `n` is the interface count.
///
/// The device is an access switch whose first two ports are uplinks with neighbours. Values are
/// plausible rather than captured — see the module doc on why a real `.snmprec` is the wrong asset
/// here (the corpus is breadth across models, not depth on one model repeated 50,000 times).
#[allow(clippy::too_many_lines)] // one table per paragraph; splitting it would hide the shape
fn build_mib(n: usize) -> Mib {
    let mut e: Vec<(Vec<u64>, Cell)> = Vec::with_capacity(n * 40 + 64);
    let s = |t: &str| Cell::Str(t.as_bytes().to_vec());

    // ── system ───────────────────────────────────────────────────────────────────────────────
    e.push((
        vec![1, 3, 6, 1, 2, 1, 1, 1, 0],
        s("Yagra snmp_sim synthetic access switch"),
    ));
    e.push((
        vec![1, 3, 6, 1, 2, 1, 1, 2, 0],
        Cell::ObjId(vec![1, 3, 6, 1, 4, 1, 9, 1, 1208]),
    ));
    e.push((vec![1, 3, 6, 1, 2, 1, 1, 3, 0], Cell::Uptime));
    e.push((vec![1, 3, 6, 1, 2, 1, 1, 4, 0], s("yagra-loadtest")));
    e.push((vec![1, 3, 6, 1, 2, 1, 1, 5, 0], s("snmp-sim")));
    e.push((vec![1, 3, 6, 1, 2, 1, 1, 6, 0], s("lab")));
    e.push((vec![1, 3, 6, 1, 2, 1, 1, 7, 0], Cell::Int(78)));
    e.push((
        vec![1, 3, 6, 1, 2, 1, 2, 1, 0],
        Cell::Int(i64::try_from(n).unwrap_or(i64::MAX)),
    ));

    // ── hrProcessorLoad: four cores, so the node-level max() aggregate has something to fold ──
    for c in 1..=4u64 {
        e.push((
            oid(&[1, 3, 6, 1, 2, 1, 25, 3, 3, 1, 2], &[c]),
            Cell::Int(10 + (i64::try_from(c).unwrap_or(1) * 7) % 40),
        ));
    }

    for i in 1..=n as u64 {
        let mac = vec![0x02, 0x00, 0x5e, (i >> 8) as u8, i as u8, 0x01];
        // Rates differ per port so the fleet is not one flat line: ~5–60 Mbit/s in, half that out.
        let bps_in = 600_000 + (i % 24) * 300_000;
        let bps_out = bps_in / 2;
        let c32 = |base: u64, per_sec: u64| Cell::Counter32 {
            base: u32::try_from(base).unwrap_or(u32::MAX),
            per_sec: u32::try_from(per_sec).unwrap_or(u32::MAX),
        };

        // ifTable
        e.push((oid(IF_TABLE, &[1, i]), Cell::Int(i as i64)));
        e.push((oid(IF_TABLE, &[2, i]), s(&format!("GigabitEthernet0/{i}"))));
        e.push((oid(IF_TABLE, &[3, i]), Cell::Int(6)));
        e.push((oid(IF_TABLE, &[4, i]), Cell::Int(1500)));
        e.push((oid(IF_TABLE, &[5, i]), Cell::Gauge(1_000_000_000)));
        e.push((oid(IF_TABLE, &[6, i]), Cell::Str(mac)));
        e.push((oid(IF_TABLE, &[7, i]), Cell::Int(1)));
        e.push((oid(IF_TABLE, &[8, i]), Cell::Int(1)));
        e.push((oid(IF_TABLE, &[9, i]), Cell::Int(0)));
        e.push((oid(IF_TABLE, &[10, i]), c32(1_000 * i, bps_in)));
        e.push((oid(IF_TABLE, &[11, i]), c32(500, bps_in / 700)));
        e.push((oid(IF_TABLE, &[13, i]), c32(0, u64::from(i % 7 == 0))));
        e.push((oid(IF_TABLE, &[14, i]), c32(0, u64::from(i % 11 == 0))));
        e.push((oid(IF_TABLE, &[16, i]), c32(2_000 * i, bps_out)));
        e.push((oid(IF_TABLE, &[17, i]), c32(700, bps_out / 700)));
        e.push((oid(IF_TABLE, &[19, i]), c32(0, 0)));
        e.push((oid(IF_TABLE, &[20, i]), c32(0, 0)));

        // ifXTable — the four 64-bit counters the built-in catalog collects, plus the metadata.
        e.push((oid(IF_X_TABLE, &[1, i]), s(&format!("Gi0/{i}"))));
        e.push((
            oid(IF_X_TABLE, &[6, i]),
            Cell::Counter64 {
                base: 1_000 * i,
                per_sec: bps_in,
            },
        ));
        e.push((
            oid(IF_X_TABLE, &[7, i]),
            Cell::Counter64 {
                base: 500,
                per_sec: bps_in / 700,
            },
        ));
        e.push((
            oid(IF_X_TABLE, &[10, i]),
            Cell::Counter64 {
                base: 2_000 * i,
                per_sec: bps_out,
            },
        ));
        e.push((
            oid(IF_X_TABLE, &[11, i]),
            Cell::Counter64 {
                base: 700,
                per_sec: bps_out / 700,
            },
        ));
        e.push((oid(IF_X_TABLE, &[15, i]), Cell::Gauge(1_000)));
        e.push((oid(IF_X_TABLE, &[18, i]), s(&format!("access-port-{i}"))));

        // dot3StatsDuplexStatus — fullDuplex(3)
        e.push((
            oid(&[1, 3, 6, 1, 2, 1, 10, 7, 2, 1, 19], &[i]),
            Cell::Int(3),
        ));

        // ifMauTable — one MAU per port, 1000BaseTFD
        e.push((
            oid(&[1, 3, 6, 1, 2, 1, 26, 2, 1, 1, 1], &[i, 1]),
            Cell::Int(i as i64),
        ));
        e.push((
            oid(&[1, 3, 6, 1, 2, 1, 26, 2, 1, 1, 3], &[i, 1]),
            Cell::ObjId(vec![1, 3, 6, 1, 2, 1, 26, 4, 30]),
        ));

        // LLDP local port
        e.push((oid(LLDP_LOC, &[2, i]), Cell::Int(5)));
        e.push((oid(LLDP_LOC, &[3, i]), s(&format!("Gi0/{i}"))));
        e.push((oid(LLDP_LOC, &[4, i]), s(&format!("GigabitEthernet0/{i}"))));

        // ENTITY-MIB: one port entity per interface, plus its alias mapping back to ifIndex.
        let ent = 1000 + i;
        e.push((
            oid(ENTITY, &[2, ent]),
            s(&format!("GigabitEthernet0/{i} port")),
        ));
        e.push((oid(ENTITY, &[4, ent]), Cell::Int(1)));
        e.push((oid(ENTITY, &[5, ent]), Cell::Int(10)));
        e.push((oid(ENTITY, &[7, ent]), s(&format!("Gi0/{i}"))));
        e.push((oid(ENTITY, &[13, ent]), s("GLC-LH-SMD")));
        e.push((oid(ENTITY, &[16, ent]), Cell::Int(1)));
        e.push((
            oid(&[1, 3, 6, 1, 2, 1, 47, 1, 3, 2, 1, 2], &[ent, 0]),
            Cell::ObjId(oid(IF_TABLE, &[1, i])),
        ));

        // Optical, Juniper dialect (ifIndex-keyed): dBm × 100. Rx around -3.5, Tx around -2.5.
        e.push((
            oid(JNX_DOM, &[5, i]),
            Cell::Int(-350 - (i64::try_from(i).unwrap_or(0) % 30) * 10),
        ));
        e.push((
            oid(JNX_DOM, &[7, i]),
            Cell::Int(-250 - (i64::try_from(i).unwrap_or(0) % 20) * 10),
        ));
    }

    // ── two uplinks have neighbours: LLDP remote + CDP cache ─────────────────────────────────
    for port in [1u64, 2u64] {
        let peer_mac = vec![0x02, 0x00, 0x5e, 0xff, 0x00, port as u8];
        // lldpRemTable is indexed by (timeMark, localPortNum, index).
        let (tm, idx) = (0u64, 1u64);
        e.push((oid(LLDP_REM, &[4, tm, port, idx]), Cell::Int(4)));
        e.push((oid(LLDP_REM, &[5, tm, port, idx]), Cell::Str(peer_mac)));
        e.push((oid(LLDP_REM, &[6, tm, port, idx]), Cell::Int(5)));
        e.push((
            oid(LLDP_REM, &[7, tm, port, idx]),
            s(&format!("Te1/0/{port}")),
        ));
        e.push((
            oid(LLDP_REM, &[8, tm, port, idx]),
            s(&format!("TenGigabitEthernet1/0/{port} to access")),
        ));
        e.push((
            oid(LLDP_REM, &[9, tm, port, idx]),
            s(&format!("core-sw-{port}")),
        ));
        e.push((
            oid(LLDP_REM, &[10, tm, port, idx]),
            s("Yagra snmp_sim synthetic core switch"),
        ));
        e.push((
            oid(LLDP_REM, &[12, tm, port, idx]),
            Cell::Str(vec![0x28, 0x00]),
        ));
        // lldpRemManAddr index: (timeMark, port, index, addrSubtype=1, len=4, a.b.c.d)
        e.push((
            oid(
                &[1, 0, 8802, 1, 1, 2, 1, 4, 2, 1, 3],
                &[tm, port, idx, 1, 4, 10, 255, 0, port],
            ),
            Cell::Int(i64::try_from(port).unwrap_or(0)),
        ));
        // CDP: the interface name is keyed by ifIndex, the cache by (ifIndex, deviceIndex).
        e.push((
            oid(&[1, 3, 6, 1, 4, 1, 9, 9, 23, 1, 1, 1, 1, 6], &[port]),
            s(&format!("GigabitEthernet0/{port}")),
        ));
        e.push((oid(CDP_CACHE, &[3, port, 1]), Cell::Int(1)));
        e.push((
            oid(CDP_CACHE, &[4, port, 1]),
            Cell::Str(vec![10, 255, 0, port as u8]),
        ));
        e.push((
            oid(CDP_CACHE, &[6, port, 1]),
            s(&format!("core-sw-{port}.lab")),
        ));
        e.push((
            oid(CDP_CACHE, &[7, port, 1]),
            s(&format!("TenGigabitEthernet1/0/{port}")),
        ));
        e.push((oid(CDP_CACHE, &[8, port, 1]), s("cisco WS-C3850-24T")));
        e.push((
            oid(CDP_CACHE, &[9, port, 1]),
            Cell::Str(vec![0x00, 0x00, 0x00, 0x28]),
        ));
    }

    // ── L3: one management address, in both the old and the new table ────────────────────────
    let mgmt = [10u64, 254, 0, 1];
    e.push((oid(&[1, 3, 6, 1, 2, 1, 4, 20, 1, 2], &mgmt), Cell::Int(1)));
    e.push((
        oid(&[1, 3, 6, 1, 2, 1, 4, 20, 1, 3], &mgmt),
        Cell::Ip([255, 255, 255, 0]),
    ));
    // ipAddressTable index: (addrType = ipv4(1), len = 4, a.b.c.d)
    let v4 = [1u64, 4, mgmt[0], mgmt[1], mgmt[2], mgmt[3]];
    e.push((oid(&[1, 3, 6, 1, 2, 1, 4, 34, 1, 3], &v4), Cell::Int(1)));
    e.push((oid(&[1, 3, 6, 1, 2, 1, 4, 34, 1, 4], &v4), Cell::Int(1)));
    e.push((
        oid(&[1, 3, 6, 1, 2, 1, 4, 34, 1, 5], &v4),
        Cell::ObjId(vec![
            1, 3, 6, 1, 2, 1, 4, 32, 1, 5, 1, 1, 4, 10, 254, 0, 0, 24,
        ]),
    ));

    // ── ARP: a few learned entries on the first uplink, in both spellings ────────────────────
    for h in 10..14u64 {
        let m = vec![0x02, 0x00, 0x5e, 0xaa, 0x00, h as u8];
        e.push((
            oid(&[1, 3, 6, 1, 2, 1, 4, 22, 1, 2], &[1, 10, 254, 0, h]),
            Cell::Str(m.clone()),
        ));
        e.push((
            oid(&[1, 3, 6, 1, 2, 1, 4, 35, 1, 4], &[1, 1, 4, 10, 254, 0, h]),
            Cell::Str(m),
        ));
    }

    // ── routing adjacency: one BGP peer established(6), one OSPF neighbour full(8) ───────────
    e.push((
        oid(&[1, 3, 6, 1, 2, 1, 15, 3, 1, 2], &[10, 255, 0, 1]),
        Cell::Int(6),
    ));
    e.push((
        oid(&[1, 3, 6, 1, 2, 1, 14, 10, 1, 6], &[10, 255, 0, 2, 0]),
        Cell::Int(8),
    ));

    Mib::new(e)
}

/// Which MIB entry (or exception) answers one requested varbind.
///
/// ⚠️ The exception carries a *kind*, not a `Value`: `snmp2::Value` implements neither `Clone` nor
/// `Copy`, so every one has to be constructed at the point it is handed to `pdu::build`.
enum Answer {
    /// Index into the MIB.
    At(usize),
    /// An exception carried back under the *requested* OID, which has no MIB index.
    Exception(usize, Exception),
}

/// The two exception values this agent ever returns.
#[derive(Clone, Copy)]
enum Exception {
    NoSuchObject,
    EndOfMibView,
}

impl Exception {
    fn value(self) -> Value<'static> {
        match self {
            Self::NoSuchObject => Value::NoSuchObject,
            Self::EndOfMibView => Value::EndOfMibView,
        }
    }
}

/// Resolve the varbind list one request asks for.
fn resolve(
    mib: &Mib,
    kind: MessageType,
    requested: &[Vec<u64>],
    bulk: (usize, u32),
) -> Vec<Answer> {
    let mut out = Vec::new();
    match kind {
        MessageType::GetRequest => {
            for (k, want) in requested.iter().enumerate() {
                out.push(
                    mib.get(want)
                        .map_or(Answer::Exception(k, Exception::NoSuchObject), Answer::At),
                );
            }
        }
        MessageType::GetNextRequest => {
            for (k, want) in requested.iter().enumerate() {
                out.push(
                    mib.next(want)
                        .map_or(Answer::Exception(k, Exception::EndOfMibView), Answer::At),
                );
            }
        }
        MessageType::GetBulkRequest => {
            let (non_rep, max_rep) = bulk;
            let non_rep = non_rep.min(requested.len());
            let max_rep = max_rep.clamp(1, MAX_REPETITIONS) as usize;
            for (k, want) in requested.iter().take(non_rep).enumerate() {
                out.push(
                    mib.next(want)
                        .map_or(Answer::Exception(k, Exception::EndOfMibView), Answer::At),
                );
            }
            for (k, want) in requested.iter().enumerate().skip(non_rep) {
                let mut cursor = mib.next(want);
                for _ in 0..max_rep {
                    match cursor {
                        Some(i) => {
                            out.push(Answer::At(i));
                            cursor = (i + 1 < mib.entries.len()).then_some(i + 1);
                        }
                        None => {
                            out.push(Answer::Exception(k, Exception::EndOfMibView));
                            break;
                        }
                    }
                    if out.len() >= MAX_VARBINDS {
                        break;
                    }
                }
                if out.len() >= MAX_VARBINDS {
                    break;
                }
            }
        }
        _ => {}
    }
    out.truncate(MAX_VARBINDS);
    out
}

/// Build one reply datagram for `req`, or `None` if this agent does not answer it.
///
/// **Synchronous on purpose** — see the module doc: the 65,507-byte `pdu::Buf` must stay on the
/// thread stack rather than becoming part of 50,000 task futures.
fn respond(mib: &Mib, community: &[u8], elapsed: Duration, req: &[u8]) -> Option<Vec<u8>> {
    let parsed = pdu::Pdu::from_bytes(req).ok()?;
    if parsed.community != community {
        return None; // a real agent stays silent on a bad community
    }
    let requested: Vec<Vec<u64>> = parsed
        .varbinds
        .clone()
        .filter_map(|(o, _)| o.iter().map(Iterator::collect))
        .collect();
    // For a *request*, snmp2 parks non_repeaters in `error_status` and max_repetitions in
    // `error_index` — the same two slots `pdu::build`'s doc says a *reply* reuses for errors.
    let bulk = (parsed.error_status as usize, parsed.error_index);
    let answers = resolve(mib, parsed.message_type, &requested, bulk);
    if answers.is_empty() && !requested.is_empty() {
        return None; // a message type this agent does not serve
    }

    // Own the OIDs first, then build the values borrowing them: `pdu::build` takes
    // `&[(&Oid, Value)]`, and `Value` is neither `Clone` nor `Copy`, so it cannot be built into an
    // owned pair and re-borrowed afterwards — each one is constructed exactly where it is handed on.
    let fallback = Oid::from(&[1u64, 3, 6, 1]).expect("static OID");
    let oids: Vec<Oid<'_>> = answers
        .iter()
        .map(|a| match a {
            Answer::At(i) => mib.oids[*i].clone(),
            Answer::Exception(k, _) => requested
                .get(*k)
                .and_then(|arcs| Oid::from(arcs).ok())
                .unwrap_or_else(|| fallback.clone()),
        })
        .collect();
    let varbinds: Vec<(&Oid, Value)> = answers
        .iter()
        .zip(oids.iter())
        .map(|(a, o)| match a {
            Answer::At(i) => (o, mib.value_at(*i, elapsed)),
            Answer::Exception(_, exc) => (o, exc.value()),
        })
        .collect();

    REPLY_BUF.with(|cell| {
        let mut buf = cell.borrow_mut();
        pdu::build(
            Version::V2C,
            community,
            snmp::MSG_RESPONSE,
            parsed.req_id,
            &varbinds,
            0, // error_status
            0, // error_index
            &mut buf,
            None,
        )
        .ok()?;
        Some(buf[..].to_vec())
    })
}

struct Stats {
    requests: AtomicU64,
    replies: AtomicU64,
    rejected: AtomicU64,
    varbinds: AtomicU64,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let base: Ipv4Addr = env_string("SNMPSIM_BASE", "10.0.0.1").parse()?;
    let targets = env_usize("SNMPSIM_TARGETS", 50_000);
    let ifaces = env_usize("SNMPSIM_IFACES", 24);
    let community = env_string("SNMPSIM_COMMUNITY", "public").into_bytes();
    let port = u16::try_from(env_usize("SNMPSIM_PORT", 161)).unwrap_or(161);
    let report = env_usize("SNMPSIM_REPORT_SECS", 10) as u64;

    let mib = Arc::new(build_mib(ifaces));
    eprintln!(
        "snmp_sim: base={base} targets={targets} ifaces={ifaces} port={port} \
         mib_entries={} community={}",
        mib.entries.len(),
        String::from_utf8_lossy(&community)
    );

    let started = Instant::now();
    let base_u32 = u32::from(base);
    let mut bound = 0usize;
    let mut first_error: Option<String> = None;
    let stats = Arc::new(Stats {
        requests: AtomicU64::new(0),
        replies: AtomicU64::new(0),
        rejected: AtomicU64::new(0),
        varbinds: AtomicU64::new(0),
    });

    for i in 0..u32::try_from(targets).unwrap_or(u32::MAX) {
        let ip = Ipv4Addr::from(base_u32.wrapping_add(i));
        let sock = match UdpSocket::bind(SocketAddr::from((ip, port))).await {
            Ok(s) => s,
            Err(e) => {
                if first_error.is_none() {
                    first_error = Some(format!("{ip}: {e}"));
                }
                continue;
            }
        };
        bound += 1;
        let mib = Arc::clone(&mib);
        let community = community.clone();
        let stats = Arc::clone(&stats);
        tokio::spawn(async move {
            let mut buf = [0u8; RECV_BUF];
            loop {
                let Ok((len, from)) = sock.recv_from(&mut buf).await else {
                    continue;
                };
                stats.requests.fetch_add(1, Ordering::Relaxed);
                match respond(&mib, &community, started.elapsed(), &buf[..len]) {
                    Some(reply) => {
                        if sock.send_to(&reply, from).await.is_ok() {
                            stats.replies.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                    None => {
                        stats.rejected.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
        });
    }

    // 🚨 The bound count is the number to read before believing any poller measurement: a device
    // that never bound looks exactly like a device that is merely slow.
    eprintln!(
        "snmp_sim: bound {bound}/{targets} sockets on :{port}{}",
        first_error.map_or(String::new(), |e| format!(" (first failure: {e})"))
    );
    if bound < targets {
        eprintln!(
            "snmp_sim: ⚠️ {} address(es) did not bind — raise `ulimit -n` and check \
             `ip route add local <range> dev lo`",
            targets - bound
        );
    }

    println!("csv,bound,req_per_sec,reply_per_sec,requests,replies,rejected,varbinds");
    let (mut prev_req, mut prev_rep, mut prev_at) = (0u64, 0u64, Instant::now());
    loop {
        tokio::time::sleep(Duration::from_secs(report)).await;
        let req = stats.requests.load(Ordering::Relaxed);
        let rep = stats.replies.load(Ordering::Relaxed);
        let rej = stats.rejected.load(Ordering::Relaxed);
        let vbs = stats.varbinds.load(Ordering::Relaxed);
        let secs = prev_at.elapsed().as_secs_f64().max(0.001);
        println!(
            "csv,{bound},{:.1},{:.1},{req},{rep},{rej},{vbs}",
            (req - prev_req) as f64 / secs,
            (rep - prev_rep) as f64 / secs,
        );
        (prev_req, prev_rep, prev_at) = (req, rep, Instant::now());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mib24() -> Mib {
        build_mib(24)
    }

    /// The generated agent must answer every table the shipped check kinds walk. A missing table is
    /// invisible in a scale run: the walk returns nothing and reads as a device that does not
    /// implement it, which is exactly what this rig must not silently be.
    #[test]
    fn every_table_the_checks_walk_has_rows() {
        let mib = mib24();
        let bases: [(&str, &[u64]); 14] = [
            ("ifTable ifDescr", &[1, 3, 6, 1, 2, 1, 2, 2, 1, 2]),
            ("ifXTable ifHCInOctets", &[1, 3, 6, 1, 2, 1, 31, 1, 1, 1, 6]),
            ("ifXTable ifAlias", &[1, 3, 6, 1, 2, 1, 31, 1, 1, 1, 18]),
            ("dot3 duplex", &[1, 3, 6, 1, 2, 1, 10, 7, 2, 1, 19]),
            ("hrProcessorLoad", &[1, 3, 6, 1, 2, 1, 25, 3, 3, 1, 2]),
            ("lldpLocPortId", &[1, 0, 8802, 1, 1, 2, 1, 3, 7, 1, 3]),
            ("lldpRemSysName", &[1, 0, 8802, 1, 1, 2, 1, 4, 1, 1, 9]),
            (
                "cdpCacheDeviceId",
                &[1, 3, 6, 1, 4, 1, 9, 9, 23, 1, 2, 1, 1, 6],
            ),
            ("ipAdEntIfIndex", &[1, 3, 6, 1, 2, 1, 4, 20, 1, 2]),
            ("ipAddressIfIndex", &[1, 3, 6, 1, 2, 1, 4, 34, 1, 3]),
            ("ipNetToMediaPhysAddress", &[1, 3, 6, 1, 2, 1, 4, 22, 1, 2]),
            ("ifMauType", &[1, 3, 6, 1, 2, 1, 26, 2, 1, 1, 3]),
            (
                "entAliasMappingIdentifier",
                &[1, 3, 6, 1, 2, 1, 47, 1, 3, 2, 1, 2],
            ),
            (
                "jnxDomCurrentRxLaserOutput",
                &[1, 3, 6, 1, 4, 1, 2636, 3, 18, 1, 1, 1, 5],
            ),
        ];
        for (name, base) in bases {
            let next = mib
                .next(base)
                .unwrap_or_else(|| panic!("{name}: MIB ends at {base:?}"));
            assert!(
                mib.entries[next].0.starts_with(base),
                "{name}: nothing under {base:?}"
            );
        }
    }

    /// The built-in catalog collects 11 per-interface metrics; every one of their columns must
    /// return exactly one row per interface, or the fleet's series count is not what a run claims.
    #[test]
    fn the_eleven_builtin_interface_columns_each_have_one_row_per_port() {
        let mib = mib24();
        let columns: [&[u64]; 11] = [
            &[1, 3, 6, 1, 2, 1, 31, 1, 1, 1, 6],  // if_hc_in_octets
            &[1, 3, 6, 1, 2, 1, 31, 1, 1, 1, 10], // if_hc_out_octets
            &[1, 3, 6, 1, 2, 1, 31, 1, 1, 1, 7],  // if_hc_in_ucast_pkts
            &[1, 3, 6, 1, 2, 1, 31, 1, 1, 1, 11], // if_hc_out_ucast_pkts
            &[1, 3, 6, 1, 2, 1, 2, 2, 1, 14],     // if_in_errors
            &[1, 3, 6, 1, 2, 1, 2, 2, 1, 20],     // if_out_errors
            &[1, 3, 6, 1, 2, 1, 2, 2, 1, 13],     // if_in_discards
            &[1, 3, 6, 1, 2, 1, 2, 2, 1, 19],     // if_out_discards
            &[1, 3, 6, 1, 2, 1, 2, 2, 1, 8],      // if_oper_status
            &[1, 3, 6, 1, 2, 1, 2, 2, 1, 7],      // if_admin_status
            &[1, 3, 6, 1, 2, 1, 31, 1, 1, 1, 15], // if_high_speed
        ];
        for col in columns {
            let rows = mib
                .entries
                .iter()
                .filter(|(o, _)| o.starts_with(col) && o.len() == col.len() + 1)
                .count();
            assert_eq!(rows, 24, "column {col:?} has {rows} rows, want 24");
        }
    }

    /// 🚨 A frozen counter is the failure this rig cannot afford: `rate()` goes to zero, core's
    /// interface-utilisation watch tracks no ports, and the most expensive path on the receiving
    /// side silently does not run. Assert the value actually moves with elapsed time.
    #[test]
    fn counters_advance_with_time() {
        let mib = mib24();
        let idx = mib
            .get(&[1, 3, 6, 1, 2, 1, 31, 1, 1, 1, 6, 1])
            .expect("if_hc_in_octets.1");
        let (Value::Counter64(t0), Value::Counter64(t60)) = (
            mib.value_at(idx, Duration::from_secs(0)),
            mib.value_at(idx, Duration::from_secs(60)),
        ) else {
            panic!("if_hc_in_octets must be a Counter64");
        };
        assert!(t60 > t0, "counter did not advance: {t0} -> {t60}");

        let up = mib.get(&[1, 3, 6, 1, 2, 1, 1, 3, 0]).expect("sysUpTime.0");
        let Value::Timeticks(ticks) = mib.value_at(up, Duration::from_secs(7)) else {
            panic!("sysUpTime must be Timeticks");
        };
        assert_eq!(ticks, 700);
    }

    /// GETNEXT walks the whole MIB in ascending order and terminates — the property `walk_bulk`
    /// relies on to know a column is exhausted.
    #[test]
    fn getnext_walks_every_entry_once_and_stops() {
        let mib = mib24();
        let mut at = vec![0u64];
        let mut seen = 0usize;
        while let Some(i) = mib.next(&at) {
            assert!(mib.entries[i].0 > at, "walk went backwards");
            at.clone_from(&mib.entries[i].0);
            seen += 1;
            assert!(seen <= mib.entries.len(), "walk did not terminate");
        }
        assert_eq!(seen, mib.entries.len());
    }

    /// A GETBULK for one column returns that column's rows in order, capped at max-repetitions —
    /// the exact shape `csnmp::walk_bulk` sends for every table column.
    #[test]
    fn getbulk_pages_a_column_the_way_the_walker_asks() {
        let mib = mib24();
        let base = Oid::from(&[1u64, 3, 6, 1, 2, 1, 2, 2, 1, 2]).unwrap();
        let mut buf = pdu::Buf::default();
        pdu::build(
            Version::V2C,
            b"public",
            snmp::MSG_GET_BULK,
            7,
            &[(&base, Value::Null)],
            0,  // non_repeaters
            20, // max_repetitions — WALK_MAX_REPETITIONS
            &mut buf,
            None,
        )
        .unwrap();
        let reply =
            respond(&mib, b"public", Duration::from_secs(1), &buf[..]).expect("agent answers");
        let parsed = pdu::Pdu::from_bytes(&reply).unwrap();
        assert_eq!(parsed.message_type, MessageType::Response);
        assert_eq!(parsed.req_id, 7);
        let rows: Vec<_> = parsed.varbinds.collect();
        assert_eq!(rows.len(), 20, "max-repetitions not honoured");
        assert_eq!(rows[0].0.to_string(), "1.3.6.1.2.1.2.2.1.2.1");
        assert_eq!(rows[19].0.to_string(), "1.3.6.1.2.1.2.2.1.2.20");
    }

    /// A GET for a scalar comes back with that scalar, and an absent OID with `noSuchObject`
    /// carried under the OID that was asked for — not dropped, which a walker would read as the
    /// end of the table.
    #[test]
    fn get_answers_a_scalar_and_names_what_it_does_not_have() {
        let mib = mib24();
        let uptime = Oid::from(&[1u64, 3, 6, 1, 2, 1, 1, 3, 0]).unwrap();
        let absent = Oid::from(&[1u64, 3, 6, 1, 4, 1, 99999, 1, 0]).unwrap();
        let mut buf = pdu::Buf::default();
        pdu::build(
            Version::V2C,
            b"public",
            snmp::MSG_GET,
            11,
            &[(&uptime, Value::Null), (&absent, Value::Null)],
            0,
            0,
            &mut buf,
            None,
        )
        .unwrap();
        let reply =
            respond(&mib, b"public", Duration::from_secs(3), &buf[..]).expect("agent answers");
        let rows: Vec<_> = pdu::Pdu::from_bytes(&reply).unwrap().varbinds.collect();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].0.to_string(), "1.3.6.1.2.1.1.3.0");
        assert!(matches!(rows[0].1, Value::Timeticks(300)));
        assert_eq!(rows[1].0.to_string(), "1.3.6.1.4.1.99999.1.0");
        assert!(matches!(rows[1].1, Value::NoSuchObject));
    }

    /// A wrong community gets silence, not a reply — a real agent's behaviour, and what keeps a
    /// mis-configured rig looking like an unreachable device rather than a working one.
    #[test]
    fn a_wrong_community_is_answered_with_silence() {
        let mib = mib24();
        let base = Oid::from(&[1u64, 3, 6, 1, 2, 1, 1, 3, 0]).unwrap();
        let mut buf = pdu::Buf::default();
        pdu::build(
            Version::V2C,
            b"private",
            snmp::MSG_GET,
            1,
            &[(&base, Value::Null)],
            0,
            0,
            &mut buf,
            None,
        )
        .unwrap();
        assert!(respond(&mib, b"public", Duration::from_secs(1), &buf[..]).is_none());
    }
}
