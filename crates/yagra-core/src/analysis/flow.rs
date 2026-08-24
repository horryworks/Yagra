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

impl Engine {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::testkit::{params, Harness};
    use crate::flowstore::FlowRow;
    use std::net::{IpAddr, Ipv4Addr};

    /// One flow record. Defaults are the uninteresting ones; a test names only what it is about.
    fn row(node: Uuid, src: &str, dst: &str, dst_port: u16, bytes: u64, ts_s: i64) -> FlowRow {
        FlowRow {
            node_id: node,
            ts_unix_ms: ts_s * 1000,
            exporter_ip: IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1)),
            if_index: 2,
            src_ip: src.parse().expect("src"),
            dst_ip: dst.parse().expect("dst"),
            src_port: 40_000,
            dst_port,
            proto: 6,
            tos: 0,
            src_as: 0,
            dst_as: 0,
            bytes,
            packets: bytes / 100,
            flows: 1,
        }
    }

    async fn seed(h: &Harness, rows: Vec<FlowRow>) {
        h.flow_store()
            .insert_batch(&rows)
            .await
            .expect("the in-memory flow store accepts rows");
    }

    /// A quiet baseline that then bursts. `run_traffic_anomaly` needs at least `MIN_POINTS` series
    /// points and half that many baseline buckets before it will score anything.
    #[tokio::test]
    async fn a_burst_of_flow_volume_is_reported_against_its_node() {
        let to = now_s();
        let h = Harness::new().with_flows();
        let node = h.inventory.node(1, "edge-fw");
        let mut rows = Vec::new();
        // 24 five-minute buckets of steady background, ending before the recent hour.
        for i in 0..24 {
            rows.push(row(
                node,
                "10.0.0.5",
                "10.0.0.9",
                443,
                1_000,
                to - 3_600 - i * 300,
            ));
        }
        // …then one bucket a thousand times larger, inside it.
        rows.push(row(node, "10.0.0.5", "10.0.0.9", 443, 1_000_000, to - 600));
        seed(&h, rows).await;

        let (findings, summary) = h
            .engine()
            .run_traffic_anomaly(
                Uuid::nil(),
                &params(AnalysisTool::TrafficAnomaly),
                &[node],
                &HashMap::from([(node, "edge-fw".to_owned())]),
                &AtomicBool::new(false),
            )
            .await
            .expect("ok")
            .expect("not cancelled");

        assert_eq!(findings.len(), 1, "{summary}");
        assert_eq!(findings[0].kind, "traffic_anomaly");
        assert_eq!(findings[0].metric, "flow_bytes");
        assert_eq!(findings[0].node_name, "edge-fw");
        assert!(summary.contains("1 nodes with flow-volume anomalies"));
    }

    /// A talker that was not in the previous window, and is now the biggest, is the finding. The
    /// baseline talker is seeded too, so a store returning nothing would fail rather than pass.
    #[tokio::test]
    async fn a_newly_dominant_talker_is_reported_and_a_familiar_one_is_not() {
        let to = now_s();
        let h = Harness::new().with_flows();
        let node = h.inventory.node(1, "n");
        seed(
            &h,
            vec![
                // Baseline window (between 2w and 1w ago): one known talker.
                row(node, "10.0.0.5", "10.0.0.9", 443, 5_000_000, to - 5_400),
                // Recent window: the same known talker, plus a bigger stranger.
                row(node, "10.0.0.5", "10.0.0.9", 443, 2_000_000, to - 600),
                row(node, "10.0.0.77", "10.0.0.9", 443, 9_000_000, to - 600),
            ],
        )
        .await;

        let (findings, _) = h
            .engine()
            .run_talker_shift(
                Uuid::nil(),
                &params(AnalysisTool::TalkerShift),
                &[node],
                &HashMap::new(),
                &AtomicBool::new(false),
            )
            .await
            .expect("ok")
            .expect("not cancelled");

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, "talker_shift");
        assert_eq!(
            findings[0].detail["addr"], "10.0.0.77",
            "the stranger is the finding, not the familiar heavy hitter"
        );
        assert_eq!(findings[0].when_label, "new #1");
    }

    /// Below the byte floor a new talker is noise, not a shift. Same shape as the test above with
    /// one number changed, so what is being measured is the floor and nothing else.
    #[tokio::test]
    async fn a_new_talker_carrying_almost_nothing_is_not_a_shift() {
        let to = now_s();
        let h = Harness::new().with_flows();
        let node = h.inventory.node(1, "n");
        seed(
            &h,
            vec![
                row(node, "10.0.0.5", "10.0.0.9", 443, 5_000_000, to - 5_400),
                row(
                    node,
                    "10.0.0.77",
                    "10.0.0.9",
                    443,
                    TALKER_FLOOR - 1,
                    to - 600,
                ),
            ],
        )
        .await;

        let (findings, _) = h
            .engine()
            .run_talker_shift(
                Uuid::nil(),
                &params(AnalysisTool::TalkerShift),
                &[node],
                &HashMap::new(),
                &AtomicBool::new(false),
            )
            .await
            .expect("ok")
            .expect("not cancelled");
        assert!(findings.is_empty(), "under the floor ⇒ nothing to say");
    }

    /// Fan-out past the scan floor is a scan; the direction it is named by is whichever dimension
    /// is wider.
    #[tokio::test]
    async fn a_source_touching_many_destinations_is_reported_as_a_horizontal_scan() {
        let to = now_s();
        let h = Harness::new().with_flows();
        let node = h.inventory.node(1, "n");
        let rows: Vec<FlowRow> = (0..60)
            .map(|i| {
                row(
                    node,
                    "10.0.0.5",
                    &format!("10.9.{}.{}", i / 250, i % 250),
                    443,
                    1_000,
                    to - 600,
                )
            })
            .collect();
        seed(&h, rows).await;

        let (findings, summary) = h
            .engine()
            .run_flow_scan(
                Uuid::nil(),
                &params(AnalysisTool::FlowScan),
                &[node],
                &HashMap::new(),
                &AtomicBool::new(false),
            )
            .await
            .expect("ok")
            .expect("not cancelled");

        assert_eq!(findings.len(), 1, "{summary}");
        assert_eq!(findings[0].kind, "flow_scan");
        assert_eq!(findings[0].detail["src"], "10.0.0.5");
        assert!(
            findings[0].duration.starts_with("horizontal"),
            "many destinations, one port ⇒ horizontal: {}",
            findings[0].duration
        );
    }

    /// A handful of destinations is ordinary traffic. The floor is 50.
    #[tokio::test]
    async fn a_source_touching_a_few_destinations_is_not_a_scan() {
        let to = now_s();
        let h = Harness::new().with_flows();
        let node = h.inventory.node(1, "n");
        let rows: Vec<FlowRow> = (0..10)
            .map(|i| {
                row(
                    node,
                    "10.0.0.5",
                    &format!("10.9.0.{i}"),
                    443,
                    1_000,
                    to - 600,
                )
            })
            .collect();
        seed(&h, rows).await;

        let (findings, _) = h
            .engine()
            .run_flow_scan(
                Uuid::nil(),
                &params(AnalysisTool::FlowScan),
                &[node],
                &HashMap::new(),
                &AtomicBool::new(false),
            )
            .await
            .expect("ok")
            .expect("not cancelled");
        assert!(findings.is_empty());
    }

    /// One conversation carrying most of a node's bytes is the finding, and the node's live
    /// interface throughput rides along as context — the only place a flow analysis reads the TSDB.
    #[tokio::test]
    async fn one_dominant_conversation_is_reported_with_the_interface_rate_beside_it() {
        let to = now_s();
        let h = Harness::new().with_flows();
        let node = h.inventory.node(1, "n");
        h.metrics.interfaces(
            node,
            HashMap::from([(
                1,
                crate::store::InterfaceLive {
                    in_bps: Some(1_000.0),
                    out_bps: Some(2_000.0),
                    oper_status: Some(1.0),
                },
            )]),
        );
        seed(
            &h,
            vec![
                row(node, "10.0.0.5", "10.0.0.9", 443, 9_000_000, to - 60),
                row(node, "10.0.0.6", "10.0.0.9", 443, 100_000, to - 60),
            ],
        )
        .await;

        let (findings, _) = h
            .engine()
            .run_saturation(
                Uuid::nil(),
                &params(AnalysisTool::Saturation),
                &[node],
                &HashMap::new(),
                &AtomicBool::new(false),
            )
            .await
            .expect("ok")
            .expect("not cancelled");

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, "saturation");
        assert_eq!(findings[0].detail["src"], "10.0.0.5");
        assert_eq!(
            findings[0].detail["interface_bps"],
            serde_json::json!(24_000.0),
            "(1000 + 2000) bytes/s over one interface, reported in bits"
        );
    }

    /// Evenly-spread traffic has no hog. Below half the node's bytes, nothing is reported.
    #[tokio::test]
    async fn evenly_spread_traffic_is_not_saturation() {
        let to = now_s();
        let h = Harness::new().with_flows();
        let node = h.inventory.node(1, "n");
        seed(
            &h,
            vec![
                row(node, "10.0.0.5", "10.0.0.9", 443, 1_000_000, to - 60),
                row(node, "10.0.0.6", "10.0.0.9", 443, 1_000_000, to - 60),
                row(node, "10.0.0.7", "10.0.0.9", 443, 1_000_000, to - 60),
            ],
        )
        .await;

        let (findings, _) = h
            .engine()
            .run_saturation(
                Uuid::nil(),
                &params(AnalysisTool::Saturation),
                &[node],
                &HashMap::new(),
                &AtomicBool::new(false),
            )
            .await
            .expect("ok")
            .expect("not cancelled");
        assert!(findings.is_empty(), "a third of the traffic is not a hog");
    }

    /// A destination AS absent from the previous window is the finding. Seeded through the AS
    /// fields rather than the addresses, because that is the dimension this analysis compares.
    #[tokio::test]
    async fn a_destination_as_absent_from_the_baseline_is_reported() {
        let to = now_s();
        let h = Harness::new().with_flows();
        let node = h.inventory.node(1, "n");
        let mut familiar = row(node, "10.0.0.5", "8.8.8.8", 443, 5_000_000, to - 5_400);
        familiar.dst_as = 15_169;
        let mut still_familiar = familiar.clone();
        still_familiar.ts_unix_ms = (to - 600) * 1_000;
        let mut stranger = row(node, "10.0.0.5", "1.1.1.1", 443, 6_000_000, to - 600);
        stranger.dst_as = 13_335;
        seed(&h, vec![familiar, still_familiar, stranger]).await;

        let (findings, _) = h
            .engine()
            .run_new_destination(
                Uuid::nil(),
                &params(AnalysisTool::NewDestination),
                &[node],
                &HashMap::new(),
                &AtomicBool::new(false),
            )
            .await
            .expect("ok")
            .expect("not cancelled");

        assert!(
            findings.iter().any(|f| f.kind == "new_destination"),
            "the AS that only appears in the recent window is the finding: {findings:?}",
        );
    }
}
