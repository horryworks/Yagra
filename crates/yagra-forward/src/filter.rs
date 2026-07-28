// SPDX-License-Identifier: AGPL-3.0-only
//! Per-destination forwarding filters (ADR-034).
//!
//! A destination forwards only what its filter accepts. The expression is a flat list of typed
//! conditions combined with `all` (AND) or `any` (OR) — deliberately not an expression language:
//! it has to be renderable as a form in the WebUI, cheap enough to evaluate on every received
//! datagram, and impossible to turn into a denial of service. **An empty condition list matches
//! everything**, so a destination with no filter is a plain tee.
//!
//! [`compile`] does all the parsing and validation up front — CIDRs, integers and regexes are
//! resolved once per config generation, never per message — and rejects an operator that does not
//! typecheck against its field (`severity regex …`). Core calls it at the API edge so a bad filter
//! is a 400 rather than a destination that silently drops everything. Patterns keep the same
//! bounds as the event-rule matcher (≤512 chars, 1 MiB compiled regex); `regex` is linear-time, so
//! an operator pattern cannot ReDoS the forwarding path.
//!
//! Text comparisons are **case-sensitive**, matching the event-rule matcher.
//!
//! Absent fields resolve so that negative operators stay intuitive: a message with no hostname
//! does *not* satisfy `hostname contains X`, and *does* satisfy `hostname not_contains X`.

use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};
use std::net::IpAddr;
use thiserror::Error;
use yagra_bus::{EventKind, EventMsg};

/// Longest accepted condition value / pattern, in characters (mirrors `event_rules`).
pub const MAX_PATTERN_CHARS: usize = 512;
/// Most conditions one filter may hold. Bounds per-message evaluation cost.
pub const MAX_CONDITIONS: usize = 32;
/// Compiled-regex size cap (mirrors the event-rule matcher).
const REGEX_SIZE_LIMIT: usize = 1 << 20;

// ── Config model (serialized into `forward_destinations.filter`, edited by the WebUI) ────────

/// How the conditions combine.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FilterMode {
    /// Every condition must hold (AND). The default.
    #[default]
    All,
    /// At least one condition must hold (OR).
    Any,
}

/// Which datum a condition inspects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FilterField {
    /// Datagram source address.
    SourceIp,
    /// Poller pool that received the event.
    Pool,
    /// `syslog` / `trap` / `webhook`.
    Kind,
    /// Syslog facility decoded from PRI (0–23).
    Facility,
    /// Syslog severity decoded from PRI (0 = emerg … 7 = debug — **lower is worse**).
    Severity,
    /// Syslog HOSTNAME field.
    Hostname,
    /// Syslog APP-NAME / TAG field.
    AppName,
    /// The normalized message text.
    Message,
    /// The trap identity OID.
    TrapOid,
    /// Trap varbinds, each tested as `"<oid>=<value>"`.
    Varbind,
    /// Flow record source address.
    SrcAddr,
    /// Flow record destination address.
    DstAddr,
    /// Flow record IP protocol number. Accepts a name (`tcp`, `udp`, `icmp`, …) as well as a number.
    Proto,
    /// Flow record source transport port.
    SrcPort,
    /// Flow record destination transport port.
    DstPort,
    /// Flow record source autonomous-system number.
    SrcAs,
    /// Flow record destination autonomous-system number.
    DstAs,
}

/// What a condition does with the datum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FilterOp {
    /// Exact equality.
    Eq,
    /// Exact inequality.
    Ne,
    /// Substring containment.
    Contains,
    /// Absence of a substring.
    NotContains,
    /// Leading-substring match.
    Prefix,
    /// Regular-expression match.
    Regex,
    /// Absence of a regular-expression match.
    NotRegex,
    /// Membership in a comma-separated list.
    InList,
    /// Address inside any of the comma-separated CIDRs.
    InCidr,
    /// Address outside every one of the comma-separated CIDRs.
    NotInCidr,
    /// Numerically less than or equal.
    Lte,
    /// Numerically greater than or equal.
    Gte,
}

/// One `field op value` test. `value` is always a string so the config has a single JSON shape;
/// [`compile`] parses it according to the field's type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Condition {
    /// The datum to inspect.
    pub field: FilterField,
    /// The comparison to apply.
    pub op: FilterOp,
    /// The operand, parsed per field type at compile time.
    #[serde(default)]
    pub value: String,
}

/// A destination's filter as stored and edited. `{}` deserializes to "match everything".
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilterExpr {
    /// How [`Self::conditions`] combine.
    #[serde(default)]
    pub mode: FilterMode,
    /// The conditions; empty means match everything.
    #[serde(default)]
    pub conditions: Vec<Condition>,
}

/// Which received stream a destination tees. Kept here (rather than in core) so the DB `CHECK`
/// strings, the API validation and the WebUI all agree on one spelling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    /// Received syslog.
    Syslog,
    /// Received SNMP traps and informs.
    Trap,
    /// Received flow exports (NetFlow v5/v9, IPFIX, sFlow), relayed as whole datagrams.
    Flow,
}

/// How a destination is spoken to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DestKind {
    /// RFC 5424 over UDP.
    SyslogUdp,
    /// RFC 5424 over TCP with RFC 6587 octet-counting framing.
    SyslogTcp,
    /// RFC 5425 syslog over TLS (octet-counting framing inside the TLS session). The only
    /// destination kind that protects the message in transit — syslog bodies routinely carry
    /// credentials, and forwarding sends them off-box.
    SyslogTls,
    /// SNMPv2c trap over UDP.
    SnmpTrapUdp,
    /// Flow-export datagrams relayed unchanged over UDP to a collector.
    FlowUdp,
    /// Normalized rows streamed into a Google BigQuery table (ADR-034 Increment 3). The odd one
    /// out: every other kind reproduces a datagram, this one produces a **structured row per
    /// event or per flow record**. That is what makes exact per-record flow filtering possible —
    /// rows are independent, so non-matching records are simply not written, where a relayed
    /// datagram has to carry its whole bundle.
    ///
    /// Renamed explicitly: `snake_case` would derive `big_query`, but Google spells the product as
    /// one word everywhere (`bigquery.googleapis.com`), and this tag is what the DB `CHECK`, the
    /// API and the WebUI all agree on.
    #[serde(rename = "bigquery")]
    BigQuery,
}

/// Errors compiling a filter. Surfaced to the API caller as `400 invalid_filter` with this text,
/// so the messages name the offending field/value rather than describing internals.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum FilterError {
    /// More conditions than [`MAX_CONDITIONS`].
    #[error("too many conditions: {0} (max 32)")]
    TooManyConditions(usize),
    /// The operator does not typecheck against the field.
    #[error("operator `{op}` cannot be used with field `{field}`")]
    OpNotAllowed {
        /// The field named in the condition.
        field: &'static str,
        /// The operator named in the condition.
        op: &'static str,
    },
    /// The operand was blank.
    #[error("field `{0}` needs a value")]
    EmptyValue(&'static str),
    /// The operand exceeded [`MAX_PATTERN_CHARS`].
    #[error("value for field `{field}` is too long: {chars} characters (max 512)")]
    ValueTooLong {
        /// The field named in the condition.
        field: &'static str,
        /// The operand's length in characters.
        chars: usize,
    },
    /// The operand was not a valid regular expression.
    #[error("invalid regular expression: {0}")]
    BadRegex(String),
    /// The operand was not a valid address or CIDR.
    #[error("invalid address or CIDR: {0}")]
    BadCidr(String),
    /// The operand was not an integer.
    #[error("value must be a whole number: {0}")]
    BadNumber(String),
}

// ── Type discipline: which operators a field accepts ─────────────────────────────────────────

/// The shape of a field's datum, which decides the operators it accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ValueType {
    Text,
    Num,
    Ip,
    /// A list of strings where a positive operator means "any element matches" and a negative
    /// operator means "no element matches".
    Multi,
}

impl FilterField {
    /// Stable string form (matches the serde tag and the WebUI's field ids).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SourceIp => "source_ip",
            Self::Pool => "pool",
            Self::Kind => "kind",
            Self::Facility => "facility",
            Self::Severity => "severity",
            Self::Hostname => "hostname",
            Self::AppName => "app_name",
            Self::Message => "message",
            Self::TrapOid => "trap_oid",
            Self::Varbind => "varbind",
            Self::SrcAddr => "src_addr",
            Self::DstAddr => "dst_addr",
            Self::Proto => "proto",
            Self::SrcPort => "src_port",
            Self::DstPort => "dst_port",
            Self::SrcAs => "src_as",
            Self::DstAs => "dst_as",
        }
    }

    const fn value_type(self) -> ValueType {
        match self {
            Self::SourceIp | Self::SrcAddr | Self::DstAddr => ValueType::Ip,
            Self::Facility
            | Self::Severity
            | Self::Proto
            | Self::SrcPort
            | Self::DstPort
            | Self::SrcAs
            | Self::DstAs => ValueType::Num,
            Self::Varbind => ValueType::Multi,
            Self::Pool
            | Self::Kind
            | Self::Hostname
            | Self::AppName
            | Self::Message
            | Self::TrapOid => ValueType::Text,
        }
    }

    /// Whether this field can ever carry a value for `source`. Core rejects a condition that never
    /// could match — `facility` on a trap destination is a configuration mistake, not a filter that
    /// happens to drop everything.
    #[must_use]
    pub const fn applies_to(self, source: SourceKind) -> bool {
        match self {
            Self::Facility | Self::Severity | Self::Hostname | Self::AppName => {
                matches!(source, SourceKind::Syslog)
            }
            Self::TrapOid | Self::Varbind => matches!(source, SourceKind::Trap),
            Self::Message => matches!(source, SourceKind::Syslog | SourceKind::Trap),
            Self::SrcAddr
            | Self::DstAddr
            | Self::Proto
            | Self::SrcPort
            | Self::DstPort
            | Self::SrcAs
            | Self::DstAs => matches!(source, SourceKind::Flow),
            // `source_ip` is the datagram source throughout — for flow that is the exporter.
            // `kind` is the wire kind: syslog/webhook, trap, or netflow/sflow.
            Self::SourceIp | Self::Pool | Self::Kind => true,
        }
    }
}

impl FilterOp {
    /// Stable string form (matches the serde tag).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Eq => "eq",
            Self::Ne => "ne",
            Self::Contains => "contains",
            Self::NotContains => "not_contains",
            Self::Prefix => "prefix",
            Self::Regex => "regex",
            Self::NotRegex => "not_regex",
            Self::InList => "in_list",
            Self::InCidr => "in_cidr",
            Self::NotInCidr => "not_in_cidr",
            Self::Lte => "lte",
            Self::Gte => "gte",
        }
    }
}

impl SourceKind {
    /// Stable string form (matches the serde tag and the `source_kind` DB column).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Syslog => "syslog",
            Self::Trap => "trap",
            Self::Flow => "flow",
        }
    }

    /// Parse the DB / API spelling. Named `parse` rather than `from_str` so it does not pretend to
    /// be `FromStr` (the failure mode here is "unknown value", with no error type worth carrying).
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "syslog" => Some(Self::Syslog),
            "trap" => Some(Self::Trap),
            "flow" => Some(Self::Flow),
            _ => None,
        }
    }

    /// Whether an event of `kind` belongs to this stream. Flow never arrives as an `EventMsg` — it
    /// rides its own subject — so a flow destination is fed nothing from the event path.
    #[must_use]
    pub const fn accepts(self, kind: EventKind) -> bool {
        match self {
            Self::Syslog => matches!(kind, EventKind::Syslog | EventKind::Webhook),
            Self::Trap => matches!(kind, EventKind::Trap),
            Self::Flow => false,
        }
    }
}

impl DestKind {
    /// Stable string form (matches the serde tag and the `dest_kind` DB column).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SyslogUdp => "syslog_udp",
            Self::SyslogTcp => "syslog_tcp",
            Self::SyslogTls => "syslog_tls",
            Self::SnmpTrapUdp => "snmp_trap_udp",
            Self::FlowUdp => "flow_udp",
            Self::BigQuery => "bigquery",
        }
    }

    /// Parse the DB / API spelling. See [`SourceKind::parse`] for why it is not `from_str`.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "syslog_udp" => Some(Self::SyslogUdp),
            "syslog_tcp" => Some(Self::SyslogTcp),
            "syslog_tls" => Some(Self::SyslogTls),
            "snmp_trap_udp" => Some(Self::SnmpTrapUdp),
            "flow_udp" => Some(Self::FlowUdp),
            "bigquery" => Some(Self::BigQuery),
            _ => None,
        }
    }

    /// Whether this destination kind takes an operator-supplied CA certificate.
    ///
    /// BigQuery is HTTPS and therefore encrypted, but it answers `false`: it talks to a fixed
    /// Google endpoint with a public certificate, so the container's trust store is the right (and
    /// only sensible) answer and there is nothing for an operator to pin.
    #[must_use]
    pub const fn is_tls(self) -> bool {
        matches!(self, Self::SyslogTls)
    }

    /// Whether the destination is addressed as `host:port`. BigQuery is not — its target names a
    /// table (`project.dataset.table`) and its endpoint is fixed.
    #[must_use]
    pub const fn is_host_port(self) -> bool {
        !matches!(self, Self::BigQuery)
    }

    /// Whether `source` can be delivered to this destination kind. Traps may be shipped to a syslog
    /// collector (rendered as a syslog line), but syslog cannot be shipped as an SNMP trap — there
    /// is no sensible PDU to build from a log line. Flow is its own world: a flow export is only
    /// meaningful to a flow collector, and nothing else can be turned into one.
    #[must_use]
    pub const fn accepts(self, source: SourceKind) -> bool {
        match self {
            Self::SyslogUdp | Self::SyslogTcp | Self::SyslogTls => {
                matches!(source, SourceKind::Syslog | SourceKind::Trap)
            }
            Self::SnmpTrapUdp => matches!(source, SourceKind::Trap),
            Self::FlowUdp => matches!(source, SourceKind::Flow),
            // A table takes rows, and every stream has a row shape (see `crate::bqrow`).
            Self::BigQuery => true,
        }
    }

    /// Whether the original datagram can be relayed byte-for-byte to this destination kind for
    /// `source`. A trap PDU put on a syslog collector's port would be undecodable, so that pairing
    /// is always rendered; core rejects `verbatim` on it at the API edge rather than silently
    /// ignoring the flag.
    #[must_use]
    pub const fn supports_verbatim(self, source: SourceKind) -> bool {
        match self {
            Self::SyslogUdp | Self::SyslogTcp | Self::SyslogTls => {
                matches!(source, SourceKind::Syslog)
            }
            Self::SnmpTrapUdp => matches!(source, SourceKind::Trap),
            Self::FlowUdp => matches!(source, SourceKind::Flow),
            // A table column cannot hold "the original datagram" in any useful sense, and a
            // raw-payload column is exactly what a BigQuery destination is designed not to be.
            Self::BigQuery => false,
        }
    }

    /// Whether a **derived** form exists for `source` on this destination kind — the `verbatim =
    /// false` half of the fidelity choice. For the relay kinds that means a re-rendered datagram;
    /// for BigQuery it means a normalized row, which exists for every stream.
    ///
    /// `flow_udp` has neither: a NetFlow/IPFIX export is a template-bound binary bundle, and
    /// re-encoding one from decoded records would be a different datagram, not a rendering of the
    /// same one. So a flow *relay* is byte-exact or nothing, and core rejects `verbatim = false` on
    /// it — while the same flow stream sent to BigQuery is rows-only.
    #[must_use]
    pub const fn supports_rendered(self, source: SourceKind) -> bool {
        match self {
            Self::SyslogUdp | Self::SyslogTcp | Self::SyslogTls | Self::SnmpTrapUdp => {
                !matches!(source, SourceKind::Flow)
            }
            Self::FlowUdp => false,
            Self::BigQuery => true,
        }
    }
}

// ── The datum a filter sees ──────────────────────────────────────────────────────────────────

/// The borrowed view a compiled filter evaluates against. Built from an [`EventMsg`] via `From`, or
/// from one decoded flow record via [`FilterView::for_flow`]; keeping it separate means filter logic
/// can be unit-tested without constructing bus messages, and lets both streams share one evaluator.
#[derive(Debug, Clone, Default)]
pub struct FilterView<'a> {
    /// Datagram source address — for flow, the exporting device.
    pub source_ip: Option<IpAddr>,
    /// Poller pool that received the event.
    pub pool: Option<&'a str>,
    /// `syslog` / `trap` / `webhook`, or `netflow` / `sflow` for flow.
    pub kind: Option<&'a str>,
    /// Syslog facility.
    pub facility: Option<i64>,
    /// Syslog severity.
    pub severity: Option<i64>,
    /// Syslog HOSTNAME.
    pub hostname: Option<&'a str>,
    /// Syslog APP-NAME / TAG.
    pub app_name: Option<&'a str>,
    /// Normalized message text.
    pub message: Option<&'a str>,
    /// Trap identity OID.
    pub trap_oid: Option<&'a str>,
    /// Trap varbinds as `(oid, rendered value)`.
    pub varbinds: &'a [(String, String)],
    /// Flow record source address.
    pub src_addr: Option<IpAddr>,
    /// Flow record destination address.
    pub dst_addr: Option<IpAddr>,
    /// Flow record IP protocol number.
    pub proto: Option<i64>,
    /// Flow record source transport port.
    pub src_port: Option<i64>,
    /// Flow record destination transport port.
    pub dst_port: Option<i64>,
    /// Flow record source AS number.
    pub src_as: Option<i64>,
    /// Flow record destination AS number.
    pub dst_as: Option<i64>,
}

/// One decoded flow record's filterable fields. A plain data struct rather than a borrow of the
/// parser's type so this crate stays free of `yagra-ingest` (and testable on hand-built records).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FlowFields {
    /// Source address.
    pub src_addr: IpAddr,
    /// Destination address.
    pub dst_addr: IpAddr,
    /// IP protocol number.
    pub proto: u8,
    /// Source transport port.
    pub src_port: u16,
    /// Destination transport port.
    pub dst_port: u16,
    /// Source AS number (0 = unknown).
    pub src_as: u32,
    /// Destination AS number (0 = unknown).
    pub dst_as: u32,
}

impl<'a> From<&'a EventMsg> for FilterView<'a> {
    fn from(ev: &'a EventMsg) -> Self {
        Self {
            source_ip: ev.source_ip,
            pool: ev.pool.as_deref(),
            kind: Some(ev.kind.as_str()),
            facility: ev.facility.map(i64::from),
            severity: ev.syslog_severity.map(i64::from),
            hostname: ev.hostname.as_deref(),
            app_name: ev.app_name.as_deref(),
            message: Some(ev.message.as_str()),
            trap_oid: ev.trap_oid.as_deref(),
            varbinds: &ev.varbinds,
            ..Self::default()
        }
    }
}

impl<'a> FilterView<'a> {
    /// The view for one flow record within a datagram. `exporter` is the datagram source (exposed as
    /// `source_ip`, matching every other stream) and `kind` is the wire format (`netflow`/`sflow`).
    #[must_use]
    pub fn for_flow(
        exporter: IpAddr,
        pool: Option<&'a str>,
        kind: &'a str,
        rec: &FlowFields,
    ) -> Self {
        Self {
            source_ip: Some(exporter),
            pool,
            kind: Some(kind),
            src_addr: Some(rec.src_addr),
            dst_addr: Some(rec.dst_addr),
            proto: Some(i64::from(rec.proto)),
            src_port: Some(i64::from(rec.src_port)),
            dst_port: Some(i64::from(rec.dst_port)),
            src_as: Some(i64::from(rec.src_as)),
            dst_as: Some(i64::from(rec.dst_as)),
            ..Self::default()
        }
    }
}

// ── Compiled form ────────────────────────────────────────────────────────────────────────────

#[derive(Debug)]
enum Predicate {
    TextEq(String),
    TextNe(String),
    TextIn(Vec<String>),
    Contains(String),
    NotContains(String),
    Prefix(String),
    Regex(Box<Regex>),
    NotRegex(Box<Regex>),
    NumEq(i64),
    NumNe(i64),
    NumLte(i64),
    NumGte(i64),
    NumIn(Vec<i64>),
    IpEq(IpAddr),
    IpNe(IpAddr),
    InCidr(Vec<Cidr>),
    NotInCidr(Vec<Cidr>),
}

#[derive(Debug)]
struct CompiledCondition {
    field: FilterField,
    pred: Predicate,
}

/// A validated, ready-to-evaluate filter. Built once per config generation by [`compile`].
#[derive(Debug)]
pub struct CompiledFilter {
    mode: FilterMode,
    conditions: Vec<CompiledCondition>,
}

impl CompiledFilter {
    /// A filter that accepts everything (no conditions).
    #[must_use]
    pub fn allow_all() -> Self {
        Self {
            mode: FilterMode::All,
            conditions: Vec::new(),
        }
    }

    /// Whether `view` passes.
    #[must_use]
    pub fn matches(&self, view: &FilterView<'_>) -> bool {
        if self.conditions.is_empty() {
            return true;
        }
        match self.mode {
            FilterMode::All => self.conditions.iter().all(|c| c.eval(view)),
            FilterMode::Any => self.conditions.iter().any(|c| c.eval(view)),
        }
    }

    /// Number of conditions (0 = accepts everything). Shown as a chip in the WebUI.
    #[must_use]
    pub fn len(&self) -> usize {
        self.conditions.len()
    }

    /// Whether this filter accepts everything.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.conditions.is_empty()
    }
}

impl CompiledCondition {
    fn eval(&self, view: &FilterView<'_>) -> bool {
        match self.field {
            FilterField::SourceIp => eval_ip(&self.pred, view.source_ip),
            FilterField::Pool => eval_text(&self.pred, view.pool),
            FilterField::Kind => eval_text(&self.pred, view.kind),
            FilterField::Facility => eval_num(&self.pred, view.facility),
            FilterField::Severity => eval_num(&self.pred, view.severity),
            FilterField::Hostname => eval_text(&self.pred, view.hostname),
            FilterField::AppName => eval_text(&self.pred, view.app_name),
            FilterField::Message => eval_text(&self.pred, view.message),
            FilterField::TrapOid => eval_text(&self.pred, view.trap_oid),
            FilterField::Varbind => eval_multi(&self.pred, view.varbinds),
            FilterField::SrcAddr => eval_ip(&self.pred, view.src_addr),
            FilterField::DstAddr => eval_ip(&self.pred, view.dst_addr),
            FilterField::Proto => eval_num(&self.pred, view.proto),
            FilterField::SrcPort => eval_num(&self.pred, view.src_port),
            FilterField::DstPort => eval_num(&self.pred, view.dst_port),
            FilterField::SrcAs => eval_num(&self.pred, view.src_as),
            FilterField::DstAs => eval_num(&self.pred, view.dst_as),
        }
    }
}

fn eval_text(pred: &Predicate, datum: Option<&str>) -> bool {
    match pred {
        Predicate::TextEq(x) => datum.is_some_and(|s| s == x),
        Predicate::TextNe(x) => datum.is_none_or(|s| s != x),
        Predicate::TextIn(xs) => datum.is_some_and(|s| xs.iter().any(|x| x == s)),
        Predicate::Contains(x) => datum.is_some_and(|s| s.contains(x.as_str())),
        Predicate::NotContains(x) => datum.is_none_or(|s| !s.contains(x.as_str())),
        Predicate::Prefix(x) => datum.is_some_and(|s| s.starts_with(x.as_str())),
        Predicate::Regex(re) => datum.is_some_and(|s| re.is_match(s)),
        Predicate::NotRegex(re) => datum.is_none_or(|s| !re.is_match(s)),
        // Unreachable: `compile` only pairs text fields with the predicates above.
        _ => false,
    }
}

fn eval_num(pred: &Predicate, datum: Option<i64>) -> bool {
    match pred {
        Predicate::NumEq(x) => datum == Some(*x),
        Predicate::NumNe(x) => datum.is_none_or(|n| n != *x),
        Predicate::NumLte(x) => datum.is_some_and(|n| n <= *x),
        Predicate::NumGte(x) => datum.is_some_and(|n| n >= *x),
        Predicate::NumIn(xs) => datum.is_some_and(|n| xs.contains(&n)),
        _ => false,
    }
}

fn eval_ip(pred: &Predicate, datum: Option<IpAddr>) -> bool {
    match pred {
        Predicate::IpEq(x) => datum == Some(*x),
        Predicate::IpNe(x) => datum.is_none_or(|ip| ip != *x),
        Predicate::InCidr(nets) => datum.is_some_and(|ip| nets.iter().any(|n| n.contains(ip))),
        Predicate::NotInCidr(nets) => datum.is_none_or(|ip| !nets.iter().any(|n| n.contains(ip))),
        _ => false,
    }
}

/// Evaluate over `"<oid>=<value>"` renderings: a positive operator holds when **any** varbind
/// matches, a negative one when **none** does.
fn eval_multi(pred: &Predicate, varbinds: &[(String, String)]) -> bool {
    let mut buf = String::new();
    let mut any = false;
    for (oid, value) in varbinds {
        buf.clear();
        buf.push_str(oid);
        buf.push('=');
        buf.push_str(value);
        let hit = match pred {
            Predicate::Contains(x) | Predicate::NotContains(x) => buf.contains(x.as_str()),
            Predicate::Prefix(x) => buf.starts_with(x.as_str()),
            Predicate::Regex(re) | Predicate::NotRegex(re) => re.is_match(&buf),
            _ => false,
        };
        if hit {
            any = true;
            break;
        }
    }
    match pred {
        Predicate::NotContains(_) | Predicate::NotRegex(_) => !any,
        _ => any,
    }
}

// ── Compilation ──────────────────────────────────────────────────────────────────────────────

/// Validate and compile `expr`. Call once per config generation, never per message.
///
/// # Errors
/// Returns [`FilterError`] when the expression has too many conditions, or a condition has a blank
/// or over-long value, an operator that does not typecheck against its field, or an unparseable
/// regex / CIDR / integer.
pub fn compile(expr: &FilterExpr) -> Result<CompiledFilter, FilterError> {
    if expr.conditions.len() > MAX_CONDITIONS {
        return Err(FilterError::TooManyConditions(expr.conditions.len()));
    }
    let conditions = expr
        .conditions
        .iter()
        .map(compile_condition)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(CompiledFilter {
        mode: expr.mode,
        conditions,
    })
}

fn compile_condition(c: &Condition) -> Result<CompiledCondition, FilterError> {
    let field = c.field.as_str();
    let value = c.value.trim();
    if value.is_empty() {
        return Err(FilterError::EmptyValue(field));
    }
    let chars = value.chars().count();
    if chars > MAX_PATTERN_CHARS {
        return Err(FilterError::ValueTooLong { field, chars });
    }

    // `proto` is the one numeric field an operator naturally writes by name; everything else
    // (severity, ports, AS numbers) genuinely is a number.
    let number: fn(&str) -> Result<i64, FilterError> = if c.field == FilterField::Proto {
        parse_proto
    } else {
        parse_num
    };

    use FilterOp as O;
    use ValueType as T;
    let pred = match (c.field.value_type(), c.op) {
        (T::Text, O::Eq) => Predicate::TextEq(value.to_owned()),
        (T::Text, O::Ne) => Predicate::TextNe(value.to_owned()),
        (T::Text, O::InList) => Predicate::TextIn(split_list(value)),
        (T::Text | T::Multi, O::Contains) => Predicate::Contains(value.to_owned()),
        (T::Text | T::Multi, O::NotContains) => Predicate::NotContains(value.to_owned()),
        (T::Text | T::Multi, O::Prefix) => Predicate::Prefix(value.to_owned()),
        (T::Text | T::Multi, O::Regex) => Predicate::Regex(Box::new(compile_regex(value)?)),
        (T::Text | T::Multi, O::NotRegex) => Predicate::NotRegex(Box::new(compile_regex(value)?)),
        (T::Num, O::Eq) => Predicate::NumEq(number(value)?),
        (T::Num, O::Ne) => Predicate::NumNe(number(value)?),
        (T::Num, O::Lte) => Predicate::NumLte(number(value)?),
        (T::Num, O::Gte) => Predicate::NumGte(number(value)?),
        (T::Num, O::InList) => Predicate::NumIn(
            split_list(value)
                .iter()
                .map(|s| number(s))
                .collect::<Result<Vec<_>, _>>()?,
        ),
        (T::Ip, O::Eq) => Predicate::IpEq(parse_ip(value)?),
        (T::Ip, O::Ne) => Predicate::IpNe(parse_ip(value)?),
        (T::Ip, O::InCidr) => Predicate::InCidr(parse_cidrs(value)?),
        (T::Ip, O::NotInCidr) => Predicate::NotInCidr(parse_cidrs(value)?),
        (_, op) => {
            return Err(FilterError::OpNotAllowed {
                field,
                op: op.as_str(),
            })
        }
    };
    Ok(CompiledCondition {
        field: c.field,
        pred,
    })
}

fn compile_regex(pattern: &str) -> Result<Regex, FilterError> {
    RegexBuilder::new(pattern)
        .size_limit(REGEX_SIZE_LIMIT)
        .build()
        .map_err(|e| FilterError::BadRegex(e.to_string()))
}

fn parse_num(value: &str) -> Result<i64, FilterError> {
    value
        .trim()
        .parse::<i64>()
        .map_err(|_| FilterError::BadNumber(value.to_owned()))
}

/// Parse an IP protocol: a number, or one of the handful of names an operator would actually type.
/// Nobody remembers that GRE is 47, and a filter that silently never matches is worse than a 400.
fn parse_proto(value: &str) -> Result<i64, FilterError> {
    let v = value.trim();
    match v.to_ascii_lowercase().as_str() {
        "icmp" => Ok(1),
        "igmp" => Ok(2),
        "tcp" => Ok(6),
        "udp" => Ok(17),
        "gre" => Ok(47),
        "esp" => Ok(50),
        "ah" => Ok(51),
        "icmpv6" | "ipv6-icmp" => Ok(58),
        "ospf" => Ok(89),
        "vrrp" => Ok(112),
        "sctp" => Ok(132),
        _ => parse_num(v),
    }
}

fn parse_ip(value: &str) -> Result<IpAddr, FilterError> {
    value
        .parse::<IpAddr>()
        .map_err(|_| FilterError::BadCidr(value.to_owned()))
}

fn parse_cidrs(value: &str) -> Result<Vec<Cidr>, FilterError> {
    let nets = split_list(value)
        .iter()
        .map(|s| Cidr::parse(s))
        .collect::<Result<Vec<_>, _>>()?;
    if nets.is_empty() {
        return Err(FilterError::BadCidr(value.to_owned()));
    }
    Ok(nets)
}

fn split_list(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

// ── CIDR ─────────────────────────────────────────────────────────────────────────────────────

/// An address prefix. Hand-rolled rather than pulling an `ipnet`-style dependency for ~40 lines;
/// v4 and v6 never match each other (no v4-mapped-v6 coercion — an operator writing `10.0.0.0/8`
/// means v4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Cidr {
    net: IpAddr,
    prefix: u8,
}

impl Cidr {
    fn parse(text: &str) -> Result<Self, FilterError> {
        let bad = || FilterError::BadCidr(text.to_owned());
        let (addr, len) = match text.split_once('/') {
            Some((a, p)) => (a.trim(), Some(p.trim())),
            None => (text.trim(), None),
        };
        let net: IpAddr = addr.parse().map_err(|_| bad())?;
        let max = if net.is_ipv4() { 32 } else { 128 };
        let prefix = match len {
            Some(p) => p.parse::<u8>().map_err(|_| bad())?,
            None => max,
        };
        if prefix > max {
            return Err(bad());
        }
        Ok(Self { net, prefix })
    }

    fn contains(&self, ip: IpAddr) -> bool {
        match (self.net, ip) {
            (IpAddr::V4(net), IpAddr::V4(addr)) => {
                same_prefix(&net.octets(), &addr.octets(), self.prefix)
            }
            (IpAddr::V6(net), IpAddr::V6(addr)) => {
                same_prefix(&net.octets(), &addr.octets(), self.prefix)
            }
            _ => false,
        }
    }
}

fn same_prefix(net: &[u8], addr: &[u8], prefix: u8) -> bool {
    let whole = usize::from(prefix / 8);
    let bits = prefix % 8;
    if net[..whole] != addr[..whole] {
        return false;
    }
    if bits == 0 {
        return true;
    }
    let mask = 0xff_u8 << (8 - bits);
    (net[whole] & mask) == (addr[whole] & mask)
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn cond(field: FilterField, op: FilterOp, value: &str) -> Condition {
        Condition {
            field,
            op,
            value: value.to_owned(),
        }
    }

    fn expr(mode: FilterMode, conditions: Vec<Condition>) -> FilterExpr {
        FilterExpr { mode, conditions }
    }

    fn syslog_event() -> EventMsg {
        EventMsg {
            event_id: Uuid::nil(),
            kind: EventKind::Syslog,
            at_unix_ms: 1_700_000_000_000,
            source_ip: Some("192.168.1.10".parse().unwrap()),
            pool: Some("tokyo".into()),
            message: "%LINEPROTO-5-UPDOWN: Line protocol on ge-0/0/1 changed state to down".into(),
            facility: Some(23),
            syslog_severity: Some(4),
            hostname: Some("edge-sw1".into()),
            app_name: Some("chassisd".into()),
            trap_oid: None,
            varbinds: Vec::new(),
            truncated: false,
            raw: None,
            src_port: None,
        }
    }

    fn trap_event() -> EventMsg {
        EventMsg {
            trap_oid: Some("1.3.6.1.6.3.1.1.5.3".into()),
            varbinds: vec![
                ("1.3.6.1.2.1.2.2.1.1.4".into(), "4".into()),
                ("1.3.6.1.2.1.2.2.1.2.4".into(), "ge-0/0/1".into()),
            ],
            kind: EventKind::Trap,
            facility: None,
            syslog_severity: None,
            hostname: None,
            app_name: None,
            message: "1.3.6.1.6.3.1.1.5.3 1.3.6.1.2.1.2.2.1.2.4=ge-0/0/1;".into(),
            ..syslog_event()
        }
    }

    fn matches(e: &FilterExpr, ev: &EventMsg) -> bool {
        compile(e).unwrap().matches(&FilterView::from(ev))
    }

    #[test]
    fn empty_filter_accepts_everything() {
        let f = compile(&FilterExpr::default()).unwrap();
        assert!(f.is_empty());
        assert!(f.matches(&FilterView::from(&syslog_event())));
        assert!(f.matches(&FilterView::default()));
    }

    #[test]
    fn json_default_shape_is_match_everything() {
        // `forward_destinations.filter` defaults to `{}` in the DB — that must mean "tee it all".
        let e: FilterExpr = serde_json::from_str("{}").unwrap();
        assert_eq!(e.mode, FilterMode::All);
        assert!(e.conditions.is_empty());
        assert!(compile(&e).unwrap().matches(&FilterView::default()));
    }

    #[test]
    fn filter_expr_round_trips_through_json() {
        let e = expr(
            FilterMode::Any,
            vec![
                cond(FilterField::Severity, FilterOp::Lte, "4"),
                cond(FilterField::Message, FilterOp::Regex, "LINEPROTO-5-\\w+"),
            ],
        );
        let json = serde_json::to_string(&e).unwrap();
        assert!(json.contains("\"mode\":\"any\""), "{json}");
        assert!(json.contains("\"field\":\"severity\""), "{json}");
        assert_eq!(serde_json::from_str::<FilterExpr>(&json).unwrap(), e);
    }

    #[test]
    fn all_mode_requires_every_condition() {
        let ev = syslog_event();
        let e = expr(
            FilterMode::All,
            vec![
                cond(FilterField::Severity, FilterOp::Lte, "4"),
                cond(FilterField::Hostname, FilterOp::Prefix, "edge-"),
            ],
        );
        assert!(matches(&e, &ev));

        let e = expr(
            FilterMode::All,
            vec![
                cond(FilterField::Severity, FilterOp::Lte, "3"), // 4 <= 3 is false
                cond(FilterField::Hostname, FilterOp::Prefix, "edge-"),
            ],
        );
        assert!(!matches(&e, &ev));
    }

    #[test]
    fn any_mode_needs_one_condition() {
        let ev = syslog_event();
        let e = expr(
            FilterMode::Any,
            vec![
                cond(FilterField::Severity, FilterOp::Lte, "0"),
                cond(FilterField::AppName, FilterOp::Eq, "chassisd"),
            ],
        );
        assert!(matches(&e, &ev));
    }

    #[test]
    fn severity_is_ordered_low_is_worse() {
        let mut ev = syslog_event();
        // "at least warning" is severity <= 4.
        let e = expr(
            FilterMode::All,
            vec![cond(FilterField::Severity, FilterOp::Lte, "4")],
        );
        assert!(matches(&e, &ev));
        ev.syslog_severity = Some(6); // informational
        assert!(!matches(&e, &ev));
        ev.syslog_severity = Some(0); // emergency
        assert!(matches(&e, &ev));
    }

    #[test]
    fn cidr_matching_covers_v4_v6_boundaries_and_families() {
        let mut ev = syslog_event();
        let inside = expr(
            FilterMode::All,
            vec![cond(
                FilterField::SourceIp,
                FilterOp::InCidr,
                "192.168.1.0/24",
            )],
        );
        assert!(matches(&inside, &ev));
        ev.source_ip = Some("192.168.2.10".parse().unwrap());
        assert!(!matches(&inside, &ev));

        // Non-byte-aligned prefix.
        ev.source_ip = Some("10.1.127.255".parse().unwrap());
        let slash17 = expr(
            FilterMode::All,
            vec![cond(FilterField::SourceIp, FilterOp::InCidr, "10.1.0.0/17")],
        );
        assert!(matches(&slash17, &ev));
        ev.source_ip = Some("10.1.128.0".parse().unwrap());
        assert!(!matches(&slash17, &ev));

        // A v6 source never matches a v4 prefix (and vice versa).
        ev.source_ip = Some("2001:db8::1".parse().unwrap());
        assert!(!matches(&slash17, &ev));
        let v6 = expr(
            FilterMode::All,
            vec![cond(
                FilterField::SourceIp,
                FilterOp::InCidr,
                "2001:db8::/32",
            )],
        );
        assert!(matches(&v6, &ev));
        ev.source_ip = Some("10.1.0.1".parse().unwrap());
        assert!(!matches(&v6, &ev));
    }

    #[test]
    fn cidr_accepts_a_comma_separated_list_and_a_bare_address() {
        let mut ev = syslog_event();
        let e = expr(
            FilterMode::All,
            vec![cond(
                FilterField::SourceIp,
                FilterOp::InCidr,
                "10.0.0.0/8, 192.168.1.10",
            )],
        );
        assert!(matches(&e, &ev)); // bare address = /32
        ev.source_ip = Some("10.9.9.9".parse().unwrap());
        assert!(matches(&e, &ev));
        ev.source_ip = Some("172.16.0.1".parse().unwrap());
        assert!(!matches(&e, &ev));
    }

    #[test]
    fn absent_field_fails_positive_and_passes_negative_operators() {
        let ev = trap_event(); // no hostname, no severity, no source-less fields
        assert!(!matches(
            &expr(
                FilterMode::All,
                vec![cond(FilterField::Hostname, FilterOp::Contains, "edge")]
            ),
            &ev
        ));
        assert!(matches(
            &expr(
                FilterMode::All,
                vec![cond(FilterField::Hostname, FilterOp::NotContains, "edge")]
            ),
            &ev
        ));
        // A missing numeric datum cannot satisfy a threshold either way round.
        assert!(!matches(
            &expr(
                FilterMode::All,
                vec![cond(FilterField::Severity, FilterOp::Lte, "4")]
            ),
            &ev
        ));
    }

    #[test]
    fn trap_oid_prefix_and_varbind_any_none_semantics() {
        let ev = trap_event();
        assert!(matches(
            &expr(
                FilterMode::All,
                vec![cond(
                    FilterField::TrapOid,
                    FilterOp::Prefix,
                    "1.3.6.1.6.3.1.1.5."
                )]
            ),
            &ev
        ));
        // Positive: any varbind matches.
        assert!(matches(
            &expr(
                FilterMode::All,
                vec![cond(FilterField::Varbind, FilterOp::Contains, "ge-0/0/1")]
            ),
            &ev
        ));
        // Negative: holds only when no varbind matches.
        assert!(!matches(
            &expr(
                FilterMode::All,
                vec![cond(
                    FilterField::Varbind,
                    FilterOp::NotContains,
                    "ge-0/0/1"
                )]
            ),
            &ev
        ));
        assert!(matches(
            &expr(
                FilterMode::All,
                vec![cond(
                    FilterField::Varbind,
                    FilterOp::NotContains,
                    "xe-9/9/9"
                )]
            ),
            &ev
        ));
    }

    #[test]
    fn in_list_works_for_text_and_numbers() {
        let ev = syslog_event();
        assert!(matches(
            &expr(
                FilterMode::All,
                vec![cond(FilterField::Pool, FilterOp::InList, "osaka, tokyo")]
            ),
            &ev
        ));
        assert!(matches(
            &expr(
                FilterMode::All,
                vec![cond(FilterField::Facility, FilterOp::InList, "16,23")]
            ),
            &ev
        ));
        assert!(!matches(
            &expr(
                FilterMode::All,
                vec![cond(FilterField::Facility, FilterOp::InList, "16,17")]
            ),
            &ev
        ));
    }

    #[test]
    fn operator_must_typecheck_against_the_field() {
        // A regex over a numeric field is a configuration mistake, not an always-false filter.
        let err = compile(&expr(
            FilterMode::All,
            vec![cond(FilterField::Severity, FilterOp::Regex, "4")],
        ))
        .unwrap_err();
        assert_eq!(
            err,
            FilterError::OpNotAllowed {
                field: "severity",
                op: "regex"
            }
        );
        // CIDR only applies to addresses.
        assert!(matches!(
            compile(&expr(
                FilterMode::All,
                vec![cond(FilterField::Message, FilterOp::InCidr, "10.0.0.0/8")]
            )),
            Err(FilterError::OpNotAllowed { .. })
        ));
        // ...and equality on a text field is fine.
        assert!(compile(&expr(
            FilterMode::All,
            vec![cond(FilterField::Message, FilterOp::Eq, "x")]
        ))
        .is_ok());
    }

    #[test]
    fn malformed_operands_are_rejected_at_compile_time() {
        let cases = [
            (
                cond(FilterField::Message, FilterOp::Regex, "([unclosed"),
                "regex",
            ),
            (
                cond(FilterField::SourceIp, FilterOp::InCidr, "10.0.0.0/33"),
                "cidr",
            ),
            (
                cond(FilterField::SourceIp, FilterOp::InCidr, "not-an-ip"),
                "cidr",
            ),
            (cond(FilterField::Severity, FilterOp::Lte, "high"), "number"),
            (
                cond(FilterField::Message, FilterOp::Contains, "   "),
                "empty",
            ),
        ];
        for (c, label) in cases {
            assert!(
                compile(&expr(FilterMode::All, vec![c])).is_err(),
                "{label} should not compile"
            );
        }
    }

    #[test]
    fn over_long_values_and_too_many_conditions_are_rejected() {
        let long = "x".repeat(MAX_PATTERN_CHARS + 1);
        assert!(matches!(
            compile(&expr(
                FilterMode::All,
                vec![cond(FilterField::Message, FilterOp::Contains, &long)]
            )),
            Err(FilterError::ValueTooLong { .. })
        ));

        let many = (0..=MAX_CONDITIONS)
            .map(|_| cond(FilterField::Message, FilterOp::Contains, "x"))
            .collect();
        assert!(matches!(
            compile(&expr(FilterMode::All, many)),
            Err(FilterError::TooManyConditions(_))
        ));
    }

    #[test]
    fn fields_are_scoped_to_the_stream_they_can_appear_in() {
        assert!(FilterField::Facility.applies_to(SourceKind::Syslog));
        assert!(!FilterField::Facility.applies_to(SourceKind::Trap));
        assert!(FilterField::TrapOid.applies_to(SourceKind::Trap));
        assert!(!FilterField::TrapOid.applies_to(SourceKind::Syslog));
        // Flow-only fields.
        for f in [
            FilterField::SrcAddr,
            FilterField::DstAddr,
            FilterField::Proto,
            FilterField::SrcPort,
            FilterField::DstPort,
            FilterField::SrcAs,
            FilterField::DstAs,
        ] {
            assert!(f.applies_to(SourceKind::Flow));
            assert!(!f.applies_to(SourceKind::Syslog) && !f.applies_to(SourceKind::Trap));
        }
        // A flow datagram has no message text.
        assert!(!FilterField::Message.applies_to(SourceKind::Flow));
        // Common fields apply to every stream — `source_ip` is the exporter for flow.
        for f in [FilterField::SourceIp, FilterField::Pool, FilterField::Kind] {
            for s in [SourceKind::Syslog, SourceKind::Trap, SourceKind::Flow] {
                assert!(
                    f.applies_to(s),
                    "{} should apply to {}",
                    f.as_str(),
                    s.as_str()
                );
            }
        }
    }

    #[test]
    fn source_and_dest_pairings_and_verbatim_support() {
        // Traps may be shipped to a syslog collector, but only as a rendered line.
        assert!(DestKind::SyslogUdp.accepts(SourceKind::Trap));
        assert!(!DestKind::SyslogUdp.supports_verbatim(SourceKind::Trap));
        assert!(DestKind::SyslogUdp.supports_verbatim(SourceKind::Syslog));
        // A syslog line has no PDU form.
        assert!(!DestKind::SnmpTrapUdp.accepts(SourceKind::Syslog));
        assert!(DestKind::SnmpTrapUdp.supports_verbatim(SourceKind::Trap));
        // TLS is a transport choice, so it pairs exactly like plain syslog TCP.
        assert!(DestKind::SyslogTls.accepts(SourceKind::Syslog));
        assert!(DestKind::SyslogTls.accepts(SourceKind::Trap));
        assert!(DestKind::SyslogTls.supports_verbatim(SourceKind::Syslog));
        assert!(DestKind::SyslogTls.is_tls() && !DestKind::SyslogTcp.is_tls());
    }

    #[test]
    fn flow_is_isolated_to_its_own_destination_kind_and_is_verbatim_only() {
        // Flow goes nowhere but a flow collector, and nothing else can become a flow export.
        assert!(DestKind::FlowUdp.accepts(SourceKind::Flow));
        assert!(!DestKind::FlowUdp.accepts(SourceKind::Syslog));
        assert!(!DestKind::FlowUdp.accepts(SourceKind::Trap));
        assert!(!DestKind::SyslogUdp.accepts(SourceKind::Flow));
        assert!(!DestKind::SnmpTrapUdp.accepts(SourceKind::Flow));
        // There is no rendered form of a template-bound binary export — byte-exact or nothing.
        assert!(DestKind::FlowUdp.supports_verbatim(SourceKind::Flow));
        assert!(!DestKind::FlowUdp.supports_rendered(SourceKind::Flow));
        assert!(DestKind::SyslogUdp.supports_rendered(SourceKind::Syslog));
        assert!(DestKind::SnmpTrapUdp.supports_rendered(SourceKind::Trap));
    }

    #[test]
    fn bigquery_takes_every_stream_as_rows_and_never_verbatim() {
        // The one destination that is not a datagram relay: it accepts all three streams, has no
        // byte-exact mode at all, and — unlike `flow_udp` — *does* have a derived form for flow,
        // which is what makes per-record flow filtering possible there.
        for source in [SourceKind::Syslog, SourceKind::Trap, SourceKind::Flow] {
            assert!(DestKind::BigQuery.accepts(source));
            assert!(!DestKind::BigQuery.supports_verbatim(source));
            assert!(DestKind::BigQuery.supports_rendered(source));
        }
        // It is HTTPS, but to a fixed Google endpoint — nothing for an operator to pin, and no
        // `host:port` to type.
        assert!(!DestKind::BigQuery.is_tls());
        assert!(!DestKind::BigQuery.is_host_port());
        assert!(DestKind::SyslogTls.is_host_port() && DestKind::FlowUdp.is_host_port());
    }

    #[test]
    fn stream_selection_routes_webhooks_with_syslog() {
        assert!(SourceKind::Syslog.accepts(EventKind::Syslog));
        assert!(SourceKind::Syslog.accepts(EventKind::Webhook));
        assert!(!SourceKind::Syslog.accepts(EventKind::Trap));
        assert!(SourceKind::Trap.accepts(EventKind::Trap));
        assert!(!SourceKind::Trap.accepts(EventKind::Syslog));
        // Flow never arrives on the event path — it has its own subject.
        for kind in [EventKind::Syslog, EventKind::Trap, EventKind::Webhook] {
            assert!(!SourceKind::Flow.accepts(kind));
        }
    }

    fn flow_rec(src: &str, dst: &str, proto: u8, dport: u16, dst_as: u32) -> FlowFields {
        FlowFields {
            src_addr: src.parse().unwrap(),
            dst_addr: dst.parse().unwrap(),
            proto,
            src_port: 40_000,
            dst_port: dport,
            src_as: 0,
            dst_as,
        }
    }

    fn flow_matches(e: &FilterExpr, rec: &FlowFields) -> bool {
        let exporter: IpAddr = "192.168.1.1".parse().unwrap();
        compile(e).unwrap().matches(&FilterView::for_flow(
            exporter,
            Some("tokyo"),
            "netflow",
            rec,
        ))
    }

    #[test]
    fn flow_conditions_match_on_record_fields() {
        let rec = flow_rec("10.0.0.5", "8.8.8.8", 6, 443, 15169);
        assert!(flow_matches(
            &expr(
                FilterMode::All,
                vec![
                    cond(FilterField::DstAddr, FilterOp::InCidr, "8.8.8.0/24"),
                    cond(FilterField::DstPort, FilterOp::Eq, "443"),
                    cond(FilterField::DstAs, FilterOp::Eq, "15169"),
                ]
            ),
            &rec
        ));
        // Source address and the exporter are different things and must not be conflated.
        assert!(flow_matches(
            &expr(
                FilterMode::All,
                vec![
                    cond(FilterField::SrcAddr, FilterOp::InCidr, "10.0.0.0/8"),
                    cond(FilterField::SourceIp, FilterOp::Eq, "192.168.1.1"),
                ]
            ),
            &rec
        ));
        assert!(!flow_matches(
            &expr(
                FilterMode::All,
                vec![cond(FilterField::SrcAddr, FilterOp::Eq, "192.168.1.1")]
            ),
            &rec
        ));
        // `kind` carries the wire format for flow.
        assert!(flow_matches(
            &expr(
                FilterMode::All,
                vec![cond(FilterField::Kind, FilterOp::Eq, "netflow")]
            ),
            &rec
        ));
    }

    #[test]
    fn proto_accepts_a_name_as_well_as_a_number() {
        let tcp = flow_rec("10.0.0.5", "8.8.8.8", 6, 443, 0);
        let udp = flow_rec("10.0.0.5", "8.8.8.8", 17, 53, 0);
        let by_name = expr(
            FilterMode::All,
            vec![cond(FilterField::Proto, FilterOp::Eq, "tcp")],
        );
        assert!(flow_matches(&by_name, &tcp));
        assert!(!flow_matches(&by_name, &udp));
        // Case-insensitive, and a list mixes names with numbers.
        let mixed = expr(
            FilterMode::All,
            vec![cond(FilterField::Proto, FilterOp::InList, "UDP, 47")],
        );
        assert!(flow_matches(&mixed, &udp));
        assert!(!flow_matches(&mixed, &tcp));
        // A number still works, and nonsense is still a compile error.
        assert!(flow_matches(
            &expr(
                FilterMode::All,
                vec![cond(FilterField::Proto, FilterOp::Eq, "6")]
            ),
            &tcp
        ));
        assert!(compile(&expr(
            FilterMode::All,
            vec![cond(FilterField::Proto, FilterOp::Eq, "carrier-pigeon")]
        ))
        .is_err());
    }

    #[test]
    fn string_forms_match_the_serde_tags() {
        for (field, tag) in [
            (FilterField::SourceIp, "source_ip"),
            (FilterField::AppName, "app_name"),
            (FilterField::TrapOid, "trap_oid"),
            (FilterField::SrcAddr, "src_addr"),
            (FilterField::DstAs, "dst_as"),
        ] {
            assert_eq!(serde_json::to_string(&field).unwrap(), format!("\"{tag}\""));
            assert_eq!(field.as_str(), tag);
        }
        for (op, tag) in [
            (FilterOp::NotInCidr, "not_in_cidr"),
            (FilterOp::InList, "in_list"),
        ] {
            assert_eq!(serde_json::to_string(&op).unwrap(), format!("\"{tag}\""));
            assert_eq!(op.as_str(), tag);
        }
        for kind in [SourceKind::Syslog, SourceKind::Trap, SourceKind::Flow] {
            assert_eq!(SourceKind::parse(kind.as_str()), Some(kind));
            assert_eq!(
                serde_json::to_string(&kind).unwrap(),
                format!("\"{}\"", kind.as_str())
            );
        }
        for kind in [
            DestKind::SyslogUdp,
            DestKind::SyslogTcp,
            DestKind::SyslogTls,
            DestKind::SnmpTrapUdp,
            DestKind::FlowUdp,
            DestKind::BigQuery,
        ] {
            assert_eq!(DestKind::parse(kind.as_str()), Some(kind));
            assert_eq!(
                serde_json::to_string(&kind).unwrap(),
                format!("\"{}\"", kind.as_str())
            );
        }
        assert_eq!(DestKind::parse("carrier_pigeon"), None);
    }
}
