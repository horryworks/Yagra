// SPDX-License-Identifier: AGPL-3.0-only
//! The checks that need **one round trip and the answer is there**: ICMP, HTTP, DNS.
//!
//! No walk, no credential-shaped session, no second question — the transport is asked once and the
//! samples fall out of the reply. That is the whole reason these three sit together rather than one
//! per file: what they share is the shape of the conversation, not the protocol.
//!
//! Two deliberate asymmetries live here and neither should be "aligned" away:
//!
//! * **HTTP emits `http_response_time_ms` only when the endpoint answered**, because the transport
//!   fills that field on the failure path too and it is then time-to-timeout, not a response time.
//!   Emitting it would draw a flat "slow response" at the timeout value for the whole outage, and a
//!   response-time threshold would page for the incident `http_up` already covers.
//! * **DNS emits `dns_resolve_ms` even when the name does not resolve**, because there the failure
//!   value is how long the resolver took to *answer* NXDOMAIN or SERVFAIL — a real measurement.
//!
//! 🚨 **The DNS arm's `outcome` match is written out variant by variant on purpose.** It drives the
//! liveness state machine, so a wildcard would decide for a failure mode nobody has thought about
//! yet — and it would decide "the device is up", cancelling an outage ICMP had already found.
//!
//! ⚠️ These three were inline in [`super::execute`] until ADR-099: HTTP at 101 lines and DNS at 53,
//! together 47% of the dispatch, owning 19 of this module's tests and reachable by no function name.
//! `guards.rs` is what stops that growing back.

use super::*;

/// Execute an ICMP reachability check: loss is always reported, round-trip time only when a
/// probe came back.
pub(super) async fn execute_icmp(
    job: &PollJob,
    transport: &dyn Transport,
    at_unix_ms: i64,
    icmp: &IcmpCheck,
) -> PollResult {
    let timeout = Duration::from_millis(u64::from(icmp.timeout_ms));
    match transport.probe_icmp(job.target, icmp.count, timeout).await {
        Ok(probe) => {
            let outcome = if probe.reachable {
                CheckOutcome::Reachable
            } else {
                CheckOutcome::Unreachable
            };
            // Echoes on the wire vs. echoes this process got back. Emitted here rather than in
            // `yagra-transport`, which holds no instrument of any kind and is the boundary that
            // keeps it that way — the probe carries the two raw counts instead (ADR-109).
            //
            // 🚨 The point is not this ratio on its own: it is the comparison against what the
            // KERNEL received (`nstat IcmpInEchoReps`). Equal means the loss is the network's;
            // a shortfall means replies are being dropped between the socket and this process,
            // and every one of those costs a full 1 s timeout while holding a concurrency permit.
            metrics::counter!("yagra_icmp_echoes_sent_total").increment(u64::from(probe.sent));
            metrics::counter!("yagra_icmp_echoes_replied_total")
                .increment(u64::from(probe.received));
            let mut samples = vec![Sample::gauge("icmp_loss_pct", probe.loss_pct)];
            if let Some(rtt) = probe.rtt_ms {
                samples.push(Sample::gauge(METRIC_ICMP_RTT_MS, rtt));
            }
            result(job, at_unix_ms, outcome, samples)
        }
        Err(err) => {
            tracing::warn!(job_id = %job.job_id, error = %err, "icmp probe failed");
            result(job, at_unix_ms, CheckOutcome::Error, Vec::new())
        }
    }
}

/// Execute an HTTP/HTTPS monitor: liveness, status, timing, certificate expiry, and — when the
/// monitor carries them — a body rule and operator-named values lifted out of a JSON document
/// (ADR-047).
///
/// `http_up` means **reachable AND the status matched expectation**; a wrong status and an
/// unreachable endpoint both read as 0 so one threshold covers both. The body rule is deliberately
/// *not* folded into it — widening that gauge would retroactively change what every existing
/// `http_up` series meant.
pub(super) async fn execute_http(
    job: &PollJob,
    transport: &dyn Transport,
    at_unix_ms: i64,
    http: &HttpCheck,
) -> PollResult {
    let timeout = Duration::from_millis(u64::from(http.timeout_ms));
    let spec = HttpProbeSpec {
        url: http.url.clone(),
        method: http.method,
        verify_tls: http.verify_tls,
        follow_redirects: http.follow_redirects,
        auth: http.auth.clone(),
        // A monitor with neither body feature never reads the body — the budget is asked
        // for only when there is something to decide with it.
        body_capture_bytes: http.body_capture_bytes(),
    };
    match transport.probe_http(&spec, timeout).await {
        Ok(probe) => {
            // http_up = reachable AND the status matched expectation (down or wrong-status
            // both read as 0 so a single threshold covers them).
            let status_ok = probe
                .status_code
                .is_some_and(|c| http.expected_status.matches(c));
            let up = probe.reachable && status_ok;
            let mut samples = vec![Sample::gauge(METRIC_HTTP_UP, if up { 1.0 } else { 0.0 })];
            if let Some(code) = probe.status_code {
                samples.push(Sample::gauge(METRIC_HTTP_STATUS_CODE, f64::from(code)));
            }
            // Only when the endpoint actually answered. The transport fills
            // `response_time_ms` on the failure path too, but that value is time-to-
            // timeout/connect-failure, not a response time: emitting it would draw a
            // "slow response" at the timeout value for the whole outage, and a
            // response-time threshold would then page for the same incident `http_up`
            // already covers.
            //
            // Deliberately unlike the DNS arm below, which emits `dns_resolve_ms` even
            // when the name does not resolve — there the failure value is how long the
            // resolver took to *answer* NXDOMAIN/SERVFAIL, a real measurement. Do not
            // "align" the two.
            if probe.reachable {
                samples.push(Sample::gauge(
                    METRIC_HTTP_RESPONSE_TIME_MS,
                    probe.response_time_ms,
                ));
            }
            if let Some(days) = probe.cert_days_to_expiry {
                samples.push(Sample::gauge(METRIC_SSL_CERT_DAYS_TO_EXPIRY, days));
            }
            // The content rule, when the monitor carries one and the endpoint answered.
            // Deliberately NOT folded into `http_up`: that gauge's meaning is liveness +
            // status, and widening it here would retroactively change what every existing
            // `http_up` series meant.
            if let (Some(rule), true) = (http.body_match.as_ref(), probe.reachable) {
                // `None` = we could not decide: the body outgrew the budget (or a read
                // failed) and the keyword was not in the prefix. Reported as 0 — the same
                // value as a genuine violation, because the one thing it must never do is
                // read as healthy (ADR-047 決定 3). `http_body_truncated` is what tells the
                // two apart afterwards.
                let verdict = probe
                    .body
                    .as_ref()
                    .and_then(|b| rule.satisfied_by(&b.text, b.truncated));
                if verdict.is_none() {
                    tracing::warn!(
                        job_id = %job.job_id,
                        url = %http.url,
                        max_bytes = http.body_max_bytes,
                        "url monitor body rule could not be decided within its byte budget; \
                         reporting it as unsatisfied"
                    );
                }
                samples.push(Sample::gauge(
                    METRIC_HTTP_BODY_MATCH,
                    if verdict == Some(true) { 1.0 } else { 0.0 },
                ));
                if let Some(body) = probe.body.as_ref() {
                    samples.push(Sample::gauge(
                        METRIC_HTTP_BODY_TRUNCATED,
                        if body.truncated { 1.0 } else { 0.0 },
                    ));
                }
            }
            // Operator-named values lifted out of a JSON body (ADR-047 Inc.3). Parsed once
            // for the whole rule set — a document big enough to matter should not be
            // re-parsed per rule.
            //
            // A truncated body is almost always invalid JSON and so yields nothing, which
            // is the correct answer rather than a special case: half a document cannot be
            // read reliably, and every failure here records **no sample**. Writing 0 would
            // be indistinguishable from the value genuinely being 0 (ADR-047 決定 3).
            if !http.json_extract.is_empty() && probe.reachable {
                match probe
                    .body
                    .as_ref()
                    .and_then(|b| serde_json::from_str::<serde_json::Value>(&b.text).ok())
                {
                    Some(doc) => {
                        for rule in &http.json_extract {
                            match rule.extract(&doc) {
                                Some(v) => samples.push(Sample::gauge(&rule.metric, v)),
                                None => tracing::debug!(
                                    job_id = %job.job_id,
                                    metric = %rule.metric,
                                    path = %rule.path,
                                    "url monitor json path yielded no usable number; recording no sample"
                                ),
                            }
                        }
                    }
                    None => tracing::warn!(
                        job_id = %job.job_id,
                        url = %http.url,
                        truncated = probe.body.as_ref().is_some_and(|b| b.truncated),
                        rules = http.json_extract.len(),
                        "url monitor response body is not valid JSON; recording no extracted metrics"
                    ),
                }
            }
            let outcome = if probe.reachable {
                CheckOutcome::Reachable
            } else {
                CheckOutcome::Unreachable
            };
            result(job, at_unix_ms, outcome, samples)
        }
        Err(err) => {
            // An un-runnable config (bad URL / SSRF-blocked): record http_up = 0 so the
            // series exists and alerts can fire, then report the error outcome.
            tracing::warn!(job_id = %job.job_id, error = %err, "http probe failed");
            result(
                job,
                at_unix_ms,
                CheckOutcome::Error,
                vec![Sample::gauge(METRIC_HTTP_UP, 0.0)],
            )
        }
    }
}

/// Execute a DNS resolution check: whether the name resolves, how long it took, and how long the
/// CNAME chain was (ADR-033).
///
/// "The resolver answered" is reachability; "the answer was absent or negative" is a threshold
/// concern. So NXDOMAIN / SERVFAIL / REFUSED stay `Reachable` with `dns_up = 0`, while a timeout —
/// no answer at all — is `Unreachable`.
pub(super) async fn execute_dns(
    job: &PollJob,
    transport: &dyn Transport,
    at_unix_ms: i64,
    dns: &DnsCheck,
) -> PollResult {
    let timeout = Duration::from_millis(u64::from(dns.timeout_ms));
    let spec = DnsProbeSpec {
        name: dns.name.clone(),
        record_type: dns.record_type,
        resolver: dns
            .resolver
            .map(|ip| std::net::SocketAddr::new(ip, dns.resolver_port)),
        max_depth: dns.max_depth,
    };
    match transport.resolve_dns(&spec, timeout).await {
        Ok(chain) => {
            let up = chain.resolved();
            let samples = vec![
                Sample::gauge(METRIC_DNS_UP, if up { 1.0 } else { 0.0 }),
                Sample::gauge(METRIC_DNS_RESOLVE_MS, chain.resolve_ms),
                // `as f64` on a hop/answer count is lossless at any realistic size (both
                // are capped well below 2^53) and the metric is a float anyway.
                Sample::gauge(METRIC_DNS_CHAIN_LENGTH, chain.hops.len() as f64),
                Sample::gauge(
                    METRIC_DNS_ANSWER_COUNT,
                    chain.terminal_answer_count() as f64,
                ),
            ];
            // Same split as the HTTP arm: "the resolver answered" is reachability, "the
            // answer was absent or negative" is a threshold concern. So NXDOMAIN /
            // SERVFAIL / REFUSED stay Reachable with dns_up = 0 (the seeded critical
            // threshold fires), while a timeout — no answer at all — is Unreachable.
            //
            // 🚨 Listed variant by variant on purpose. This value drives the **liveness
            // state machine** (`alerts/engine.rs::observe` folds every `outcome` into the dwell
            // window), so a wildcard here decides for a failure mode nobody has thought
            // about yet — and it decided `Reachable`, i.e. "the device is up". A new
            // transport-level variant (connection refused, network unreachable, TLS
            // failure on DoT) is exactly the kind that would land there, and it would
            // cancel a real outage ICMP had already found. Adding a `DnsFailure` variant
            // must be a compile error here, not a silent default.
            let outcome = match chain.failure {
                // No answer arrived at all — the only thing that says nothing is there.
                Some(DnsFailure::Timeout) => CheckOutcome::Unreachable,
                // Something answered, so the resolver is alive. What it said is the
                // `dns_up` threshold's problem, not liveness'.
                None
                | Some(
                    DnsFailure::NxDomain
                    | DnsFailure::NoData
                    | DnsFailure::ServFail
                    | DnsFailure::Refused
                    | DnsFailure::OtherRcode { .. }
                    | DnsFailure::LoopDetected { .. }
                    | DnsFailure::DepthExceeded { .. }
                    | DnsFailure::Malformed,
                ) => CheckOutcome::Reachable,
            };
            let mut r = result(job, at_unix_ms, outcome, samples);
            r.dns_chain = Some(chain);
            r
        }
        Err(err) => {
            // An un-runnable config (bad name / SSRF-blocked resolver / no system
            // resolver): record dns_up = 0 so the series exists and alerts can fire.
            tracing::warn!(job_id = %job.job_id, error = %err, "dns probe failed");
            result(
                job,
                at_unix_ms,
                CheckOutcome::Error,
                vec![Sample::gauge(METRIC_DNS_UP, 0.0)],
            )
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::worker::testkit::*;
    use std::net::{IpAddr, Ipv4Addr};
    use uuid::Uuid;
    use yagra_common::NodeId;
    use yagra_transport::FakeTransport;

    fn http_job(check: yagra_bus::HttpCheck) -> PollJob {
        PollJob::http(
            Uuid::nil(),
            NodeId::from(Uuid::nil()),
            IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
            check,
            60,
        )
    }

    fn http_check(url: &str) -> yagra_bus::HttpCheck {
        yagra_bus::HttpCheck {
            url: url.to_owned(),
            method: yagra_common::HttpMethod::Get,
            expected_status: yagra_common::ExpectedStatus::TwoXx,
            verify_tls: true,
            follow_redirects: true,
            timeout_ms: 5000,
            auth: None,
            body_match: None,
            json_extract: Vec::new(),
            body_max_bytes: yagra_common::DEFAULT_BODY_MAX_BYTES,
        }
    }

    /// A reachable 200 whose body is `text`, captured whole or cut short.
    fn http_probe_with_body(text: &str, truncated: bool) -> yagra_transport::HttpProbe {
        yagra_transport::HttpProbe {
            reachable: true,
            status_code: Some(200),
            response_time_ms: 12.0,
            cert_days_to_expiry: None,
            body: Some(yagra_transport::BodyCapture {
                text: text.to_owned(),
                truncated,
            }),
        }
    }

    fn http_check_matching(url: &str, rule: yagra_common::BodyMatch) -> yagra_bus::HttpCheck {
        let mut check = http_check(url);
        check.body_match = Some(rule);
        check
    }

    fn http_check_extracting(url: &str, rules: &[(&str, &str)]) -> yagra_bus::HttpCheck {
        let mut check = http_check(url);
        check.json_extract = rules
            .iter()
            .map(|(metric, path)| yagra_common::JsonExtract {
                metric: (*metric).to_owned(),
                path: (*path).to_owned(),
            })
            .collect();
        check
    }

    const HEALTH_JSON: &str = r#"{"queue":{"depth":42,"lag_s":1.5},"healthy":true,"note":"fine"}"#;

    fn dns_job(check: yagra_bus::DnsCheck) -> PollJob {
        PollJob::dns(
            Uuid::nil(),
            NodeId::from(Uuid::nil()),
            IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            check,
            60,
        )
    }

    fn dns_check(name: &str) -> yagra_bus::DnsCheck {
        yagra_bus::DnsCheck {
            name: name.to_owned(),
            record_type: yagra_common::DnsRecordType::A,
            resolver: None,
            resolver_port: 53,
            max_depth: 8,
            timeout_ms: 3000,
        }
    }

    /// `horryworks.net → CNAME horry.net → A 10.1.2.3`, the shape from the feature request.
    fn resolved_chain() -> yagra_common::DnsChain {
        yagra_common::DnsChain {
            query: "horryworks.net".into(),
            record_type: yagra_common::DnsRecordType::A,
            resolver: "10.0.0.53:53".into(),
            hops: vec![
                yagra_common::DnsHop {
                    name: "horryworks.net".into(),
                    answers: vec![yagra_common::DnsAnswer {
                        record: yagra_common::DnsRecord::Cname {
                            target: "horry.net".into(),
                        },
                        ttl: 300,
                    }],
                },
                yagra_common::DnsHop {
                    name: "horry.net".into(),
                    answers: vec![yagra_common::DnsAnswer {
                        record: yagra_common::DnsRecord::A {
                            addr: "10.1.2.3".parse().unwrap(),
                        },
                        ttl: 60,
                    }],
                },
            ],
            failure: None,
            resolve_ms: 14.0,
        }
    }

    fn failed_chain(failure: yagra_common::DnsFailure) -> yagra_common::DnsChain {
        yagra_common::DnsChain {
            hops: Vec::new(),
            failure: Some(failure),
            ..resolved_chain()
        }
    }

    /// The other direction, which is the half that is easy to forget: an ordinary liveness check
    /// must **not** be marked observational, or it stops driving alerts entirely.
    #[tokio::test]
    async fn an_icmp_result_is_not_observational() {
        let transport = FakeTransport::reachable(1.0);
        let job = PollJob::icmp(
            Uuid::nil(),
            NodeId::from(Uuid::nil()),
            IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
            yagra_bus::IcmpCheck::default(),
            60,
        );
        let r = execute(&job, &transport, 0).await;
        assert!(!r.observational);
        assert!(r.l3.is_none());
    }

    #[tokio::test]
    async fn reachable_probe_yields_rtt_and_loss_samples() {
        let t = FakeTransport::reachable(7.5);
        let r = execute(&icmp_job(), &t, 1_000).await;
        assert_eq!(r.outcome, CheckOutcome::Reachable);
        // Both loss and rtt samples present.
        assert!(r
            .samples
            .iter()
            .any(|s| s.metric == "icmp_rtt_ms" && s.value == 7.5));
        assert!(r
            .samples
            .iter()
            .any(|s| s.metric == "icmp_loss_pct" && s.value == 0.0));
    }

    #[tokio::test]
    async fn unreachable_probe_has_no_rtt_sample() {
        let t = FakeTransport::unreachable();
        let r = execute(&icmp_job(), &t, 1_000).await;
        assert_eq!(r.outcome, CheckOutcome::Unreachable);
        assert!(!r.samples.iter().any(|s| s.metric == "icmp_rtt_ms"));
        assert!(r
            .samples
            .iter()
            .any(|s| s.metric == "icmp_loss_pct" && s.value == 100.0));
    }

    #[tokio::test]
    async fn http_up_when_reachable_and_status_matches() {
        use yagra_transport::HttpProbe;
        let t = FakeTransport::reachable(0.0).with_http(HttpProbe {
            reachable: true,
            status_code: Some(200),
            response_time_ms: 12.0,
            cert_days_to_expiry: Some(45.0),
            body: None,
        });
        let r = execute(&http_job(http_check("https://example.com/")), &t, 1_000).await;
        assert_eq!(r.outcome, CheckOutcome::Reachable);
        assert!(r
            .samples
            .iter()
            .any(|s| s.metric == METRIC_HTTP_UP && s.value == 1.0));
        assert!(r
            .samples
            .iter()
            .any(|s| s.metric == METRIC_HTTP_STATUS_CODE && s.value == 200.0));
        assert!(r
            .samples
            .iter()
            .any(|s| s.metric == METRIC_SSL_CERT_DAYS_TO_EXPIRY && s.value == 45.0));
        // The transport has always measured this; for a long time nothing turned it into a sample,
        // so a URL monitor could say whether the endpoint was up but never whether it was slow.
        assert!(r
            .samples
            .iter()
            .any(|s| s.metric == METRIC_HTTP_RESPONSE_TIME_MS && s.value == 12.0));
    }

    #[tokio::test]
    async fn http_down_when_status_unexpected() {
        use yagra_transport::HttpProbe;
        // Reachable, but a 500 doesn't match the default any-2xx expectation → http_up = 0.
        let t = FakeTransport::reachable(0.0).with_http(HttpProbe {
            reachable: true,
            status_code: Some(500),
            response_time_ms: 8.0,
            cert_days_to_expiry: None,
            body: None,
        });
        let r = execute(&http_job(http_check("https://example.com/")), &t, 1_000).await;
        assert_eq!(r.outcome, CheckOutcome::Reachable);
        assert!(r
            .samples
            .iter()
            .any(|s| s.metric == METRIC_HTTP_UP && s.value == 0.0));
        assert!(r
            .samples
            .iter()
            .any(|s| s.metric == METRIC_HTTP_STATUS_CODE && s.value == 500.0));
        // The endpoint answered — it just answered wrongly — so the response time is a real
        // measurement and must still be recorded. The gate below is on *reachability*, not on
        // `http_up`; conflating the two would blind the latency series for every monitor whose
        // expected-status check is failing, which is exactly when latency is worth seeing.
        assert!(r
            .samples
            .iter()
            .any(|s| s.metric == METRIC_HTTP_RESPONSE_TIME_MS && s.value == 8.0));
    }

    #[tokio::test]
    async fn http_down_and_unreachable_when_no_response() {
        use yagra_transport::HttpProbe;
        // A timeout: the transport still fills `response_time_ms` (elapsed until it gave up), so
        // this fake carries the realistic 5000 ms rather than the plain `unreachable()` fake's 0.0
        // — otherwise dropping the reachability gate would emit a 0.0 nobody would notice.
        let t = FakeTransport::unreachable().with_http(HttpProbe {
            reachable: false,
            status_code: None,
            response_time_ms: 5000.0,
            cert_days_to_expiry: None,
            body: None,
        });
        let r = execute(&http_job(http_check("https://example.com/")), &t, 1_000).await;
        assert_eq!(r.outcome, CheckOutcome::Unreachable);
        assert!(r
            .samples
            .iter()
            .any(|s| s.metric == METRIC_HTTP_UP && s.value == 0.0));
        assert!(!r
            .samples
            .iter()
            .any(|s| s.metric == METRIC_HTTP_STATUS_CODE));
        // No response, no response time. Emitting the timeout duration would draw a flat "slow
        // response" line for the whole outage and let a latency threshold page for the same
        // incident `http_up` already covers.
        assert!(
            !r.samples
                .iter()
                .any(|s| s.metric == METRIC_HTTP_RESPONSE_TIME_MS),
            "an unreachable endpoint must not report a response time"
        );
    }

    // ── URL body keyword matching (ADR-047 Increment 2) ─────────────────────────────

    #[tokio::test]
    async fn a_monitor_with_no_rule_asks_for_no_body_and_reports_no_match_gauge() {
        // The default for every URL monitor that existed before this increment. It must stay
        // byte-identical: no body read (so `http_response_time_ms` keeps meaning the same thing)
        // and no new series appearing on monitors nobody reconfigured.
        let t = FakeTransport::reachable(0.0).with_http(http_probe_with_body("anything", false));
        let r = execute(&http_job(http_check("https://example.com/")), &t, 1_000).await;
        assert_eq!(sample(&r, METRIC_HTTP_BODY_MATCH), None);
        assert_eq!(sample(&r, METRIC_HTTP_BODY_TRUNCATED), None);
    }

    #[tokio::test]
    async fn a_satisfied_rule_reports_one_and_an_untruncated_read() {
        let t = FakeTransport::reachable(0.0)
            .with_http(http_probe_with_body(r#"{"status":"ok"}"#, false));
        let rule = yagra_common::BodyMatch::contains(r#""status":"ok""#);
        let r = execute(
            &http_job(http_check_matching("https://example.com/health", rule)),
            &t,
            1_000,
        )
        .await;
        assert_eq!(sample(&r, METRIC_HTTP_BODY_MATCH), Some(1.0));
        assert_eq!(sample(&r, METRIC_HTTP_BODY_TRUNCATED), Some(0.0));
        // The content rule is its own gauge: a green body must not have altered availability, and
        // a failing one (below) must not silently redefine what `http_up` has always meant.
        assert_eq!(sample(&r, METRIC_HTTP_UP), Some(1.0));
    }

    #[tokio::test]
    async fn a_two_hundred_whose_body_says_it_is_broken_is_caught_without_touching_http_up() {
        // The whole point of the feature: the endpoint answers 200, so `http_up` is 1 and always
        // was — only the body says the service is down.
        let t = FakeTransport::reachable(0.0)
            .with_http(http_probe_with_body("<h1>Database unavailable</h1>", false));
        let rule = yagra_common::BodyMatch {
            pattern: "Database unavailable".to_owned(),
            mode: yagra_common::BodyMatchMode::NotContains,
        };
        let r = execute(
            &http_job(http_check_matching("https://example.com/", rule)),
            &t,
            1_000,
        )
        .await;
        assert_eq!(sample(&r, METRIC_HTTP_UP), Some(1.0));
        assert_eq!(sample(&r, METRIC_HTTP_BODY_MATCH), Some(0.0));
        assert_eq!(
            r.outcome,
            CheckOutcome::Reachable,
            "a content failure is not a liveness failure — the endpoint answered"
        );
    }

    #[tokio::test]
    async fn an_undecidable_rule_reports_zero_and_says_the_body_was_truncated() {
        // The budget ran out before the keyword appeared. Reporting 1 here would be the silent lie
        // ADR-047 決定 3 forbids; reporting 0 alerts, and the truncation gauge is what tells the
        // operator it was the budget rather than the keyword.
        let t = FakeTransport::reachable(0.0).with_http(http_probe_with_body("<html>…", true));
        let rule = yagra_common::BodyMatch::contains("healthy");
        let r = execute(
            &http_job(http_check_matching("https://example.com/", rule)),
            &t,
            1_000,
        )
        .await;
        assert_eq!(sample(&r, METRIC_HTTP_BODY_MATCH), Some(0.0));
        assert_eq!(sample(&r, METRIC_HTTP_BODY_TRUNCATED), Some(1.0));

        // And the direction that would otherwise go unnoticed: a `not_contains` rule must NOT read
        // as satisfied just because the forbidden text was past the cut.
        let quiet = yagra_common::BodyMatch {
            pattern: "unavailable".to_owned(),
            mode: yagra_common::BodyMatchMode::NotContains,
        };
        let r = execute(
            &http_job(http_check_matching("https://example.com/", quiet)),
            &t,
            1_000,
        )
        .await;
        assert_eq!(
            sample(&r, METRIC_HTTP_BODY_MATCH),
            Some(0.0),
            "a truncated body must never report a satisfied not_contains rule"
        );
    }

    #[tokio::test]
    async fn an_unreachable_endpoint_reports_no_body_verdict_at_all() {
        // There is no body to judge, and emitting 0 would double-page for the outage `http_up`
        // already covers — the same reasoning as the response-time gate above.
        let t = FakeTransport::unreachable().with_http(yagra_transport::HttpProbe {
            reachable: false,
            status_code: None,
            response_time_ms: 5000.0,
            cert_days_to_expiry: None,
            body: None,
        });
        let rule = yagra_common::BodyMatch::contains("ok");
        let r = execute(
            &http_job(http_check_matching("https://example.com/", rule)),
            &t,
            1_000,
        )
        .await;
        assert_eq!(sample(&r, METRIC_HTTP_UP), Some(0.0));
        assert_eq!(sample(&r, METRIC_HTTP_BODY_MATCH), None);
        assert_eq!(sample(&r, METRIC_HTTP_BODY_TRUNCATED), None);
    }

    // ── URL JSON extraction (ADR-047 Increment 3) ───────────────────────────────────

    #[tokio::test]
    async fn every_rule_records_its_value_under_the_operators_own_metric_name() {
        let t = FakeTransport::reachable(0.0).with_http(http_probe_with_body(HEALTH_JSON, false));
        let check = http_check_extracting(
            "https://example.com/health",
            &[
                ("queue_depth", "queue.depth"),
                ("queue_lag_seconds", "queue.lag_s"),
                ("service_healthy", "healthy"),
            ],
        );
        let r = execute(&http_job(check), &t, 1_000).await;
        assert_eq!(sample(&r, "queue_depth"), Some(42.0));
        assert_eq!(sample(&r, "queue_lag_seconds"), Some(1.5));
        // A boolean health flag becomes a 0/1 gauge — which wants the same 0.5 threshold bound as
        // every other boolean here (migration 0030's trap).
        assert_eq!(sample(&r, "service_healthy"), Some(1.0));
        // Extraction is additive: it must not disturb what the monitor already reported.
        assert_eq!(sample(&r, METRIC_HTTP_UP), Some(1.0));
        assert_eq!(sample(&r, METRIC_HTTP_RESPONSE_TIME_MS), Some(12.0));
    }

    #[tokio::test]
    async fn a_rule_that_finds_nothing_records_nothing_rather_than_zero() {
        // The ADR-047 決定 3 rule, and the reason `extract` returns an Option: a `0` here would be
        // indistinguishable from the queue genuinely being empty, so a "queue is fine" dashboard
        // would be showing the absence of a reading.
        let t = FakeTransport::reachable(0.0).with_http(http_probe_with_body(HEALTH_JSON, false));
        let check = http_check_extracting(
            "https://example.com/health",
            &[
                ("missing_key", "queue.nope"),
                ("not_a_number", "note"),
                ("good", "queue.depth"),
            ],
        );
        let r = execute(&http_job(check), &t, 1_000).await;
        assert_eq!(sample(&r, "missing_key"), None);
        assert_eq!(sample(&r, "not_a_number"), None);
        // One failing rule must not suppress its siblings.
        assert_eq!(sample(&r, "good"), Some(42.0));
    }

    #[tokio::test]
    async fn a_body_that_is_not_json_records_no_extracted_metrics() {
        let t = FakeTransport::reachable(0.0)
            .with_http(http_probe_with_body("<html>not json</html>", false));
        let check =
            http_check_extracting("https://example.com/", &[("queue_depth", "queue.depth")]);
        let r = execute(&http_job(check), &t, 1_000).await;
        assert_eq!(sample(&r, "queue_depth"), None);
        // Availability is untouched: an HTML page is a perfectly reachable endpoint.
        assert_eq!(sample(&r, METRIC_HTTP_UP), Some(1.0));
    }

    #[tokio::test]
    async fn a_truncated_body_records_no_extracted_metrics() {
        // Half a JSON document does not parse, so this falls out of the parse rather than needing a
        // special case — but it is worth pinning, because the alternative (parsing a prefix
        // leniently) would record numbers from a document we never saw the end of.
        let cut = &HEALTH_JSON[..30];
        let t = FakeTransport::reachable(0.0).with_http(http_probe_with_body(cut, true));
        let check =
            http_check_extracting("https://example.com/", &[("queue_depth", "queue.depth")]);
        let r = execute(&http_job(check), &t, 1_000).await;
        assert_eq!(sample(&r, "queue_depth"), None);
    }

    #[tokio::test]
    async fn an_unreachable_endpoint_records_no_extracted_metrics() {
        let t = FakeTransport::unreachable().with_http(yagra_transport::HttpProbe {
            reachable: false,
            status_code: None,
            response_time_ms: 5000.0,
            cert_days_to_expiry: None,
            body: None,
        });
        let check =
            http_check_extracting("https://example.com/", &[("queue_depth", "queue.depth")]);
        let r = execute(&http_job(check), &t, 1_000).await;
        assert_eq!(sample(&r, "queue_depth"), None);
        assert_eq!(sample(&r, METRIC_HTTP_UP), Some(0.0));
    }

    #[tokio::test]
    async fn extraction_alone_still_opens_the_body() {
        // The budget moved off `BodyMatch` so that a monitor with only extraction has an answer to
        // "how much do I read". If that regressed, the transport would be asked for no body and
        // every rule would silently record nothing.
        let check = http_check_extracting("https://example.com/", &[("q", "queue.depth")]);
        assert!(check.body_match.is_none());
        assert_eq!(
            check.body_capture_bytes(),
            Some(yagra_common::DEFAULT_BODY_MAX_BYTES)
        );
        assert_eq!(
            http_check("https://example.com/").body_capture_bytes(),
            None,
            "neither feature ⇒ the transport never reads the body"
        );
    }

    // ── DNS name-resolution monitoring (ADR-033) ────────────────────────────────────

    #[tokio::test]
    async fn dns_resolved_chain_emits_up_one_and_attaches_the_chain() {
        let t = FakeTransport::reachable(0.0).with_dns(resolved_chain());
        let r = execute(&dns_job(dns_check("horryworks.net")), &t, 1_000).await;

        assert_eq!(r.outcome, CheckOutcome::Reachable);
        assert_eq!(sample(&r, METRIC_DNS_UP), Some(1.0));
        assert_eq!(sample(&r, METRIC_DNS_CHAIN_LENGTH), Some(2.0));
        assert_eq!(sample(&r, METRIC_DNS_ANSWER_COUNT), Some(1.0));
        assert_eq!(sample(&r, METRIC_DNS_RESOLVE_MS), Some(14.0));

        // The chain rides the result so core can persist it; it is NOT expressed as metrics.
        let chain = r.dns_chain.expect("the chain must travel with the result");
        assert_eq!(chain.hops.len(), 2);
        assert_eq!(chain.hops[1].name, "horry.net");
    }

    #[tokio::test]
    async fn dns_nxdomain_emits_up_zero_but_stays_reachable() {
        // The resolver answered — it just said "no such name". Node state stays Ok and the
        // dns_up threshold is what fires, exactly like a reachable URL returning HTTP 500.
        let t = FakeTransport::reachable(0.0)
            .with_dns(failed_chain(yagra_common::DnsFailure::NxDomain));
        let r = execute(&dns_job(dns_check("nope.example")), &t, 1_000).await;

        assert_eq!(r.outcome, CheckOutcome::Reachable);
        assert_eq!(sample(&r, METRIC_DNS_UP), Some(0.0));
        assert_eq!(sample(&r, METRIC_DNS_ANSWER_COUNT), Some(0.0));
        assert!(r.dns_chain.is_some(), "a failed chain is still recorded");
    }

    #[tokio::test]
    async fn dns_timeout_emits_up_zero_and_unreachable() {
        // No answer at all — that, and only that, means the target is unreachable.
        let t =
            FakeTransport::reachable(0.0).with_dns(failed_chain(yagra_common::DnsFailure::Timeout));
        let r = execute(&dns_job(dns_check("horryworks.net")), &t, 1_000).await;

        assert_eq!(r.outcome, CheckOutcome::Unreachable);
        assert_eq!(sample(&r, METRIC_DNS_UP), Some(0.0));
    }

    #[tokio::test]
    async fn dns_servfail_and_refused_stay_reachable() {
        for failure in [
            yagra_common::DnsFailure::ServFail,
            yagra_common::DnsFailure::Refused,
            yagra_common::DnsFailure::NoData,
        ] {
            let t = FakeTransport::reachable(0.0).with_dns(failed_chain(failure.clone()));
            let r = execute(&dns_job(dns_check("horryworks.net")), &t, 1_000).await;
            assert_eq!(r.outcome, CheckOutcome::Reachable, "{failure:?}");
            assert_eq!(sample(&r, METRIC_DNS_UP), Some(0.0), "{failure:?}");
        }
    }

    #[tokio::test]
    async fn dns_samples_are_node_level_gauges_with_valid_metric_names() {
        // Thin-label model (ADR-011): a DNS chain must never become a series label, so every
        // sample it produces has to be a plain node-level gauge.
        let t = FakeTransport::reachable(0.0).with_dns(resolved_chain());
        let r = execute(&dns_job(dns_check("horryworks.net")), &t, 1_000).await;
        assert_eq!(r.samples.len(), 4);
        for s in &r.samples {
            assert!(s.ifindex.is_none(), "{} must be node-level", s.metric);
            assert_eq!(s.kind, MetricKind::Gauge, "{} must be a gauge", s.metric);
            assert!(
                yagra_common::is_valid_metric_name(&s.metric),
                "{} must be TSDB-safe",
                s.metric
            );
        }
    }
}
