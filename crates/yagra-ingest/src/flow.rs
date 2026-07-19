//! Flow export parsing (NetFlow v9 / IPFIX) and edge top-N aggregation (Phase 3, ADR-031).
//!
//! Devices export **flow records** (who talked to whom, on which port/protocol, how much) as
//! passive UDP datagrams — the same class of edge intake as syslog/traps. NetFlow v9 and IPFIX
//! are **template-based**: a template FlowSet/Set defines the field layout, and later data records
//! are decoded against the cached template. [`FlowTemplates`] holds that cache (bounded, FIFO
//! eviction) keyed by `(exporter, observation-domain, template-id)`.
//!
//! The raw flow tuple `(src ip × dst ip × src port × dst port × proto)` is **extreme cardinality**
//! and must never reach the TSDB (CLAUDE.md §7.1). [`FlowAggregator`] folds identical tuples within
//! a bucket window and keeps only the **top-N by bytes**, bounding both memory (a distinct-key cap)
//! and the wire/WAN volume before anything is published to core.
//!
//! **Robustness contract** (same as [`crate::trap`]): every input is an attacker-controlled
//! datagram. Parsing never panics — all reads are bounds-checked, malformed input returns `Err` or
//! is skipped, and output counts are bounded ([`MAX_RECORDS_PER_DATAGRAM`], the template/key caps).
//! NetFlow v5 and sFlow are handled in a later increment; unknown versions return
//! [`FlowError::UnsupportedVersion`].

use std::collections::{HashMap, VecDeque};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use thiserror::Error;

/// Default number of top flows (by bytes) kept per bucket per exporter. Tunable via
/// `YAGRA_FLOW_TOP_N` on the poller. Balances drill-down fidelity against cardinality/WAN.
pub const DEFAULT_FLOW_TOP_N: usize = 500;

/// Upper bound on distinct flow keys tracked in one bucket before overflow is dropped (counted).
/// Caps aggregator memory against a source-cycling flood, mirroring the rate limiter's source cap.
pub const MAX_AGG_KEYS: usize = 100_000;

/// Upper bound on cached templates (across all exporters). FIFO eviction beyond this.
pub const MAX_TEMPLATES: usize = 8_192;

/// Upper bound on flow records decoded from a single datagram (a crafted-packet backstop).
pub const MAX_RECORDS_PER_DATAGRAM: usize = 8_192;

/// Upper bound on fields in one template (a crafted-template backstop).
const MAX_TEMPLATE_FIELDS: usize = 128;

// ── NetFlow v9 / IPFIX Information Element numbers we decode ─────────────────────────
// The common IEs share numbers between NetFlow v9 field types and IPFIX (RFC 7011 / IANA).
const IE_OCTET_DELTA: u16 = 1;
const IE_PACKET_DELTA: u16 = 2;
const IE_PROTOCOL: u16 = 4;
const IE_TOS: u16 = 5;
const IE_SRC_PORT: u16 = 7;
const IE_SRC_IPV4: u16 = 8;
const IE_INGRESS_IF: u16 = 10;
const IE_DST_PORT: u16 = 11;
const IE_DST_IPV4: u16 = 12;
const IE_SRC_IPV6: u16 = 27;
const IE_DST_IPV6: u16 = 28;
const IE_OCTET_TOTAL: u16 = 85;
const IE_PACKET_TOTAL: u16 = 86;

/// Errors parsing a flow export datagram.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum FlowError {
    /// The datagram is shorter than the version's header.
    #[error("datagram too short for flow header")]
    Truncated,
    /// The version field is not one this increment handles (NetFlow v9 / IPFIX).
    #[error("unsupported flow version {0}")]
    UnsupportedVersion(u16),
    /// Structurally malformed (e.g. a zero-length set that would loop forever).
    #[error("malformed flow export: {0}")]
    Malformed(String),
}

/// One decoded flow record (parser output). A 5-tuple plus ingress ifIndex / ToS and raw
/// byte/packet counts. Addresses are [`IpAddr`] — v4 or v6, never assume v4.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RawFlow {
    /// Source address.
    pub src_ip: IpAddr,
    /// Destination address.
    pub dst_ip: IpAddr,
    /// Source transport port (0 if the template carried none).
    pub src_port: u16,
    /// Destination transport port (0 if the template carried none).
    pub dst_port: u16,
    /// IP protocol number.
    pub proto: u8,
    /// IP type-of-service / DSCP byte.
    pub tos: u8,
    /// Ingress interface ifIndex (0 = unknown).
    pub if_index: u32,
    /// Bytes reported by this flow record.
    pub bytes: u64,
    /// Packets reported by this flow record.
    pub packets: u64,
}

/// A flow folded to its top-N aggregate over a bucket window (aggregator output). Same shape as
/// [`RawFlow`] plus `flows`, the number of raw records summed into it. The poller maps this to the
/// bus `FlowRecord` (setting `src_as`/`dst_as` = 0 — reserved for a later increment).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AggregatedFlow {
    /// Source address.
    pub src_ip: IpAddr,
    /// Destination address.
    pub dst_ip: IpAddr,
    /// Source transport port.
    pub src_port: u16,
    /// Destination transport port.
    pub dst_port: u16,
    /// IP protocol number.
    pub proto: u8,
    /// IP type-of-service / DSCP byte.
    pub tos: u8,
    /// Ingress interface ifIndex.
    pub if_index: u32,
    /// Summed bytes over the window.
    pub bytes: u64,
    /// Summed packets over the window.
    pub packets: u64,
    /// Number of raw records folded into this row.
    pub flows: u32,
}

// ── Template cache ──────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct TemplateKey {
    exporter: IpAddr,
    domain: u32,
    template_id: u16,
}

#[derive(Clone, Copy)]
struct TemplateField {
    /// Information-element number (enterprise bit already stripped).
    ie: u16,
    /// Field length in bytes.
    len: u16,
    /// Whether this is an enterprise-specific field (decoded as opaque — length skipped).
    enterprise: bool,
}

/// Bounded cache of NetFlow v9 / IPFIX templates. Keyed by `(exporter, observation-domain,
/// template-id)`; FIFO eviction at [`MAX_TEMPLATES`] so a template-churning flood can't grow memory
/// unbounded. Data records whose template has not been seen yet are simply skipped (standard flow
/// behaviour — the exporter re-sends templates periodically).
pub struct FlowTemplates {
    map: HashMap<TemplateKey, Vec<TemplateField>>,
    order: VecDeque<TemplateKey>,
    cap: usize,
}

impl Default for FlowTemplates {
    fn default() -> Self {
        Self::with_capacity(MAX_TEMPLATES)
    }
}

impl FlowTemplates {
    /// New cache with the default capacity.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// New cache with an explicit capacity.
    #[must_use]
    pub fn with_capacity(cap: usize) -> Self {
        Self {
            map: HashMap::new(),
            order: VecDeque::new(),
            cap: cap.max(1),
        }
    }

    fn insert(&mut self, key: TemplateKey, fields: Vec<TemplateField>) {
        if !self.map.contains_key(&key) {
            if self.map.len() >= self.cap {
                if let Some(oldest) = self.order.pop_front() {
                    self.map.remove(&oldest);
                }
            }
            self.order.push_back(key);
        }
        self.map.insert(key, fields);
    }

    fn get(&self, key: &TemplateKey) -> Option<&Vec<TemplateField>> {
        self.map.get(key)
    }

    /// Number of cached templates (test/observability).
    #[must_use]
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// Whether the cache is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

// ── Byte reader (bounds-checked, never panics) ──────────────────────────────────────

struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    fn remaining(&self) -> usize {
        self.buf.len().saturating_sub(self.pos)
    }

    fn u16(&mut self) -> Option<u16> {
        let end = self.pos.checked_add(2)?;
        let v = u16::from_be_bytes([*self.buf.get(self.pos)?, *self.buf.get(self.pos + 1)?]);
        self.pos = end;
        Some(v)
    }

    fn u32(&mut self) -> Option<u32> {
        if self.remaining() < 4 {
            return None;
        }
        let v = u32::from_be_bytes([
            self.buf[self.pos],
            self.buf[self.pos + 1],
            self.buf[self.pos + 2],
            self.buf[self.pos + 3],
        ]);
        self.pos += 4;
        Some(v)
    }

    /// Borrow the next `n` bytes and advance, or `None` if fewer remain.
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(n)?;
        let slice = self.buf.get(self.pos..end)?;
        self.pos = end;
        Some(slice)
    }
}

/// Decode a big-endian unsigned integer from up to 8 bytes (IPFIX reduced-size encoding). For
/// oversized fields, the low 8 bytes are used; empty ⇒ 0.
fn be_uint(bytes: &[u8]) -> u64 {
    let take = bytes.len().min(8);
    let start = bytes.len() - take;
    let mut v = 0u64;
    for &b in &bytes[start..] {
        v = (v << 8) | u64::from(b);
    }
    v
}

// ── Top-level parse ─────────────────────────────────────────────────────────────────

/// Parse a flow export datagram from `exporter`, decoding data records against `templates`
/// (which is updated in place as template sets are seen). Returns the decoded [`RawFlow`]s (empty
/// if the datagram carried only templates / options / unseen-template data). Never panics.
pub fn parse_flow_export(
    templates: &mut FlowTemplates,
    exporter: IpAddr,
    datagram: &[u8],
) -> Result<Vec<RawFlow>, FlowError> {
    if datagram.len() < 2 {
        return Err(FlowError::Truncated);
    }
    let version = u16::from_be_bytes([datagram[0], datagram[1]]);
    match version {
        9 => parse_netflow_v9(templates, exporter, datagram),
        10 => parse_ipfix(templates, exporter, datagram),
        other => Err(FlowError::UnsupportedVersion(other)),
    }
}

fn parse_netflow_v9(
    templates: &mut FlowTemplates,
    exporter: IpAddr,
    datagram: &[u8],
) -> Result<Vec<RawFlow>, FlowError> {
    // Header: version(2) count(2) sys_uptime(4) unix_secs(4) seq(4) source_id(4) = 20 bytes.
    if datagram.len() < 20 {
        return Err(FlowError::Truncated);
    }
    let domain = u32::from_be_bytes([datagram[16], datagram[17], datagram[18], datagram[19]]);
    let mut reader = Reader::new(&datagram[20..]);
    let mut out = Vec::new();

    while reader.remaining() >= 4 {
        let flowset_id = reader.u16().ok_or(FlowError::Truncated)?;
        let length = reader.u16().ok_or(FlowError::Truncated)? as usize;
        if length < 4 {
            // A zero/short FlowSet length would loop forever — bail out.
            return Err(FlowError::Malformed("flowset length < 4".into()));
        }
        let content_len = length - 4;
        let content = match reader.take(content_len) {
            Some(c) => c,
            None => break, // truncated trailer — stop, keep what we have
        };
        match flowset_id {
            0 => parse_v9_templates(templates, exporter, domain, content),
            1 => { /* options template — not decoded this increment */ }
            id if id >= 256 => {
                decode_data_records(templates, exporter, domain, id, content, &mut out);
                if out.len() >= MAX_RECORDS_PER_DATAGRAM {
                    break;
                }
            }
            _ => { /* reserved (2..256) — skip */ }
        }
    }
    Ok(out)
}

fn parse_v9_templates(
    templates: &mut FlowTemplates,
    exporter: IpAddr,
    domain: u32,
    content: &[u8],
) {
    let mut r = Reader::new(content);
    while r.remaining() >= 4 {
        let (Some(template_id), Some(field_count)) = (r.u16(), r.u16()) else {
            break;
        };
        let field_count = (field_count as usize).min(MAX_TEMPLATE_FIELDS);
        let mut fields = Vec::with_capacity(field_count);
        let mut ok = true;
        for _ in 0..field_count {
            let (Some(ie), Some(len)) = (r.u16(), r.u16()) else {
                ok = false;
                break;
            };
            fields.push(TemplateField {
                ie,
                len,
                enterprise: false,
            });
        }
        if !ok || fields.is_empty() {
            break;
        }
        templates.insert(
            TemplateKey {
                exporter,
                domain,
                template_id,
            },
            fields,
        );
    }
}

fn parse_ipfix(
    templates: &mut FlowTemplates,
    exporter: IpAddr,
    datagram: &[u8],
) -> Result<Vec<RawFlow>, FlowError> {
    // Header: version(2) length(2) export_time(4) seq(4) obs_domain(4) = 16 bytes.
    if datagram.len() < 16 {
        return Err(FlowError::Truncated);
    }
    let msg_len = u16::from_be_bytes([datagram[2], datagram[3]]) as usize;
    let domain = u32::from_be_bytes([datagram[12], datagram[13], datagram[14], datagram[15]]);
    // Trust the declared length only up to the actual datagram size.
    let end = msg_len.clamp(16, datagram.len());
    let mut reader = Reader::new(&datagram[16..end]);
    let mut out = Vec::new();

    while reader.remaining() >= 4 {
        let set_id = reader.u16().ok_or(FlowError::Truncated)?;
        let set_length = reader.u16().ok_or(FlowError::Truncated)? as usize;
        if set_length < 4 {
            return Err(FlowError::Malformed("set length < 4".into()));
        }
        let content = match reader.take(set_length - 4) {
            Some(c) => c,
            None => break,
        };
        match set_id {
            2 => parse_ipfix_templates(templates, exporter, domain, content),
            3 => { /* options template — not decoded this increment */ }
            id if id >= 256 => {
                decode_data_records(templates, exporter, domain, id, content, &mut out);
                if out.len() >= MAX_RECORDS_PER_DATAGRAM {
                    break;
                }
            }
            _ => { /* reserved — skip */ }
        }
    }
    Ok(out)
}

fn parse_ipfix_templates(
    templates: &mut FlowTemplates,
    exporter: IpAddr,
    domain: u32,
    content: &[u8],
) {
    let mut r = Reader::new(content);
    while r.remaining() >= 4 {
        let (Some(template_id), Some(field_count)) = (r.u16(), r.u16()) else {
            break;
        };
        let field_count = (field_count as usize).min(MAX_TEMPLATE_FIELDS);
        let mut fields = Vec::with_capacity(field_count);
        let mut ok = true;
        for _ in 0..field_count {
            let (Some(ie_raw), Some(len)) = (r.u16(), r.u16()) else {
                ok = false;
                break;
            };
            let enterprise = ie_raw & 0x8000 != 0;
            if enterprise {
                // Enterprise-specific field: consume the 4-byte PEN; decode as opaque later.
                if r.u32().is_none() {
                    ok = false;
                    break;
                }
            }
            fields.push(TemplateField {
                ie: ie_raw & 0x7fff,
                len,
                enterprise,
            });
        }
        if !ok || fields.is_empty() {
            break;
        }
        templates.insert(
            TemplateKey {
                exporter,
                domain,
                template_id,
            },
            fields,
        );
    }
}

/// Decode a data FlowSet/Set against a cached template, appending [`RawFlow`]s to `out`. Unknown
/// template ⇒ no-op (records skipped until the template is re-sent). Records missing both addresses
/// are dropped (not a useful flow).
fn decode_data_records(
    templates: &FlowTemplates,
    exporter: IpAddr,
    domain: u32,
    template_id: u16,
    content: &[u8],
    out: &mut Vec<RawFlow>,
) {
    let key = TemplateKey {
        exporter,
        domain,
        template_id,
    };
    let Some(fields) = templates.get(&key) else {
        return;
    };
    let record_len: usize = fields.iter().map(|f| f.len as usize).sum();
    if record_len == 0 {
        return;
    }
    let mut r = Reader::new(content);
    while r.remaining() >= record_len && out.len() < MAX_RECORDS_PER_DATAGRAM {
        let Some(record) = r.take(record_len) else {
            break;
        };
        if let Some(flow) = decode_one_record(fields, record) {
            out.push(flow);
        }
    }
}

fn decode_one_record(fields: &[TemplateField], record: &[u8]) -> Option<RawFlow> {
    let mut fr = Reader::new(record);
    let mut src_ip: Option<IpAddr> = None;
    let mut dst_ip: Option<IpAddr> = None;
    let mut src_port = 0u16;
    let mut dst_port = 0u16;
    let mut proto = 0u8;
    let mut tos = 0u8;
    let mut if_index = 0u32;
    let mut bytes = 0u64;
    let mut packets = 0u64;

    for f in fields {
        let val = fr.take(f.len as usize)?;
        if f.enterprise {
            continue; // opaque enterprise field — length already consumed
        }
        match f.ie {
            IE_OCTET_DELTA | IE_OCTET_TOTAL => bytes = be_uint(val),
            IE_PACKET_DELTA | IE_PACKET_TOTAL => packets = be_uint(val),
            IE_PROTOCOL => proto = val.first().copied().unwrap_or(0),
            IE_TOS => tos = val.first().copied().unwrap_or(0),
            IE_SRC_PORT => src_port = be_uint(val) as u16,
            IE_DST_PORT => dst_port = be_uint(val) as u16,
            IE_INGRESS_IF => if_index = be_uint(val) as u32,
            IE_SRC_IPV4 if val.len() == 4 => {
                src_ip = Some(IpAddr::V4(Ipv4Addr::new(val[0], val[1], val[2], val[3])));
            }
            IE_DST_IPV4 if val.len() == 4 => {
                dst_ip = Some(IpAddr::V4(Ipv4Addr::new(val[0], val[1], val[2], val[3])));
            }
            IE_SRC_IPV6 if val.len() == 16 => src_ip = Some(ipv6_from(val)),
            IE_DST_IPV6 if val.len() == 16 => dst_ip = Some(ipv6_from(val)),
            _ => { /* unmapped IE — skip */ }
        }
    }

    Some(RawFlow {
        src_ip: src_ip?,
        dst_ip: dst_ip?,
        src_port,
        dst_port,
        proto,
        tos,
        if_index,
        bytes,
        packets,
    })
}

fn ipv6_from(val: &[u8]) -> IpAddr {
    let mut octets = [0u8; 16];
    octets.copy_from_slice(&val[..16]);
    IpAddr::V6(Ipv6Addr::from(octets))
}

// ── Edge top-N aggregator ───────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct FlowKey {
    src_ip: IpAddr,
    dst_ip: IpAddr,
    src_port: u16,
    dst_port: u16,
    proto: u8,
    tos: u8,
    if_index: u32,
}

#[derive(Clone, Copy, Default)]
struct Counts {
    bytes: u64,
    packets: u64,
    flows: u32,
}

/// Folds [`RawFlow`]s within a bucket window into distinct 5-tuples, then emits the **top-N by
/// bytes** on drain. Distinct keys are bounded ([`MAX_AGG_KEYS`]); overflow raw flows and keys
/// beyond the top-N are counted into the returned `dropped` (observability only — best-effort tier).
pub struct FlowAggregator {
    top_n: usize,
    map: HashMap<FlowKey, Counts>,
    key_cap: usize,
    dropped_overflow: u32,
}

impl FlowAggregator {
    /// New aggregator keeping `top_n` flows (0 ⇒ [`DEFAULT_FLOW_TOP_N`]) with the default key cap.
    #[must_use]
    pub fn new(top_n: usize) -> Self {
        Self::with_caps(top_n, MAX_AGG_KEYS)
    }

    /// New aggregator with explicit top-N and distinct-key caps.
    #[must_use]
    pub fn with_caps(top_n: usize, key_cap: usize) -> Self {
        Self {
            top_n: if top_n == 0 {
                DEFAULT_FLOW_TOP_N
            } else {
                top_n
            },
            map: HashMap::new(),
            key_cap: key_cap.max(1),
            dropped_overflow: 0,
        }
    }

    /// Fold one raw flow into the current bucket.
    pub fn add(&mut self, f: RawFlow) {
        let key = FlowKey {
            src_ip: f.src_ip,
            dst_ip: f.dst_ip,
            src_port: f.src_port,
            dst_port: f.dst_port,
            proto: f.proto,
            tos: f.tos,
            if_index: f.if_index,
        };
        if let Some(c) = self.map.get_mut(&key) {
            c.bytes = c.bytes.saturating_add(f.bytes);
            c.packets = c.packets.saturating_add(f.packets);
            c.flows = c.flows.saturating_add(1);
        } else if self.map.len() < self.key_cap {
            self.map.insert(
                key,
                Counts {
                    bytes: f.bytes,
                    packets: f.packets,
                    flows: 1,
                },
            );
        } else {
            // Key cap reached — this untracked flow is dropped (counted).
            self.dropped_overflow = self.dropped_overflow.saturating_add(1);
        }
    }

    /// Whether nothing has been aggregated (nothing to publish).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// Number of distinct flows currently held.
    #[must_use]
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// Take the top-N flows by bytes and reset the aggregator for the next bucket. Returns the
    /// aggregated records (sorted bytes-descending) and the count of flows/keys dropped beyond the
    /// caps (key-cap overflow + top-N truncation).
    #[must_use]
    pub fn drain_top_n(&mut self) -> (Vec<AggregatedFlow>, u32) {
        let overflow = self.dropped_overflow;
        self.dropped_overflow = 0;
        let map = std::mem::take(&mut self.map);
        let total = map.len();
        let mut aggs: Vec<AggregatedFlow> = map
            .into_iter()
            .map(|(k, c)| AggregatedFlow {
                src_ip: k.src_ip,
                dst_ip: k.dst_ip,
                src_port: k.src_port,
                dst_port: k.dst_port,
                proto: k.proto,
                tos: k.tos,
                if_index: k.if_index,
                bytes: c.bytes,
                packets: c.packets,
                flows: c.flows,
            })
            .collect();
        // Stable-ish ordering: bytes desc, then packets desc, then a deterministic tie-break so
        // the top-N cut is reproducible (tests, and no undefined ordering on equal bytes).
        aggs.sort_by(|a, b| {
            b.bytes
                .cmp(&a.bytes)
                .then_with(|| b.packets.cmp(&a.packets))
                .then_with(|| {
                    (a.src_ip, a.dst_ip, a.dst_port).cmp(&(b.src_ip, b.dst_ip, b.dst_port))
                })
        });
        let truncated = total.saturating_sub(self.top_n) as u32;
        aggs.truncate(self.top_n);
        (aggs, overflow.saturating_add(truncated))
    }
}

/// Upper bound on distinct exporters tracked in one bucket. A poller realistically sees far fewer;
/// beyond this, a new exporter's flows are dropped (counted) so a spoofed-source flood can't grow
/// memory unbounded.
pub const MAX_EXPORTERS: usize = 4_096;

/// One exporter's aggregated top-N flows, ready to become a bus `FlowBatch`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExporterBatch {
    /// The exporting device's source address (core maps this to a node).
    pub exporter: IpAddr,
    /// Top-N aggregated flows for this exporter, bytes-descending.
    pub records: Vec<AggregatedFlow>,
    /// Flows/keys dropped beyond the top-N or key cap for this exporter (observability).
    pub dropped: u32,
}

/// Per-exporter [`FlowAggregator`]s: the poller receives flows from many devices, but each bus
/// `FlowBatch` is single-exporter (core maps `exporter_ip` → node), so aggregation is kept separate
/// per exporter. The exporter count is bounded ([`MAX_EXPORTERS`]).
pub struct ExporterBuckets {
    per_exporter: HashMap<IpAddr, FlowAggregator>,
    top_n: usize,
    max_exporters: usize,
    dropped_exporter_flows: u32,
}

impl ExporterBuckets {
    /// New set keeping `top_n` flows per exporter, default exporter cap.
    #[must_use]
    pub fn new(top_n: usize) -> Self {
        Self::with_caps(top_n, MAX_EXPORTERS)
    }

    /// New set with explicit top-N and exporter caps.
    #[must_use]
    pub fn with_caps(top_n: usize, max_exporters: usize) -> Self {
        Self {
            per_exporter: HashMap::new(),
            top_n,
            max_exporters: max_exporters.max(1),
            dropped_exporter_flows: 0,
        }
    }

    /// Fold one raw flow from `exporter` into its bucket.
    pub fn add(&mut self, exporter: IpAddr, f: RawFlow) {
        if let Some(agg) = self.per_exporter.get_mut(&exporter) {
            agg.add(f);
        } else if self.per_exporter.len() < self.max_exporters {
            let mut agg = FlowAggregator::new(self.top_n);
            agg.add(f);
            self.per_exporter.insert(exporter, agg);
        } else {
            self.dropped_exporter_flows = self.dropped_exporter_flows.saturating_add(1);
        }
    }

    /// Whether nothing is buffered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.per_exporter.values().all(FlowAggregator::is_empty)
    }

    /// Drain every exporter's top-N and reset for the next bucket. Returns one [`ExporterBatch`] per
    /// exporter that had flows, plus the count of flows dropped because the exporter cap was hit.
    #[must_use]
    pub fn drain(&mut self) -> (Vec<ExporterBatch>, u32) {
        let dropped_exporter_flows = self.dropped_exporter_flows;
        self.dropped_exporter_flows = 0;
        let map = std::mem::take(&mut self.per_exporter);
        let batches = map
            .into_iter()
            .filter_map(|(exporter, mut agg)| {
                if agg.is_empty() {
                    return None;
                }
                let (records, dropped) = agg.drain_top_n();
                Some(ExporterBatch {
                    exporter,
                    records,
                    dropped,
                })
            })
            .collect();
        (batches, dropped_exporter_flows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v4(a: u8, b: u8, c: u8, d: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(a, b, c, d))
    }

    fn raw(src_last: u8, dst_last: u8, bytes: u64) -> RawFlow {
        RawFlow {
            src_ip: v4(10, 0, 0, src_last),
            dst_ip: v4(10, 0, 1, dst_last),
            src_port: 1234,
            dst_port: 443,
            proto: 6,
            tos: 0,
            if_index: 2,
            bytes,
            packets: bytes / 100,
        }
    }

    // ── Aggregator ──

    #[test]
    fn aggregator_folds_identical_tuples_and_counts_flows() {
        let mut agg = FlowAggregator::new(10);
        agg.add(raw(1, 1, 100));
        agg.add(raw(1, 1, 250));
        let (out, dropped) = agg.drain_top_n();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].bytes, 350);
        assert_eq!(out[0].flows, 2);
        assert_eq!(dropped, 0);
        // Drain resets.
        assert!(agg.is_empty());
    }

    #[test]
    fn aggregator_keeps_top_n_by_bytes_and_reports_truncation() {
        let mut agg = FlowAggregator::new(2);
        agg.add(raw(1, 1, 100));
        agg.add(raw(2, 2, 900));
        agg.add(raw(3, 3, 500));
        let (out, dropped) = agg.drain_top_n();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].bytes, 900); // sorted desc
        assert_eq!(out[1].bytes, 500);
        assert_eq!(dropped, 1); // the 100-byte flow was truncated
    }

    #[test]
    fn aggregator_bounds_distinct_keys_and_counts_overflow() {
        let mut agg = FlowAggregator::with_caps(1000, 2);
        agg.add(raw(1, 1, 100));
        agg.add(raw(2, 2, 100));
        agg.add(raw(3, 3, 100)); // over the key cap → dropped
        assert_eq!(agg.len(), 2);
        let (out, dropped) = agg.drain_top_n();
        assert_eq!(out.len(), 2);
        assert_eq!(dropped, 1);
    }

    #[test]
    fn exporter_buckets_separate_by_exporter_and_drain_per_exporter() {
        let mut buckets = ExporterBuckets::new(10);
        let e1 = v4(192, 168, 0, 1);
        let e2 = v4(192, 168, 0, 2);
        buckets.add(e1, raw(1, 1, 100));
        buckets.add(e1, raw(1, 1, 50)); // folds into the same tuple for e1
        buckets.add(e2, raw(1, 1, 400));
        let (mut batches, dropped_exporters) = buckets.drain();
        assert_eq!(dropped_exporters, 0);
        assert_eq!(batches.len(), 2);
        batches.sort_by_key(|b| b.exporter);
        // e1 folded two records into one flow of 150 bytes.
        let b1 = batches.iter().find(|b| b.exporter == e1).unwrap();
        assert_eq!(b1.records.len(), 1);
        assert_eq!(b1.records[0].bytes, 150);
        assert_eq!(b1.records[0].flows, 2);
        // e2 is independent.
        let b2 = batches.iter().find(|b| b.exporter == e2).unwrap();
        assert_eq!(b2.records[0].bytes, 400);
        // Drain reset.
        assert!(buckets.is_empty());
    }

    #[test]
    fn exporter_buckets_bound_distinct_exporters() {
        let mut buckets = ExporterBuckets::with_caps(10, 2);
        buckets.add(v4(10, 0, 0, 1), raw(1, 1, 100));
        buckets.add(v4(10, 0, 0, 2), raw(1, 1, 100));
        buckets.add(v4(10, 0, 0, 3), raw(1, 1, 100)); // over exporter cap → dropped
        let (batches, dropped_exporters) = buckets.drain();
        assert_eq!(batches.len(), 2);
        assert_eq!(dropped_exporters, 1);
    }

    // ── Parser: build a minimal NetFlow v9 packet ──

    /// Build a NetFlow v9 datagram: one template FlowSet + one data FlowSet with `records`.
    fn nf9_packet(
        template_id: u16,
        records: &[(Ipv4Addr, Ipv4Addr, u16, u16, u8, u64, u64)],
    ) -> Vec<u8> {
        // Template fields: IPV4_SRC(8,4) IPV4_DST(12,4) SRC_PORT(7,2) DST_PORT(11,2)
        //                  PROTOCOL(4,1) IN_BYTES(1,4) IN_PKTS(2,4)  → record_len = 21
        let fields: [(u16, u16); 7] = [
            (IE_SRC_IPV4, 4),
            (IE_DST_IPV4, 4),
            (IE_SRC_PORT, 2),
            (IE_DST_PORT, 2),
            (IE_PROTOCOL, 1),
            (IE_OCTET_DELTA, 4),
            (IE_PACKET_DELTA, 4),
        ];
        let record_len: usize = fields.iter().map(|(_, l)| *l as usize).sum();

        let mut tmpl = Vec::new();
        tmpl.extend_from_slice(&template_id.to_be_bytes());
        tmpl.extend_from_slice(&(fields.len() as u16).to_be_bytes());
        for (ie, len) in fields {
            tmpl.extend_from_slice(&ie.to_be_bytes());
            tmpl.extend_from_slice(&len.to_be_bytes());
        }
        // Template FlowSet: id=0, length = 4 + tmpl.len()
        let mut tmpl_set = Vec::new();
        tmpl_set.extend_from_slice(&0u16.to_be_bytes());
        tmpl_set.extend_from_slice(&((4 + tmpl.len()) as u16).to_be_bytes());
        tmpl_set.extend_from_slice(&tmpl);

        let mut data = Vec::new();
        for (src, dst, sp, dp, proto, bytes, pkts) in records {
            data.extend_from_slice(&src.octets());
            data.extend_from_slice(&dst.octets());
            data.extend_from_slice(&sp.to_be_bytes());
            data.extend_from_slice(&dp.to_be_bytes());
            data.push(*proto);
            data.extend_from_slice(&(*bytes as u32).to_be_bytes());
            data.extend_from_slice(&(*pkts as u32).to_be_bytes());
        }
        assert_eq!(data.len(), records.len() * record_len);
        // Data FlowSet: id=template_id, length = 4 + data.len()
        let mut data_set = Vec::new();
        data_set.extend_from_slice(&template_id.to_be_bytes());
        data_set.extend_from_slice(&((4 + data.len()) as u16).to_be_bytes());
        data_set.extend_from_slice(&data);

        let mut pkt = Vec::new();
        pkt.extend_from_slice(&9u16.to_be_bytes()); // version
        pkt.extend_from_slice(&2u16.to_be_bytes()); // count (unused by parser)
        pkt.extend_from_slice(&0u32.to_be_bytes()); // sys_uptime
        pkt.extend_from_slice(&0u32.to_be_bytes()); // unix_secs
        pkt.extend_from_slice(&0u32.to_be_bytes()); // seq
        pkt.extend_from_slice(&7u32.to_be_bytes()); // source_id (domain)
        pkt.extend_from_slice(&tmpl_set);
        pkt.extend_from_slice(&data_set);
        pkt
    }

    #[test]
    fn netflow_v9_template_then_data_decodes() {
        let exporter = v4(192, 168, 1, 1);
        let recs = [
            (
                Ipv4Addr::new(10, 0, 0, 5),
                Ipv4Addr::new(8, 8, 8, 8),
                40000u16,
                443u16,
                6u8,
                5000u64,
                10u64,
            ),
            (
                Ipv4Addr::new(10, 0, 0, 6),
                Ipv4Addr::new(1, 1, 1, 1),
                40001u16,
                53u16,
                17u8,
                200u64,
                2u64,
            ),
        ];
        let pkt = nf9_packet(256, &recs);
        let mut templates = FlowTemplates::new();
        let flows = parse_flow_export(&mut templates, exporter, &pkt).unwrap();
        assert_eq!(flows.len(), 2);
        assert_eq!(flows[0].src_ip, v4(10, 0, 0, 5));
        assert_eq!(flows[0].dst_ip, v4(8, 8, 8, 8));
        assert_eq!(flows[0].dst_port, 443);
        assert_eq!(flows[0].proto, 6);
        assert_eq!(flows[0].bytes, 5000);
        assert_eq!(flows[1].proto, 17);
        assert_eq!(templates.len(), 1);
    }

    #[test]
    fn netflow_v9_data_without_template_is_skipped() {
        // A data FlowSet whose template hasn't been seen yields no flows (no panic, no error).
        let exporter = v4(192, 168, 1, 1);
        let full = nf9_packet(
            256,
            &[(
                Ipv4Addr::new(10, 0, 0, 5),
                Ipv4Addr::new(8, 8, 8, 8),
                1u16,
                2u16,
                6u8,
                10u64,
                1u64,
            )],
        );
        // Strip the template FlowSet: header(20) + template_set. Reconstruct with data only is
        // fiddly; instead parse the data against an empty cache by using a fresh cache and a packet
        // whose template id differs from what's cached.
        let mut templates = FlowTemplates::new();
        // Parse a packet that defines template 256, then a second packet referencing template 300.
        let _ = parse_flow_export(&mut templates, exporter, &full).unwrap();
        let orphan = nf9_orphan_data(300);
        let flows = parse_flow_export(&mut templates, exporter, &orphan).unwrap();
        assert!(flows.is_empty());
    }

    /// A NetFlow v9 packet with only a data FlowSet for `template_id` (no template).
    fn nf9_orphan_data(template_id: u16) -> Vec<u8> {
        let data = vec![0u8; 21]; // one record-sized blob
        let mut data_set = Vec::new();
        data_set.extend_from_slice(&template_id.to_be_bytes());
        data_set.extend_from_slice(&((4 + data.len()) as u16).to_be_bytes());
        data_set.extend_from_slice(&data);
        let mut pkt = Vec::new();
        pkt.extend_from_slice(&9u16.to_be_bytes());
        pkt.extend_from_slice(&1u16.to_be_bytes());
        pkt.extend_from_slice(&[0u8; 12]); // uptime/secs/seq
        pkt.extend_from_slice(&7u32.to_be_bytes()); // domain
        pkt.extend_from_slice(&data_set);
        pkt
    }

    #[test]
    fn unsupported_version_errs() {
        let mut t = FlowTemplates::new();
        // NetFlow v5 (version 5) is a later increment.
        let pkt = [0u8, 5, 0, 0];
        assert_eq!(
            parse_flow_export(&mut t, v4(1, 1, 1, 1), &pkt),
            Err(FlowError::UnsupportedVersion(5))
        );
    }

    #[test]
    fn hostile_inputs_never_panic() {
        let mut t = FlowTemplates::new();
        let exporter = v4(1, 1, 1, 1);
        // Empty, tiny, truncated headers, random junk, zero-length sets.
        let cases: Vec<Vec<u8>> = vec![
            vec![],
            vec![0],
            vec![0, 9],        // v9 version, no header
            vec![0, 10, 0, 0], // v10, short header
            vec![
                0, 9, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            ], // v9 with a zero-len flowset
            vec![
                0, 10, 255, 255, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2, 0, 0,
            ], // v10 bogus msg_len + set_len 0
            (0..255u8).cycle().take(600).collect(),
        ];
        for c in cases {
            // Must return (Ok or Err) without panicking.
            let _ = parse_flow_export(&mut t, exporter, &c);
        }
    }

    #[test]
    fn ipv6_addresses_decode() {
        // Minimal IPFIX packet: template with IPv6 src/dst + bytes, one data record.
        let exporter = v4(192, 168, 1, 1);
        let fields: [(u16, u16); 3] = [(IE_SRC_IPV6, 16), (IE_DST_IPV6, 16), (IE_OCTET_DELTA, 4)];
        let record_len: usize = fields.iter().map(|(_, l)| *l as usize).sum();

        let mut tmpl = Vec::new();
        tmpl.extend_from_slice(&300u16.to_be_bytes());
        tmpl.extend_from_slice(&(fields.len() as u16).to_be_bytes());
        for (ie, len) in fields {
            tmpl.extend_from_slice(&ie.to_be_bytes());
            tmpl.extend_from_slice(&len.to_be_bytes());
        }
        let mut tmpl_set = Vec::new();
        tmpl_set.extend_from_slice(&2u16.to_be_bytes()); // IPFIX template set id
        tmpl_set.extend_from_slice(&((4 + tmpl.len()) as u16).to_be_bytes());
        tmpl_set.extend_from_slice(&tmpl);

        let src = Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1);
        let dst = Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 2);
        let mut data = Vec::new();
        data.extend_from_slice(&src.octets());
        data.extend_from_slice(&dst.octets());
        data.extend_from_slice(&1234u32.to_be_bytes());
        let mut data_set = Vec::new();
        data_set.extend_from_slice(&300u16.to_be_bytes());
        data_set.extend_from_slice(&((4 + data.len()) as u16).to_be_bytes());
        data_set.extend_from_slice(&data);

        let body_len = 16 + tmpl_set.len() + data_set.len();
        let mut pkt = Vec::new();
        pkt.extend_from_slice(&10u16.to_be_bytes());
        pkt.extend_from_slice(&(body_len as u16).to_be_bytes());
        pkt.extend_from_slice(&0u32.to_be_bytes()); // export time
        pkt.extend_from_slice(&0u32.to_be_bytes()); // seq
        pkt.extend_from_slice(&7u32.to_be_bytes()); // obs domain
        pkt.extend_from_slice(&tmpl_set);
        pkt.extend_from_slice(&data_set);
        assert_eq!(pkt.len(), body_len);

        let mut templates = FlowTemplates::new();
        let flows = parse_flow_export(&mut templates, exporter, &pkt).unwrap();
        assert_eq!(flows.len(), 1);
        assert_eq!(flows[0].src_ip, IpAddr::V6(src));
        assert_eq!(flows[0].dst_ip, IpAddr::V6(dst));
        assert_eq!(flows[0].bytes, 1234);
        assert_eq!(record_len, 36);
    }

    #[test]
    fn templates_are_bounded_fifo() {
        let mut t = FlowTemplates::with_capacity(4);
        let exporter = v4(1, 1, 1, 1);
        for id in 256..256 + 10u16 {
            let pkt = nf9_packet(
                id,
                &[(
                    Ipv4Addr::new(10, 0, 0, 1),
                    Ipv4Addr::new(10, 0, 0, 2),
                    1,
                    2,
                    6,
                    10,
                    1,
                )],
            );
            let _ = parse_flow_export(&mut t, exporter, &pkt);
        }
        assert!(t.len() <= 4);
    }
}
