// SPDX-License-Identifier: AGPL-3.0-only
//! DNS name-resolution monitoring: configuration, observed chains, and the change key (ADR-033).
//!
//! A DNS monitor is modelled as a dedicated *node kind* (profile category
//! [`crate::ProfileCategory::DnsCheck`]) carrying a single [`DnsCheckConfig`] (1:1 with the node),
//! exactly like URL monitoring. It reuses the whole monitoring spine — thresholds
//! (profile→group→node), maintenance windows, dependency suppression, dashboards — so nothing here
//! is bespoke beyond the probe shape.
//!
//! Two things make this module different from [`crate::url_check`]:
//!
//! 1. **The artifact is structured, not numeric.** A resolution chain
//!    (`horryworks.net → CNAME horry.net → A 10.1.2.3`) cannot live in the TSDB: [`crate::SeriesKey`]
//!    is fixed at `{node, ifindex, metric}` (ADR-011) with no API for extra labels, and a CNAME
//!    target is unbounded free text. Chains therefore travel on `PollResult.dns_chain` and land in
//!    PostgreSQL — the same tier as `sys_descr` and the interface inventory. Only the numeric
//!    summaries below become metrics.
//! 2. **History is append-on-change.** A DNS answer is normally constant; what matters is *when it
//!    changed*. That makes [`DnsChain::content_key`] load-bearing: get its normalization wrong and
//!    every poll looks like a change, turning the history into a poll log. See its docs.

use serde::{Deserialize, Serialize};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// Stable TSDB metric: `1` = the name resolved to at least one record of the requested type,
/// `0` = it did not, for **any** reason (NXDOMAIN / SERVFAIL / REFUSED / timeout / CNAME loop /
/// depth exceeded). One metric expresses every "does not resolve" case so a single threshold
/// (`dns_up` below 0.5 — only the 0 state, since the engine's below-comparison is inclusive)
/// covers them all.
pub const METRIC_DNS_UP: &str = "dns_up";
/// Stable TSDB metric: wall-clock milliseconds to walk the whole chain (display/graph only — no
/// seeded threshold, because resolver latency varies far too much across environments for a
/// useful default).
pub const METRIC_DNS_RESOLVE_MS: &str = "dns_resolve_ms";
/// Stable TSDB metric: number of hops in the chain (1 = direct answer, 2+ = CNAME indirection).
pub const METRIC_DNS_CHAIN_LENGTH: &str = "dns_chain_length";
/// Stable TSDB metric: number of terminal records the chain resolved to.
pub const METRIC_DNS_ANSWER_COUNT: &str = "dns_answer_count";

/// Longest DNS name we accept, in bytes (RFC 1035 §2.3.4).
const MAX_NAME_LEN: usize = 253;
/// Longest single label, in bytes (RFC 1035 §2.3.4).
const MAX_LABEL_LEN: usize = 63;
/// Cap on the answers recorded per hop, so a pathological RRset can't make the content key
/// unbounded. Applied **after** sorting so the truncation is deterministic.
const MAX_ANSWERS_PER_HOP: usize = 64;

/// Which record type a DNS check resolves to. Stored as an UPPERCASE token (the `record_type`
/// column).
///
/// `Cname` is a legitimate terminal type: it answers "what is this name an alias for?" without
/// chasing on to an address.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, utoipa::ToSchema)]
#[serde(rename_all = "UPPERCASE")]
pub enum DnsRecordType {
    /// IPv4 address record (the default).
    #[default]
    A,
    /// IPv6 address record.
    Aaaa,
    /// Canonical-name alias; terminal when asked for explicitly.
    Cname,
}

impl DnsRecordType {
    /// The stable UPPERCASE token stored in the DB / sent over the bus.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            DnsRecordType::A => "A",
            DnsRecordType::Aaaa => "AAAA",
            DnsRecordType::Cname => "CNAME",
        }
    }

    /// Parse a stored/operator token back into a record type (case-insensitive).
    #[must_use]
    pub fn from_token(s: &str) -> Option<Self> {
        match s.to_ascii_uppercase().as_str() {
            "A" => Some(DnsRecordType::A),
            "AAAA" => Some(DnsRecordType::Aaaa),
            "CNAME" => Some(DnsRecordType::Cname),
            _ => None,
        }
    }
}

const fn default_resolver_port() -> u16 {
    53
}
const fn default_max_depth() -> u8 {
    8
}
const fn default_dns_timeout_ms() -> u32 {
    3000
}

/// A node's DNS-monitoring configuration (1:1 with the node).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct DnsCheckConfig {
    /// The name to resolve, e.g. `horryworks.net`. Stored normalized (lowercase, no trailing dot).
    pub name: String,
    /// Which record type the chain must reach (default `A`).
    #[serde(default)]
    pub record_type: DnsRecordType,
    /// Recursive resolver to query. `None` ⇒ the poller container's system resolver.
    // utoipa has no schema for `IpAddr`; on the wire it is the serde `Display` form either way.
    #[serde(default)]
    #[schema(value_type = Option<String>)]
    pub resolver: Option<IpAddr>,
    /// Resolver port (default 53).
    #[serde(default = "default_resolver_port")]
    pub resolver_port: u16,
    /// Maximum CNAME hops before giving up (default 8).
    #[serde(default = "default_max_depth")]
    pub max_depth: u8,
    /// **Total** budget for the whole chain walk, in milliseconds (default 3000).
    #[serde(default = "default_dns_timeout_ms")]
    pub timeout_ms: u32,
}

impl DnsCheckConfig {
    /// A new DNS check with MVP defaults (A record, system resolver, depth 8, 3 s budget).
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            record_type: DnsRecordType::default(),
            resolver: None,
            resolver_port: default_resolver_port(),
            max_depth: default_max_depth(),
            timeout_ms: default_dns_timeout_ms(),
        }
    }
}

/// One record in an answer set.
///
/// Typed rather than a rendered `String` so ordering is **numeric**: `Ipv4Addr`'s `Ord` compares
/// octets, so `9.9.9.9` sorts before `10.1.2.3`. Text ordering gets that backwards, which would
/// make the canonical form depend on how the resolver happened to rotate a round-robin RRset.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, utoipa::ToSchema,
)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DnsRecord {
    /// A CNAME alias target (normalized: lowercase, no trailing dot).
    Cname {
        /// The name this hop is an alias for.
        target: String,
    },
    /// An IPv4 address record.
    A {
        /// The resolved IPv4 address.
        // utoipa has no schema for `Ipv4Addr`; on the wire it is the serde `Display` form.
        #[schema(value_type = String)]
        addr: Ipv4Addr,
    },
    /// An IPv6 address record.
    Aaaa {
        /// The resolved IPv6 address.
        #[schema(value_type = String)]
        addr: Ipv6Addr,
    },
}

impl DnsRecord {
    /// The record type this answer carries.
    #[must_use]
    pub const fn record_type(&self) -> DnsRecordType {
        match self {
            DnsRecord::Cname { .. } => DnsRecordType::Cname,
            DnsRecord::A { .. } => DnsRecordType::A,
            DnsRecord::Aaaa { .. } => DnsRecordType::Aaaa,
        }
    }

    /// The rdata rendered for display and for the content key.
    #[must_use]
    pub fn value(&self) -> String {
        match self {
            DnsRecord::Cname { target } => target.clone(),
            DnsRecord::A { addr } => addr.to_string(),
            DnsRecord::Aaaa { addr } => addr.to_string(),
        }
    }
}

/// One answer record plus its TTL.
///
/// The TTL is **display-only**: it counts down between polls, so including it in the content key
/// would make every single poll register as a change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct DnsAnswer {
    /// The record itself.
    pub record: DnsRecord,
    /// Time-to-live as reported by the resolver, in seconds.
    pub ttl: u32,
}

/// One hop of a resolution chain: the name asked about and the answers that came back for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct DnsHop {
    /// The name queried at this hop (normalized).
    pub name: String,
    /// The answers whose owner is `name`, canonicalized (sorted, deduped, capped).
    pub answers: Vec<DnsAnswer>,
}

/// Why a resolution did not reach a terminal record set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DnsFailure {
    /// Authoritative "no such name" (RCODE 3).
    NxDomain,
    /// NOERROR, but no record of the requested type and no CNAME to follow.
    NoData,
    /// The resolver failed to answer the question (RCODE 2).
    ServFail,
    /// The resolver refused to answer (RCODE 5).
    Refused,
    /// Any other response code, kept numeric so a new RCODE is never lost.
    OtherRcode {
        /// The raw DNS response code.
        rcode: u16,
    },
    /// The resolver did not answer within the check's budget.
    Timeout,
    /// A CNAME pointed at a name already visited in this chain.
    LoopDetected {
        /// The name the loop closed on.
        at: String,
    },
    /// The chain was longer than the configured `max_depth`.
    DepthExceeded {
        /// The limit that was hit.
        max_depth: u8,
    },
    /// The response could not be decoded or did not match the question asked.
    Malformed,
}

impl DnsFailure {
    /// A stable snake_case token for the `failure_kind` column and for UI keying. The
    /// discriminating payload (rcode / loop name / depth) is intentionally **not** included — the
    /// column is for grouping, and the full detail lives in the stored chain JSON.
    #[must_use]
    pub const fn kind_token(&self) -> &'static str {
        match self {
            DnsFailure::NxDomain => "nx_domain",
            DnsFailure::NoData => "no_data",
            DnsFailure::ServFail => "serv_fail",
            DnsFailure::Refused => "refused",
            DnsFailure::OtherRcode { .. } => "other_rcode",
            DnsFailure::Timeout => "timeout",
            DnsFailure::LoopDetected { .. } => "loop_detected",
            DnsFailure::DepthExceeded { .. } => "depth_exceeded",
            DnsFailure::Malformed => "malformed",
        }
    }
}

/// One observed resolution chain — the artifact of a DNS check.
///
/// Produced by the transport, carried on `PollResult.dns_chain`, persisted to PostgreSQL. **Never
/// a TSDB label** (ADR-011); it is the same tier as the interface inventory and `sys_descr`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct DnsChain {
    /// The name originally asked for (normalized).
    pub query: String,
    /// The record type asked for.
    pub record_type: DnsRecordType,
    /// Where we asked, rendered for display (e.g. `10.0.0.53:53` or `system`). Provenance, **not**
    /// content: swapping resolvers while the answer stays the same is not a change.
    pub resolver: String,
    /// The hops in walk order. Order is significant and is never sorted.
    pub hops: Vec<DnsHop>,
    /// `None` ⇒ resolved. A failed chain may still carry hops (e.g. one CNAME, then NXDOMAIN).
    #[serde(default)]
    pub failure: Option<DnsFailure>,
    /// Total wall-clock milliseconds for the walk. **Not** part of the content key.
    pub resolve_ms: f64,
}

impl DnsChain {
    /// Whether the chain reached a terminal record set.
    #[must_use]
    pub const fn resolved(&self) -> bool {
        self.failure.is_none()
    }

    /// How many records the chain finally resolved to (0 when it failed).
    #[must_use]
    pub fn terminal_answer_count(&self) -> usize {
        if !self.resolved() {
            return 0;
        }
        self.hops.last().map_or(0, |h| h.answers.len())
    }

    /// Put the chain into its canonical form **in place**.
    ///
    /// The transport must call this before the chain leaves the poller, so that the canonical form
    /// is what travels the bus and what is stored. Every rule here exists because violating it
    /// produces a false "change" on some poll:
    ///
    /// 1. Names are ASCII-lowercased and lose a trailing root dot. DNS is case-insensitive, and
    ///    resolvers using 0x20 query-name randomization **echo mixed case back**.
    /// 2. Each hop's answers are sorted by [`DnsRecord`]'s derived `Ord`. Round-robin RRsets arrive
    ///    rotated on every query.
    /// 3. Exact duplicates within a hop are removed (some resolvers echo them).
    /// 4. Answers are capped at [`MAX_ANSWERS_PER_HOP`] *after* sorting, so truncation is stable.
    /// 5. Hops are **never** reordered — the chain order is the information.
    pub fn canonicalize(&mut self) {
        self.query = normalize_dns_name(&self.query);
        if let Some(DnsFailure::LoopDetected { at }) = &mut self.failure {
            *at = normalize_dns_name(at);
        }
        for hop in &mut self.hops {
            hop.name = normalize_dns_name(&hop.name);
            for answer in &mut hop.answers {
                if let DnsRecord::Cname { target } = &mut answer.record {
                    *target = normalize_dns_name(target);
                }
            }
            // Sort by record only — two answers differing solely in TTL must collapse to one, and
            // sorting on TTL would let a TTL countdown reorder the set.
            hop.answers.sort_by(|a, b| a.record.cmp(&b.record));
            hop.answers.dedup_by(|a, b| a.record == b.record);
            hop.answers.truncate(MAX_ANSWERS_PER_HOP);
        }
    }

    /// The stable content key used for append-on-change comparison.
    ///
    /// **Excludes** every `ttl` (counts down each poll), `resolve_ms` (varies each poll), and
    /// `resolver` (provenance, not content). **Includes** the query, the record type, the hop
    /// sequence with each hop's canonical answers, and the failure — so `resolved → NXDOMAIN` and
    /// `NXDOMAIN → SERVFAIL` both register as real changes.
    ///
    /// ⚠️ This encoding is effectively a wire format. Changing it re-keys every stored chain and
    /// emits exactly one spurious change row per DNS node on the first poll after the upgrade. It
    /// is versioned (`v1` first line); bump the version and say so in RELEASE_NOTES if it ever
    /// changes. It is built by hand rather than via `serde_json` precisely so serde's output format
    /// cannot drift underneath it.
    ///
    /// Call [`Self::canonicalize`] first — this method reads the chain as-is and does not sort.
    #[must_use]
    pub fn content_key(&self) -> String {
        let mut out = String::from("v1\n");
        out.push_str("q=");
        out.push_str(&self.query);
        out.push_str("\nt=");
        out.push_str(self.record_type.as_str());
        out.push('\n');
        for hop in &self.hops {
            out.push_str("h=");
            out.push_str(&hop.name);
            out.push('\n');
            for answer in &hop.answers {
                out.push_str("r=");
                out.push_str(answer.record.record_type().as_str());
                out.push(':');
                out.push_str(&answer.record.value());
                out.push('\n');
            }
        }
        out.push_str("f=");
        match &self.failure {
            None => out.push('-'),
            Some(f) => {
                out.push_str(f.kind_token());
                match f {
                    DnsFailure::OtherRcode { rcode } => {
                        out.push(':');
                        out.push_str(&rcode.to_string());
                    }
                    DnsFailure::LoopDetected { at } => {
                        out.push(':');
                        out.push_str(at);
                    }
                    DnsFailure::DepthExceeded { max_depth } => {
                        out.push(':');
                        out.push_str(&max_depth.to_string());
                    }
                    _ => {}
                }
            }
        }
        out.push('\n');
        out
    }
}

/// Normalize a DNS name for storage and comparison: trim, ASCII-lowercase, drop one trailing dot.
#[must_use]
pub fn normalize_dns_name(name: &str) -> String {
    let trimmed = name.trim();
    let without_root = trimmed.strip_suffix('.').unwrap_or(trimmed);
    without_root.to_ascii_lowercase()
}

/// Validate an operator-supplied DNS name, returning its normalized form.
///
/// Parsing into a checked value at the API edge is the security rule (security.md); this is also
/// what stops a malformed name reaching the resolver. `_` is allowed because underscore-prefixed
/// names (`_dmarc.example.com`, `_acme-challenge…`) are real monitoring targets.
///
/// Returns `None` when the name is unusable; the API edge maps that to `invalid_dns_name`.
#[must_use]
pub fn validate_dns_name(name: &str) -> Option<String> {
    let normalized = normalize_dns_name(name);
    if normalized.is_empty() || normalized.len() > MAX_NAME_LEN {
        return None;
    }
    for label in normalized.split('.') {
        if label.is_empty() || label.len() > MAX_LABEL_LEN {
            return None;
        }
        if label.starts_with('-') || label.ends_with('-') {
            return None;
        }
        if !label
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return None;
        }
    }
    Some(normalized)
}

/// Whether a configured resolver address must be refused.
///
/// Delegates to the URL-monitor policy — the two node kinds share **one** policy on purpose: an NMS
/// legitimately points at internal DNS servers (so RFC1918 / ULA are allowed), and the escalation
/// surface it must never reach (loopback, link-local including 169.254.169.254, multicast, the
/// unspecified address) is identical for both.
///
/// Note this covers only where we *ask*. Answers are never filtered: they are the data being
/// recorded, and nothing dials them.
#[must_use]
pub fn is_resolver_blocked(ip: IpAddr) -> bool {
    crate::url_check::is_ssrf_blocked(ip)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(addr: &str, ttl: u32) -> DnsAnswer {
        DnsAnswer {
            record: DnsRecord::A {
                addr: addr.parse().unwrap(),
            },
            ttl,
        }
    }

    fn cname(target: &str, ttl: u32) -> DnsAnswer {
        DnsAnswer {
            record: DnsRecord::Cname {
                target: target.into(),
            },
            ttl,
        }
    }

    /// `horryworks.net → CNAME horry.net → A 10.1.2.3` — the shape from the feature request.
    fn sample_chain() -> DnsChain {
        DnsChain {
            query: "horryworks.net".into(),
            record_type: DnsRecordType::A,
            resolver: "10.0.0.53:53".into(),
            hops: vec![
                DnsHop {
                    name: "horryworks.net".into(),
                    answers: vec![cname("horry.net", 300)],
                },
                DnsHop {
                    name: "horry.net".into(),
                    answers: vec![a("10.1.2.3", 60)],
                },
            ],
            failure: None,
            resolve_ms: 14.0,
        }
    }

    fn key_of(mut chain: DnsChain) -> String {
        chain.canonicalize();
        chain.content_key()
    }

    // ── The content key: what must NOT count as a change ─────────────────────────────

    #[test]
    fn content_key_ignores_ttl() {
        // The single most important property here. TTLs count down between polls, so a key that
        // included them would append a history row on literally every poll.
        let mut ttl_expired = sample_chain();
        ttl_expired.hops[0].answers[0].ttl = 1;
        ttl_expired.hops[1].answers[0].ttl = 42;
        assert_eq!(key_of(sample_chain()), key_of(ttl_expired));
    }

    #[test]
    fn content_key_ignores_answer_order() {
        // Round-robin RRsets arrive rotated on each query.
        let mut ordered = sample_chain();
        ordered.hops[1].answers = vec![a("10.1.2.3", 60), a("10.1.2.4", 60), a("10.1.2.5", 60)];
        let mut rotated = sample_chain();
        rotated.hops[1].answers = vec![a("10.1.2.5", 60), a("10.1.2.3", 60), a("10.1.2.4", 60)];
        assert_eq!(key_of(ordered), key_of(rotated));
    }

    #[test]
    fn content_key_ignores_name_case_and_trailing_dot() {
        // Resolvers using 0x20 randomization echo the question back in mixed case.
        let mut shouty = sample_chain();
        shouty.query = "HorryWorks.NET.".into();
        shouty.hops[0].name = "HORRYWORKS.net.".into();
        shouty.hops[0].answers = vec![cname("Horry.Net.", 300)];
        shouty.hops[1].name = "horry.NET".into();
        assert_eq!(key_of(sample_chain()), key_of(shouty));
    }

    #[test]
    fn content_key_ignores_resolve_ms_and_resolver() {
        // Latency varies every poll, and which resolver we asked is provenance, not content.
        let mut elsewhere = sample_chain();
        elsewhere.resolve_ms = 481.7;
        elsewhere.resolver = "1.1.1.1:53".into();
        assert_eq!(key_of(sample_chain()), key_of(elsewhere));
    }

    #[test]
    fn canonicalize_dedups_duplicate_answers() {
        let mut dupes = sample_chain();
        dupes.hops[1].answers = vec![a("10.1.2.3", 60), a("10.1.2.3", 55)];
        assert_eq!(key_of(sample_chain()), key_of(dupes));
    }

    #[test]
    fn canonicalize_is_idempotent() {
        let mut once = sample_chain();
        once.canonicalize();
        let mut twice = once.clone();
        twice.canonicalize();
        assert_eq!(once, twice);
    }

    #[test]
    fn canonicalize_sorts_ipv4_numerically_not_lexically() {
        // Text ordering would put "10.1.2.3" before "9.9.9.9"; octet ordering must not.
        let mut chain = sample_chain();
        chain.hops[1].answers = vec![a("10.1.2.3", 60), a("9.9.9.9", 60)];
        chain.canonicalize();
        let values: Vec<String> = chain.hops[1]
            .answers
            .iter()
            .map(|x| x.record.value())
            .collect();
        assert_eq!(values, vec!["9.9.9.9".to_string(), "10.1.2.3".to_string()]);
    }

    #[test]
    fn canonicalize_caps_answers_per_hop_deterministically() {
        let mut chain = sample_chain();
        chain.hops[1].answers = (0..100u32)
            .map(|i| a(&format!("10.0.{}.{}", i / 256, i % 256), 60))
            .rev() // reversed input must still yield the same retained set
            .collect();
        chain.canonicalize();
        assert_eq!(chain.hops[1].answers.len(), MAX_ANSWERS_PER_HOP);

        let mut shuffled = sample_chain();
        shuffled.hops[1].answers = (0..100u32)
            .map(|i| a(&format!("10.0.{}.{}", i / 256, i % 256), 60))
            .collect();
        shuffled.canonicalize();
        assert_eq!(chain.hops[1].answers, shuffled.hops[1].answers);
    }

    // ── The content key: what MUST count as a change ─────────────────────────────────

    #[test]
    fn content_key_changes_when_address_changes() {
        let mut moved = sample_chain();
        moved.hops[1].answers = vec![a("10.1.2.9", 60)];
        assert_ne!(key_of(sample_chain()), key_of(moved));
    }

    #[test]
    fn content_key_changes_when_address_added_or_removed() {
        let mut added = sample_chain();
        added.hops[1].answers = vec![a("10.1.2.3", 60), a("10.1.2.4", 60)];
        assert_ne!(key_of(sample_chain()), key_of(added.clone()));

        let mut removed = sample_chain();
        removed.hops[1].answers = vec![];
        assert_ne!(key_of(added), key_of(removed));
    }

    #[test]
    fn content_key_changes_when_cname_target_changes() {
        let mut repointed = sample_chain();
        repointed.hops[0].answers = vec![cname("elsewhere.net", 300)];
        assert_ne!(key_of(sample_chain()), key_of(repointed));
    }

    #[test]
    fn content_key_changes_when_hop_added() {
        // A name that used to answer directly now goes through a CNAME.
        let direct = DnsChain {
            query: "horry.net".into(),
            hops: vec![DnsHop {
                name: "horry.net".into(),
                answers: vec![a("10.1.2.3", 60)],
            }],
            ..sample_chain()
        };
        assert_ne!(key_of(direct), key_of(sample_chain()));
    }

    #[test]
    fn content_key_is_hop_order_sensitive() {
        let mut reversed = sample_chain();
        reversed.hops.reverse();
        assert_ne!(key_of(sample_chain()), key_of(reversed));
    }

    #[test]
    fn content_key_changes_on_resolved_to_nxdomain() {
        let mut gone = sample_chain();
        gone.hops.clear();
        gone.failure = Some(DnsFailure::NxDomain);
        assert_ne!(key_of(sample_chain()), key_of(gone));
    }

    #[test]
    fn content_key_changes_between_failure_kinds() {
        let base = DnsChain {
            hops: vec![],
            failure: Some(DnsFailure::NxDomain),
            ..sample_chain()
        };
        let servfail = DnsChain {
            failure: Some(DnsFailure::ServFail),
            ..base.clone()
        };
        let rcode9 = DnsChain {
            failure: Some(DnsFailure::OtherRcode { rcode: 9 }),
            ..base.clone()
        };
        assert_ne!(key_of(base.clone()), key_of(servfail));
        assert_ne!(key_of(base), key_of(rcode9));
    }

    #[test]
    fn content_key_is_versioned() {
        // Guards against a silent format drift: the version prefix must be explicit.
        assert!(key_of(sample_chain()).starts_with("v1\n"));
    }

    // ── Chain helpers ────────────────────────────────────────────────────────────────

    #[test]
    fn terminal_answer_count_reads_the_last_hop_and_is_zero_when_failed() {
        let mut chain = sample_chain();
        chain.hops[1].answers = vec![a("10.1.2.3", 60), a("10.1.2.4", 60)];
        assert_eq!(chain.terminal_answer_count(), 2);
        assert!(chain.resolved());

        chain.failure = Some(DnsFailure::Timeout);
        assert_eq!(chain.terminal_answer_count(), 0);
        assert!(!chain.resolved());
    }

    // ── Config, tokens, serde ────────────────────────────────────────────────────────

    #[test]
    fn dns_check_config_defaults() {
        let cfg: DnsCheckConfig = serde_json::from_str(r#"{"name":"example.com"}"#).unwrap();
        assert_eq!(cfg.record_type, DnsRecordType::A);
        assert_eq!(cfg.resolver, None);
        assert_eq!(cfg.resolver_port, 53);
        assert_eq!(cfg.max_depth, 8);
        assert_eq!(cfg.timeout_ms, 3000);
        assert_eq!(cfg, DnsCheckConfig::new("example.com"));
    }

    #[test]
    fn dns_record_type_round_trips_token() {
        for rt in [DnsRecordType::A, DnsRecordType::Aaaa, DnsRecordType::Cname] {
            assert_eq!(DnsRecordType::from_token(rt.as_str()), Some(rt));
        }
        assert_eq!(DnsRecordType::from_token("aaaa"), Some(DnsRecordType::Aaaa));
        assert_eq!(DnsRecordType::from_token("MX"), None);
    }

    #[test]
    fn dns_failure_serializes_tagged() {
        let json = serde_json::to_string(&DnsFailure::NxDomain).unwrap();
        assert_eq!(json, r#"{"kind":"nx_domain"}"#);
        let json = serde_json::to_string(&DnsFailure::OtherRcode { rcode: 9 }).unwrap();
        assert_eq!(json, r#"{"kind":"other_rcode","rcode":9}"#);
        let back: DnsFailure = serde_json::from_str(json.as_str()).unwrap();
        assert_eq!(back, DnsFailure::OtherRcode { rcode: 9 });
    }

    #[test]
    fn dns_record_serializes_tagged() {
        let json = serde_json::to_string(&DnsRecord::A {
            addr: "10.1.2.3".parse().unwrap(),
        })
        .unwrap();
        assert_eq!(json, r#"{"kind":"a","addr":"10.1.2.3"}"#);
    }

    #[test]
    fn metric_names_are_valid() {
        for m in [
            METRIC_DNS_UP,
            METRIC_DNS_RESOLVE_MS,
            METRIC_DNS_CHAIN_LENGTH,
            METRIC_DNS_ANSWER_COUNT,
        ] {
            assert!(crate::is_valid_metric_name(m), "{m} must be TSDB-safe");
        }
    }

    // ── Name normalization / validation ──────────────────────────────────────────────

    #[test]
    fn normalize_dns_name_lowercases_and_strips_trailing_dot() {
        assert_eq!(normalize_dns_name("  HorryWorks.NET.  "), "horryworks.net");
        assert_eq!(normalize_dns_name("horry.net"), "horry.net");
    }

    #[test]
    fn validate_dns_name_accepts_real_targets() {
        assert_eq!(
            validate_dns_name("HorryWorks.net."),
            Some("horryworks.net".into())
        );
        assert_eq!(
            validate_dns_name("_dmarc.example.com"),
            Some("_dmarc.example.com".into())
        );
        assert_eq!(validate_dns_name("a-b.example"), Some("a-b.example".into()));
        assert!(validate_dns_name(&format!("{}.example", "a".repeat(63))).is_some());
    }

    #[test]
    fn validate_dns_name_rejects_malformed() {
        assert_eq!(validate_dns_name(""), None);
        assert_eq!(validate_dns_name("   "), None);
        assert_eq!(validate_dns_name("a..b"), None);
        assert_eq!(validate_dns_name("-lead.example"), None);
        assert_eq!(validate_dns_name("trail-.example"), None);
        assert_eq!(validate_dns_name("has space.example"), None);
        assert_eq!(validate_dns_name("emoji.\u{2603}"), None);
        assert_eq!(
            validate_dns_name(&format!("{}.example", "a".repeat(64))),
            None
        );
        let too_long = format!("{}.example", ["abcdefghij"; 26].join("."));
        assert!(too_long.len() > MAX_NAME_LEN);
        assert_eq!(validate_dns_name(&too_long), None);
    }

    // ── SSRF ─────────────────────────────────────────────────────────────────────────

    #[test]
    fn resolver_ssrf_policy_matches_url_check() {
        // One policy, shared on purpose — an NMS monitors internal DNS servers, but must never be
        // steered at loopback or the cloud metadata address.
        let blocked = [
            "127.0.0.1",
            "169.254.169.254",
            "0.0.0.0",
            "224.0.0.1",
            "::1",
            "::ffff:127.0.0.1",
            "fe80::1",
        ];
        for ip in blocked {
            let parsed: IpAddr = ip.parse().unwrap();
            assert!(is_resolver_blocked(parsed), "{ip} must be refused");
            assert_eq!(
                is_resolver_blocked(parsed),
                crate::url_check::is_ssrf_blocked(parsed),
                "{ip} must follow the URL-monitor policy exactly"
            );
        }

        let allowed = [
            "10.0.0.53",
            "192.168.1.1",
            "172.16.0.53",
            "8.8.8.8",
            "2001:db8::1",
        ];
        for ip in allowed {
            let parsed: IpAddr = ip.parse().unwrap();
            assert!(!is_resolver_blocked(parsed), "{ip} must be allowed");
            assert_eq!(
                is_resolver_blocked(parsed),
                crate::url_check::is_ssrf_blocked(parsed),
                "{ip} must follow the URL-monitor policy exactly"
            );
        }
    }
}
