// SPDX-License-Identifier: AGPL-3.0-only
//! The analyses that read the flow store: traffic anomaly, talker shift, new destination, scan,
//! saturation (ADR-031, ADR-089).
//!
//! All five short-circuit to `flow_tier_off()` when ClickHouse is not configured, which is the
//! other half of [`super::AnalysisTool::needs_flow_tier`] —
//! `guards::needs_flow_tier_matches_which_analyses_actually_short_circuit` reads this file as text
//! and fails if the two ever disagree. Saturation sits here rather than with the cross-store group
//! because it, too, has nothing to say with the tier off.

use super::*;

impl AnalysisRunner {
    // ── Engine: Traffic Anomaly (flow) ────────────────────────────────────────────
    //
    // Per node: sum flow bytes per 5-minute bucket, learn a baseline, flag a recent spike (DDoS,
    // saturation, runaway backup, exfiltration).
    pub(super) async fn run_traffic_anomaly(
        &self,
        id: Uuid,
        params: &JobParams,
        node_ids: &[Uuid],
        names: &HashMap<Uuid, String>,
        cancel: &AtomicBool,
    ) -> anyhow::Result<Option<(Vec<NewFinding>, String)>> {
        let Some(flows) = self.flows.clone() else {
            return Ok(Some(flow_tier_off()));
        };
        let to = now_s();
        let from = to - params.baseline_secs.max(6 * 3600);
        let recent_cutoff = to - params.window_secs.max(600);
        let sigma = params.sensitivity.max(0.5);
        let total = node_ids.len().max(1);
        let mut findings: Vec<NewFinding> = Vec::new();
        for (i, node) in node_ids.iter().enumerate() {
            if cancel.load(Ordering::Relaxed) {
                return Ok(None);
            }
            self.progress(id, 15 + (i * 70 / total) as i32, "Reading flow volume…")
                .await;
            let q = FlowSeriesQuery {
                node_id: Some(*node),
                from_unix_ms: from * 1000,
                to_unix_ms: to * 1000,
                proto: Vec::new(),
            };
            let pts = flows.series(&q).await.unwrap_or_default();
            if pts.len() < MIN_POINTS {
                continue;
            }
            let mut per_bucket: std::collections::BTreeMap<i64, f64> =
                std::collections::BTreeMap::new();
            for p in &pts {
                *per_bucket.entry(p.ts_unix_ms / 1000).or_default() += p.bytes as f64;
            }
            let baseline: Vec<f64> = per_bucket
                .iter()
                .filter(|(t, _)| **t < recent_cutoff)
                .map(|(_, v)| *v)
                .collect();
            let recent: Vec<(i64, f64)> = per_bucket
                .iter()
                .filter(|(t, _)| **t >= recent_cutoff)
                .map(|(t, v)| (*t, *v))
                .collect();
            if recent.is_empty() || baseline.len() < MIN_POINTS / 2 {
                continue;
            }
            let (peak_t, peak) =
                recent
                    .iter()
                    .copied()
                    .fold((0i64, 0f64), |a, (t, c)| if c > a.1 { (t, c) } else { a });
            let Some(score) = burst_score(&baseline, peak, sigma) else {
                continue;
            };
            findings.push(NewFinding {
                score,
                severity: severity_for(score).to_owned(),
                node_id: Some(*node),
                node_name: name_lookup(names, node),
                metric: "flow_bytes".to_owned(),
                kind: "traffic_anomaly".to_owned(),
                when_label: rel_label(peak_t, to),
                duration: format!("{} peak", human_bytes(peak)),
                detail: traffic_detail(peak, mean(&baseline), peak_t),
            });
        }
        finalize(&mut findings);
        let summary = format!("{} nodes with flow-volume anomalies", findings.len());
        Ok(Some((findings, summary)))
    }

    // ── Engine: Talker Shift (flow) ───────────────────────────────────────────────
    //
    // A talker that is newly dominant vs the previous equal-length window (new heavy host / exfil
    // source / rogue device).
    pub(super) async fn run_talker_shift(
        &self,
        id: Uuid,
        params: &JobParams,
        node_ids: &[Uuid],
        names: &HashMap<Uuid, String>,
        cancel: &AtomicBool,
    ) -> anyhow::Result<Option<(Vec<NewFinding>, String)>> {
        let Some(flows) = self.flows.clone() else {
            return Ok(Some(flow_tier_off()));
        };
        let to = now_s();
        let window = params.window_secs.max(1800);
        let recent_from = to - window;
        let base_from = to - 2 * window;
        let total = node_ids.len().max(1);
        let mut findings: Vec<NewFinding> = Vec::new();
        for (i, node) in node_ids.iter().enumerate() {
            if cancel.load(Ordering::Relaxed) {
                return Ok(None);
            }
            self.progress(id, 15 + (i * 70 / total) as i32, "Comparing talkers…")
                .await;
            let recent_q = FlowQuery {
                node_id: Some(*node),
                from_unix_ms: recent_from * 1000,
                to_unix_ms: to * 1000,
                limit: 10,
                proto: Vec::new(),
                dst_port: Vec::new(),
                peer: Vec::new(),
                asn: Vec::new(),
            };
            let base_q = FlowQuery {
                from_unix_ms: base_from * 1000,
                to_unix_ms: recent_from * 1000,
                limit: 100,
                // `.clone()` rather than the struct-update move: `FlowQuery` stopped being `Copy`
                // when its filters became `Vec`s (ADR-053 Inc.8), so `..recent_q` would move the
                // vectors out of a value still used two lines below.
                ..recent_q.clone()
            };
            let recent = flows.top_talkers(&recent_q).await.unwrap_or_default();
            if recent.is_empty() {
                continue;
            }
            let base = flows.top_talkers(&base_q).await.unwrap_or_default();
            let base_keys: HashSet<String> = base.iter().map(|t| t.addr.clone()).collect();
            let recent_keys: Vec<String> = recent.iter().map(|t| t.addr.clone()).collect();
            let Some((addr, rank)) = first_novel(&recent_keys, &base_keys) else {
                continue;
            };
            let bytes = recent
                .iter()
                .find(|t| t.addr == addr)
                .map_or(0, |t| t.bytes);
            if bytes < TALKER_FLOOR {
                continue;
            }
            let score = novelty_score(rank);
            findings.push(NewFinding {
                score,
                severity: severity_for(score).to_owned(),
                node_id: Some(*node),
                node_name: name_lookup(names, node),
                metric: "top_talker".to_owned(),
                kind: "talker_shift".to_owned(),
                when_label: format!("new #{}", rank + 1),
                duration: human_bytes(bytes as f64),
                detail: serde_json::json!({ "addr": addr, "bytes": bytes, "rank": rank + 1 }),
            });
        }
        finalize(&mut findings);
        let summary = format!("{} nodes with a new dominant talker", findings.len());
        Ok(Some((findings, summary)))
    }

    // ── Engine: New Destination (flow) ────────────────────────────────────────────
    //
    // Traffic to a destination AS or port absent from the baseline window — a new external
    // destination (possible C2/exfil), a new service, or a scan target.
    pub(super) async fn run_new_destination(
        &self,
        id: Uuid,
        params: &JobParams,
        node_ids: &[Uuid],
        names: &HashMap<Uuid, String>,
        cancel: &AtomicBool,
    ) -> anyhow::Result<Option<(Vec<NewFinding>, String)>> {
        let Some(flows) = self.flows.clone() else {
            return Ok(Some(flow_tier_off()));
        };
        let to = now_s();
        let window = params.window_secs.max(1800);
        let recent_from = to - window;
        let base_from = to - 2 * window;
        let total = node_ids.len().max(1);
        let mut findings: Vec<NewFinding> = Vec::new();
        for (i, node) in node_ids.iter().enumerate() {
            if cancel.load(Ordering::Relaxed) {
                return Ok(None);
            }
            self.progress(id, 15 + (i * 70 / total) as i32, "Comparing destinations…")
                .await;
            let recent_q = FlowQuery {
                node_id: Some(*node),
                from_unix_ms: recent_from * 1000,
                to_unix_ms: to * 1000,
                limit: 10,
                proto: Vec::new(),
                dst_port: Vec::new(),
                peer: Vec::new(),
                asn: Vec::new(),
            };
            let base_q = FlowQuery {
                from_unix_ms: base_from * 1000,
                to_unix_ms: recent_from * 1000,
                limit: 200,
                // Clone, not move — see the same note on the talker-shift query above.
                ..recent_q.clone()
            };
            // Destination AS novelty (the headline signal, using the AS enrichment).
            let recent_as = flows
                .top_as(&recent_q, AsDir::Dst)
                .await
                .unwrap_or_default();
            let base_as = flows.top_as(&base_q, AsDir::Dst).await.unwrap_or_default();
            let base_as_keys: HashSet<String> = base_as
                .iter()
                .filter(|a| a.asn != 0)
                .map(|a| a.asn.to_string())
                .collect();
            let recent_as_keys: Vec<String> = recent_as
                .iter()
                .filter(|a| a.asn != 0)
                .map(|a| a.asn.to_string())
                .collect();
            if let Some((asn_str, rank)) = first_novel(&recent_as_keys, &base_as_keys) {
                let asn: u32 = asn_str.parse().unwrap_or(0);
                let bytes = recent_as
                    .iter()
                    .find(|a| a.asn == asn)
                    .map_or(0, |a| a.bytes);
                if bytes >= DEST_FLOOR {
                    let name = self.resolve_as_name(asn);
                    let score = novelty_score(rank);
                    findings.push(NewFinding {
                        score,
                        severity: severity_for(score).to_owned(),
                        node_id: Some(*node),
                        node_name: name_lookup(names, node),
                        metric: "dst_as".to_owned(),
                        kind: "new_destination".to_owned(),
                        when_label: format!("AS{asn}"),
                        duration: name.clone().unwrap_or_else(|| human_bytes(bytes as f64)),
                        detail: serde_json::json!({ "asn": asn, "as_name": name, "bytes": bytes }),
                    });
                }
            }
            // Destination port novelty (noisier — score capped just under warning).
            let recent_ports = flows.top_ports(&recent_q).await.unwrap_or_default();
            let base_ports = flows.top_ports(&base_q).await.unwrap_or_default();
            let base_port_keys: HashSet<String> =
                base_ports.iter().map(|p| p.port.to_string()).collect();
            let recent_port_keys: Vec<String> =
                recent_ports.iter().map(|p| p.port.to_string()).collect();
            if let Some((port_str, rank)) = first_novel(&recent_port_keys, &base_port_keys) {
                let port: u16 = port_str.parse().unwrap_or(0);
                let bytes = recent_ports
                    .iter()
                    .find(|p| p.port == port)
                    .map_or(0, |p| p.bytes);
                if bytes >= DEST_FLOOR {
                    let score = novelty_score(rank).min(74.0);
                    findings.push(NewFinding {
                        score,
                        severity: severity_for(score).to_owned(),
                        node_id: Some(*node),
                        node_name: name_lookup(names, node),
                        metric: "dst_port".to_owned(),
                        kind: "new_destination".to_owned(),
                        when_label: format!("port {port}"),
                        duration: human_bytes(bytes as f64),
                        detail: serde_json::json!({ "port": port, "bytes": bytes }),
                    });
                }
            }
        }
        finalize(&mut findings);
        let summary = format!("{} new destination signals", findings.len());
        Ok(Some((findings, summary)))
    }

    // ── Engine: Flow Scan (flow) ──────────────────────────────────────────────────
    //
    // A source contacting an abnormal number of distinct destinations (horizontal) or destination
    // ports (vertical) — scan / worm behaviour, via the ClickHouse distinct-count fan-out.
    pub(super) async fn run_flow_scan(
        &self,
        id: Uuid,
        params: &JobParams,
        node_ids: &[Uuid],
        names: &HashMap<Uuid, String>,
        cancel: &AtomicBool,
    ) -> anyhow::Result<Option<(Vec<NewFinding>, String)>> {
        let Some(flows) = self.flows.clone() else {
            return Ok(Some(flow_tier_off()));
        };
        let to = now_s();
        let from = to - params.window_secs.max(1800);
        let total = node_ids.len().max(1);
        let mut findings: Vec<NewFinding> = Vec::new();
        for (i, node) in node_ids.iter().enumerate() {
            if cancel.load(Ordering::Relaxed) {
                return Ok(None);
            }
            self.progress(id, 15 + (i * 70 / total) as i32, "Scanning fan-out…")
                .await;
            let q = FlowQuery {
                node_id: Some(*node),
                from_unix_ms: from * 1000,
                to_unix_ms: to * 1000,
                limit: 50,
                proto: Vec::new(),
                dst_port: Vec::new(),
                peer: Vec::new(),
                asn: Vec::new(),
            };
            let fan = flows.fanout_by_src(&q).await.unwrap_or_default();
            for f in fan {
                let Some(score) = scan_score(f.distinct_dst, f.distinct_ports) else {
                    continue;
                };
                let (kind_label, n) = if f.distinct_dst >= f.distinct_ports {
                    ("horizontal", f.distinct_dst)
                } else {
                    ("vertical", f.distinct_ports)
                };
                findings.push(NewFinding {
                    score,
                    severity: severity_for(score).to_owned(),
                    node_id: Some(*node),
                    node_name: name_lookup(names, node),
                    metric: "flow_fanout".to_owned(),
                    kind: "flow_scan".to_owned(),
                    when_label: format!("{} → {} dst", f.src, f.distinct_dst),
                    duration: format!("{kind_label} · {n}"),
                    detail: serde_json::json!({
                        "src": f.src, "distinct_dst": f.distinct_dst,
                        "distinct_ports": f.distinct_ports, "flows": f.flows,
                    }),
                });
            }
        }
        finalize(&mut findings);
        let summary = format!("{} scanning sources", findings.len());
        Ok(Some((findings, summary)))
    }

    // ── Engine: Saturation (cross-store) ──────────────────────────────────────────
    //
    // A single conversation dominating a busy node's traffic (link hog). Concentration comes from
    // the flow store; the node's current interface throughput (TSDB) is attached as context.
    pub(super) async fn run_saturation(
        &self,
        id: Uuid,
        params: &JobParams,
        node_ids: &[Uuid],
        names: &HashMap<Uuid, String>,
        cancel: &AtomicBool,
    ) -> anyhow::Result<Option<(Vec<NewFinding>, String)>> {
        let Some(flows) = self.flows.clone() else {
            return Ok(Some(flow_tier_off()));
        };
        let to = now_s();
        let from = to - params.window_secs.max(900);
        let total = node_ids.len().max(1);
        let mut findings: Vec<NewFinding> = Vec::new();
        for (i, node) in node_ids.iter().enumerate() {
            if cancel.load(Ordering::Relaxed) {
                return Ok(None);
            }
            self.progress(
                id,
                15 + (i * 70 / total) as i32,
                "Checking traffic concentration…",
            )
            .await;
            let conv_q = FlowQuery {
                node_id: Some(*node),
                from_unix_ms: from * 1000,
                to_unix_ms: to * 1000,
                limit: 5,
                proto: Vec::new(),
                dst_port: Vec::new(),
                peer: Vec::new(),
                asn: Vec::new(),
            };
            let convos = flows.top_conversations(&conv_q).await.unwrap_or_default();
            let Some(top) = convos.first() else {
                continue;
            };
            let proto_q = FlowQuery {
                limit: 256,
                ..conv_q
            };
            let protos = flows.top_protocols(&proto_q).await.unwrap_or_default();
            let node_total = protos
                .iter()
                .map(|p| p.bytes)
                .sum::<u64>()
                .max(top.bytes)
                .max(1);
            let ratio = top.bytes as f64 / node_total as f64;
            let Some(score) = concentration_score(ratio) else {
                continue;
            };
            let iface_bps = self.node_throughput_bps(*node).await;
            findings.push(NewFinding {
                score,
                severity: severity_for(score).to_owned(),
                node_id: Some(*node),
                node_name: name_lookup(names, node),
                metric: "flow_concentration".to_owned(),
                kind: "saturation".to_owned(),
                when_label: format!("{:.0}% one flow", ratio * 100.0),
                duration: format!("{} → {}", top.src, top.dst),
                detail: serde_json::json!({
                    "src": top.src, "dst": top.dst, "conversation_bytes": top.bytes,
                    "node_bytes": node_total, "ratio": ratio, "interface_bps": iface_bps,
                }),
            });
        }
        finalize(&mut findings);
        let summary = format!("{} nodes with a dominant conversation", findings.len());
        Ok(Some((findings, summary)))
    }

    /// Resolve an AS number to its organization name via the hot-swappable IP→ASN table (`None`
    /// when the table is unloaded or the ASN is unknown/0).
    fn resolve_as_name(&self, asn: u32) -> Option<String> {
        if asn == 0 {
            return None;
        }
        let db = self.ipasn.read().ok()?.clone();
        db.and_then(|d| d.name_of(asn).map(str::to_owned))
    }

    /// Best-effort current total interface throughput (bits/sec) for a node from the TSDB — context
    /// for the saturation finding. `None` when the node has no interface series.
    async fn node_throughput_bps(&self, node: Uuid) -> Option<f64> {
        let live = self.store.node_interface_live(node, 300).await;
        if live.is_empty() {
            return None;
        }
        let bytes: f64 = live
            .values()
            .map(|v| v.in_bps.unwrap_or(0.0) + v.out_bps.unwrap_or(0.0))
            .sum();
        Some(bytes * 8.0)
    }
}
