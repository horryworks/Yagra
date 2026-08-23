// SPDX-License-Identifier: AGPL-3.0-only
//! Scoring, statistics and formatting — **the `self`-free half of the analysis module**
//! (ADR-089).
//!
//! The boundary is mechanical rather than thematic, and that is the point: **nothing here takes
//! `self`**, so which side of the line an item belongs on is a question anyone can answer the same
//! way twice. `no_function_here_takes_self` pins it. Thematic boundaries between "statistics",
//! "scoring" and "formatting" exist, but nobody can apply them identically, so they would drift.
//!
//! The consequence is a large file — larger than the four analysis groups it serves. That was
//! preferred to drawing a line no future reader could reproduce.

use super::*;

/// A candidate series for correlation (its label, variance, and points).
pub(super) struct CandidateSeries {
    pub(super) label: String,
    pub(super) var: f64,
    pub(super) points: Vec<MetricPoint>,
}

// ── Pure analysis maths (unit-tested) ────────────────────────────────────────────────

/// Sample step that keeps a window under [`MAX_POINTS`] samples (min 60s).
pub(super) fn read_step(from_s: i64, to_s: i64) -> u64 {
    let span = (to_s - from_s).max(1);
    ((span / MAX_POINTS).max(60)) as u64
}

pub(super) fn mean(xs: &[f64]) -> f64 {
    if xs.is_empty() {
        return 0.0;
    }
    xs.iter().sum::<f64>() / xs.len() as f64
}

pub(super) fn variance(xs: &[f64], mean: f64) -> f64 {
    if xs.len() < 2 {
        return 0.0;
    }
    xs.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / xs.len() as f64
}

pub(super) fn stddev(xs: &[f64], mean: f64) -> f64 {
    variance(xs, mean).sqrt()
}

/// Least-squares slope of `ys` against `xs` (per unit of x). `None` if x has no spread.
pub(super) fn linreg_slope(xs: &[f64], ys: &[f64]) -> Option<f64> {
    let n = xs.len();
    if n < 2 || n != ys.len() {
        return None;
    }
    let mx = mean(xs);
    let my = mean(ys);
    let mut num = 0.0;
    let mut den = 0.0;
    for (x, y) in xs.iter().zip(ys) {
        let dx = x - mx;
        num += dx * (y - my);
        den += dx * dx;
    }
    if den.abs() <= f64::EPSILON {
        None
    } else {
        Some(num / den)
    }
}

/// Pearson correlation of two equal-length vectors. `None` if either is constant.
pub(super) fn pearson(xs: &[f64], ys: &[f64]) -> Option<f64> {
    let n = xs.len();
    if n < 2 || n != ys.len() {
        return None;
    }
    let mx = mean(xs);
    let my = mean(ys);
    let (mut sxy, mut sxx, mut syy) = (0.0, 0.0, 0.0);
    for (x, y) in xs.iter().zip(ys) {
        let dx = x - mx;
        let dy = y - my;
        sxy += dx * dy;
        sxx += dx * dx;
        syy += dy * dy;
    }
    let den = (sxx * syy).sqrt();
    if den <= f64::EPSILON {
        None
    } else {
        Some(sxy / den)
    }
}

/// Correlate two point series on their shared timestamps; returns `(r, sample_count)`.
pub(super) fn correlate(a: &[MetricPoint], b: &[MetricPoint]) -> Option<(f64, usize)> {
    use std::collections::HashMap;
    let bmap: HashMap<i64, f64> = b.iter().map(|p| (p.t, p.v)).collect();
    let mut xs = Vec::new();
    let mut ys = Vec::new();
    for p in a {
        if let Some(v) = bmap.get(&p.t) {
            xs.push(p.v);
            ys.push(*v);
        }
    }
    pearson(&xs, &ys).map(|r| (r, xs.len()))
}

/// A scored anomaly within a series.
pub(super) struct ScoredAnomaly {
    pub(super) score: f64,
    pub(super) kind: &'static str,
    pub(super) when_s: i64,
    pub(super) duration: String,
    pub(super) detail: serde_json::Value,
}

/// Detect the largest baseline-relative deviation in the recent window. Baseline stats come from
/// points before `recent_cutoff` (falling back to the whole series if the baseline is too short).
pub(super) fn score_anomaly(
    pts: &[MetricPoint],
    recent_cutoff: i64,
    sigma: f64,
) -> Option<ScoredAnomaly> {
    let baseline: Vec<f64> = pts
        .iter()
        .filter(|p| p.t < recent_cutoff)
        .map(|p| p.v)
        .collect();
    let recent: Vec<&MetricPoint> = pts.iter().filter(|p| p.t >= recent_cutoff).collect();
    if recent.is_empty() {
        return None;
    }
    // Baseline statistics (fall back to the whole series when the pre-window history is too thin).
    let (base_mean, base_sd) = if baseline.len() >= MIN_POINTS / 2 {
        let m = mean(&baseline);
        (m, stddev(&baseline, m))
    } else {
        let all: Vec<f64> = pts.iter().map(|p| p.v).collect();
        let m = mean(&all);
        (m, stddev(&all, m))
    };
    if base_sd <= f64::EPSILON {
        // Flat baseline: only a real move off the constant is interesting.
        let moved = recent
            .iter()
            .any(|p| (p.v - base_mean).abs() > base_mean.abs().max(1.0) * 0.25);
        if !moved {
            return None;
        }
    }
    let sd = base_sd.max(base_mean.abs().max(1.0) * 1e-3); // floor to avoid divide-by-zero
                                                           // Largest |z| in the recent window.
    let mut zmax = 0.0;
    let mut at = recent[0].t;
    for p in &recent {
        let z = (p.v - base_mean).abs() / sd;
        if z > zmax {
            zmax = z;
            at = p.t;
        }
    }
    if zmax < sigma {
        return None;
    }
    // Score: at the threshold → 75 (warning); ~1.5× threshold → ~100 (critical).
    let score = (75.0 * zmax / sigma).clamp(0.0, 100.0);
    let recent_vals: Vec<f64> = recent.iter().map(|p| p.v).collect();
    let kind = classify_shape(base_mean, sd, &recent_vals, &recent);
    // Downsample the series for the report chart (≤64 points).
    let detail = chart_detail(pts, base_mean, sd, recent_cutoff);
    let dur_pts = recent
        .iter()
        .filter(|p| (p.v - base_mean).abs() / sd >= sigma)
        .count();
    let duration = if dur_pts >= recent.len().saturating_sub(1) {
        "ongoing".to_owned()
    } else {
        format!("{dur_pts} samples")
    };
    Some(ScoredAnomaly {
        score,
        kind,
        when_s: at,
        duration,
        detail,
    })
}

/// Classify the recent segment's shape relative to the baseline.
pub(super) fn classify_shape(
    base_mean: f64,
    sd: f64,
    recent: &[f64],
    pts: &[&MetricPoint],
) -> &'static str {
    if recent.len() < 3 {
        return "spike";
    }
    let rmean = mean(recent);
    let rsd = stddev(recent, rmean);
    // Stuck / flatline: recent variance collapses well below the baseline's.
    if rsd <= sd * 0.1 && (rmean - base_mean).abs() < sd * 0.5 {
        return "flat";
    }
    // Trend drift: a sustained slope across the recent window.
    let xs: Vec<f64> = pts.iter().map(|p| p.t as f64).collect();
    if let Some(slope) = linreg_slope(&xs, recent) {
        let span = (pts.last().map(|p| p.t).unwrap_or(0) - pts.first().map(|p| p.t).unwrap_or(0))
            .max(1) as f64;
        if (slope * span).abs() > sd * 2.0 {
            return "drift";
        }
    }
    // Level shift: the recent mean settled far from the baseline and stays there.
    if (rmean - base_mean).abs() > sd * 1.5 && rsd < sd * 1.5 {
        return "level";
    }
    // A single brief excursion that returns is a spike.
    let over = recent
        .iter()
        .filter(|v| (*v - base_mean).abs() > sd * 2.0)
        .count();
    if over <= 2 {
        return "spike";
    }
    // Otherwise the rhythm changed without a clean level/drift — a seasonality break.
    "season"
}

/// Build the report chart payload: a downsampled actual line plus the baseline mean/σ band.
pub(super) fn chart_detail(
    pts: &[MetricPoint],
    mean: f64,
    sd: f64,
    recent_cutoff: i64,
) -> serde_json::Value {
    const MAX: usize = 64;
    let stride = (pts.len() / MAX).max(1);
    let sampled: Vec<&MetricPoint> = pts.iter().step_by(stride).collect();
    let points: Vec<serde_json::Value> = sampled
        .iter()
        .map(|p| serde_json::json!({ "t": p.t, "v": p.v, "recent": p.t >= recent_cutoff }))
        .collect();
    serde_json::json!({
        "points": points,
        "mean": mean,
        "sigma": sd,
        "recent_from": recent_cutoff,
    })
}

/// A capacity projection: current value, growth slope, and seconds to 100%.
pub(super) struct Projection {
    pub(super) current: f64,
    pub(super) slope_per_s: f64,
    pub(super) tte_secs: i64,
}

/// Project when a utilization-percent series reaches 100% by least-squares trend. `None` if it
/// isn't rising, is already full, or exhaustion is beyond a 1-year horizon.
pub(super) fn project_exhaustion(pts: &[MetricPoint]) -> Option<Projection> {
    let xs: Vec<f64> = pts.iter().map(|p| p.t as f64).collect();
    let ys: Vec<f64> = pts.iter().map(|p| p.v).collect();
    let slope = linreg_slope(&xs, &ys)?;
    let current = *ys.last()?;
    if slope <= 0.0 || !(0.0..100.0).contains(&current) {
        return None;
    }
    let tte = (100.0 - current) / slope;
    if tte <= 0.0 || tte > 365.0 * 86_400.0 {
        return None;
    }
    Some(Projection {
        current,
        slope_per_s: slope,
        tte_secs: tte as i64,
    })
}

/// Urgency score from days-to-exhaustion: ≤7d critical, ≤30d warning, else info.
pub(super) fn capacity_score(days: f64) -> f64 {
    if days <= 7.0 {
        95.0
    } else if days <= 30.0 {
        82.0
    } else if days <= 90.0 {
        65.0
    } else {
        52.0
    }
}

/// Count reachability flaps: gaps between consecutive samples larger than 2× the expected step
/// each mark one down→up cycle.
pub(super) fn count_flaps(pts: &[MetricPoint], step_s: i64) -> u32 {
    let threshold = step_s.max(1) * 2;
    let mut flaps = 0u32;
    for w in pts.windows(2) {
        if w[1].t - w[0].t > threshold {
            flaps += 1;
        }
    }
    flaps
}

/// Score a flapping node by its flap count: ≥6 critical, ≥3 warning, else info.
pub(super) fn flap_score(flaps: u32) -> f64 {
    if flaps >= 6 {
        92.0
    } else if flaps >= 3 {
        80.0
    } else {
        60.0
    }
}

/// Human "N days" / "N hours" label.
pub(super) fn human_days(days: f64) -> String {
    if days < 1.0 {
        format!("{}h", (days * 24.0).round() as i64)
    } else if days < 90.0 {
        format!("{}d", days.round() as i64)
    } else {
        format!("{}mo", (days / 30.0).round() as i64)
    }
}

/// Relative "when" label from an event time vs now.
pub(super) fn rel_label(at_s: i64, now_s: i64) -> String {
    let d = (now_s - at_s).max(0);
    if d < 3600 {
        format!("{}m ago", d / 60)
    } else if d < 86_400 {
        format!("{}h ago", d / 3600)
    } else {
        format!("{}d ago", d / 86_400)
    }
}

// ── Event/flow analysis maths + constants (ADR-022 event/flow increment, unit-tested) ──

/// Bucket width for event-storm volume counting (seconds).
pub(super) const EVENT_BUCKET_SECS: i64 = 300;
/// Minimum peak-bucket event count before an event storm is worth reporting.
pub(super) const EVENT_STORM_FLOOR: f64 = 5.0;
/// Minimum recent syslog volume before a severity shift is meaningful.
pub(super) const SEVERITY_FLOOR: i64 = 10;
/// Minimum count for an unmatched signature to count as a rule gap.
pub(super) const RULE_GAP_FLOOR: i64 = 20;
/// Minimum auth-failure count from a source before flagging.
pub(super) const AUTH_FLOOR: i64 = 5;
/// Minimum bytes a novel talker must carry to be a real shift (1 MB).
pub(super) const TALKER_FLOOR: u64 = 1_000_000;
/// Minimum bytes a novel destination must carry (0.5 MB).
pub(super) const DEST_FLOOR: u64 = 500_000;
/// Hard node cap for the per-node multi-store incident correlation.
pub(super) const INCIDENT_NODE_CAP: usize = 20;
/// Distinct nodes whose signals one incident job may fetch, subjects and neighbours together.
///
/// [`INCIDENT_NODE_CAP`] subjects each expanding to [`NEIGHBOUR_CAP`] peers is the worst case, and
/// this is what stops that arithmetic from being unbounded when the graph is dense.
pub(super) const INCIDENT_CACHE_CAP: usize = INCIDENT_NODE_CAP * (NEIGHBOUR_CAP + 1);
/// How close a neighbour's signal must land to one of the subject's to count as corroboration.
///
/// One [`EVENT_BUCKET_SECS`], because that is already the resolution at which this codebase treats
/// passive events as contemporaneous. Wider would let a chatty upstream corroborate anything.
pub(super) const NEIGHBOUR_COINCIDENCE_SECS: i64 = EVENT_BUCKET_SECS;
/// Most neighbours carried on one incident finding, after sorting by peak severity.
///
/// A core switch can have hundreds of links; an incident report naming hundreds of peers is not a
/// report. ⚠️ This number is a guess until the derivation runs against a real multi-vendor fleet —
/// the lab cannot verify a single derived edge (ADR-043).
pub(super) const NEIGHBOUR_CAP: usize = 4;

/// Whether a peer's signals corroborate the subject's: at least one peer signal lands within
/// `window_s` of a subject signal.
///
/// Pure, so the correlation rule is testable without any store — the rest of `incident_correlate`
/// needs three of them. Coincidence is required rather than mere adjacency in the graph: without
/// it, one noisy upstream manufactures an incident for every quiet device hanging off it.
pub(super) fn peak_severity(signals: &[IncidentSignal]) -> f64 {
    signals.iter().map(|s| s.severity).fold(0.0, f64::max)
}

/// The one-hop neighbourhood of every authorized node, from the union of the two graphs.
///
/// ⚠️ **The scope rule lives here, and it is the security-relevant part of the expansion**: a peer
/// appears only if it is itself in `authorized`. Pure, so that rule is testable without a database
/// — which is the point, because the failure it prevents is silent. See
/// `incident_neighbourhood` for why both graphs are unioned and why the topology mode is not a gate.
pub(super) fn one_hop_neighbours(
    derived: &Topology,
    manual: &Topology,
    authorized: &HashSet<Uuid>,
) -> HashMap<Uuid, Vec<(Uuid, &'static str)>> {
    let mut out: HashMap<Uuid, Vec<(Uuid, &'static str)>> = HashMap::new();
    for &node in authorized {
        let id = NodeId::from(node);
        let mut seen: HashSet<Uuid> = HashSet::new();
        let mut peers: Vec<(Uuid, &'static str)> = Vec::new();
        // Upstream first, so a node that is somehow both keeps the more useful label.
        for (set, relation) in [
            (derived.parents_of(id), "upstream"),
            (manual.parents_of(id), "upstream"),
            (derived.children_of(id), "downstream"),
            (manual.children_of(id), "downstream"),
        ] {
            for peer in set {
                let peer = peer.as_uuid();
                if peer != node && authorized.contains(&peer) && seen.insert(peer) {
                    peers.push((peer, relation));
                }
            }
        }
        if !peers.is_empty() {
            out.insert(node, peers);
        }
    }
    out
}

/// Whether a peer's signals corroborate the subject's: at least one peer signal lands within
/// `window_s` of a subject signal.
pub(super) fn signals_coincide(
    subject: &[IncidentSignal],
    peer: &[IncidentSignal],
    window_s: i64,
) -> bool {
    subject.iter().any(|s| {
        peer.iter()
            .any(|p| (p.at_s - s.at_s).abs() <= window_s.max(0))
    })
}

/// Per-node split of event-bucket counts for `event_storm`: (baseline counts, recent (bucket, count)).
pub(super) type StormBuckets = (Vec<f64>, Vec<(i64, f64)>);

/// The `event_storm` finding detail. `peak_at` is the peak bucket's start (Unix **seconds**) so the
/// WebUI can render a *localized* relative time instead of falling back to the pre-rendered English
/// `when_label` — the label itself is built by `rel_label` and can't go through `t()`. Purely
/// additive to the JSONB blob (older rows simply lack the key, and the UI falls back), so this is
/// N-1 safe with no migration. Split out from the engine fn so it is unit-testable — the engine
/// itself needs a live event store.
pub(super) fn storm_detail(peak: f64, baseline_mean: f64, peak_at: i64) -> serde_json::Value {
    serde_json::json!({
        "peak": peak,
        "baseline_mean": baseline_mean,
        "bucket_secs": EVENT_BUCKET_SECS,
        "peak_at": peak_at,
    })
}

/// The `traffic_anomaly` finding detail — the flow twin of [`storm_detail`], carrying the same
/// additive `peak_at` (Unix seconds) for a localizable relative label.
pub(super) fn traffic_detail(
    peak_bytes: f64,
    baseline_mean_bytes: f64,
    peak_at: i64,
) -> serde_json::Value {
    serde_json::json!({
        "peak_bytes": peak_bytes,
        "baseline_mean_bytes": baseline_mean_bytes,
        "peak_at": peak_at,
    })
}

/// One dated signal on an incident timeline (`incident_correlate`).
///
/// `Serialize` because an RCA report stores the timeline it was grounded in alongside the answer:
/// the UI shows the two together so a reader can check the explanation against its evidence rather
/// than taking it on faith (ADR-029).
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub(crate) struct IncidentSignal {
    pub(crate) at_s: i64,
    pub(crate) severity: f64,
    pub(crate) kind: &'static str,
    pub(crate) label: String,
}

/// Most passive events carried into one node's timeline. A noisy device can log hundreds in the
/// window; past a handful they stop adding evidence and start crowding out the other signal kinds
/// (and, for the RCA context, the prompt budget).
pub(super) const INCIDENT_EVENT_CAP: usize = 8;

/// The flow-tier-off result: a single info finding + summary (mirrors `top_flows`' availability note).
pub(super) fn flow_tier_off() -> (Vec<NewFinding>, String) {
    (
        vec![info_finding("flow", "flow tier not enabled on this core")],
        "flow tier not enabled".to_owned(),
    )
}

/// A zero-score info finding carrying a note (used for the flow-tier-off case).
pub(super) fn info_finding(metric: &str, note: &str) -> NewFinding {
    NewFinding {
        score: 0.0,
        severity: "info".to_owned(),
        node_id: None,
        node_name: "—".to_owned(),
        metric: metric.to_owned(),
        kind: "info".to_owned(),
        when_label: String::new(),
        duration: String::new(),
        detail: serde_json::json!({ "note": note }),
    }
}

/// Sort findings by score (highest first) and cap at [`MAX_FINDINGS`].
pub(super) fn finalize(findings: &mut Vec<NewFinding>) {
    findings.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    findings.truncate(MAX_FINDINGS);
}

/// Upward-spike score: how far a recent peak exceeds the baseline mean, in σ, mapped to 0..100 (at
/// the σ threshold → 75; ~1.5× → ~100). `None` if within threshold or the baseline is empty. Unlike
/// [`score_anomaly`] this is one-sided (only *more* volume/traffic matters for storms/DDoS).
pub(super) fn burst_score(baseline: &[f64], recent_peak: f64, sigma: f64) -> Option<f64> {
    if baseline.is_empty() {
        return None;
    }
    let m = mean(baseline);
    let sd = stddev(baseline, m).max(m.max(1.0) * 1e-3);
    let sig = sigma.max(0.5);
    let z = (recent_peak - m) / sd;
    if z < sig {
        return None;
    }
    Some((75.0 * z / sig).clamp(0.0, 100.0))
}

/// Per-node high-severity (syslog ≤ 3: err/crit/alert/emerg) fraction, restricted to `scope`.
/// Returns `node → (high_count, total_count, fraction)`.
pub(super) fn severity_high_fractions(
    counts: &[EventSeverityCount],
    scope: &HashSet<Uuid>,
) -> HashMap<Uuid, (i64, i64, f64)> {
    let mut acc: HashMap<Uuid, (i64, i64)> = HashMap::new();
    for c in counts {
        if !scope.contains(&c.node_id) {
            continue;
        }
        let e = acc.entry(c.node_id).or_default();
        e.1 += c.count;
        if c.severity <= 3 {
            e.0 += c.count;
        }
    }
    acc.into_iter()
        .map(|(k, (high, total))| {
            let frac = if total > 0 {
                high as f64 / total as f64
            } else {
                0.0
            };
            (k, (high, total, frac))
        })
        .collect()
}

/// Severity-shift score from the baseline vs recent high-severity fraction. `None` if the recent
/// mix didn't skew meaningfully more toward error/critical.
pub(super) fn severity_shift_score(baseline_frac: f64, recent_frac: f64) -> Option<f64> {
    let delta = recent_frac - baseline_frac;
    if delta < 0.15 {
        return None;
    }
    Some((60.0 + delta * 60.0).clamp(0.0, 100.0))
}

/// Score an unmatched-signature volume (rule-coverage gap). Capped at warning — advice, not an outage.
pub(super) fn gap_score(count: i64) -> f64 {
    if count >= 500 {
        80.0
    } else if count >= 100 {
        72.0
    } else {
        60.0
    }
}

/// Score an auth-failure volume from one source.
pub(super) fn auth_score(count: i64) -> f64 {
    if count >= 50 {
        90.0
    } else if count >= 10 {
        78.0
    } else {
        62.0
    }
}

/// The highest-ranked recent key absent from the baseline set, with its 0-based rank.
pub(super) fn first_novel(
    recent: &[String],
    baseline: &HashSet<String>,
) -> Option<(String, usize)> {
    recent
        .iter()
        .enumerate()
        .find(|(_, k)| !baseline.contains(*k))
        .map(|(i, k)| (k.clone(), i))
}

/// Novelty score by the rank a new key entered at (a brand-new #1 is the strongest signal).
pub(super) fn novelty_score(rank: usize) -> f64 {
    match rank {
        0 => 82.0,
        1 => 74.0,
        2 => 66.0,
        _ => 55.0,
    }
}

/// Scan score from a source's distinct destination / port fan-out. `None` below the scan floor.
pub(super) fn scan_score(distinct_dst: u64, distinct_ports: u64) -> Option<f64> {
    let d = distinct_dst.max(distinct_ports);
    if d < 50 {
        return None;
    }
    if d >= 500 {
        Some(92.0)
    } else if d >= 150 {
        Some(80.0)
    } else {
        Some(66.0)
    }
}

/// Concentration score from the top conversation's share of a node's traffic. `None` below 50%.
pub(super) fn concentration_score(top_ratio: f64) -> Option<f64> {
    if top_ratio < 0.5 {
        return None;
    }
    Some((60.0 + (top_ratio - 0.5) * 80.0).clamp(0.0, 100.0))
}

/// Human byte size (`1.2GB`, `512B`) for flow-finding labels.
pub(super) fn human_bytes(b: f64) -> String {
    const U: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = b.max(0.0);
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{v:.0}{}", U[i])
    } else {
        format!("{v:.1}{}", U[i])
    }
}

/// Severity weight for a passive event on an incident timeline: a fired alert is strongest, else
/// scale by syslog severity.
pub(super) fn event_signal_severity(action: EventAction, syslog_severity: Option<i16>) -> f64 {
    match action {
        EventAction::Fired => 85.0,
        EventAction::Refreshed => 70.0,
        EventAction::Cleared => 60.0,
        // An event that raised no alert is scored by what the device itself called it. Listed
        // rather than left to a wildcard so a new outcome has to choose a side.
        EventAction::Suppressed | EventAction::Info | EventAction::None => match syslog_severity {
            Some(s) if s <= 2 => 80.0,
            Some(3) => 65.0,
            Some(4) => 50.0,
            _ => 35.0,
        },
    }
}

/// A compact label for one event on an incident timeline (trap/app name + clipped message).
pub(super) fn incident_event_label(e: &EventRow) -> String {
    let head = e
        .trap_name
        .clone()
        .or_else(|| e.app_name.clone())
        .unwrap_or_else(|| e.kind.as_str().to_owned());
    let msg: String = e.message.chars().take(60).collect();
    format!("{head}: {msg}")
}

// ── Metric classification (by name) ───────────────────────────────────────────────────

/// Gauges suitable for anomaly/correlation: numeric, continuous, not raw counters or discrete
/// status enums.
pub(super) fn anomaly_usable(metric: &str) -> bool {
    if is_counter(metric) {
        return false;
    }
    if metric.contains("status") || metric.contains("state") {
        return false; // discrete enums (oper/admin/bgp state)
    }
    true
}

/// Raw counters (rate-derived elsewhere) — excluded from level-based anomaly/capacity reads.
/// The built-in catalog's declared [`yagra_common::MetricKind`] is authoritative; the substring
/// heuristic survives only for custom metrics outside the catalog (there is no DB handle here,
/// and a counter-ish name is the best remaining signal).
pub(super) fn is_counter(metric: &str) -> bool {
    match yagra_common::builtin_metric_kind(metric) {
        Some(yagra_common::MetricKind::Counter) => true,
        Some(yagra_common::MetricKind::Gauge) => false,
        None => {
            metric.contains("octets")
                || metric.contains("errors")
                || metric.contains("discards")
                || metric.contains("packets")
        }
    }
}

/// Percent-like utilization gauges the capacity forecast can extrapolate toward 100%.
pub(super) fn is_utilization(metric: &str) -> bool {
    metric.contains("pct")
        || metric.contains("util")
        || metric.contains("usage")
        || metric.ends_with("_pct")
}

/// Whether a metric belongs to the job's requested family filter.
pub(super) fn family_matches(params: &JobParams, metric: &str) -> bool {
    match params.family.as_str() {
        "reachability_interface" => metric == "icmp_rtt_ms" || metric.starts_with("if_"),
        "system" => {
            metric.contains("cpu")
                || metric.contains("mem")
                || metric.contains("temp")
                || metric.contains("load")
                || metric.contains("usage")
                || metric.contains("util")
                || metric.contains("processor")
                || metric.contains("disk")
                || metric.contains("storage")
                || metric.contains("sessions")
                || metric.contains("swap")
        }
        _ => true, // "all"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pt(t: i64, v: f64) -> MetricPoint {
        MetricPoint { t, v }
    }

    #[test]
    fn mean_and_stddev_basic() {
        let xs = [2.0, 4.0, 6.0];
        assert!((mean(&xs) - 4.0).abs() < 1e-9);
        assert!((stddev(&xs, 4.0) - (8.0_f64 / 3.0).sqrt()).abs() < 1e-9);
    }

    #[test]
    fn linreg_slope_recovers_line() {
        let xs = [0.0, 1.0, 2.0, 3.0];
        let ys = [1.0, 3.0, 5.0, 7.0]; // slope 2
        assert!((linreg_slope(&xs, &ys).unwrap() - 2.0).abs() < 1e-9);
    }

    #[test]
    fn pearson_perfect_and_inverse() {
        let xs = [1.0, 2.0, 3.0, 4.0];
        let up = [2.0, 4.0, 6.0, 8.0];
        let down = [8.0, 6.0, 4.0, 2.0];
        assert!((pearson(&xs, &up).unwrap() - 1.0).abs() < 1e-9);
        assert!((pearson(&xs, &down).unwrap() + 1.0).abs() < 1e-9);
        assert!(pearson(&xs, &[1.0, 1.0, 1.0, 1.0]).is_none()); // constant
    }

    #[test]
    fn correlate_uses_shared_timestamps() {
        let a = [pt(0, 1.0), pt(10, 2.0), pt(20, 3.0), pt(30, 4.0)];
        let b = [pt(10, 2.0), pt(20, 4.0), pt(30, 6.0), pt(40, 8.0)];
        let (r, n) = correlate(&a, &b).unwrap();
        assert_eq!(n, 3); // shared t = 10,20,30
        assert!((r - 1.0).abs() < 1e-9);
    }

    #[test]
    fn read_step_bounds_points() {
        // 14 days at ≤300 points ⇒ step ≥ ~4032s.
        let step = read_step(0, 14 * 86_400);
        assert!(step >= 60);
        assert!((14 * 86_400) as u64 / step <= MAX_POINTS as u64 + 1);
    }

    #[test]
    fn project_exhaustion_linear_rise() {
        // 50% climbing 1%/day → ~50 days to 100%.
        let day = 86_400;
        let pts: Vec<MetricPoint> = (0..15).map(|i| pt(i * day, 50.0 + i as f64)).collect();
        let proj = project_exhaustion(&pts).expect("rising series projects");
        assert!((proj.current - 64.0).abs() < 1e-6);
        let days = proj.tte_secs as f64 / 86_400.0;
        assert!((days - 36.0).abs() < 2.0); // (100-64)/1 ≈ 36 days
    }

    #[test]
    fn project_exhaustion_skips_flat_and_full() {
        let day = 86_400;
        let flat: Vec<MetricPoint> = (0..15).map(|i| pt(i * day, 40.0)).collect();
        assert!(project_exhaustion(&flat).is_none());
        let full: Vec<MetricPoint> = (0..15).map(|i| pt(i * day, 100.0)).collect();
        assert!(project_exhaustion(&full).is_none());
    }

    #[test]
    fn count_flaps_detects_gaps() {
        // Regular 60s samples with two big gaps.
        let mut pts = vec![pt(0, 1.0), pt(60, 1.0), pt(120, 1.0)];
        pts.push(pt(600, 1.0)); // gap 1 (480s > 120s)
        pts.push(pt(660, 1.0));
        pts.push(pt(2000, 1.0)); // gap 2
        assert_eq!(count_flaps(&pts, 60), 2);
    }

    #[test]
    fn score_anomaly_flags_spike_past_sigma() {
        // Flat baseline ~10 with tiny noise, then a recent spike to 30.
        let step = 300;
        let cutoff = 40 * step;
        let mut pts: Vec<MetricPoint> = (0..40)
            .map(|i| pt(i * step, 10.0 + ((i % 2) as f64) * 0.1))
            .collect();
        pts.push(pt(40 * step, 30.0)); // recent spike
        pts.push(pt(41 * step, 10.1));
        let found = score_anomaly(&pts, cutoff, 3.0).expect("spike flagged");
        assert!(found.score >= 75.0);
    }

    #[test]
    fn score_anomaly_ignores_normal_recent() {
        let step = 300;
        let cutoff = 40 * step;
        let mut pts: Vec<MetricPoint> = (0..40)
            .map(|i| pt(i * step, 10.0 + ((i % 2) as f64) * 0.5))
            .collect();
        pts.push(pt(40 * step, 10.2)); // within noise
        pts.push(pt(41 * step, 9.9));
        assert!(score_anomaly(&pts, cutoff, 3.0).is_none());
    }

    #[test]
    fn severity_thresholds() {
        assert_eq!(severity_for(95.0), "crit");
        assert_eq!(severity_for(80.0), "warn");
        assert_eq!(severity_for(50.0), "info");
    }

    #[test]
    fn every_severity_the_engine_writes_is_one_the_search_accepts() {
        // The two readers of the same set: `severity_for` writes it, the Saved-findings edge
        // validates `?severity=` against `FINDING_SEVERITIES`. A value the engine can produce but
        // the edge rejects would be findings nobody can filter for — so sweep the whole score
        // range rather than the three thresholds.
        for score in (-50..=150).map(f64::from) {
            let written = severity_for(score);
            assert!(
                FINDING_SEVERITIES.contains(&written),
                "severity_for({score}) = {written:?}, which is not in FINDING_SEVERITIES"
            );
        }
    }

    // ── incident_correlate neighbour expansion (ADR-022 Increment 2) ────────────────────────────

    fn sig(at_s: i64, kind: &'static str, severity: f64) -> IncidentSignal {
        IncidentSignal {
            at_s,
            severity,
            kind,
            label: "x".to_owned(),
        }
    }

    /// Coincidence is what stops a chatty upstream from manufacturing an incident for every quiet
    /// device hanging off it — adjacency in the graph alone is not evidence.
    #[test]
    fn signals_coincide_only_within_the_window() {
        let subject = [sig(1_000, "metric", 1.0)];
        assert!(signals_coincide(&subject, &[sig(1_100, "event", 1.0)], 300));
        assert!(signals_coincide(&subject, &[sig(900, "event", 1.0)], 300));
        // Exactly at the boundary counts; one second past it does not.
        assert!(signals_coincide(&subject, &[sig(1_300, "event", 1.0)], 300));
        assert!(!signals_coincide(
            &subject,
            &[sig(1_301, "event", 1.0)],
            300
        ));
        assert!(!signals_coincide(
            &subject,
            &[sig(9_999, "event", 1.0)],
            300
        ));
        // Either side empty is no corroboration, never a vacuous yes.
        assert!(!signals_coincide(&subject, &[], 300));
        assert!(!signals_coincide(&[], &[sig(1_000, "event", 1.0)], 300));
        // Any pair inside the window is enough, not every pair.
        assert!(signals_coincide(
            &subject,
            &[sig(9_999, "event", 1.0), sig(1_050, "event", 1.0)],
            300
        ));
    }

    /// **The security test.** A neighbour is consulted, scored and named only if it is itself in
    /// the job's authorized node set.
    ///
    /// The weaker rule — consult anything, name only what is visible — leaks by inference: the
    /// finding's score, its signal count, and whether it is emitted at all would move with data the
    /// caller cannot see. This is the analogue of `TopoLinkRepo::list_page`'s "both endpoints
    /// visible", and `ScopeKind::Group` is an inventory-folder subtree, never a topology
    /// neighbourhood — so a peer is not in scope merely by being adjacent.
    #[test]
    fn a_neighbour_outside_the_job_scope_is_never_consulted() {
        let (mine, theirs) = (Uuid::from_u128(1), Uuid::from_u128(2));
        let mut derived = Topology::new();
        // `theirs` is `mine`'s upstream, but the caller cannot see it.
        derived.add_dependency(NodeId::from(mine), NodeId::from(theirs));

        let authorized: HashSet<Uuid> = [mine].into_iter().collect();
        let n = one_hop_neighbours(&derived, &Topology::new(), &authorized);
        assert!(
            !n.contains_key(&mine),
            "an out-of-scope neighbour must not appear at all: {n:?}"
        );

        // …and with both endpoints authorized, the edge is used and labelled from the subject's
        // point of view.
        let authorized: HashSet<Uuid> = [mine, theirs].into_iter().collect();
        let n = one_hop_neighbours(&derived, &Topology::new(), &authorized);
        assert_eq!(
            n.get(&mine).map(Vec::as_slice),
            Some(&[(theirs, "upstream")][..])
        );
        assert_eq!(
            n.get(&theirs).map(Vec::as_slice),
            Some(&[(mine, "downstream")][..])
        );
    }

    /// The manual graph counts as evidence alongside the derived one, and a node appearing in both
    /// is listed once. This is what makes the expansion useful on a deployment still in `manual`
    /// topology mode — which is the default, and where upgrades land.
    #[test]
    fn hand_authored_and_derived_edges_are_unioned_without_duplicates() {
        let (child, parent) = (Uuid::from_u128(1), Uuid::from_u128(2));
        let mut derived = Topology::new();
        derived.add_dependency(NodeId::from(child), NodeId::from(parent));
        let mut manual = Topology::new();
        manual.add_dependency(NodeId::from(child), NodeId::from(parent));

        let authorized: HashSet<Uuid> = [child, parent].into_iter().collect();
        let n = one_hop_neighbours(&derived, &manual, &authorized);
        assert_eq!(n[&child], vec![(parent, "upstream")], "listed twice");

        // A manual-only edge still counts, so the feature is not dead in `manual` mode.
        let n = one_hop_neighbours(&Topology::new(), &manual, &authorized);
        assert_eq!(n[&child], vec![(parent, "upstream")]);
    }

    /// A self-edge is not a neighbour, and a node with no edges gets no entry at all (rather than
    /// an empty vector the caller would have to distinguish).
    #[test]
    fn a_node_is_not_its_own_neighbour() {
        let a = Uuid::from_u128(1);
        let mut topo = Topology::new();
        topo.add_dependency(NodeId::from(a), NodeId::from(a));
        let authorized: HashSet<Uuid> = [a].into_iter().collect();
        assert!(one_hop_neighbours(&topo, &Topology::new(), &authorized).is_empty());
        assert!(one_hop_neighbours(&Topology::new(), &Topology::new(), &authorized).is_empty());
    }

    /// The fan-out bound. Twenty subjects each expanding to four peers is the worst case, and the
    /// cache cap is what keeps `incident_signals`' three-store fetch from multiplying by it.
    #[test]
    fn the_incident_cache_bounds_the_worst_case_fan_out() {
        // Every subject fits, plus room for each one's peers.
        assert_eq!(INCIDENT_CACHE_CAP, INCIDENT_NODE_CAP * (NEIGHBOUR_CAP + 1));
        // One event bucket: the resolution at which this codebase already treats passive events as
        // contemporaneous. Widening it would let an unrelated upstream corroborate anything.
        assert_eq!(NEIGHBOUR_COINCIDENCE_SECS, EVENT_BUCKET_SECS);
    }

    #[test]
    fn storm_detail_carries_peak_at_for_a_localizable_label() {
        // `when_label` is pre-rendered English (`rel_label`), so the WebUI needs the raw peak time
        // to format a JA-correct relative label. Regression guard: don't drop `peak_at`.
        let d = storm_detail(42.0, 3.5, 1_700_000_000);
        assert_eq!(d["peak"], 42.0);
        assert_eq!(d["baseline_mean"], 3.5);
        assert_eq!(d["peak_at"], 1_700_000_000_i64);
        assert_eq!(d["bucket_secs"], EVENT_BUCKET_SECS);
    }

    #[test]
    fn traffic_detail_carries_peak_at_for_a_localizable_label() {
        let d = traffic_detail(1_048_576.0, 1024.0, 1_700_000_500);
        assert_eq!(d["peak_bytes"], 1_048_576.0);
        assert_eq!(d["baseline_mean_bytes"], 1024.0);
        assert_eq!(d["peak_at"], 1_700_000_500_i64);
    }

    #[test]
    fn burst_score_flags_upward_spike_only() {
        let baseline = vec![10.0, 12.0, 11.0, 9.0, 10.0, 11.0];
        // A big upward peak past 3σ scores; a value within the baseline does not.
        assert!(burst_score(&baseline, 60.0, 3.0).is_some());
        assert!(burst_score(&baseline, 11.0, 3.0).is_none());
        // One-sided: a drop below the mean is never a burst.
        assert!(burst_score(&baseline, 0.0, 3.0).is_none());
        assert!(burst_score(&[], 100.0, 3.0).is_none());
    }

    #[test]
    fn severity_shift_needs_a_real_skew() {
        assert!(severity_shift_score(0.1, 0.15).is_none()); // +0.05, below threshold
        let s = severity_shift_score(0.1, 0.6).unwrap(); // +0.5 skew
        assert!(s > 75.0);
    }

    #[test]
    fn severity_high_fractions_counts_err_and_worse() {
        let scope: HashSet<Uuid> = [Uuid::from_u128(1)].into_iter().collect();
        let counts = vec![
            EventSeverityCount {
                node_id: Uuid::from_u128(1),
                severity: 3,
                count: 3,
            }, // err → high
            EventSeverityCount {
                node_id: Uuid::from_u128(1),
                severity: 6,
                count: 7,
            }, // info → not high
            EventSeverityCount {
                node_id: Uuid::from_u128(9),
                severity: 0,
                count: 100,
            }, // out of scope
        ];
        let f = severity_high_fractions(&counts, &scope);
        let (high, total, frac) = f[&Uuid::from_u128(1)];
        assert_eq!((high, total), (3, 10));
        assert!((frac - 0.3).abs() < 1e-9);
        assert!(!f.contains_key(&Uuid::from_u128(9)));
    }

    #[test]
    fn first_novel_finds_highest_ranked_new_key() {
        let baseline: HashSet<String> = ["a".to_owned(), "b".to_owned()].into_iter().collect();
        let recent = vec!["a".to_owned(), "z".to_owned(), "b".to_owned()];
        assert_eq!(first_novel(&recent, &baseline), Some(("z".to_owned(), 1)));
        // Nothing new → None.
        let recent2 = vec!["a".to_owned(), "b".to_owned()];
        assert_eq!(first_novel(&recent2, &baseline), None);
    }

    #[test]
    fn scan_and_concentration_thresholds() {
        assert!(scan_score(10, 5).is_none());
        assert_eq!(scan_score(600, 3), Some(92.0));
        assert_eq!(scan_score(3, 200), Some(80.0)); // vertical scan on the port axis
        assert!(concentration_score(0.4).is_none());
        assert_eq!(concentration_score(1.0), Some(100.0));
    }

    #[test]
    fn human_bytes_scales_units() {
        assert_eq!(human_bytes(512.0), "512B");
        assert_eq!(human_bytes(1536.0), "1.5KB");
        assert_eq!(human_bytes(5.0 * 1024.0 * 1024.0 * 1024.0), "5.0GB");
    }

    #[test]
    fn event_signal_severity_ranks_fired_highest() {
        assert!(
            event_signal_severity(EventAction::Fired, None)
                > event_signal_severity(EventAction::Cleared, None)
        );
        assert!(
            event_signal_severity(EventAction::None, Some(0))
                > event_signal_severity(EventAction::None, Some(6)),
            "emergency syslog outweighs debug"
        );
    }

    #[test]
    fn metric_classification() {
        assert!(is_counter("if_hc_in_octets"));
        assert!(is_counter("if_in_errors"));
        assert!(!is_counter("huawei_cpu_usage"));
        assert!(!anomaly_usable("if_oper_status"));
        assert!(!anomaly_usable("if_hc_in_octets"));
        assert!(anomaly_usable("icmp_rtt_ms"));
        assert!(is_utilization("huawei_mem_usage"));
        assert!(is_utilization("ucd_disk_used_pct"));
        assert!(!is_utilization("icmp_rtt_ms"));
    }
}
