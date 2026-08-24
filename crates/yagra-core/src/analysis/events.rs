// SPDX-License-Identifier: AGPL-3.0-only
//! The analyses that read passive events: storm, flap, severity shift, rule gap, auth probe
//! (ADR-022 Increment 2, ADR-024, ADR-089).
//!
//! 🚨 **None of these may touch `self.events` directly.** Once a log store is configured,
//! PostgreSQL holds only the alert-linked subset, so a direct read answers about a fraction — and
//! `rule_gap`, which looks for *unmatched* events, about the empty set. Every count goes through
//! the `agg_*` routers in `mod.rs`; `guards::every_event_analysis_reads_through_the_store_router`
//! reads this file as text to enforce it.

use super::*;

impl Engine {
    // ── Engine: Event Storm (passive) ─────────────────────────────────────────────
    //
    // Per node: bucket the passive-event volume, learn a baseline rate, and flag a recent bucket
    // whose count spikes past the sensitivity σ (boot loop, interface churn, chatty misconfig).
    pub(super) async fn run_event_storm(
        &self,
        id: Uuid,
        params: &JobParams,
        node_ids: &[Uuid],
        names: &HashMap<Uuid, String>,
        cancel: &AtomicBool,
    ) -> anyhow::Result<Option<(Vec<NewFinding>, String)>> {
        let to = now_s();
        let from = to - params.baseline_secs.max(6 * 3600);
        let recent_cutoff = to - params.window_secs.max(600);
        let sigma = params.sensitivity.max(0.5);
        self.progress(id, 25, "Reading event volume…").await;
        if cancel.load(Ordering::Relaxed) {
            return Ok(None);
        }
        let window = Self::scoped_window(params, node_ids, from, to);
        let rows = self
            .agg_counts_by_bucket(&window, EVENT_BUCKET_SECS)
            .await?;
        // The store restricts to the caller's scope; this fold additionally honours `node_cap`,
        // which bounds how many nodes a `quick`/`standard` run scores at all.
        let scope: HashSet<Uuid> = node_ids.iter().copied().collect();
        // node → (baseline bucket counts, recent bucket counts with their bucket time).
        let mut per_node: HashMap<Uuid, StormBuckets> = HashMap::new();
        for r in rows {
            if !scope.contains(&r.node_id) {
                continue;
            }
            let e = per_node.entry(r.node_id).or_default();
            if r.bucket_start_s < recent_cutoff {
                e.0.push(r.count as f64);
            } else {
                e.1.push((r.bucket_start_s, r.count as f64));
            }
        }
        self.progress(id, 75, "Scoring volume spikes…").await;
        let mut findings: Vec<NewFinding> = Vec::new();
        for (node, (baseline, recent)) in per_node {
            let (peak_bucket, peak) =
                recent
                    .iter()
                    .copied()
                    .fold((0i64, 0f64), |a, (t, c)| if c > a.1 { (t, c) } else { a });
            if peak < EVENT_STORM_FLOOR {
                continue;
            }
            let Some(score) = burst_score(&baseline, peak, sigma) else {
                continue;
            };
            findings.push(NewFinding {
                score,
                severity: severity_for(score).to_owned(),
                node_id: Some(node),
                node_name: name_lookup(names, &node),
                metric: "event_rate".to_owned(),
                kind: "event_storm".to_owned(),
                when_label: rel_label(peak_bucket, to),
                duration: format!("{peak:.0} in {}m", EVENT_BUCKET_SECS / 60),
                detail: storm_detail(peak, mean(&baseline), peak_bucket),
            });
        }
        finalize(&mut findings);
        let summary = format!("{} nodes with event-volume spikes", findings.len());
        Ok(Some((findings, summary)))
    }

    // ── Engine: Event Flap (passive) ──────────────────────────────────────────────
    //
    // Repeated fire↔clear of the same event rule per node (linkDown/linkUp thrash, BGP session
    // churn) — complements the ICMP-only `flap`. A completed cycle is one fire paired with a clear.
    pub(super) async fn run_event_flap(
        &self,
        id: Uuid,
        params: &JobParams,
        node_ids: &[Uuid],
        names: &HashMap<Uuid, String>,
        cancel: &AtomicBool,
    ) -> anyhow::Result<Option<(Vec<NewFinding>, String)>> {
        let to = now_s();
        let from = to - params.window_secs.max(6 * 3600);
        self.progress(id, 30, "Reading event churn…").await;
        if cancel.load(Ordering::Relaxed) {
            return Ok(None);
        }
        let stats = self.events.flap_stats(from * 1000, to * 1000).await?;
        let scope: HashSet<Uuid> = node_ids.iter().copied().collect();
        let window_hours = ((to - from) as f64 / 3600.0).max(1.0);
        let mut findings: Vec<NewFinding> = Vec::new();
        for s in stats {
            if !scope.contains(&s.node_id) {
                continue;
            }
            let cycles = s.fires.min(s.clears);
            if cycles < 2 {
                continue;
            }
            let score = flap_score(u32::try_from(cycles).unwrap_or(u32::MAX));
            let rate = cycles as f64 / window_hours;
            findings.push(NewFinding {
                score,
                severity: severity_for(score).to_owned(),
                node_id: Some(s.node_id),
                node_name: name_lookup(names, &s.node_id),
                metric: format!("event:{}", s.rule_name),
                kind: "event_flap".to_owned(),
                when_label: format!("{cycles} cycles"),
                duration: format!("{rate:.1}/h"),
                detail: serde_json::json!({
                    "rule_id": s.rule_id, "fires": s.fires, "clears": s.clears,
                    "cycles": cycles, "per_hour": rate,
                }),
            });
        }
        finalize(&mut findings);
        let summary = format!("{} event-flapping (rule, node) pairs", findings.len());
        Ok(Some((findings, summary)))
    }

    // ── Engine: Severity Shift (passive) ──────────────────────────────────────────
    //
    // A node whose syslog severity mix skews toward error/critical in the recent window vs its
    // baseline — a quiet degradation signal.
    pub(super) async fn run_severity_shift(
        &self,
        id: Uuid,
        params: &JobParams,
        node_ids: &[Uuid],
        names: &HashMap<Uuid, String>,
        cancel: &AtomicBool,
    ) -> anyhow::Result<Option<(Vec<NewFinding>, String)>> {
        let to = now_s();
        let from = to - params.baseline_secs.max(6 * 3600);
        let recent_cutoff = to - params.window_secs.max(600);
        self.progress(id, 30, "Reading severity mix…").await;
        if cancel.load(Ordering::Relaxed) {
            return Ok(None);
        }
        let baseline = self
            .agg_severity_counts(&Self::scoped_window(params, node_ids, from, recent_cutoff))
            .await?;
        if cancel.load(Ordering::Relaxed) {
            return Ok(None);
        }
        let recent = self
            .agg_severity_counts(&Self::scoped_window(params, node_ids, recent_cutoff, to))
            .await?;
        // As in `run_event_storm`: the store applies the caller's scope, this fold applies
        // `node_cap`.
        let scope: HashSet<Uuid> = node_ids.iter().copied().collect();
        let base_frac = severity_high_fractions(&baseline, &scope);
        let recent_frac = severity_high_fractions(&recent, &scope);
        let mut findings: Vec<NewFinding> = Vec::new();
        for (node, (rhigh, rtotal, rfrac)) in &recent_frac {
            if *rtotal < SEVERITY_FLOOR {
                continue;
            }
            let bfrac = base_frac.get(node).map_or(0.0, |x| x.2);
            let Some(score) = severity_shift_score(bfrac, *rfrac) else {
                continue;
            };
            findings.push(NewFinding {
                score,
                severity: severity_for(score).to_owned(),
                node_id: Some(*node),
                node_name: name_lookup(names, node),
                metric: "syslog_severity".to_owned(),
                kind: "severity_shift".to_owned(),
                when_label: format!("{:.0}% err+", rfrac * 100.0),
                duration: format!("was {:.0}%", bfrac * 100.0),
                detail: serde_json::json!({
                    "recent_high_frac": rfrac, "baseline_high_frac": bfrac,
                    "recent_high": rhigh, "recent_total": rtotal,
                }),
            });
        }
        finalize(&mut findings);
        let summary = format!("{} nodes with a severity shift", findings.len());
        Ok(Some((findings, summary)))
    }

    // ── Engine: Rule Gap (passive) ────────────────────────────────────────────────
    //
    // High-volume unmatched events clustered by signature (trap OID / syslog app-name): "you're
    // receiving N of these but no rule matches — consider one". Coverage advice, capped at warning.
    pub(super) async fn run_rule_gap(
        &self,
        id: Uuid,
        params: &JobParams,
        node_ids: &[Uuid],
        names: &HashMap<Uuid, String>,
        cancel: &AtomicBool,
    ) -> anyhow::Result<Option<(Vec<NewFinding>, String)>> {
        let to = now_s();
        let from = to - params.window_secs.max(86_400);
        self.progress(id, 35, "Clustering unmatched events…").await;
        if cancel.load(Ordering::Relaxed) {
            return Ok(None);
        }
        let sigs = self
            .agg_unmatched_signatures(&Self::scoped_window(params, node_ids, from, to), 200)
            .await?;
        // No sample-node scope filter here any more: the store already restricted, and filtering on
        // the representative node dropped signatures that *did* occur inside the caller's group
        // whenever some out-of-group node sorted lower. On the log-store path `sample_node` is
        // `None` for every row anyway (LogsQL has no `min(uuid)`), which that filter would have
        // read as "out of scope" and discarded wholesale.
        let mut findings: Vec<NewFinding> = Vec::new();
        for s in sigs {
            if s.count < RULE_GAP_FLOOR {
                continue;
            }
            let score = gap_score(s.count);
            findings.push(NewFinding {
                score,
                severity: severity_for(score).to_owned(),
                node_id: s.sample_node,
                node_name: s
                    .sample_node
                    .map_or_else(|| "fleet".to_owned(), |n| name_lookup(names, &n)),
                metric: format!("{}:{}", s.kind, s.signature),
                kind: "rule_gap".to_owned(),
                when_label: format!("{} events", s.count),
                duration: "unmatched".to_owned(),
                detail: serde_json::json!({
                    "kind": s.kind, "signature": s.signature, "count": s.count,
                }),
            });
        }
        finalize(&mut findings);
        let summary = format!("{} unmatched-event signatures (rule gaps)", findings.len());
        Ok(Some((findings, summary)))
    }

    // ── Engine: Auth Probe (passive) ──────────────────────────────────────────────
    //
    // authenticationFailure traps + auth-failure syslog clustered by source — brute force or a
    // misconfigured NMS hammering SNMP/SSH.
    pub(super) async fn run_auth_probe(
        &self,
        id: Uuid,
        params: &JobParams,
        node_ids: &[Uuid],
        names: &HashMap<Uuid, String>,
        cancel: &AtomicBool,
    ) -> anyhow::Result<Option<(Vec<NewFinding>, String)>> {
        let to = now_s();
        let from = to - params.window_secs.max(3600);
        self.progress(id, 35, "Clustering auth failures…").await;
        if cancel.load(Ordering::Relaxed) {
            return Ok(None);
        }
        let sources = self
            .agg_auth_sources(&Self::scoped_window(params, node_ids, from, to), 100)
            .await?;
        // Same as `run_rule_gap`: the store restricted, so no post-filter on the correlated node.
        // The old one also hid every auth source that mapped to no inventory node at all — which is
        // precisely what an external prober looks like.
        let mut findings: Vec<NewFinding> = Vec::new();
        for s in sources {
            if s.count < AUTH_FLOOR {
                continue;
            }
            let score = auth_score(s.count);
            let src = s.source_ip.clone().unwrap_or_else(|| "unknown".to_owned());
            findings.push(NewFinding {
                score,
                severity: severity_for(score).to_owned(),
                node_id: s.node_id,
                node_name: s
                    .node_id
                    .map_or_else(|| src.clone(), |n| name_lookup(names, &n)),
                metric: "auth_failures".to_owned(),
                kind: "auth_probe".to_owned(),
                when_label: format!("{} failures", s.count),
                duration: src,
                detail: serde_json::json!({ "source_ip": s.source_ip, "count": s.count }),
            });
        }
        finalize(&mut findings);
        let summary = format!("{} auth-failure sources", findings.len());
        Ok(Some((findings, summary)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::testkit::{params, Harness};
    use crate::events::EventFlapStat;

    /// Acceptance first: a node whose event volume bursts past its baseline is reported.
    #[tokio::test]
    async fn a_burst_of_events_is_reported_against_its_node() {
        let to = now_s();
        let h = Harness::new();
        let node = h.inventory.node(1, "chatty-sw");
        let mut buckets: Vec<EventBucketCount> = (0..20)
            .map(|i| EventBucketCount {
                node_id: node,
                bucket_start_s: to - 86_400 + i * 300,
                count: 1,
            })
            .collect();
        buckets.push(EventBucketCount {
            node_id: node,
            bucket_start_s: to - 600,
            count: 50,
        });
        *h.events.buckets.lock().expect("lock") = buckets;

        let (findings, summary) = h
            .engine()
            .run_event_storm(
                Uuid::nil(),
                &params(AnalysisTool::EventStorm),
                &[node],
                &HashMap::from([(node, "chatty-sw".to_owned())]),
                &AtomicBool::new(false),
            )
            .await
            .expect("ok")
            .expect("not cancelled");

        assert_eq!(findings.len(), 1, "{summary}");
        assert_eq!(findings[0].kind, "event_storm");
        assert_eq!(findings[0].metric, "event_rate");
        assert_eq!(findings[0].node_name, "chatty-sw");
        assert!(summary.contains("1 nodes with event-volume spikes"));
    }

    /// 🎯 The store applies the caller's scope, but `node_cap` is applied *here* — a run limited to
    /// N nodes must not score a node outside that set even when the store hands one back. Both
    /// nodes are seeded with a burst, so a fold that dropped everything would fail this too.
    #[tokio::test]
    async fn a_node_outside_the_runs_own_set_is_not_scored() {
        let to = now_s();
        let h = Harness::new();
        let inside = h.inventory.node(1, "inside");
        let outside = h.inventory.node(2, "outside");
        let mut buckets = Vec::new();
        for node in [inside, outside] {
            for i in 0..20 {
                buckets.push(EventBucketCount {
                    node_id: node,
                    bucket_start_s: to - 86_400 + i * 300,
                    count: 1,
                });
            }
            buckets.push(EventBucketCount {
                node_id: node,
                bucket_start_s: to - 600,
                count: 50,
            });
        }
        *h.events.buckets.lock().expect("lock") = buckets;

        let (findings, _) = h
            .engine()
            .run_event_storm(
                Uuid::nil(),
                &params(AnalysisTool::EventStorm),
                &[inside], // the run's resolved set, after node_cap
                &HashMap::new(),
                &AtomicBool::new(false),
            )
            .await
            .expect("ok")
            .expect("not cancelled");

        assert_eq!(
            findings
                .iter()
                .filter_map(|f| f.node_id)
                .collect::<Vec<_>>(),
            vec![inside],
            "only the run's own nodes are scored"
        );
    }

    /// 🎯 A cycle is a fire **paired with** a clear, so the count is the smaller of the two. Nine
    /// fires and three clears is three cycles — reading `fires` would triple the score.
    #[tokio::test]
    async fn an_event_flap_counts_completed_cycles_not_fires() {
        let h = Harness::new();
        let node = h.inventory.node(1, "n");
        *h.events.flaps.lock().expect("lock") = vec![
            EventFlapStat {
                node_id: node,
                rule_id: Uuid::from_u128(0xB1),
                rule_name: "linkDown".to_owned(),
                fires: 9,
                clears: 3,
            },
            EventFlapStat {
                node_id: node,
                rule_id: Uuid::from_u128(0xB2),
                rule_name: "bgpDown".to_owned(),
                fires: 9,
                clears: 1, // one completed cycle — below the floor of two
            },
        ];

        let (findings, _) = h
            .engine()
            .run_event_flap(
                Uuid::nil(),
                &params(AnalysisTool::EventFlap),
                &[node],
                &HashMap::new(),
                &AtomicBool::new(false),
            )
            .await
            .expect("ok")
            .expect("not cancelled");

        assert_eq!(findings.len(), 1, "one rule is above the floor");
        assert_eq!(findings[0].metric, "event:linkDown");
        assert_eq!(findings[0].when_label, "3 cycles");
        assert_eq!(findings[0].detail["fires"], 9);
        assert_eq!(findings[0].detail["cycles"], 3);
    }

    /// A severity mix that skews toward error and worse in the recent window is the finding. The
    /// two windows are two separate store reads, and the fake answers them in order.
    #[tokio::test]
    async fn a_syslog_mix_skewing_to_errors_is_reported() {
        let h = Harness::new();
        let node = h.inventory.node(1, "n");
        *h.events.severities.lock().expect("lock") = vec![
            // Baseline: all informational.
            vec![EventSeverityCount {
                node_id: node,
                severity: 6,
                count: 100,
            }],
            // Recent: three fifths at error or worse.
            vec![
                EventSeverityCount {
                    node_id: node,
                    severity: 3,
                    count: 30,
                },
                EventSeverityCount {
                    node_id: node,
                    severity: 6,
                    count: 20,
                },
            ],
        ];

        let (findings, _) = h
            .engine()
            .run_severity_shift(
                Uuid::nil(),
                &params(AnalysisTool::SeverityShift),
                &[node],
                &HashMap::new(),
                &AtomicBool::new(false),
            )
            .await
            .expect("ok")
            .expect("not cancelled");

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, "severity_shift");
        assert_eq!(findings[0].when_label, "60% err+");
        assert_eq!(findings[0].duration, "was 0%");
    }

    /// An unchanged mix is not a shift. Same shape as the test above with the recent window equal
    /// to the baseline, so what is measured is the comparison and not the plumbing.
    #[tokio::test]
    async fn an_unchanged_severity_mix_is_not_a_shift() {
        let h = Harness::new();
        let node = h.inventory.node(1, "n");
        let same = vec![
            EventSeverityCount {
                node_id: node,
                severity: 3,
                count: 30,
            },
            EventSeverityCount {
                node_id: node,
                severity: 6,
                count: 20,
            },
        ];
        *h.events.severities.lock().expect("lock") = vec![same.clone(), same];

        let (findings, _) = h
            .engine()
            .run_severity_shift(
                Uuid::nil(),
                &params(AnalysisTool::SeverityShift),
                &[node],
                &HashMap::new(),
                &AtomicBool::new(false),
            )
            .await
            .expect("ok")
            .expect("not cancelled");
        assert!(findings.is_empty(), "no delta ⇒ nothing to report");
    }

    /// 🎯 A signature with **no** representative node must survive.
    ///
    /// `rule_gap` used to filter its results by whether the sample node was in scope. That dropped
    /// signatures which genuinely occurred inside the caller's group whenever some out-of-group
    /// node sorted lower — and on the log-store path `sample_node` is `None` for *every* row
    /// (LogsQL has no `min(uuid)`), which the filter read as "out of scope" and discarded
    /// wholesale. Until now the only thing saying so was a comment.
    #[tokio::test]
    async fn an_unmatched_signature_with_no_sample_node_is_still_reported() {
        let h = Harness::new();
        let node = h.inventory.node(1, "n");
        *h.events.signatures.lock().expect("lock") = vec![
            EventSignatureCount {
                kind: "trap".to_owned(),
                signature: "1.3.6.1.6.3.1.1.5.4".to_owned(),
                count: 200,
                sample_node: None, // what the log-store path always returns
            },
            EventSignatureCount {
                kind: "syslog".to_owned(),
                signature: "sshd".to_owned(),
                count: RULE_GAP_FLOOR - 1,
                sample_node: Some(node),
            },
        ];

        let (findings, summary) = h
            .engine()
            .run_rule_gap(
                Uuid::nil(),
                &params(AnalysisTool::RuleGap),
                &[node],
                &HashMap::new(),
                &AtomicBool::new(false),
            )
            .await
            .expect("ok")
            .expect("not cancelled");

        assert_eq!(findings.len(), 1, "{summary}");
        assert_eq!(findings[0].kind, "rule_gap");
        assert_eq!(findings[0].node_id, None);
        assert_eq!(
            findings[0].node_name, "fleet",
            "a signature with no sample node belongs to the fleet, not to nobody"
        );
        assert_eq!(findings[0].metric, "trap:1.3.6.1.6.3.1.1.5.4");
    }

    /// 🎯 An auth-failure source that maps to no inventory node at all must survive — that is
    /// exactly what an external prober looks like, and the old scope filter hid it.
    #[tokio::test]
    async fn an_auth_source_outside_the_inventory_is_still_reported() {
        let h = Harness::new();
        let node = h.inventory.node(1, "n");
        *h.events.auth.lock().expect("lock") = vec![
            EventAuthSource {
                source_ip: Some("203.0.113.9".to_owned()),
                node_id: None, // no inventory node — an outsider
                count: 40,
            },
            EventAuthSource {
                source_ip: Some("10.0.0.5".to_owned()),
                node_id: Some(node),
                count: AUTH_FLOOR - 1,
            },
        ];

        let (findings, _) = h
            .engine()
            .run_auth_probe(
                Uuid::nil(),
                &params(AnalysisTool::AuthProbe),
                &[node],
                &HashMap::new(),
                &AtomicBool::new(false),
            )
            .await
            .expect("ok")
            .expect("not cancelled");

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, "auth_probe");
        assert_eq!(findings[0].node_id, None);
        assert_eq!(
            findings[0].node_name, "203.0.113.9",
            "an unknown source is named by its address"
        );
        assert_eq!(findings[0].duration, "203.0.113.9");
    }

    /// An event store that cannot answer fails the job rather than reporting a healthy fleet.
    /// Silence and health look identical in a finding list, so this must be an error.
    #[tokio::test]
    async fn an_unreachable_event_store_fails_the_run_rather_than_reporting_nothing() {
        let h = Harness::new();
        let node = h.inventory.node(1, "n");
        *h.events.failing.lock().expect("lock") = true;

        let out = h
            .engine()
            .run_event_storm(
                Uuid::nil(),
                &params(AnalysisTool::EventStorm),
                &[node],
                &HashMap::new(),
                &AtomicBool::new(false),
            )
            .await;
        assert!(
            out.is_err(),
            "an unanswerable store is an error, not an empty report"
        );
    }
}
