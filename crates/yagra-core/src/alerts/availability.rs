// SPDX-License-Identifier: AGPL-3.0-only
//! One node's availability over a period (ADR-223): when it was down, in maintenance, or not
//! recorded at all, and the share of the rest it was up.
//!
//! "Down" is the node's liveness alert (`__liveness__`) being open — never an absence of ICMP
//! samples, which a stopped poller produces just as well as a dead device (ADR-156 decision 3).
//! A blip shorter than the rule's dwell therefore counts as up, exactly as it does for alerting.
//!
//! Everything here is pure: the caller (`api/availability.rs`) reads the stores and hands this
//! module plain intervals, so every rule below is reachable from a test.
//!
//! ⚠️ **Two facts are read off the *current* inventory, because nothing recorded them at the
//! time**: which maintenance windows cover the node (a folder, profile or label window stores only
//! its target) and which pool's monitoring gaps apply (a gap names its pool, not its nodes). A node
//! that moved shows its new home's history. ADR-223 decision 3 accepts that.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// The most spans one answer carries. Past it the oldest are dropped and
/// [`NodeAvailability::truncated_before_unix_ms`] says where the answer stops being complete.
pub const SPANS_MAX: usize = 1_000;

/// How far back the graph looks. The tokens are the query parameter's values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub enum AvailabilityPeriod {
    #[serde(rename = "24h")]
    Day,
    #[serde(rename = "7d")]
    Week,
    #[serde(rename = "30d")]
    Month,
    #[serde(rename = "90d")]
    Quarter,
}

yagra_base::stored_enum::token_enum!(AvailabilityPeriod, [
    Day => "24h",
    Week => "7d",
    Month => "30d",
    Quarter => "90d",
]);

impl AvailabilityPeriod {
    /// The period's length in milliseconds.
    #[must_use]
    pub fn millis(self) -> i64 {
        const DAY_MS: i64 = 86_400_000;
        match self {
            Self::Day => DAY_MS,
            Self::Week => 7 * DAY_MS,
            Self::Month => 30 * DAY_MS,
            Self::Quarter => 90 * DAY_MS,
        }
    }
}

/// What a span of time was. Up is not a kind: it is whatever no span covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SpanKind {
    /// The node's liveness alert was open.
    Down,
    /// A maintenance window covered the node.
    Maintenance,
    /// Nothing can say: before the node existed, before the alert history's retention, or while
    /// the poller of the node's pool could not reach core.
    NoRecord,
}

impl SpanKind {
    /// Which kind wins where two overlap, highest first. Not recorded beats down: during a
    /// monitoring gap an open alert stays open because nothing closes on absence, so "down" there
    /// would be a guess. Down beats maintenance: the alert really was open while a window's dwell
    /// was still closing it.
    fn rank(self) -> u8 {
        match self {
            Self::NoRecord => 3,
            Self::Down => 2,
            Self::Maintenance => 1,
        }
    }
}

/// One stretch of time of one kind, clipped to the period.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct AvailabilitySpan {
    pub kind: SpanKind,
    pub from_unix_ms: i64,
    pub to_unix_ms: i64,
}

/// One node's availability over one period. Totals cover the whole period even when `spans` was
/// cut.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct NodeAvailability {
    pub node_id: Uuid,
    pub period: AvailabilityPeriod,
    pub from_unix_ms: i64,
    pub to_unix_ms: i64,
    /// Every down, maintenance and not-recorded stretch, oldest first. Time no span covers was up.
    pub spans: Vec<AvailabilitySpan>,
    /// Set when more than [`SPANS_MAX`] spans existed: the oldest were dropped, and before this
    /// instant `spans` is incomplete (read it as not recorded, never as up).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub truncated_before_unix_ms: Option<i64>,
    pub up_secs: i64,
    pub down_secs: i64,
    pub maintenance_secs: i64,
    pub no_record_secs: i64,
    /// How many times the liveness alert opened, or was already open, inside the period.
    pub outages: u32,
    /// Up ÷ (up + down) × 100. Maintenance and not-recorded time are left out of both sides, the
    /// rule `reports::render::availability_from_snapshots` uses. `None` with neither.
    pub availability_pct: Option<f64>,
}

/// One `__liveness__` row of `alert_history`.
///
/// ⚠️ A resolve row's `at_unix_ms` is the **fire's** time (the engine records the alert it took
/// out of the active set), so the recovery instant is only in `recorded_at_ms`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LivenessRow {
    pub at_unix_ms: i64,
    pub recorded_at_ms: i64,
    pub resolved: bool,
}

/// What [`compute`] is handed. Intervals are `[from, to)` in Unix milliseconds and need not be
/// sorted, merged or inside the period.
#[derive(Debug, Clone, Default)]
pub struct Inputs {
    /// The node's liveness rows: the last one recorded before the period, if any, then every one
    /// inside it, ordered by `recorded_at` with a fire before a resolve recorded together.
    pub liveness: Vec<LivenessRow>,
    pub maintenance: Vec<(i64, i64)>,
    pub monitoring_gaps: Vec<(i64, i64)>,
    pub created_at_ms: i64,
    /// The oldest instant the alert history still holds (now − the alert-linked retention).
    pub history_start_ms: i64,
}

/// The down intervals the liveness rows describe, unclipped, plus how many there were.
fn down_intervals(rows: &[LivenessRow], from_ms: i64, now_ms: i64) -> Vec<(i64, i64)> {
    let mut out = Vec::new();
    let mut open: Option<i64> = None;
    for (i, row) in rows.iter().enumerate() {
        if row.resolved {
            match open.take() {
                Some(start) => out.push((start, row.recorded_at_ms)),
                // The fire is gone (pruned by retention) while its clear survived. Only the first
                // row can be that: anything later had its fire read above it. Count it as down
                // from the start of the period, because it was open at least that long.
                None if i == 0 && row.recorded_at_ms > from_ms => {
                    out.push((from_ms.min(row.at_unix_ms), row.recorded_at_ms));
                }
                None => {}
            }
        } else if open.is_none() {
            // A second fire with no clear between is the same outage (a restart re-fired it).
            open = Some(row.at_unix_ms);
        }
    }
    if let Some(start) = open {
        out.push((start, now_ms));
    }
    out
}

/// Build the answer for `[now − period, now)`.
#[must_use]
pub fn compute(
    node_id: Uuid,
    period: AvailabilityPeriod,
    now_ms: i64,
    inputs: &Inputs,
) -> NodeAvailability {
    let from = now_ms - period.millis();
    let to = now_ms;
    let clip = |(a, b): (i64, i64)| -> Option<(i64, i64)> {
        let (a, b) = (a.max(from), b.min(to));
        (a < b).then_some((a, b))
    };

    let downs: Vec<(i64, i64)> = down_intervals(&inputs.liveness, from, now_ms)
        .into_iter()
        .filter_map(clip)
        .collect();
    let outages = u32::try_from(downs.len()).unwrap_or(u32::MAX);

    let mut tagged: Vec<(SpanKind, i64, i64)> = Vec::new();
    tagged.extend(downs.iter().map(|&(a, b)| (SpanKind::Down, a, b)));
    tagged.extend(
        inputs
            .maintenance
            .iter()
            .filter_map(|&w| clip(w))
            .map(|(a, b)| (SpanKind::Maintenance, a, b)),
    );
    let unrecorded_until = inputs.created_at_ms.max(inputs.history_start_ms);
    tagged.extend(
        inputs
            .monitoring_gaps
            .iter()
            .copied()
            .chain(std::iter::once((from, unrecorded_until)))
            .filter_map(clip)
            .map(|(a, b)| (SpanKind::NoRecord, a, b)),
    );

    let spans = flatten(&tagged);
    let secs_of = |kind: SpanKind| -> i64 {
        spans
            .iter()
            .filter(|s| s.kind == kind)
            .map(|s| s.to_unix_ms - s.from_unix_ms)
            .sum::<i64>()
            / 1000
    };
    let (down_secs, maintenance_secs, no_record_secs) = (
        secs_of(SpanKind::Down),
        secs_of(SpanKind::Maintenance),
        secs_of(SpanKind::NoRecord),
    );
    let up_secs = ((to - from) / 1000 - down_secs - maintenance_secs - no_record_secs).max(0);
    let availability_pct =
        (up_secs + down_secs > 0).then(|| up_secs as f64 / (up_secs + down_secs) as f64 * 100.0);

    let (spans, truncated_before_unix_ms) = if spans.len() > SPANS_MAX {
        let kept = spans[spans.len() - SPANS_MAX..].to_vec();
        let cut = kept[0].from_unix_ms;
        (kept, Some(cut))
    } else {
        (spans, None)
    };

    NodeAvailability {
        node_id,
        period,
        from_unix_ms: from,
        to_unix_ms: to,
        spans,
        truncated_before_unix_ms,
        up_secs,
        down_secs,
        maintenance_secs,
        no_record_secs,
        outages,
        availability_pct,
    }
}

/// Lay overlapping tagged intervals onto one timeline: at each instant the highest-ranked kind
/// wins, and neighbours of one kind are joined.
fn flatten(tagged: &[(SpanKind, i64, i64)]) -> Vec<AvailabilitySpan> {
    let mut edges: Vec<i64> = tagged.iter().flat_map(|&(_, a, b)| [a, b]).collect();
    edges.sort_unstable();
    edges.dedup();
    let mut out: Vec<AvailabilitySpan> = Vec::new();
    for pair in edges.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        let Some(kind) = tagged
            .iter()
            .filter(|&&(_, s, e)| s <= a && b <= e)
            .map(|&(k, _, _)| k)
            .max_by_key(|k| k.rank())
        else {
            continue;
        };
        match out.last_mut() {
            Some(last) if last.kind == kind && last.to_unix_ms == a => last.to_unix_ms = b,
            _ => out.push(AvailabilitySpan {
                kind,
                from_unix_ms: a,
                to_unix_ms: b,
            }),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const H: i64 = 3_600_000;
    const NOW: i64 = 1_000 * 24 * H;
    const NODE: Uuid = Uuid::nil();

    fn fire(at: i64) -> LivenessRow {
        LivenessRow {
            at_unix_ms: at,
            recorded_at_ms: at,
            resolved: false,
        }
    }

    /// A clear for the fire at `fired`, recorded at `back` — the shape the engine writes.
    fn clear(fired: i64, back: i64) -> LivenessRow {
        LivenessRow {
            at_unix_ms: fired,
            recorded_at_ms: back,
            resolved: true,
        }
    }

    fn day(inputs: Inputs) -> NodeAvailability {
        compute(NODE, AvailabilityPeriod::Day, NOW, &inputs)
    }

    fn old_node() -> Inputs {
        Inputs {
            created_at_ms: 0,
            history_start_ms: 0,
            ..Inputs::default()
        }
    }

    #[test]
    fn a_quiet_day_is_all_up() {
        let a = day(old_node());
        assert!(a.spans.is_empty());
        assert_eq!(a.up_secs, 86_400);
        assert_eq!(a.availability_pct, Some(100.0));
        assert_eq!(a.outages, 0);
    }

    #[test]
    fn an_outage_runs_from_the_fire_to_the_clears_recorded_time() {
        let fired = NOW - 5 * H;
        let a = day(Inputs {
            liveness: vec![fire(fired), clear(fired, fired + H)],
            ..old_node()
        });
        assert_eq!(
            a.spans,
            vec![AvailabilitySpan {
                kind: SpanKind::Down,
                from_unix_ms: fired,
                to_unix_ms: fired + H
            }]
        );
        assert_eq!(a.down_secs, 3_600);
        assert_eq!(a.outages, 1);
        let pct = a.availability_pct.unwrap();
        assert!((pct - 23.0 / 24.0 * 100.0).abs() < 1e-9, "{pct}");
    }

    #[test]
    fn a_fire_before_the_period_counts_from_the_periods_start() {
        let fired = NOW - 30 * H;
        let a = day(Inputs {
            liveness: vec![fire(fired), clear(fired, NOW - 20 * H)],
            ..old_node()
        });
        assert_eq!(a.spans[0].from_unix_ms, NOW - 24 * H);
        assert_eq!(a.down_secs, 4 * 3_600);
        assert_eq!(a.outages, 1);
    }

    #[test]
    fn a_clear_whose_fire_was_pruned_is_down_from_the_periods_start() {
        let a = day(Inputs {
            liveness: vec![clear(NOW - 40 * H, NOW - 22 * H)],
            ..old_node()
        });
        assert_eq!(a.down_secs, 2 * 3_600);
        assert_eq!(a.outages, 1);
    }

    #[test]
    fn a_clear_recorded_before_the_period_adds_nothing() {
        let a = day(Inputs {
            liveness: vec![clear(NOW - 40 * H, NOW - 30 * H)],
            ..old_node()
        });
        assert!(a.spans.is_empty());
        assert_eq!(a.outages, 0);
    }

    #[test]
    fn an_alert_still_open_is_down_until_now() {
        let fired = NOW - 2 * H;
        let a = day(Inputs {
            liveness: vec![fire(fired)],
            ..old_node()
        });
        assert_eq!(a.spans[0].to_unix_ms, NOW);
        assert_eq!(a.down_secs, 2 * 3_600);
    }

    #[test]
    fn a_second_fire_without_a_clear_is_the_same_outage() {
        let a = day(Inputs {
            liveness: vec![
                fire(NOW - 6 * H),
                fire(NOW - 5 * H),
                clear(NOW - 6 * H, NOW - 4 * H),
            ],
            ..old_node()
        });
        assert_eq!(a.outages, 1);
        assert_eq!(a.down_secs, 2 * 3_600);
    }

    #[test]
    fn maintenance_is_left_out_of_the_share_and_down_wins_where_they_overlap() {
        let a = day(Inputs {
            liveness: vec![fire(NOW - 10 * H), clear(NOW - 10 * H, NOW - 8 * H)],
            maintenance: vec![(NOW - 9 * H, NOW - 6 * H)],
            ..old_node()
        });
        assert_eq!(a.down_secs, 2 * 3_600);
        assert_eq!(a.maintenance_secs, 2 * 3_600);
        assert_eq!(a.up_secs, 20 * 3_600);
        let pct = a.availability_pct.unwrap();
        assert!((pct - 20.0 / 22.0 * 100.0).abs() < 1e-9, "{pct}");
        assert_eq!(
            a.spans.iter().map(|s| s.kind).collect::<Vec<_>>(),
            vec![SpanKind::Down, SpanKind::Maintenance]
        );
    }

    #[test]
    fn time_before_the_node_existed_is_not_recorded() {
        let a = day(Inputs {
            created_at_ms: NOW - 6 * H,
            history_start_ms: 0,
            ..Inputs::default()
        });
        assert_eq!(a.no_record_secs, 18 * 3_600);
        assert_eq!(a.up_secs, 6 * 3_600);
        assert_eq!(a.availability_pct, Some(100.0));
    }

    #[test]
    fn time_older_than_the_history_retention_is_not_recorded() {
        let a = compute(
            NODE,
            AvailabilityPeriod::Quarter,
            NOW,
            &Inputs {
                created_at_ms: 0,
                history_start_ms: NOW - 30 * 24 * H,
                ..Inputs::default()
            },
        );
        assert_eq!(a.no_record_secs, 60 * 86_400);
    }

    #[test]
    fn a_monitoring_gap_hides_an_open_alert() {
        let a = day(Inputs {
            liveness: vec![fire(NOW - 10 * H), clear(NOW - 10 * H, NOW - 4 * H)],
            monitoring_gaps: vec![(NOW - 8 * H, NOW - 6 * H)],
            ..old_node()
        });
        assert_eq!(a.down_secs, 4 * 3_600);
        assert_eq!(a.no_record_secs, 2 * 3_600);
        assert_eq!(
            a.spans.iter().map(|s| s.kind).collect::<Vec<_>>(),
            vec![SpanKind::Down, SpanKind::NoRecord, SpanKind::Down]
        );
        // The gap split the span on screen, not the outage.
        assert_eq!(a.outages, 1);
    }

    #[test]
    fn a_node_never_recorded_has_no_share() {
        let a = day(Inputs {
            created_at_ms: NOW,
            history_start_ms: 0,
            ..Inputs::default()
        });
        assert_eq!(a.availability_pct, None);
    }

    #[test]
    fn past_the_span_cap_the_oldest_are_dropped_and_the_cut_is_named() {
        let liveness: Vec<LivenessRow> = (0..(SPANS_MAX as i64 + 5))
            .flat_map(|i| {
                let at = NOW - 23 * H + i * 60_000;
                [fire(at), clear(at, at + 10_000)]
            })
            .collect();
        let a = day(Inputs {
            liveness,
            ..old_node()
        });
        assert_eq!(a.spans.len(), SPANS_MAX);
        assert_eq!(a.truncated_before_unix_ms, Some(a.spans[0].from_unix_ms));
        // Totals still count every outage.
        assert_eq!(a.outages, SPANS_MAX as u32 + 5);
        assert_eq!(a.down_secs, (SPANS_MAX as i64 + 5) * 10);
    }

    #[test]
    fn every_period_round_trips_through_its_token_and_through_serde() {
        for &p in AvailabilityPeriod::ALL.iter() {
            assert_eq!(AvailabilityPeriod::from_token(p.as_str()), Some(p));
            let json = serde_json::to_string(&p).unwrap();
            assert_eq!(json, format!("\"{}\"", p.as_str()));
        }
    }
}
