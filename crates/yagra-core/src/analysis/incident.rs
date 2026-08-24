// SPDX-License-Identifier: AGPL-3.0-only
//! The cross-store analysis: one node's incident timeline, and the neighbours it reaches
//! (ADR-022 Increment 2, ADR-043, ADR-089).
//!
//! Unlike the flow group this one degrades rather than refusing: it reads flow when the tier is
//! there and simply omits that signal when it is not, which is why
//! [`super::AnalysisTool::needs_flow_tier`] answers `false` for it.
//!
//! [`super::AnalysisRunner::incident_signals`] is `pub(crate)` because the AI-assisted RCA
//! endpoint (ADR-029) assembles its evidence from the same signals rather than from a second
//! correlation of its own.

use super::*;

impl Engine {
    // ── Engine: Incident Correlate (cross-store) ──────────────────────────────────
    //
    // For each node, assemble a cross-signal timeline over the window: a reachability metric anomaly
    // (TSDB), passive events (events store), and the dominant flow (ClickHouse). Emit a finding only
    // when ≥2 signals of ≥2 distinct kinds coincide — the root-cause on-ramp (ADR-029). Single-node
    // for now; topology-neighbour expansion is a follow-up.
    pub(super) async fn run_incident_correlate(
        &self,
        id: Uuid,
        params: &JobParams,
        node_ids: &[Uuid],
        names: &HashMap<Uuid, String>,
        cancel: &AtomicBool,
    ) -> anyhow::Result<Option<(Vec<NewFinding>, String)>> {
        let to = now_s();
        let window = params.window_secs.max(3600);
        let from = to - window;
        let nodes = &node_ids[..node_ids.len().min(INCIDENT_NODE_CAP)];
        let total = nodes.len().max(1);

        // The one-hop neighbourhood, built once per job rather than per node.
        //
        // ⚠️ **Scope rule**: a neighbour may be consulted, scored or named only if it is itself in
        // the job's resolved node set, which was checked against the launching principal at create
        // time. This is the direct analogue of `TopoLinkRepo::list_page`'s "both endpoints visible"
        // rule — one visible end still tells a scoped operator that a node exists outside their
        // scope. The weaker "consult anything, name only what is visible" leaks by inference: the
        // finding's score, its signal count, and whether it is emitted at all would move with data
        // the caller cannot see.
        let authorized: HashSet<Uuid> = node_ids.iter().copied().collect();
        let neighbours = self.incident_neighbourhood(&authorized).await;

        // Memoized signal fetch: each `incident_signals` call is one TSDB read plus one event query
        // plus one ClickHouse query, so a naive expansion would multiply the job's I/O by the fan-out
        // (20 nodes × 4 peers = up to 100 fetches instead of 20). Bounded by `INCIDENT_NODE_CAP`
        // distinct nodes overall.
        let mut cache: HashMap<Uuid, Vec<IncidentSignal>> = HashMap::new();
        let mut findings: Vec<NewFinding> = Vec::new();
        for (i, node) in nodes.iter().enumerate() {
            if cancel.load(Ordering::Relaxed) {
                return Ok(None);
            }
            self.progress(
                id,
                15 + (i * 75 / total) as i32,
                "Assembling incident timeline…",
            )
            .await;
            let own = self.signals_for(&mut cache, *node, params, from, to).await;
            // A node never gets a finding purely from its neighbours: it must show something of its
            // own. This keeps the pre-expansion behaviour as the floor.
            if own.is_empty() {
                continue;
            }

            // Corroborating peers, most severe first, capped.
            let mut peers: Vec<(Uuid, &'static str, Vec<IncidentSignal>)> = Vec::new();
            for (peer, relation) in neighbours.get(node).into_iter().flatten() {
                if cancel.load(Ordering::Relaxed) {
                    return Ok(None);
                }
                let sigs = self.signals_for(&mut cache, *peer, params, from, to).await;
                if sigs.is_empty() || !signals_coincide(&own, &sigs, NEIGHBOUR_COINCIDENCE_SECS) {
                    continue;
                }
                peers.push((*peer, relation, sigs));
            }
            peers.sort_by(|a, b| {
                peak_severity(&b.2)
                    .partial_cmp(&peak_severity(&a.2))
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            peers.truncate(NEIGHBOUR_CAP);

            // Cross-signal evidence required: ≥2 signals across ≥2 kinds. A corroborating peer's
            // signals count toward both — that is what the expansion buys, and why an outage that
            // only shows one kind of symptom locally can now be recognised.
            let mut all: Vec<(Option<Uuid>, &IncidentSignal)> =
                own.iter().map(|s| (None, s)).collect();
            for (peer, _, sigs) in &peers {
                all.extend(sigs.iter().map(|s| (Some(*peer), s)));
            }
            let kinds: HashSet<&str> = all.iter().map(|(_, s)| s.kind).collect();
            if all.len() < 2 || kinds.len() < 2 {
                continue;
            }
            all.sort_by_key(|(_, s)| s.at_s);
            let score = all.iter().map(|(_, s)| s.severity).fold(0.0, f64::max);
            let earliest = all.first().map_or(to, |(_, s)| s.at_s);
            // The subject's own entries carry no `node_id`/`node_name`, so the shape stays purely
            // additive and `format.ts::timelineOf` renders old and new findings unchanged.
            let timeline: Vec<serde_json::Value> = all
                .iter()
                .map(|(peer, s)| {
                    let mut v = serde_json::json!({
                        "at": s.at_s, "kind": s.kind, "label": s.label, "severity": s.severity,
                    });
                    if let (Some(p), Some(obj)) = (peer, v.as_object_mut()) {
                        obj.insert("node_id".into(), serde_json::json!(p));
                        obj.insert("node_name".into(), serde_json::json!(name_lookup(names, p)));
                    }
                    v
                })
                .collect();
            let peer_rows: Vec<serde_json::Value> = peers
                .iter()
                .map(|(p, relation, sigs)| {
                    serde_json::json!({
                        "node_id": p,
                        "node_name": name_lookup(names, p),
                        "relation": relation,
                        "signals": sigs.len(),
                    })
                })
                .collect();
            findings.push(NewFinding {
                score,
                severity: severity_for(score).to_owned(),
                node_id: Some(*node),
                node_name: name_lookup(names, node),
                metric: "incident".to_owned(),
                kind: "incident_correlate".to_owned(),
                when_label: rel_label(earliest, to),
                duration: format!("{} signals", all.len()),
                detail: serde_json::json!({
                    "timeline": timeline,
                    "peers": peer_rows,
                    "peer_count": peer_rows.len(),
                }),
            });
        }
        finalize(&mut findings);
        let summary = format!("{} correlated incidents", findings.len());
        Ok(Some((findings, summary)))
    }

    /// One-hop neighbours per node, restricted to `authorized`, labelled upstream/downstream.
    ///
    /// **Not gated on [`crate::topology_mode::TopologyMode`], deliberately.** That gate exists
    /// because a wrong derived edge *suppresses a real outage*, and silence is unrecoverable;
    /// `incident_correlate` suppresses nothing, so a wrong edge here only adds a peer to a
    /// diagnostic — the noisy direction. Gating on it would ship this dead on every default
    /// deployment (`manual` is the default and is where upgrades land), which is the "built it and
    /// nobody used it" failure ADR-043 exists to fix. `topology_mode.rs` says the same thing from
    /// the other side: nothing outside the read endpoint should branch on the mode.
    ///
    /// Both graphs are unioned, so a hand-authored `parent_id` counts as an edge alongside a
    /// derived one. A node with the "never suppress" opt-out gets no parents from
    /// [`crate::topology_projection::derived_topology`], and that is kept rather than worked
    /// around: "do not reason about this node's upstream" is an operator statement.
    async fn incident_neighbourhood(
        &self,
        authorized: &HashSet<Uuid>,
    ) -> HashMap<Uuid, Vec<(Uuid, &'static str)>> {
        let nodes = match self.inventory.list_nodes().await {
            Ok(n) => n,
            Err(e) => {
                // Degrade to no expansion rather than failing the job: single-node correlation is
                // exactly the behaviour this analysis had before, so it is a safe floor.
                tracing::warn!(error = %e, "incident correlation: reading the inventory failed");
                return HashMap::new();
            }
        };
        let derived = self.graph.derived(&nodes).await;
        let manual = crate::topology_projection::manual_topology(&nodes);
        one_hop_neighbours(&derived, &manual, authorized)
    }

    /// `incident_signals` for one node, memoized for the job.
    ///
    /// Two things this buys. Each call is one TSDB read plus one event query plus one ClickHouse
    /// query, and a node is typically both a subject and some other subject's neighbour — so
    /// without the cache the expansion multiplies the job's I/O by the fan-out. And the cache is
    /// also the bound: at most [`INCIDENT_CACHE_CAP`] distinct nodes are ever fetched, so a hub
    /// with a hundred links cannot turn a bounded job into an unbounded one. A node past the cap
    /// contributes no signals rather than being fetched — the same direction as every other cap
    /// here, less evidence rather than a longer job.
    async fn signals_for(
        &self,
        cache: &mut HashMap<Uuid, Vec<IncidentSignal>>,
        node: Uuid,
        params: &JobParams,
        from: i64,
        to: i64,
    ) -> Vec<IncidentSignal> {
        if let Some(hit) = cache.get(&node) {
            return hit.clone();
        }
        if cache.len() >= INCIDENT_CACHE_CAP {
            return Vec::new();
        }
        let sigs = self
            .incident_signals(
                node,
                from,
                to,
                params.window_secs.max(600),
                params.sensitivity.max(2.0),
            )
            .await;
        cache.insert(node, sigs.clone());
        sigs
    }

    /// Assemble one node's cross-signal timeline over `[from_s, to_s]`: a reachability metric
    /// anomaly (TSDB), its recent passive events (event store), and the dominant flow conversation
    /// (ClickHouse, when the flow tier is on). Signals are unordered and unfiltered — the caller
    /// decides how much evidence is enough.
    ///
    /// Shared by the `incident_correlate` analysis and the LLM RCA context builder (ADR-029) so
    /// there is exactly one definition of "what this node's incident looked like". A second
    /// implementation would drift, and the two would then disagree about the same outage.
    ///
    /// `recent_window_s` is how far back still counts as "recent" for the anomaly scorer, and
    /// `sigma` its sensitivity.
    pub(super) async fn incident_signals(
        &self,
        node: Uuid,
        from_s: i64,
        to_s: i64,
        recent_window_s: i64,
        sigma: f64,
    ) -> Vec<IncidentSignal> {
        let mut signals: Vec<IncidentSignal> = Vec::new();
        // 1) Reachability metric anomaly.
        let step = read_step(from_s, to_s);
        let rtt = self
            .gauge_range(node, "icmp_rtt_ms", from_s, to_s, step)
            .await;
        if rtt.len() >= MIN_POINTS {
            if let Some(a) = score_anomaly(&rtt, to_s - recent_window_s, sigma) {
                signals.push(IncidentSignal {
                    at_s: a.when_s,
                    severity: a.score,
                    kind: "metric",
                    label: format!("icmp_rtt_ms {}", a.kind),
                });
            }
        }
        // 2) Passive events on the node. Read through the log store when one is configured: with
        // ADR-024 on, PostgreSQL holds only the alert-linked subset, so a timeline built from it
        // showed the events that had already alerted and nothing that led up to them.
        let filter = EventFilter {
            since: DateTime::from_timestamp(from_s, 0),
            node_id: Some(node),
            ..Default::default()
        };
        let events = self
            .events
            .recent_events(&filter, 20)
            .await
            .unwrap_or_default();
        for e in events.iter().take(INCIDENT_EVENT_CAP) {
            signals.push(IncidentSignal {
                at_s: e.at_unix_ms / 1000,
                severity: event_signal_severity(e.action, e.syslog_severity),
                kind: "event",
                label: incident_event_label(e),
            });
        }
        // 3) Dominant flow conversation.
        if let Some(flows) = self.flows.clone() {
            let q = FlowQuery {
                node_id: Some(node),
                from_unix_ms: from_s * 1000,
                to_unix_ms: to_s * 1000,
                limit: 1,
                proto: Vec::new(),
                dst_port: Vec::new(),
                peer: Vec::new(),
                asn: Vec::new(),
            };
            if let Ok(cs) = flows.top_conversations(&q).await {
                if let Some(c) = cs.first() {
                    signals.push(IncidentSignal {
                        at_s: to_s,
                        severity: 40.0,
                        kind: "flow",
                        label: format!(
                            "top flow {} → {} ({})",
                            c.src,
                            c.dst,
                            human_bytes(c.bytes as f64)
                        ),
                    });
                }
            }
        }
        signals
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::testkit::{params, Harness};
    use crate::events::{EventAction, EventRow};
    use chrono::TimeZone;
    use yagra_bus::EventKind;

    /// A stored event on `node`, `ago_s` seconds ago, that fired a rule.
    fn event(node: Uuid, ago_s: i64, message: &str) -> EventRow {
        let at_ms = (now_s() - ago_s) * 1_000;
        EventRow {
            id: Uuid::new_v4(),
            kind: EventKind::Syslog,
            at_unix_ms: at_ms,
            recorded_at: Utc.timestamp_opt(at_ms / 1_000, 0).single().expect("ts"),
            source_ip: None,
            node_id: Some(node),
            source_id: None,
            pool: None,
            facility: Some(4),
            syslog_severity: Some(3),
            hostname: None,
            app_name: Some("kernel".to_owned()),
            trap_oid: None,
            trap_name: None,
            varbinds: None,
            message: message.to_owned(),
            matched_rule_id: None,
            action: EventAction::Fired,
        }
    }

    /// A window narrow enough that the reachability series has a real baseline: the analysis floors
    /// its read window at an hour, but `recent_window_s` follows `window_secs`.
    fn incident_params() -> JobParams {
        let mut p = params(AnalysisTool::IncidentCorrelate);
        p.window_secs = 600;
        p
    }

    /// Seed a reachability series that is flat for most of the hour and then jumps.
    fn seed_rtt_anomaly(h: &Harness, node: Uuid) {
        let to = now_s();
        let mut pts: Vec<MetricPoint> = (0..20)
            .map(|i| MetricPoint {
                t: to - 3_600 + i * 145,
                v: 4.0,
            })
            .collect();
        pts.push(MetricPoint {
            t: to - 300,
            v: 40.0,
        });
        pts.push(MetricPoint {
            t: to - 100,
            v: 40.0,
        });
        h.metrics.series(node, "icmp_rtt_ms", pts);
    }

    /// Acceptance first: two kinds of signal on the same node is what a correlated incident is.
    #[tokio::test]
    async fn a_node_showing_two_kinds_of_signal_is_a_correlated_incident() {
        let h = Harness::new();
        let node = h.inventory.node(1, "core-sw-01");
        seed_rtt_anomaly(&h, node);
        *h.events.recent.lock().expect("lock") = vec![event(node, 200, "link down")];

        let (findings, summary) = h
            .engine()
            .run_incident_correlate(
                Uuid::nil(),
                &incident_params(),
                &[node],
                &HashMap::from([(node, "core-sw-01".to_owned())]),
                &AtomicBool::new(false),
            )
            .await
            .expect("ok")
            .expect("not cancelled");

        assert_eq!(findings.len(), 1, "{summary}");
        assert_eq!(findings[0].kind, "incident_correlate");
        assert_eq!(findings[0].metric, "incident");
        assert_eq!(findings[0].node_name, "core-sw-01");
        let kinds: std::collections::BTreeSet<&str> = findings[0].detail["timeline"]
            .as_array()
            .expect("a timeline")
            .iter()
            .map(|e| e["kind"].as_str().expect("kind"))
            .collect();
        assert_eq!(
            kinds,
            ["event", "metric"].into_iter().collect(),
            "the timeline carries both kinds: {:?}",
            findings[0].detail["timeline"]
        );
        assert_eq!(findings[0].detail["peer_count"], 0, "no graph ⇒ no peers");
    }

    /// Two signals of the *same* kind are not a correlation. Evidence has to cross stores, or the
    /// analysis is just an event list with a score on it.
    #[tokio::test]
    async fn two_signals_of_one_kind_are_not_a_correlated_incident() {
        let h = Harness::new();
        let node = h.inventory.node(1, "n");
        // Events only — no reachability series at all.
        *h.events.recent.lock().expect("lock") =
            vec![event(node, 200, "one"), event(node, 100, "two")];

        let (findings, _) = h
            .engine()
            .run_incident_correlate(
                Uuid::nil(),
                &incident_params(),
                &[node],
                &HashMap::new(),
                &AtomicBool::new(false),
            )
            .await
            .expect("ok")
            .expect("not cancelled");
        assert!(
            findings.is_empty(),
            "two events are two signals of one kind: {findings:?}"
        );
    }

    /// 🎯 The inventory being unreachable costs the neighbour expansion, not the finding.
    ///
    /// `incident_neighbourhood` says so in its own doc — "degrade to no expansion rather than
    /// failing the job: single-node correlation is exactly the behaviour this analysis had before,
    /// so it is a safe floor". Nothing checked it. A failing read here would otherwise be the
    /// difference between a diagnostic with fewer peers and no diagnostic at all.
    #[tokio::test]
    async fn an_unreachable_inventory_costs_the_expansion_not_the_finding() {
        let h = Harness::new();
        let node = h.inventory.node(1, "n");
        seed_rtt_anomaly(&h, node);
        *h.events.recent.lock().expect("lock") = vec![event(node, 200, "link down")];
        h.inventory.fail();

        let (findings, _) = h
            .engine()
            .run_incident_correlate(
                Uuid::nil(),
                &incident_params(),
                &[node],
                &HashMap::new(),
                &AtomicBool::new(false),
            )
            .await
            .expect("a failed inventory read is not a failed job")
            .expect("not cancelled");

        assert_eq!(findings.len(), 1, "the node's own signals still stand");
        assert_eq!(findings[0].detail["peer_count"], 0);
    }

    /// A derived neighbour whose signals land in the same window corroborates the incident, and
    /// says which side of the link it is on.
    #[tokio::test]
    async fn a_neighbour_in_scope_with_coinciding_signals_corroborates() {
        let h = Harness::new();
        let subject = h.inventory.node(1, "leaf");
        let upstream = h.inventory.node(2, "spine");
        for n in [subject, upstream] {
            seed_rtt_anomaly(&h, n);
            h.events
                .recent
                .lock()
                .expect("lock")
                .push(event(n, 200, "link down"));
        }
        let mut topo = Topology::new();
        topo.add_dependency(NodeId::from(subject), NodeId::from(upstream));
        h.graph.set(topo);

        let (findings, _) = h
            .engine()
            .run_incident_correlate(
                Uuid::nil(),
                &incident_params(),
                &[subject, upstream],
                &HashMap::from([(upstream, "spine".to_owned())]),
                &AtomicBool::new(false),
            )
            .await
            .expect("ok")
            .expect("not cancelled");

        let leaf = findings
            .iter()
            .find(|f| f.node_id == Some(subject))
            .expect("the subject has a finding");
        assert_eq!(leaf.detail["peer_count"], 1, "the upstream corroborates");
        assert_eq!(
            leaf.detail["peers"][0]["node_id"],
            serde_json::json!(upstream)
        );
        assert_eq!(leaf.detail["peers"][0]["relation"], "upstream");
    }
}
