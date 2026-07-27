// SPDX-License-Identifier: AGPL-3.0-only
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
//! [`parse_flow_export`] handles the collector-port formats — NetFlow v5 (fixed-format), NetFlow v9,
//! and IPFIX; unknown versions return [`FlowError::UnsupportedVersion`]. sFlow rides a **separate
//! datagram shape on its own port** and is decoded by [`parse_sflow`], including sampling-rate scale
//! correction, so both feed the same [`RawFlow`] downstream.

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
const IE_SRC_AS: u16 = 16;
const IE_DST_AS: u16 = 17;
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
    /// Source BGP autonomous-system number (0 = unknown — the exporter carried none).
    pub src_as: u32,
    /// Destination BGP autonomous-system number (0 = unknown).
    pub dst_as: u32,
    /// Bytes reported by this flow record.
    pub bytes: u64,
    /// Packets reported by this flow record.
    pub packets: u64,
}

/// A flow folded to its top-N aggregate over a bucket window (aggregator output). Same shape as
/// [`RawFlow`] plus `flows`, the number of raw records summed into it. The poller maps this to the
/// bus `FlowRecord`. `src_as`/`dst_as` carry the first non-zero AS seen for the tuple (0 = unknown).
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
    /// Source BGP autonomous-system number (0 = unknown).
    pub src_as: u32,
    /// Destination BGP autonomous-system number (0 = unknown).
    pub dst_as: u32,
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

    fn u8(&mut self) -> Option<u8> {
        let b = *self.buf.get(self.pos)?;
        self.pos += 1;
        Some(b)
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
        5 => parse_netflow_v5(datagram),
        9 => parse_netflow_v9(templates, exporter, datagram),
        10 => parse_ipfix(templates, exporter, datagram),
        other => Err(FlowError::UnsupportedVersion(other)),
    }
}

/// Parse a NetFlow **v5** datagram (fixed-format, no templates — older Cisco). The 24-byte header is
/// followed by `count` fixed 48-byte records. If the header's sampling interval is set (low 14 bits
/// of `sampling_interval`, top 2 = mode), byte/packet counts are scaled by it so a sampled v5
/// exporter isn't undercounted — matching sFlow's estimate semantics. Never panics.
fn parse_netflow_v5(datagram: &[u8]) -> Result<Vec<RawFlow>, FlowError> {
    const V5_HEADER: usize = 24;
    const V5_RECORD: usize = 48;
    if datagram.len() < V5_HEADER {
        return Err(FlowError::Truncated);
    }
    let count = u16::from_be_bytes([datagram[2], datagram[3]]) as usize;
    // Sampling interval: low 14 bits carry the 1-in-N rate (top 2 bits are the mode). 0/1 ⇒ none.
    let sampling = u16::from_be_bytes([datagram[22], datagram[23]]) & 0x3fff;
    let scale = u64::from(sampling.max(1));

    let mut reader = Reader::new(&datagram[V5_HEADER..]);
    let mut out = Vec::new();
    let max = count.min(MAX_RECORDS_PER_DATAGRAM);
    for _ in 0..max {
        let Some(rec) = reader.take(V5_RECORD) else {
            break; // truncated trailer — keep what we have
        };
        if let Some(flow) = decode_v5_record(rec, scale) {
            out.push(flow);
        }
    }
    Ok(out)
}

/// Decode one 48-byte NetFlow v5 record. Reads fields at their fixed offsets via the bounds-checked
/// [`Reader`]; the trailing `src_mask`/`dst_mask`/`pad2` are ignored (only the AS pair is used).
fn decode_v5_record(rec: &[u8], scale: u64) -> Option<RawFlow> {
    let mut r = Reader::new(rec);
    let src = r.take(4)?;
    let src_ip = IpAddr::V4(Ipv4Addr::new(src[0], src[1], src[2], src[3]));
    let dst = r.take(4)?;
    let dst_ip = IpAddr::V4(Ipv4Addr::new(dst[0], dst[1], dst[2], dst[3]));
    r.take(4)?; // nexthop
    let if_index = u32::from(r.u16()?); // input ifIndex
    r.u16()?; // output ifIndex
    let packets = u64::from(r.u32()?); // dPkts
    let bytes = u64::from(r.u32()?); // dOctets
    r.u32()?; // first (switched)
    r.u32()?; // last (switched)
    let src_port = r.u16()?;
    let dst_port = r.u16()?;
    r.u8()?; // pad1
    r.u8()?; // tcp_flags
    let proto = r.u8()?;
    let tos = r.u8()?;
    let src_as = u32::from(r.u16()?); // v5 AS fields are 16-bit
    let dst_as = u32::from(r.u16()?);
    // Remaining src_mask/dst_mask/pad2 are ignored.
    Some(RawFlow {
        src_ip,
        dst_ip,
        src_port,
        dst_port,
        proto,
        tos,
        if_index,
        src_as,
        dst_as,
        bytes: bytes.saturating_mul(scale),
        packets: packets.saturating_mul(scale),
    })
}

/// Whether a datagram carries template (or options-template) definitions.
///
/// The forwarder needs this to decide what a *filtered* flow destination must receive (ADR-034): a
/// datagram holding templates but no flow records carries nothing a filter could exclude, while a
/// collector that never receives it can never decode the datagrams that do pass the filter. Cheap
/// by construction — it walks FlowSet headers and decodes nothing. `false` for NetFlow v5 and
/// sFlow, which have no templates, and for anything malformed.
#[must_use]
pub fn carries_template_set(datagram: &[u8]) -> bool {
    if datagram.len() < 2 {
        return false;
    }
    match u16::from_be_bytes([datagram[0], datagram[1]]) {
        // v9: header is 20 bytes; FlowSet id 0 = template, 1 = options template.
        9 if datagram.len() >= 20 => has_set_id(&datagram[20..], 0, 1),
        // IPFIX: header is 16 bytes and declares the message length; set id 2 = template,
        // 3 = options template. Trust the declared length only up to what actually arrived.
        10 if datagram.len() >= 16 => {
            let msg_len = u16::from_be_bytes([datagram[2], datagram[3]]) as usize;
            let end = msg_len.clamp(16, datagram.len());
            has_set_id(&datagram[16..end], 2, 3)
        }
        _ => false,
    }
}

/// Scan FlowSet/Set headers for either template id. Stops on the first malformed length rather
/// than guessing, so a truncated trailer can never loop or over-read.
fn has_set_id(body: &[u8], template: u16, options_template: u16) -> bool {
    let mut r = Reader::new(body);
    while r.remaining() >= 4 {
        let (Some(set_id), Some(length)) = (r.u16(), r.u16()) else {
            return false;
        };
        if set_id == template || set_id == options_template {
            return true;
        }
        let length = length as usize;
        if length < 4 || r.take(length - 4).is_none() {
            return false;
        }
    }
    false
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
    let mut src_as = 0u32;
    let mut dst_as = 0u32;
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
            IE_SRC_AS => src_as = be_uint(val) as u32,
            IE_DST_AS => dst_as = be_uint(val) as u32,
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
        src_as,
        dst_as,
        bytes,
        packets,
    })
}

fn ipv6_from(val: &[u8]) -> IpAddr {
    let mut octets = [0u8; 16];
    octets.copy_from_slice(&val[..16]);
    IpAddr::V6(Ipv6Addr::from(octets))
}

// ── sFlow v5 ──────────────────────────────────────────────────────────────────────────
// sFlow rides its own UDP port (:6343) with a wholly different datagram shape: a header followed by
// N *samples*. We decode flow samples (standard format 1 and expanded format 3) that carry a *raw
// sampled packet header*, extract the 5-tuple from that header, and scale byte/packet counts by the
// sample's 1-in-N sampling rate to estimate real traffic. Counter samples and unknown formats are
// skipped by their declared length. Every read is bounds-checked; parsing never panics.

/// Upper bound on samples decoded from one sFlow datagram (a crafted-packet backstop).
const MAX_SFLOW_SAMPLES: usize = 1_024;
/// Upper bound on flow records decoded from one sFlow sample.
const MAX_SFLOW_RECORDS: usize = 256;

// sFlow sample formats (enterprise 0).
const SFLOW_FLOW_SAMPLE: u32 = 1;
const SFLOW_FLOW_SAMPLE_EXPANDED: u32 = 3;
// sFlow flow-record formats we decode (enterprise 0).
const SFLOW_RAW_PACKET_HEADER: u32 = 1;
const SFLOW_EXTENDED_GATEWAY: u32 = 1003;
// Sampled-header link/network protocols.
const SFLOW_HEADER_ETHERNET: u32 = 1;
const SFLOW_HEADER_IPV4: u32 = 11;
const SFLOW_HEADER_IPV6: u32 = 12;

/// Parse an sFlow **v5** datagram, returning the decoded [`RawFlow`]s with byte/packet counts
/// already scaled by each flow sample's sampling rate. Stateless (no template cache). Non-flow
/// samples and unknown record formats are skipped by length. Never panics.
pub fn parse_sflow(datagram: &[u8]) -> Result<Vec<RawFlow>, FlowError> {
    let mut r = Reader::new(datagram);
    let version = r.u32().ok_or(FlowError::Truncated)?;
    if version != 5 {
        // Only sFlow v5 is defined/deployed; anything else is unsupported.
        return Err(FlowError::UnsupportedVersion(
            u16::try_from(version).unwrap_or(u16::MAX),
        ));
    }
    let agent_type = r.u32().ok_or(FlowError::Truncated)?;
    let agent_len = match agent_type {
        1 => 4,  // IPv4 agent address
        2 => 16, // IPv6 agent address
        other => {
            return Err(FlowError::Malformed(format!(
                "sflow agent addr type {other}"
            )))
        }
    };
    r.take(agent_len).ok_or(FlowError::Truncated)?; // agent address (unused — core keys on peer IP)
    r.u32().ok_or(FlowError::Truncated)?; // sub_agent_id
    r.u32().ok_or(FlowError::Truncated)?; // sequence_number
    r.u32().ok_or(FlowError::Truncated)?; // uptime
    let num_samples = r.u32().ok_or(FlowError::Truncated)? as usize;

    let mut out = Vec::new();
    for _ in 0..num_samples.min(MAX_SFLOW_SAMPLES) {
        let (Some(sample_type), Some(sample_len)) = (r.u32(), r.u32()) else {
            break;
        };
        let Some(body) = r.take(sample_len as usize) else {
            break; // truncated trailer — stop, keep what we have
        };
        // Low 12 bits = format, high 20 = enterprise number (0 for standard samples).
        if (sample_type >> 12) == 0 {
            match sample_type & 0x0fff {
                SFLOW_FLOW_SAMPLE => {
                    let _ = decode_sflow_flow_sample(body, false, &mut out);
                }
                SFLOW_FLOW_SAMPLE_EXPANDED => {
                    let _ = decode_sflow_flow_sample(body, true, &mut out);
                }
                _ => { /* counter/other sample — skip (already consumed by length) */ }
            }
        }
        if out.len() >= MAX_RECORDS_PER_DATAGRAM {
            break;
        }
    }
    Ok(out)
}

/// Decode one sFlow flow sample body (compact `format 1` or `expanded` `format 3`), appending a
/// [`RawFlow`] per raw-packet-header record. An `extended_gateway` (format 1003) record in the same
/// sample carries the BGP AS pair for the sampled packet, so its `(src_as, dst_as)` is stamped onto
/// every raw-header flow from this sample (records may appear in any order — hence the buffer).
/// Returns `None` only on a truncated body (caller ignores — the sample is simply dropped).
fn decode_sflow_flow_sample(body: &[u8], expanded: bool, out: &mut Vec<RawFlow>) -> Option<()> {
    let mut r = Reader::new(body);
    r.u32()?; // sequence_number
    r.take(if expanded { 8 } else { 4 })?; // source_id (expanded: type + index)
    let sampling_rate = r.u32()?;
    r.u32()?; // sample_pool
    r.u32()?; // drops
              // input/output ifIndex: 4 bytes each (compact) or format+value 8 bytes each (expanded).
    let input_if = if expanded {
        r.u32()?; // input_format
        let v = r.u32()?; // input_value (ifIndex)
        r.u32()?; // output_format
        r.u32()?; // output_value
        v
    } else {
        let v = r.u32()?; // input ifIndex
        r.u32()?; // output ifIndex
        v
    };
    let num_records = r.u32()?;
    let rate = u64::from(sampling_rate.max(1));
    let mut sample_flows: Vec<RawFlow> = Vec::new();
    let mut gateway_as: Option<(u32, u32)> = None;
    for _ in 0..(num_records as usize).min(MAX_SFLOW_RECORDS) {
        let (Some(flow_format), Some(rec_len)) = (r.u32(), r.u32()) else {
            break;
        };
        let Some(rec_body) = r.take(rec_len as usize) else {
            break;
        };
        if (flow_format >> 12) == 0 {
            match flow_format & 0x0fff {
                SFLOW_RAW_PACKET_HEADER => {
                    if let Some(flow) = decode_sflow_raw_header(rec_body, rate, input_if) {
                        sample_flows.push(flow);
                    }
                }
                SFLOW_EXTENDED_GATEWAY => {
                    if let Some(asn) = decode_sflow_extended_gateway(rec_body) {
                        gateway_as = Some(asn);
                    }
                }
                _ => { /* other flow-record format — skip (already consumed by length) */ }
            }
        }
    }
    if let Some((src_as, dst_as)) = gateway_as {
        for f in &mut sample_flows {
            f.src_as = src_as;
            f.dst_as = dst_as;
        }
    }
    out.extend(sample_flows);
    Some(())
}

/// Decode an sFlow `extended_gateway` (format 1003) record into `(src_as, dst_as)`. Unlike NetFlow,
/// sFlow carries BGP AS here rather than in the sampled packet header: `src_as` is the source-AS
/// field and `dst_as` is the origin (last) AS of the destination AS-path. Bounds-checked with capped
/// segment/AS counts; returns `None` on truncation or an unknown next-hop address type. Never panics.
fn decode_sflow_extended_gateway(body: &[u8]) -> Option<(u32, u32)> {
    const MAX_AS_PATH_SEGMENTS: usize = 64;
    const MAX_AS_PATH_TOTAL: usize = 512;
    let mut r = Reader::new(body);
    // next_hop address: address_type(4) + 4 (IPv4) | 16 (IPv6).
    let addr_len = match r.u32()? {
        1 => 4,
        2 => 16,
        _ => return None,
    };
    r.take(addr_len)?;
    r.u32()?; // as (the agent's own AS)
    let src_as = r.u32()?; // src_as
    r.u32()?; // src_peer_as
              // dst_as_path: a sequence of path segments; the origin AS is the last AS across the path.
    let segments = r.u32()? as usize;
    let mut dst_as = 0u32;
    let mut budget = MAX_AS_PATH_TOTAL;
    for _ in 0..segments.min(MAX_AS_PATH_SEGMENTS) {
        r.u32()?; // segment type (AS_SET / AS_SEQUENCE)
        let seg_len = r.u32()? as usize;
        for _ in 0..seg_len.min(budget) {
            dst_as = r.u32()?;
            budget -= 1;
        }
        if budget == 0 {
            break;
        }
    }
    Some((src_as, dst_as))
}

/// Decode a raw-packet-header flow record into a [`RawFlow`], scaling counts by `rate`. `frame_length`
/// is the *original* packet length (pre-sampling), so `bytes ≈ frame_length × rate` and one sample
/// represents `rate` packets.
fn decode_sflow_raw_header(body: &[u8], rate: u64, if_index: u32) -> Option<RawFlow> {
    let mut r = Reader::new(body);
    let header_protocol = r.u32()?;
    let frame_length = r.u32()?;
    r.u32()?; // stripped
    let header_length = r.u32()? as usize;
    let header = r.take(header_length)?; // the sampled packet header bytes
    let (src_ip, dst_ip, src_port, dst_port, proto, tos) =
        parse_sampled_5tuple(header_protocol, header)?;
    Some(RawFlow {
        src_ip,
        dst_ip,
        src_port,
        dst_port,
        proto,
        tos,
        if_index,
        // AS is not in the sampled packet header — an extended_gateway record in the same sample
        // supplies it (stamped by the caller); 0 until/unless one is present.
        src_as: 0,
        dst_as: 0,
        bytes: u64::from(frame_length).saturating_mul(rate),
        packets: rate,
    })
}

/// Extract `(src, dst, src_port, dst_port, proto, tos)` from a raw sampled packet header.
/// `header_protocol` selects the layer the header starts at (Ethernet / IPv4 / IPv6). Bounds-checked;
/// returns `None` (record dropped) on anything it can't decode — never panics.
fn parse_sampled_5tuple(
    header_protocol: u32,
    header: &[u8],
) -> Option<(IpAddr, IpAddr, u16, u16, u8, u8)> {
    match header_protocol {
        SFLOW_HEADER_ETHERNET => {
            let (ethertype, l3) = strip_ethernet(header)?;
            match ethertype {
                0x0800 => parse_ipv4_header(l3),
                0x86DD => parse_ipv6_header(l3),
                _ => None,
            }
        }
        SFLOW_HEADER_IPV4 => parse_ipv4_header(header),
        SFLOW_HEADER_IPV6 => parse_ipv6_header(header),
        _ => None,
    }
}

/// Skip the Ethernet II header and any 802.1Q/802.1ad VLAN tags, returning the inner ethertype and
/// the remaining L3 bytes.
fn strip_ethernet(buf: &[u8]) -> Option<(u16, &[u8])> {
    let mut r = Reader::new(buf);
    r.take(12)?; // dst + src MAC
    let mut ethertype = r.u16()?;
    // Walk stacked VLAN tags (each: TCI(2) + inner ethertype(2)); bound the walk.
    let mut guard = 0;
    while (ethertype == 0x8100 || ethertype == 0x88A8) && guard < 4 {
        r.u16()?; // TCI
        ethertype = r.u16()?;
        guard += 1;
    }
    let n = r.remaining();
    let rest = r.take(n)?;
    Some((ethertype, rest))
}

/// Decode an IPv4 header's 5-tuple contributions. `buf` starts at the IP header.
fn parse_ipv4_header(buf: &[u8]) -> Option<(IpAddr, IpAddr, u16, u16, u8, u8)> {
    if buf.len() < 20 {
        return None;
    }
    if buf[0] >> 4 != 4 {
        return None;
    }
    let ihl = usize::from(buf[0] & 0x0f) * 4;
    if ihl < 20 || buf.len() < ihl {
        return None;
    }
    let tos = buf[1];
    let proto = buf[9];
    let src = IpAddr::V4(Ipv4Addr::new(buf[12], buf[13], buf[14], buf[15]));
    let dst = IpAddr::V4(Ipv4Addr::new(buf[16], buf[17], buf[18], buf[19]));
    let (src_port, dst_port) = l4_ports(proto, buf.get(ihl..).unwrap_or(&[]));
    Some((src, dst, src_port, dst_port, proto, tos))
}

/// Decode an IPv6 base header's 5-tuple contributions. Extension headers are not walked — if the
/// next header isn't a transport we recognise, ports are 0 (the src/dst/proto flow is still useful).
fn parse_ipv6_header(buf: &[u8]) -> Option<(IpAddr, IpAddr, u16, u16, u8, u8)> {
    if buf.len() < 40 {
        return None;
    }
    // Traffic class spans the low nibble of byte 0 and the high nibble of byte 1.
    let tclass = ((buf[0] & 0x0f) << 4) | (buf[1] >> 4);
    let next_header = buf[6];
    let mut src_o = [0u8; 16];
    let mut dst_o = [0u8; 16];
    src_o.copy_from_slice(&buf[8..24]);
    dst_o.copy_from_slice(&buf[24..40]);
    let src = IpAddr::V6(Ipv6Addr::from(src_o));
    let dst = IpAddr::V6(Ipv6Addr::from(dst_o));
    let (src_port, dst_port) = l4_ports(next_header, buf.get(40..).unwrap_or(&[]));
    Some((src, dst, src_port, dst_port, next_header, tclass))
}

/// TCP(6)/UDP(17) carry src/dst ports in the first 4 L4 bytes; any other protocol has no ports.
fn l4_ports(proto: u8, l4: &[u8]) -> (u16, u16) {
    if (proto == 6 || proto == 17) && l4.len() >= 4 {
        (
            u16::from_be_bytes([l4[0], l4[1]]),
            u16::from_be_bytes([l4[2], l4[3]]),
        )
    } else {
        (0, 0)
    }
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
    /// First non-zero source AS seen for this tuple (0 = still unknown). Latched — not keyed on —
    /// so an exporter that sets AS on only some records doesn't split one talker into two buckets.
    src_as: u32,
    /// First non-zero destination AS seen for this tuple (0 = unknown).
    dst_as: u32,
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
            // Latch the first non-zero AS seen (export-provided AS is authoritative; enrichment
            // fills the rest at write time in core).
            if c.src_as == 0 {
                c.src_as = f.src_as;
            }
            if c.dst_as == 0 {
                c.dst_as = f.dst_as;
            }
        } else if self.map.len() < self.key_cap {
            self.map.insert(
                key,
                Counts {
                    bytes: f.bytes,
                    packets: f.packets,
                    flows: 1,
                    src_as: f.src_as,
                    dst_as: f.dst_as,
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
                src_as: c.src_as,
                dst_as: c.dst_as,
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
            src_as: 0,
            dst_as: 0,
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
    fn aggregator_latches_first_nonzero_as_without_splitting() {
        // Same 5-tuple exported twice: once with AS unknown (0), once with AS set. Both must fold
        // into ONE bucket (AS is not part of the key), and the non-zero AS is latched.
        let mut agg = FlowAggregator::new(10);
        agg.add(raw(1, 1, 100)); // src_as/dst_as = 0
        let mut with_as = raw(1, 1, 200);
        with_as.src_as = 64500;
        with_as.dst_as = 15169;
        agg.add(with_as);
        let (out, _) = agg.drain_top_n();
        assert_eq!(out.len(), 1, "AS must not split the tuple into two buckets");
        assert_eq!(out[0].bytes, 300);
        assert_eq!(out[0].flows, 2);
        assert_eq!(out[0].src_as, 64500);
        assert_eq!(out[0].dst_as, 15169);
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
    fn netflow_v9_decodes_bgp_as() {
        // Template carrying SRC_AS (IE 16) + DST_AS (IE 17) as 4-byte fields; one data record.
        let exporter = v4(192, 168, 1, 1);
        let fields: [(u16, u16); 5] = [
            (IE_SRC_IPV4, 4),
            (IE_DST_IPV4, 4),
            (IE_SRC_AS, 4),
            (IE_DST_AS, 4),
            (IE_OCTET_DELTA, 4),
        ];
        let mut tmpl = Vec::new();
        tmpl.extend_from_slice(&256u16.to_be_bytes());
        tmpl.extend_from_slice(&(fields.len() as u16).to_be_bytes());
        for (ie, len) in fields {
            tmpl.extend_from_slice(&ie.to_be_bytes());
            tmpl.extend_from_slice(&len.to_be_bytes());
        }
        let mut tmpl_set = Vec::new();
        tmpl_set.extend_from_slice(&0u16.to_be_bytes());
        tmpl_set.extend_from_slice(&((4 + tmpl.len()) as u16).to_be_bytes());
        tmpl_set.extend_from_slice(&tmpl);

        let mut data = Vec::new();
        data.extend_from_slice(&Ipv4Addr::new(10, 0, 0, 5).octets());
        data.extend_from_slice(&Ipv4Addr::new(8, 8, 8, 8).octets());
        data.extend_from_slice(&64500u32.to_be_bytes()); // src_as
        data.extend_from_slice(&15169u32.to_be_bytes()); // dst_as (Google)
        data.extend_from_slice(&5000u32.to_be_bytes()); // bytes
        let mut data_set = Vec::new();
        data_set.extend_from_slice(&256u16.to_be_bytes());
        data_set.extend_from_slice(&((4 + data.len()) as u16).to_be_bytes());
        data_set.extend_from_slice(&data);

        let mut pkt = Vec::new();
        pkt.extend_from_slice(&9u16.to_be_bytes());
        pkt.extend_from_slice(&1u16.to_be_bytes());
        pkt.extend_from_slice(&[0u8; 12]); // uptime/secs/seq
        pkt.extend_from_slice(&7u32.to_be_bytes()); // domain
        pkt.extend_from_slice(&tmpl_set);
        pkt.extend_from_slice(&data_set);

        let mut templates = FlowTemplates::new();
        let flows = parse_flow_export(&mut templates, exporter, &pkt).unwrap();
        assert_eq!(flows.len(), 1);
        assert_eq!(flows[0].src_as, 64500);
        assert_eq!(flows[0].dst_as, 15169);
        assert_eq!(flows[0].bytes, 5000);
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
        // NetFlow v1 (version 1) is not decoded (v5/v9/IPFIX are).
        let pkt = [0u8, 1, 0, 0];
        assert_eq!(
            parse_flow_export(&mut t, v4(1, 1, 1, 1), &pkt),
            Err(FlowError::UnsupportedVersion(1))
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
            vec![0, 5, 0, 10, 0, 0, 0, 0], // v5, count 10, truncated before the 24-byte header
            vec![
                0, 5, 0, 5, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            ], // v5 full header claiming 5 records, zero record bytes
            (0..255u8).cycle().take(600).collect(),
        ];
        for c in cases {
            // Must return (Ok or Err) without panicking.
            let _ = parse_flow_export(&mut t, exporter, &c);
            // The forwarder's template probe walks the same hostile bytes.
            let _ = carries_template_set(&c);
        }
    }

    /// A NetFlow v9 packet carrying only a template FlowSet — the shape an exporter that refreshes
    /// templates on a timer emits, and the one a filtered forwarding destination must still receive.
    fn nf9_template_only(template_id: u16) -> Vec<u8> {
        let fields: [(u16, u16); 3] = [(IE_SRC_IPV4, 4), (IE_DST_IPV4, 4), (IE_OCTET_DELTA, 4)];
        let mut tmpl = Vec::new();
        tmpl.extend_from_slice(&template_id.to_be_bytes());
        tmpl.extend_from_slice(&(fields.len() as u16).to_be_bytes());
        for (ie, len) in fields {
            tmpl.extend_from_slice(&ie.to_be_bytes());
            tmpl.extend_from_slice(&len.to_be_bytes());
        }
        let mut set = Vec::new();
        set.extend_from_slice(&0u16.to_be_bytes()); // FlowSet id 0 = template
        set.extend_from_slice(&((4 + tmpl.len()) as u16).to_be_bytes());
        set.extend_from_slice(&tmpl);

        let mut pkt = Vec::new();
        pkt.extend_from_slice(&9u16.to_be_bytes());
        pkt.extend_from_slice(&1u16.to_be_bytes());
        pkt.extend_from_slice(&[0u8; 12]); // uptime/secs/seq
        pkt.extend_from_slice(&7u32.to_be_bytes()); // domain
        pkt.extend_from_slice(&set);
        pkt
    }

    #[test]
    fn carries_template_set_sees_templates_and_only_templates() {
        // Template-only: the datagram a filtered destination would otherwise never be sent.
        assert!(carries_template_set(&nf9_template_only(256)));
        // Template + data in one datagram (the shape an inline-template exporter emits).
        assert!(carries_template_set(&nf9_packet(
            256,
            &[(
                Ipv4Addr::new(10, 0, 0, 5),
                Ipv4Addr::new(8, 8, 8, 8),
                1,
                2,
                6,
                10,
                1
            )],
        )));
        // Data only — nothing to teach a collector, so the filter still decides.
        assert!(!carries_template_set(&nf9_orphan_data(256)));
        // v5 and sFlow have no templates at all.
        assert!(!carries_template_set(&nf5_packet(&[], 0)));
        assert!(!carries_template_set(&[0, 0, 0, 5, 0, 0, 0, 1]));
    }

    #[test]
    fn carries_template_set_handles_ipfix_and_options_templates() {
        // IPFIX set id 2 = template, 3 = options template; header is 16 bytes and declares length.
        let body: [u8; 8] = [0, 2, 0, 8, 1, 2, 3, 4]; // one template set, 8 bytes
        let mut pkt = Vec::new();
        pkt.extend_from_slice(&10u16.to_be_bytes());
        pkt.extend_from_slice(&((16 + body.len()) as u16).to_be_bytes());
        pkt.extend_from_slice(&[0u8; 12]); // export time + seq + domain
        pkt.extend_from_slice(&body);
        assert!(carries_template_set(&pkt));

        // Same packet with the set id changed to a data set: no templates.
        let mut data = pkt.clone();
        data[16] = 1;
        data[17] = 0; // set id 256
        assert!(!carries_template_set(&data));

        // A v9 options template (FlowSet id 1) also counts — it is still not flow data.
        let mut opts = nf9_template_only(256);
        opts[20] = 0;
        opts[21] = 1;
        assert!(carries_template_set(&opts));
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

    // ── NetFlow v5 ──

    /// One NetFlow v5 test record:
    /// (src, dst, sport, dport, proto, tos, input_if, dPkts, dOctets, src_as, dst_as).
    type Nf5Rec = (
        Ipv4Addr,
        Ipv4Addr,
        u16,
        u16,
        u8,
        u8,
        u16,
        u32,
        u32,
        u16,
        u16,
    );

    /// Build a NetFlow v5 datagram. `sampling` is the raw 16-bit header field (low 14 bits = interval).
    fn nf5_packet(records: &[Nf5Rec], sampling: u16) -> Vec<u8> {
        let mut pkt = Vec::new();
        pkt.extend_from_slice(&5u16.to_be_bytes()); // version
        pkt.extend_from_slice(&(records.len() as u16).to_be_bytes()); // count
        pkt.extend_from_slice(&0u32.to_be_bytes()); // sys_uptime
        pkt.extend_from_slice(&0u32.to_be_bytes()); // unix_secs
        pkt.extend_from_slice(&0u32.to_be_bytes()); // unix_nsecs
        pkt.extend_from_slice(&0u32.to_be_bytes()); // flow_sequence
        pkt.push(0); // engine_type
        pkt.push(0); // engine_id
        pkt.extend_from_slice(&sampling.to_be_bytes()); // sampling_interval
        for (src, dst, sp, dp, proto, tos, input, dpkts, doctets, src_as, dst_as) in records {
            pkt.extend_from_slice(&src.octets());
            pkt.extend_from_slice(&dst.octets());
            pkt.extend_from_slice(&[0u8; 4]); // nexthop
            pkt.extend_from_slice(&input.to_be_bytes()); // input
            pkt.extend_from_slice(&0u16.to_be_bytes()); // output
            pkt.extend_from_slice(&dpkts.to_be_bytes()); // dPkts
            pkt.extend_from_slice(&doctets.to_be_bytes()); // dOctets
            pkt.extend_from_slice(&0u32.to_be_bytes()); // first
            pkt.extend_from_slice(&0u32.to_be_bytes()); // last
            pkt.extend_from_slice(&sp.to_be_bytes()); // srcport
            pkt.extend_from_slice(&dp.to_be_bytes()); // dstport
            pkt.push(0); // pad1
            pkt.push(0); // tcp_flags
            pkt.push(*proto); // prot
            pkt.push(*tos); // tos
            pkt.extend_from_slice(&src_as.to_be_bytes()); // src_as
            pkt.extend_from_slice(&dst_as.to_be_bytes()); // dst_as
            pkt.push(0); // src_mask
            pkt.push(0); // dst_mask
            pkt.extend_from_slice(&0u16.to_be_bytes()); // pad2
        }
        pkt
    }

    #[test]
    fn netflow_v5_decodes_fixed_records() {
        let exporter = v4(192, 168, 1, 1);
        let recs = [
            (
                Ipv4Addr::new(10, 0, 0, 5),
                Ipv4Addr::new(8, 8, 8, 8),
                40000u16,
                443u16,
                6u8,
                0u8,
                3u16,
                10u32,
                5000u32,
                0u16,
                0u16,
            ),
            (
                Ipv4Addr::new(10, 0, 0, 6),
                Ipv4Addr::new(1, 1, 1, 1),
                40001u16,
                53u16,
                17u8,
                0u8,
                3u16,
                2u32,
                200u32,
                0u16,
                0u16,
            ),
        ];
        let pkt = nf5_packet(&recs, 0);
        let mut t = FlowTemplates::new();
        let flows = parse_flow_export(&mut t, exporter, &pkt).unwrap();
        assert_eq!(flows.len(), 2);
        assert_eq!(flows[0].src_ip, v4(10, 0, 0, 5));
        assert_eq!(flows[0].dst_ip, v4(8, 8, 8, 8));
        assert_eq!(flows[0].dst_port, 443);
        assert_eq!(flows[0].proto, 6);
        assert_eq!(flows[0].if_index, 3);
        assert_eq!(flows[0].bytes, 5000);
        assert_eq!(flows[0].packets, 10);
        assert_eq!(flows[1].proto, 17);
        assert_eq!(flows[1].bytes, 200);
        // v5 is fixed-format — no template cached.
        assert_eq!(t.len(), 0);
    }

    #[test]
    fn netflow_v5_applies_sampling_scale() {
        let exporter = v4(192, 168, 1, 1);
        let recs = [(
            Ipv4Addr::new(10, 0, 0, 5),
            Ipv4Addr::new(8, 8, 8, 8),
            1u16,
            2u16,
            6u8,
            0u8,
            1u16,
            4u32,
            1000u32,
            0u16,
            0u16,
        )];
        // Interval 100 in the low 14 bits (top 2 bits = mode, set here to exercise masking).
        let pkt = nf5_packet(&recs, 0xC000 | 100);
        let mut t = FlowTemplates::new();
        let flows = parse_flow_export(&mut t, exporter, &pkt).unwrap();
        assert_eq!(flows.len(), 1);
        assert_eq!(flows[0].bytes, 1000 * 100);
        assert_eq!(flows[0].packets, 4 * 100);
    }

    #[test]
    fn netflow_v5_decodes_as_trailer() {
        let exporter = v4(192, 168, 1, 1);
        let recs = [(
            Ipv4Addr::new(10, 0, 0, 5),
            Ipv4Addr::new(8, 8, 8, 8),
            40000u16,
            443u16,
            6u8,
            0u8,
            3u16,
            10u32,
            5000u32,
            64500u16, // src_as
            15169u16, // dst_as (Google)
        )];
        let pkt = nf5_packet(&recs, 0);
        let mut t = FlowTemplates::new();
        let flows = parse_flow_export(&mut t, exporter, &pkt).unwrap();
        assert_eq!(flows.len(), 1);
        assert_eq!(flows[0].src_as, 64500);
        assert_eq!(flows[0].dst_as, 15169);
    }

    // ── sFlow v5 ──

    /// A 20-byte IPv4 header + 4 TCP port bytes (+ padding of the TCP header).
    fn ipv4_tcp(src: Ipv4Addr, dst: Ipv4Addr, sport: u16, dport: u16) -> Vec<u8> {
        let mut h = Vec::new();
        h.push(0x45); // version 4, IHL 5
        h.push(0x00); // tos
        h.extend_from_slice(&40u16.to_be_bytes()); // total length (unused)
        h.extend_from_slice(&0u16.to_be_bytes()); // id
        h.extend_from_slice(&0u16.to_be_bytes()); // flags/frag
        h.push(64); // ttl
        h.push(6); // proto = TCP
        h.extend_from_slice(&0u16.to_be_bytes()); // checksum
        h.extend_from_slice(&src.octets());
        h.extend_from_slice(&dst.octets());
        h.extend_from_slice(&sport.to_be_bytes());
        h.extend_from_slice(&dport.to_be_bytes());
        h.extend_from_slice(&[0u8; 12]); // rest of the TCP header
        h
    }

    /// A 40-byte IPv6 header + 4 UDP port bytes.
    fn ipv6_udp(src: Ipv6Addr, dst: Ipv6Addr, sport: u16, dport: u16) -> Vec<u8> {
        let mut h = Vec::new();
        h.push(0x60); // version 6, traffic-class hi nibble 0
        h.push(0x00); // traffic-class lo / flow label
        h.extend_from_slice(&0u16.to_be_bytes()); // flow label
        h.extend_from_slice(&16u16.to_be_bytes()); // payload length
        h.push(17); // next header = UDP
        h.push(64); // hop limit
        h.extend_from_slice(&src.octets());
        h.extend_from_slice(&dst.octets());
        h.extend_from_slice(&sport.to_be_bytes());
        h.extend_from_slice(&dport.to_be_bytes());
        h.extend_from_slice(&[0u8; 4]); // udp length + checksum
        h
    }

    /// Wrap an L3 payload in an Ethernet II header, inserting `vlan_tags` 802.1Q tags.
    fn eth(ethertype: u16, payload: &[u8], vlan_tags: usize) -> Vec<u8> {
        let mut h = Vec::new();
        h.extend_from_slice(&[0x00, 0x11, 0x22, 0x33, 0x44, 0x55]); // dst MAC
        h.extend_from_slice(&[0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb]); // src MAC
        for _ in 0..vlan_tags {
            h.extend_from_slice(&0x8100u16.to_be_bytes()); // TPID
            h.extend_from_slice(&0x000au16.to_be_bytes()); // TCI (VLAN 10)
        }
        h.extend_from_slice(&ethertype.to_be_bytes());
        h.extend_from_slice(payload);
        h
    }

    /// Build an sFlow v5 datagram carrying one flow sample with one raw-packet-header record.
    fn sflow_datagram(
        sample_type: u32,
        expanded: bool,
        sampling_rate: u32,
        input_if: u32,
        header_protocol: u32,
        header: &[u8],
        frame_length: u32,
    ) -> Vec<u8> {
        // Raw-packet-header flow record body.
        let mut rec_body = Vec::new();
        rec_body.extend_from_slice(&header_protocol.to_be_bytes());
        rec_body.extend_from_slice(&frame_length.to_be_bytes());
        rec_body.extend_from_slice(&0u32.to_be_bytes()); // stripped
        rec_body.extend_from_slice(&(header.len() as u32).to_be_bytes());
        rec_body.extend_from_slice(header);
        while rec_body.len() % 4 != 0 {
            rec_body.push(0); // pad to 4-byte boundary
        }
        let mut record = Vec::new();
        record.extend_from_slice(&1u32.to_be_bytes()); // flow_format = raw packet header
        record.extend_from_slice(&(rec_body.len() as u32).to_be_bytes());
        record.extend_from_slice(&rec_body);

        // Flow sample body.
        let mut sample = Vec::new();
        sample.extend_from_slice(&0u32.to_be_bytes()); // sequence_number
        if expanded {
            sample.extend_from_slice(&0u32.to_be_bytes()); // source_id type
            sample.extend_from_slice(&0u32.to_be_bytes()); // source_id index
        } else {
            sample.extend_from_slice(&0u32.to_be_bytes()); // source_id
        }
        sample.extend_from_slice(&sampling_rate.to_be_bytes());
        sample.extend_from_slice(&0u32.to_be_bytes()); // sample_pool
        sample.extend_from_slice(&0u32.to_be_bytes()); // drops
        if expanded {
            sample.extend_from_slice(&0u32.to_be_bytes()); // input_format
            sample.extend_from_slice(&input_if.to_be_bytes()); // input_value
            sample.extend_from_slice(&0u32.to_be_bytes()); // output_format
            sample.extend_from_slice(&0u32.to_be_bytes()); // output_value
        } else {
            sample.extend_from_slice(&input_if.to_be_bytes()); // input
            sample.extend_from_slice(&0u32.to_be_bytes()); // output
        }
        sample.extend_from_slice(&1u32.to_be_bytes()); // num_records
        sample.extend_from_slice(&record);

        // Datagram header + one sample.
        let mut pkt = Vec::new();
        pkt.extend_from_slice(&5u32.to_be_bytes()); // version
        pkt.extend_from_slice(&1u32.to_be_bytes()); // agent addr type = IPv4
        pkt.extend_from_slice(&[192, 168, 1, 1]); // agent address
        pkt.extend_from_slice(&0u32.to_be_bytes()); // sub_agent_id
        pkt.extend_from_slice(&0u32.to_be_bytes()); // sequence_number
        pkt.extend_from_slice(&0u32.to_be_bytes()); // uptime
        pkt.extend_from_slice(&1u32.to_be_bytes()); // num_samples
        pkt.extend_from_slice(&sample_type.to_be_bytes());
        pkt.extend_from_slice(&(sample.len() as u32).to_be_bytes());
        pkt.extend_from_slice(&sample);
        pkt
    }

    #[test]
    fn sflow_flow_sample_decodes_and_scales() {
        let header = eth(
            0x0800,
            &ipv4_tcp(
                Ipv4Addr::new(10, 0, 0, 5),
                Ipv4Addr::new(8, 8, 8, 8),
                12345,
                443,
            ),
            0,
        );
        let pkt = sflow_datagram(SFLOW_FLOW_SAMPLE, false, 1000, 7, 1, &header, 1500);
        let flows = parse_sflow(&pkt).unwrap();
        assert_eq!(flows.len(), 1);
        assert_eq!(flows[0].src_ip, v4(10, 0, 0, 5));
        assert_eq!(flows[0].dst_ip, v4(8, 8, 8, 8));
        assert_eq!(flows[0].src_port, 12345);
        assert_eq!(flows[0].dst_port, 443);
        assert_eq!(flows[0].proto, 6);
        assert_eq!(flows[0].if_index, 7);
        // Scaled by the 1-in-1000 sampling rate.
        assert_eq!(flows[0].bytes, 1500 * 1000);
        assert_eq!(flows[0].packets, 1000);
    }

    #[test]
    fn sflow_expanded_flow_sample_decodes() {
        let header = eth(
            0x0800,
            &ipv4_tcp(
                Ipv4Addr::new(172, 16, 0, 9),
                Ipv4Addr::new(1, 1, 1, 1),
                5555,
                53,
            ),
            0,
        );
        let pkt = sflow_datagram(SFLOW_FLOW_SAMPLE_EXPANDED, true, 512, 42, 1, &header, 590);
        let flows = parse_sflow(&pkt).unwrap();
        assert_eq!(flows.len(), 1);
        assert_eq!(flows[0].src_ip, v4(172, 16, 0, 9));
        assert_eq!(flows[0].dst_port, 53);
        assert_eq!(flows[0].if_index, 42);
        assert_eq!(flows[0].bytes, 590 * 512);
        assert_eq!(flows[0].packets, 512);
    }

    #[test]
    fn sflow_decodes_vlan_tagged_and_ipv6() {
        // 802.1Q-tagged IPv4/TCP.
        let vlan = eth(
            0x0800,
            &ipv4_tcp(
                Ipv4Addr::new(10, 1, 2, 3),
                Ipv4Addr::new(10, 4, 5, 6),
                1000,
                80,
            ),
            1,
        );
        let pkt = sflow_datagram(SFLOW_FLOW_SAMPLE, false, 100, 1, 1, &vlan, 200);
        let flows = parse_sflow(&pkt).unwrap();
        assert_eq!(flows.len(), 1);
        assert_eq!(flows[0].src_ip, v4(10, 1, 2, 3));
        assert_eq!(flows[0].dst_port, 80);

        // IPv6/UDP inside Ethernet.
        let src6 = Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1);
        let dst6 = Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 2);
        let v6 = eth(0x86DD, &ipv6_udp(src6, dst6, 4444, 123), 0);
        let pkt6 = sflow_datagram(SFLOW_FLOW_SAMPLE, false, 100, 1, 1, &v6, 300);
        let flows6 = parse_sflow(&pkt6).unwrap();
        assert_eq!(flows6.len(), 1);
        assert_eq!(flows6[0].src_ip, IpAddr::V6(src6));
        assert_eq!(flows6[0].dst_ip, IpAddr::V6(dst6));
        assert_eq!(flows6[0].dst_port, 123);
        assert_eq!(flows6[0].proto, 17);
    }

    /// Build a complete sFlow flow-record TLV (format + length + padded body).
    fn sflow_record(flow_format: u32, body: &[u8]) -> Vec<u8> {
        let mut b = body.to_vec();
        while !b.len().is_multiple_of(4) {
            b.push(0);
        }
        let mut rec = Vec::new();
        rec.extend_from_slice(&flow_format.to_be_bytes());
        rec.extend_from_slice(&(b.len() as u32).to_be_bytes());
        rec.extend_from_slice(&b);
        rec
    }

    /// A raw-packet-header (format 1) record body.
    fn sflow_raw_header_body(header_protocol: u32, header: &[u8], frame_length: u32) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(&header_protocol.to_be_bytes());
        body.extend_from_slice(&frame_length.to_be_bytes());
        body.extend_from_slice(&0u32.to_be_bytes()); // stripped
        body.extend_from_slice(&(header.len() as u32).to_be_bytes());
        body.extend_from_slice(header);
        body
    }

    /// An extended_gateway (format 1003) record body: IPv4 next-hop, `src_as`, and a single
    /// AS_SEQUENCE dst path (origin AS = the last element).
    fn sflow_gateway_body(src_as: u32, dst_as_path: &[u32]) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(&1u32.to_be_bytes()); // next_hop address type = IPv4
        body.extend_from_slice(&[10, 0, 0, 254]); // next_hop address
        body.extend_from_slice(&64500u32.to_be_bytes()); // as (agent's own)
        body.extend_from_slice(&src_as.to_be_bytes()); // src_as
        body.extend_from_slice(&64500u32.to_be_bytes()); // src_peer_as
        body.extend_from_slice(&1u32.to_be_bytes()); // dst_as_path segment count
        body.extend_from_slice(&2u32.to_be_bytes()); // segment type = AS_SEQUENCE
        body.extend_from_slice(&(dst_as_path.len() as u32).to_be_bytes()); // segment length
        for asn in dst_as_path {
            body.extend_from_slice(&asn.to_be_bytes());
        }
        body
    }

    /// Wrap flow-record TLVs into a compact flow sample, then into a v5 datagram.
    fn sflow_datagram_records(sampling_rate: u32, input_if: u32, records: &[Vec<u8>]) -> Vec<u8> {
        let mut sample = Vec::new();
        sample.extend_from_slice(&0u32.to_be_bytes()); // sequence_number
        sample.extend_from_slice(&0u32.to_be_bytes()); // source_id
        sample.extend_from_slice(&sampling_rate.to_be_bytes());
        sample.extend_from_slice(&0u32.to_be_bytes()); // sample_pool
        sample.extend_from_slice(&0u32.to_be_bytes()); // drops
        sample.extend_from_slice(&input_if.to_be_bytes()); // input
        sample.extend_from_slice(&0u32.to_be_bytes()); // output
        sample.extend_from_slice(&(records.len() as u32).to_be_bytes()); // num_records
        for rec in records {
            sample.extend_from_slice(rec);
        }
        let mut pkt = Vec::new();
        pkt.extend_from_slice(&5u32.to_be_bytes()); // version
        pkt.extend_from_slice(&1u32.to_be_bytes()); // agent addr type = IPv4
        pkt.extend_from_slice(&[192, 168, 1, 1]); // agent address
        pkt.extend_from_slice(&0u32.to_be_bytes()); // sub_agent_id
        pkt.extend_from_slice(&0u32.to_be_bytes()); // sequence_number
        pkt.extend_from_slice(&0u32.to_be_bytes()); // uptime
        pkt.extend_from_slice(&1u32.to_be_bytes()); // num_samples
        pkt.extend_from_slice(&SFLOW_FLOW_SAMPLE.to_be_bytes());
        pkt.extend_from_slice(&(sample.len() as u32).to_be_bytes());
        pkt.extend_from_slice(&sample);
        pkt
    }

    #[test]
    fn sflow_extended_gateway_supplies_as() {
        // A flow sample carrying both a raw packet header and an extended_gateway record: the AS
        // pair from the gateway record must be stamped onto the raw-header flow (same sample).
        let header = eth(
            0x0800,
            &ipv4_tcp(
                Ipv4Addr::new(10, 0, 0, 5),
                Ipv4Addr::new(8, 8, 8, 8),
                12345,
                443,
            ),
            0,
        );
        let raw_rec = sflow_record(1, &sflow_raw_header_body(1, &header, 1500));
        // AS_SEQUENCE 174 → 15169; origin (last) AS = 15169.
        let gw_rec = sflow_record(
            SFLOW_EXTENDED_GATEWAY,
            &sflow_gateway_body(64500, &[174, 15169]),
        );
        // Gateway record BEFORE the raw header — ordering must not matter.
        let pkt = sflow_datagram_records(1000, 7, &[gw_rec, raw_rec]);
        let flows = parse_sflow(&pkt).unwrap();
        assert_eq!(flows.len(), 1);
        assert_eq!(flows[0].src_ip, v4(10, 0, 0, 5));
        assert_eq!(flows[0].dst_port, 443);
        assert_eq!(flows[0].src_as, 64500);
        assert_eq!(flows[0].dst_as, 15169); // origin AS from the path
        assert_eq!(flows[0].bytes, 1500 * 1000);
    }

    #[test]
    fn sflow_counter_sample_is_skipped() {
        // A counter sample (format 2) is skipped by length — no flows, no error.
        let header = eth(
            0x0800,
            &ipv4_tcp(Ipv4Addr::new(10, 0, 0, 1), Ipv4Addr::new(10, 0, 0, 2), 1, 2),
            0,
        );
        let pkt = sflow_datagram(2, false, 1000, 1, 1, &header, 100);
        let flows = parse_sflow(&pkt).unwrap();
        assert!(flows.is_empty());
    }

    #[test]
    fn sflow_wrong_version_errs() {
        let pkt = [0u8, 0, 0, 4, 0, 0, 0, 1]; // version 4
        assert_eq!(parse_sflow(&pkt), Err(FlowError::UnsupportedVersion(4)));
    }

    #[test]
    fn sflow_hostile_inputs_never_panic() {
        let cases: Vec<Vec<u8>> = vec![
            vec![],
            vec![0, 0, 0, 5],                   // version only
            vec![0, 0, 0, 5, 0, 0, 0, 9],       // unknown agent addr type
            vec![0, 0, 0, 5, 0, 0, 0, 1, 1, 2], // v4 agent type, truncated address
            {
                // Valid header claiming a huge sample with a huge length.
                let mut v = vec![
                    0, 0, 0, 5, // version
                    0, 0, 0, 1, // agent type v4
                    192, 168, 1, 1, // agent ip
                    0, 0, 0, 0, // sub agent
                    0, 0, 0, 0, // seq
                    0, 0, 0, 0, // uptime
                    0xff, 0xff, 0xff, 0xff, // num_samples (huge)
                ];
                v.extend_from_slice(&[0, 0, 0, 1, 0xff, 0xff, 0xff, 0xff]); // sample type 1, len huge
                v
            },
            {
                // A flow sample whose extended_gateway record claims a huge AS-path segment count
                // and segment length — the bounded decoder must not spin or panic.
                let mut gw = Vec::new();
                gw.extend_from_slice(&1u32.to_be_bytes()); // next_hop type v4
                gw.extend_from_slice(&[10, 0, 0, 1]);
                gw.extend_from_slice(&0u32.to_be_bytes()); // as
                gw.extend_from_slice(&0u32.to_be_bytes()); // src_as
                gw.extend_from_slice(&0u32.to_be_bytes()); // src_peer_as
                gw.extend_from_slice(&0xffff_ffffu32.to_be_bytes()); // segment count (huge)
                gw.extend_from_slice(&2u32.to_be_bytes()); // seg type
                gw.extend_from_slice(&0xffff_ffffu32.to_be_bytes()); // seg len (huge)
                let rec = sflow_record(SFLOW_EXTENDED_GATEWAY, &gw);
                sflow_datagram_records(1000, 1, &[rec])
            },
            (0..255u8).cycle().take(500).collect(),
        ];
        for c in cases {
            let _ = parse_sflow(&c);
        }
    }
}
