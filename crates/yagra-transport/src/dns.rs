// SPDX-License-Identifier: AGPL-3.0-only
//! DNS name-resolution probe: resolve a name through one recursive resolver and record the whole
//! CNAME chain, the way `dig` shows its ANSWER section (ADR-033).
//!
//! ## Why the resolver is configured the way it is
//!
//! Three [`ResolverOpts`] settings are load-bearing and easy to get wrong:
//!
//! * `cache_size = 0` — with hickory's cache on we would be monitoring *our own cache*, and a
//!   changed record would stay invisible until its TTL expired. That defeats the entire feature.
//! * `preserve_intermediates = true` — keeps the CNAME records in the answer instead of returning
//!   only the terminal address. Without it there is no chain to record.
//! * `ndots = 0` plus an explicitly-rooted name — otherwise the container's `search` domains get
//!   appended and we would silently resolve `horryworks.net.my-cluster.local`.
//!
//! ## What is pure and what does I/O
//!
//! Everything decisional lives in a pure function, because the socket path **cannot** be unit
//! tested here: our own SSRF policy refuses loopback, so a localhost stub resolver is unreachable
//! by design. [`chain_from_answers`] therefore takes an already-fetched answer set, and the tests
//! below drive it directly.

use crate::{DnsProbeSpec, TransportError};
use hickory_resolver::config::{NameServerConfig, ResolverConfig, ResolverOpts};
use hickory_resolver::net::runtime::TokioRuntimeProvider;
use hickory_resolver::net::{DnsError, NetError};
use hickory_resolver::proto::op::ResponseCode;
use hickory_resolver::proto::rr::{RData, Record, RecordType};
use hickory_resolver::{Resolver, TokioResolver};
use std::collections::BTreeSet;
use std::net::SocketAddr;
use std::time::{Duration, Instant};
use yagra_common::{
    normalize_dns_name, DnsAnswer, DnsChain, DnsFailure, DnsHop, DnsRecord, DnsRecordType,
};

/// Resolve `spec.name`, walking the CNAME chain, and return the observed chain.
///
/// A DNS-level failure is reported **inside** the chain (`failure = Some(..)`), never as `Err` —
/// the same contract as `probe_http`. `Err` is reserved for a check that cannot be run at all:
/// an unusable name, an SSRF-blocked resolver, or no system resolver to fall back on.
pub(crate) async fn resolve_dns(
    spec: &DnsProbeSpec,
    timeout: Duration,
) -> Result<DnsChain, TransportError> {
    let name = yagra_common::validate_dns_name(&spec.name)
        .ok_or_else(|| TransportError::Io(format!("invalid DNS name: {}", spec.name)))?;

    // Refuse a blocked resolver *before* opening any socket (defense in depth: core's API edge
    // already checked, but the transport is the last gate).
    if let Some(addr) = spec.resolver {
        if yagra_common::is_resolver_blocked(addr.ip()) {
            return Err(TransportError::Io(format!(
                "resolver address is not allowed: {}",
                addr.ip()
            )));
        }
    }

    let resolver = build_resolver(spec.resolver, timeout)?;
    let started = Instant::now();
    let walk = tokio::time::timeout(
        timeout,
        walk_chain(&resolver, &name, spec.record_type, spec.max_depth),
    )
    .await;

    // A blown overall budget is a timeout regardless of how far the walk had got.
    let (hops, failure) = match walk {
        Ok(outcome) => outcome,
        Err(_elapsed) => (Vec::new(), Some(DnsFailure::Timeout)),
    };

    let mut chain = DnsChain {
        query: name,
        record_type: spec.record_type,
        resolver: spec
            .resolver
            .map_or_else(|| "system".to_owned(), |a| a.to_string()),
        hops,
        failure,
        resolve_ms: started.elapsed().as_secs_f64() * 1000.0,
    };
    // Canonicalize before the chain leaves the poller, so the canonical form is what travels the
    // bus, what is stored, and what the change key is computed from.
    chain.canonicalize();
    Ok(chain)
}

/// Build a resolver aimed at `resolver`, or at the system resolver when `None`.
fn build_resolver(
    resolver: Option<SocketAddr>,
    timeout: Duration,
) -> Result<TokioResolver, TransportError> {
    let provider = TokioRuntimeProvider::default();
    let mut builder = match resolver {
        Some(addr) => {
            let mut ns = NameServerConfig::udp_and_tcp(addr.ip());
            // The port lives on each connection config and hickory's helpers hardcode 53, so a
            // non-standard resolver port has to be stamped on before the server is registered.
            for conn in &mut ns.connections {
                conn.port = addr.port();
            }
            // Built from parts with no domain and no search list: the operator named an exact
            // resolver, so nothing about the host's resolv.conf should leak into this query.
            let config = ResolverConfig::from_parts(None, Vec::new(), vec![ns]);
            Resolver::builder_with_config(config, provider)
        }
        None => Resolver::builder(provider)
            .map_err(|e| TransportError::Io(format!("no system DNS resolver available: {e}")))?,
    };

    *builder.options_mut() = probe_options(timeout);
    builder
        .build()
        .map_err(|e| TransportError::Io(format!("dns resolver build failed: {e}")))
}

/// The options every DNS monitor probe runs with. See the module docs for why each matters.
fn probe_options(timeout: Duration) -> ResolverOpts {
    let mut opts = ResolverOpts::default();
    // Measure DNS, not our own cache.
    opts.cache_size = 0;
    // Keep the CNAME records so there is a chain to record.
    opts.preserve_intermediates = true;
    // Never let the container's search domains rewrite the operator's name.
    opts.ndots = 0;
    // We are asking a recursive resolver; we want it to do the recursion.
    opts.recursion_desired = true;
    // One retry inside a bounded per-query slice; the overall budget is enforced by the caller.
    opts.attempts = 1;
    opts.timeout = per_query_budget(timeout);
    opts
}

/// Per-query slice of the check's total budget. Bounded so one slow hop cannot consume the whole
/// walk, and floored so a short budget still gets a usable query window.
fn per_query_budget(total: Duration) -> Duration {
    const FLOOR: Duration = Duration::from_millis(200);
    const CEILING: Duration = Duration::from_secs(2);
    (total / 2).clamp(FLOOR, CEILING)
}

/// Walk `name` to a terminal record set of `want`, following CNAMEs.
///
/// A recursive resolver normally returns the entire chain (CNAMEs plus the final address records)
/// in one answer, so `chain_from_answers` usually consumes everything in a single round trip; the
/// loop only issues a second query when the answer stops at a CNAME whose target it didn't include.
async fn walk_chain(
    resolver: &TokioResolver,
    name: &str,
    want: DnsRecordType,
    max_depth: u8,
) -> (Vec<DnsHop>, Option<DnsFailure>) {
    let record_type = to_hickory_type(want);
    let mut hops: Vec<DnsHop> = Vec::new();
    let mut visited: BTreeSet<String> = BTreeSet::new();
    let mut current = normalize_dns_name(name);

    loop {
        if hops.len() >= usize::from(max_depth) {
            return (hops, Some(DnsFailure::DepthExceeded { max_depth }));
        }
        if !visited.insert(current.clone()) {
            return (hops, Some(DnsFailure::LoopDetected { at: current }));
        }

        // Query the rooted form so no search domain is ever appended.
        let query_name = format!("{current}.");
        match resolver.lookup(query_name.as_str(), record_type).await {
            Ok(lookup) => {
                let (mut new_hops, next) = chain_from_answers(&current, lookup.answers(), want);
                hops.append(&mut new_hops);
                match next {
                    // Answered fully: either the terminal records, or nothing more to chase.
                    None => {
                        return if hops
                            .iter()
                            .any(|h| h.answers.iter().any(|a| a.record.record_type() == want))
                        {
                            (hops, None)
                        } else {
                            (hops, Some(DnsFailure::NoData))
                        }
                    }
                    Some(target) => current = target,
                }
            }
            Err(err) => return (hops, Some(classify_failure(&err))),
        }
    }
}

/// Fold one answer section into the hops it covers, starting at `current`.
///
/// Returns the hops built locally plus the next name to query, if the chain ended on a CNAME whose
/// target this answer did not also resolve. Only the ANSWER section is represented (that is what
/// `dig` shows); AUTHORITY and ADDITIONAL are deliberately dropped.
fn chain_from_answers(
    current: &str,
    answers: &[Record],
    want: DnsRecordType,
) -> (Vec<DnsHop>, Option<String>) {
    let mut hops = Vec::new();
    let mut name = normalize_dns_name(current);
    let mut guard = 0usize;

    loop {
        // Everything in this answer whose owner is the name we are currently at.
        let owned: Vec<DnsAnswer> = answers
            .iter()
            .filter(|r| normalize_dns_name(&r.name.to_ascii()) == name)
            .filter_map(to_answer)
            .collect();

        if owned.is_empty() {
            // Nothing here for this name: the resolver stopped short, so the caller must query it.
            return (hops, Some(name));
        }

        let reached_target = owned.iter().any(|a| a.record.record_type() == want);
        let cname_target = owned.iter().find_map(|a| match &a.record {
            DnsRecord::Cname { target } => Some(target.clone()),
            _ => None,
        });

        hops.push(DnsHop {
            name: name.clone(),
            answers: owned,
        });

        // Asking for CNAME means the alias itself is the answer — stop rather than chase it.
        if reached_target {
            return (hops, None);
        }
        match cname_target {
            Some(target) => name = target,
            // No target type and no alias to follow: the caller reports NoData.
            None => return (hops, None),
        }

        // The answer section is finite, but a malformed one could still describe a cycle; bound
        // the local fold independently of the outer depth limit.
        guard += 1;
        if guard > 64 {
            return (hops, None);
        }
    }
}

/// Convert one hickory record into our wire form, dropping types we don't model.
fn to_answer(record: &Record) -> Option<DnsAnswer> {
    let dns_record = match &record.data {
        RData::A(a) => DnsRecord::A { addr: a.0 },
        RData::AAAA(a) => DnsRecord::Aaaa { addr: a.0 },
        RData::CNAME(c) => DnsRecord::Cname {
            target: normalize_dns_name(&c.0.to_ascii()),
        },
        _ => return None,
    };
    Some(DnsAnswer {
        record: dns_record,
        ttl: record.ttl,
    })
}

/// Map a resolver error onto the failure we record.
///
/// The distinction that matters downstream is timeout (no answer at all ⇒ the node is unreachable)
/// versus an answered-but-negative response (⇒ the node is reachable, `dns_up` is 0).
fn classify_failure(err: &NetError) -> DnsFailure {
    match err {
        NetError::Timeout => DnsFailure::Timeout,
        NetError::Dns(DnsError::NoRecordsFound(no_records)) => {
            match no_records.response_code {
                ResponseCode::NXDomain => DnsFailure::NxDomain,
                // NOERROR with no records is the classic "name exists, type doesn't".
                ResponseCode::NoError => DnsFailure::NoData,
                other => rcode_failure(other),
            }
        }
        NetError::Dns(DnsError::ResponseCode(code)) => rcode_failure(*code),
        NetError::Proto(_) => DnsFailure::Malformed,
        _ => DnsFailure::Timeout,
    }
}

/// Map a raw response code onto a failure, keeping unknown codes numerically.
fn rcode_failure(code: ResponseCode) -> DnsFailure {
    match code {
        ResponseCode::NXDomain => DnsFailure::NxDomain,
        ResponseCode::ServFail => DnsFailure::ServFail,
        ResponseCode::Refused => DnsFailure::Refused,
        ResponseCode::NoError => DnsFailure::NoData,
        other => DnsFailure::OtherRcode {
            rcode: u16::from(other),
        },
    }
}

/// Our record type as hickory's.
const fn to_hickory_type(t: DnsRecordType) -> RecordType {
    match t {
        DnsRecordType::A => RecordType::A,
        DnsRecordType::Aaaa => RecordType::AAAA,
        DnsRecordType::Cname => RecordType::CNAME,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hickory_resolver::proto::rr::rdata::{A, AAAA, CNAME};
    use hickory_resolver::proto::rr::Name;
    use std::str::FromStr;

    fn rec(name: &str, ttl: u32, data: RData) -> Record {
        Record::from_rdata(Name::from_str(name).unwrap(), ttl, data)
    }

    fn a_rec(name: &str, addr: &str) -> Record {
        rec(name, 60, RData::A(A(addr.parse().unwrap())))
    }

    fn cname_rec(name: &str, target: &str) -> Record {
        rec(
            name,
            300,
            RData::CNAME(CNAME(Name::from_str(target).unwrap())),
        )
    }

    // ── chain_from_answers: the CNAME walk ───────────────────────────────────────────

    #[test]
    fn builds_two_hops_from_one_cname_plus_a_answer() {
        // The common case: a recursive resolver returns the whole chain in ONE response, so the
        // walk must consume it without a second query. This is what keeps a DNS monitor at one
        // round trip while still recording each hop separately.
        let answers = vec![
            cname_rec("horryworks.net.", "horry.net."),
            a_rec("horry.net.", "10.1.2.3"),
        ];
        let (hops, next) = chain_from_answers("horryworks.net", &answers, DnsRecordType::A);

        assert_eq!(next, None, "no follow-up query should be needed");
        assert_eq!(hops.len(), 2);
        assert_eq!(hops[0].name, "horryworks.net");
        assert_eq!(
            hops[0].answers[0].record,
            DnsRecord::Cname {
                target: "horry.net".into()
            }
        );
        assert_eq!(hops[1].name, "horry.net");
        assert_eq!(
            hops[1].answers[0].record,
            DnsRecord::A {
                addr: "10.1.2.3".parse().unwrap()
            }
        );
    }

    #[test]
    fn requests_next_query_when_cname_target_absent() {
        // The resolver returned the alias but not its target — we must ask again.
        let answers = vec![cname_rec("horryworks.net.", "horry.net.")];
        let (hops, next) = chain_from_answers("horryworks.net", &answers, DnsRecordType::A);
        assert_eq!(hops.len(), 1);
        assert_eq!(next.as_deref(), Some("horry.net"));
    }

    #[test]
    fn single_hop_direct_answer_has_no_next() {
        let answers = vec![a_rec("horry.net.", "10.1.2.3")];
        let (hops, next) = chain_from_answers("horry.net", &answers, DnsRecordType::A);
        assert_eq!(hops.len(), 1);
        assert_eq!(next, None);
    }

    #[test]
    fn empty_answer_asks_the_caller_to_query_the_name() {
        let (hops, next) = chain_from_answers("horry.net", &[], DnsRecordType::A);
        assert!(hops.is_empty());
        assert_eq!(next.as_deref(), Some("horry.net"));
    }

    #[test]
    fn cname_query_stops_at_the_alias_instead_of_chasing_it() {
        // Asking for CNAME means "what is this an alias for?" — the alias IS the answer.
        let answers = vec![
            cname_rec("horryworks.net.", "horry.net."),
            a_rec("horry.net.", "10.1.2.3"),
        ];
        let (hops, next) = chain_from_answers("horryworks.net", &answers, DnsRecordType::Cname);
        assert_eq!(next, None);
        assert_eq!(hops.len(), 1, "must not chase on to the address");
    }

    #[test]
    fn owner_matching_is_case_and_root_dot_insensitive() {
        // Resolvers using 0x20 randomization echo the question back in mixed case.
        let answers = vec![a_rec("HoRrY.NeT.", "10.1.2.3")];
        let (hops, next) = chain_from_answers("horry.net", &answers, DnsRecordType::A);
        assert_eq!(next, None);
        assert_eq!(hops.len(), 1);
        assert_eq!(hops[0].name, "horry.net");
    }

    #[test]
    fn unmodelled_record_types_are_dropped_not_fatal() {
        let answers = vec![
            rec(
                "horry.net.",
                60,
                RData::AAAA(AAAA("2001:db8::1".parse().unwrap())),
            ),
            a_rec("horry.net.", "10.1.2.3"),
        ];
        let (hops, _) = chain_from_answers("horry.net", &answers, DnsRecordType::A);
        assert_eq!(hops[0].answers.len(), 2, "A and AAAA are both modelled");
    }

    // ── Failure classification ───────────────────────────────────────────────────────

    #[test]
    fn rcode_failure_maps_the_negative_responses() {
        assert_eq!(rcode_failure(ResponseCode::NXDomain), DnsFailure::NxDomain);
        assert_eq!(rcode_failure(ResponseCode::ServFail), DnsFailure::ServFail);
        assert_eq!(rcode_failure(ResponseCode::Refused), DnsFailure::Refused);
        assert_eq!(rcode_failure(ResponseCode::NoError), DnsFailure::NoData);
        // An unrecognized code keeps its number rather than being flattened away.
        assert!(matches!(
            rcode_failure(ResponseCode::NotAuth),
            DnsFailure::OtherRcode { .. }
        ));
    }

    #[test]
    fn timeout_is_classified_distinctly_from_a_negative_answer() {
        // This distinction drives the node state: a timeout is Unreachable, NXDOMAIN is Reachable
        // with dns_up = 0.
        assert_eq!(classify_failure(&NetError::Timeout), DnsFailure::Timeout);
        assert_eq!(
            classify_failure(&NetError::Dns(DnsError::ResponseCode(
                ResponseCode::ServFail
            ))),
            DnsFailure::ServFail
        );
    }

    // ── Budget + options ─────────────────────────────────────────────────────────────

    #[test]
    fn per_query_budget_is_clamped_both_ways() {
        assert_eq!(
            per_query_budget(Duration::from_millis(100)),
            Duration::from_millis(200),
            "a tiny budget still gets a usable query window"
        );
        assert_eq!(
            per_query_budget(Duration::from_secs(3)),
            Duration::from_millis(1500)
        );
        assert_eq!(
            per_query_budget(Duration::from_secs(30)),
            Duration::from_secs(2),
            "one hop must not be able to eat the whole walk"
        );
    }

    #[test]
    fn probe_options_disable_caching_and_search_domains() {
        // Each of these silently breaks the feature if it regresses, so pin them.
        let opts = probe_options(Duration::from_secs(3));
        assert_eq!(
            opts.cache_size, 0,
            "caching would monitor our cache, not DNS"
        );
        assert!(
            opts.preserve_intermediates,
            "without intermediates there is no CNAME chain to record"
        );
        assert_eq!(opts.ndots, 0, "search domains must never rewrite the name");
        assert!(opts.recursion_desired);
    }

    // ── Un-runnable configs must be Err, not a failed chain ──────────────────────────

    #[tokio::test]
    async fn errs_on_unparseable_name() {
        let spec = DnsProbeSpec {
            name: "not a dns name".into(),
            record_type: DnsRecordType::A,
            resolver: None,
            max_depth: 8,
        };
        let err = resolve_dns(&spec, Duration::from_millis(500)).await;
        assert!(
            err.is_err(),
            "a malformed name is a config error, not a chain"
        );
    }

    #[tokio::test]
    async fn errs_on_blocked_resolver_before_opening_a_socket() {
        // Loopback and the cloud metadata address are the SSRF escalation surface. The check runs
        // before the resolver is built, so no packet is ever sent.
        for blocked in ["127.0.0.1:53", "169.254.169.254:53", "[::1]:53"] {
            let spec = DnsProbeSpec {
                name: "horryworks.net".into(),
                record_type: DnsRecordType::A,
                resolver: Some(blocked.parse().unwrap()),
                max_depth: 8,
            };
            let out = resolve_dns(&spec, Duration::from_millis(500)).await;
            assert!(out.is_err(), "{blocked} must be refused");
        }
    }
}
