// SPDX-License-Identifier: AGPL-3.0-only
// The `detail` field names each report body reads.
//
// **These assertions look trivial and are not.** `detail` is free-form JSON composed in Rust
// (`crates/yagra-core/src/analysis/*.rs`), so a key renamed there is not a compile error here — the
// reader falls through to its default and the body renders `0`, an empty bar, or `∞`, all of which
// look like "the analysis found nothing". Naming each key in a test is the only place the two
// halves of that mirror are written down together.
//
// The defaults are asserted for the same reason: each one was chosen so a missing key sorts and
// tones the way the *absence* should read, and three of them disagree with each other deliberately.
import { describe, expect, it } from 'vitest';
import {
  capacityCurrent,
  capacitySlopePerDay,
  capacityTteDays,
  correlationPair,
  correlationR,
  correlationText,
  eventFlapClears,
  eventFlapCycles,
  eventFlapFires,
  flapCount,
  incidentEarliest,
  incidentKinds,
  incidentTimeline,
  perHour,
  ratioOver,
  saturationConversationBytes,
  saturationInterfaceBps,
  saturationNodeBytes,
  saturationRatio,
  shiftBaselineFrac,
  shiftDeltaPp,
  shiftRecentFrac,
  shiftVolume,
  stormBaseline,
  stormPeak,
  stormRatio,
  talkerAddr,
  talkerBytes,
  talkerRank,
  trafficBaselineBytes,
  trafficPeakBytes,
  trafficRatio,
} from './findingFacts';
import type { AnalysisFinding } from '../../types/api';

function f(detail: Record<string, unknown> | null): AnalysisFinding {
  return {
    id: 'x',
    score: 80,
    severity: 'warn',
    node_id: 'n1',
    node_name: 'edge-1',
    metric: 'icmp_rtt_ms',
    kind: 'spike',
    when_label: '1h ago',
    duration: 'ongoing',
    detail,
  } as AnalysisFinding;
}

describe('ratioOver', () => {
  it('divides, and calls a peak over no baseline unbounded rather than zero', () => {
    expect(ratioOver(10, 2)).toBe(5);
    // Infinity, not 0: a node that was silent and is now shouting is the MOST anomalous case, so it
    // has to sort to the top and fill the meter. Returning 0 would bury it.
    expect(ratioOver(10, 0)).toBe(Infinity);
    expect(ratioOver(0, 0)).toBe(Infinity);
    expect(ratioOver(10, -1)).toBe(Infinity);
  });
});

describe('capacity', () => {
  it('reads tte_days / current / slope_per_day', () => {
    const row = f({ tte_days: 12, current: 71.5, slope_per_day: 0.8 });
    expect(capacityTteDays(row)).toBe(12);
    expect(capacityCurrent(row)).toBe(71.5);
    expect(capacitySlopePerDay(row)).toBe(0.8);
  });

  it('defaults a missing projection to Infinity, never to zero days left', () => {
    // `?? 0` here would tone every row with no projection as `crit` and sort it first.
    expect(capacityTteDays(f({}))).toBe(Infinity);
    expect(capacityCurrent(f({}))).toBe(0);
    expect(capacitySlopePerDay(f({}))).toBe(0);
  });
});

describe('correlation', () => {
  it('reads r and splits the pair the backend joined', () => {
    const row = f({ r: -0.87 });
    expect(correlationR(row)).toBe(-0.87);
    expect(correlationPair({ ...row, metric: 'cpu_pct ↔ if_in_bps' })).toEqual([
      'cpu_pct',
      'if_in_bps',
    ]);
  });

  it('yields the whole string and an empty half when there is no separator', () => {
    // A row from an older core, or a future one using another separator: visibly odd beats throwing
    // inside a report body.
    expect(correlationPair({ ...f({}), metric: 'cpu_pct' })).toEqual(['cpu_pct', '']);
  });

  it('always signs r, so +0.91 and -0.91 never read alike', () => {
    expect(correlationText(0.91)).toBe('r = +0.91');
    expect(correlationText(-0.91)).toBe('r = -0.91');
    expect(correlationText(0)).toBe('r = +0.00');
  });
});

describe('event flap', () => {
  it('reads cycles / fires / clears', () => {
    const row = f({ cycles: 9, fires: 5, clears: 4 });
    expect(eventFlapCycles(row)).toBe(9);
    expect(eventFlapFires(row)).toBe(5);
    expect(eventFlapClears(row)).toBe(4);
  });
});

describe('perHour', () => {
  it('is one reader for both flap analyses, which write the same field', () => {
    // Event flap and interface flap each had their own one-line copy of this.
    expect(perHour(f({ per_hour: 2.5 }))).toBe(2.5);
    expect(perHour(f({}))).toBe(0);
  });
});

describe('interface flap', () => {
  it('reads flaps', () => {
    expect(flapCount(f({ flaps: 14 }))).toBe(14);
    expect(flapCount(f({}))).toBe(0);
  });
});

describe('event storm vs traffic anomaly', () => {
  // The two analyses compute the same ratio over DIFFERENT fields. Merging the readers would have
  // made one of them silently read the other's baseline.
  it('reads counts for a storm and bytes for a traffic anomaly', () => {
    const storm = f({ peak: 300, baseline_mean: 20 });
    expect(stormPeak(storm)).toBe(300);
    expect(stormBaseline(storm)).toBe(20);
    expect(stormRatio(storm)).toBe(15);

    const traffic = f({ peak_bytes: 8000, baseline_mean_bytes: 2000 });
    expect(trafficPeakBytes(traffic)).toBe(8000);
    expect(trafficBaselineBytes(traffic)).toBe(2000);
    expect(trafficRatio(traffic)).toBe(4);
  });

  it('does not read each other’s fields', () => {
    expect(stormPeak(f({ peak_bytes: 8000 }))).toBe(0);
    expect(trafficPeakBytes(f({ peak: 300 }))).toBe(0);
  });

  it('reports an unbounded ratio when the node had no baseline', () => {
    expect(stormRatio(f({ peak: 300 }))).toBe(Infinity);
    expect(trafficRatio(f({ peak_bytes: 8000 }))).toBe(Infinity);
  });
});

describe('incident correlate', () => {
  const sig = (at: number, kind: string) => ({ at, kind, label: 'x', severity: 50 });

  it('reads the timeline, its kinds, and when the incident began', () => {
    const row = f({ timeline: [sig(300, 'flow'), sig(100, 'metric'), sig(200, 'metric')] });
    expect(incidentTimeline(row)).toHaveLength(3);
    expect([...incidentKinds(row)].sort()).toEqual(['flow', 'metric']);
    expect(incidentEarliest(row)).toBe(100);
  });

  it('drops a signal with no usable timestamp instead of plotting it', () => {
    const row = f({ timeline: [sig(100, 'metric'), { kind: 'event' }, null, 'nope', 7] });
    expect(incidentTimeline(row)).toHaveLength(1);
    expect(incidentEarliest(row)).toBe(100);
  });

  it('reads a missing or malformed timeline as empty, not as a throw', () => {
    expect(incidentTimeline(f({}))).toEqual([]);
    expect(incidentTimeline(f({ timeline: 'soon' }))).toEqual([]);
    expect(incidentTimeline(f(null))).toEqual([]);
    expect(incidentEarliest(f({}))).toBe(0);
    expect(incidentKinds(f({})).size).toBe(0);
  });
});

describe('saturation', () => {
  it('reads a STORED ratio, not a computed one', () => {
    // Unlike the storm and traffic ratios above. Same word, different provenance.
    const row = f({ ratio: 0.91, conversation_bytes: 900, node_bytes: 1000, interface_bps: 1e9 });
    expect(saturationRatio(row)).toBe(0.91);
    expect(saturationConversationBytes(row)).toBe(900);
    expect(saturationNodeBytes(row)).toBe(1000);
    expect(saturationInterfaceBps(row)).toBe(1e9);
  });

  it('leaves an unknown link speed undefined rather than defaulting it to zero', () => {
    // The body renders "unknown speed" differently from "zero speed"; a `?? 0` here would erase
    // that difference and claim every un-speeded port runs at 0 bps.
    expect(saturationInterfaceBps(f({}))).toBeUndefined();
    expect(saturationRatio(f({}))).toBe(0);
  });
});

describe('severity shift', () => {
  it('reads both fractions and reports the delta in percentage POINTS', () => {
    const row = f({ recent_high_frac: 0.42, baseline_high_frac: 0.1, recent_total: 880 });
    expect(shiftRecentFrac(row)).toBe(0.42);
    expect(shiftBaselineFrac(row)).toBe(0.1);
    // 0.42 - 0.10 = 0.32 of the whole ⇒ 32 points, not 320 percent.
    expect(shiftDeltaPp(row)).toBeCloseTo(32, 10);
    expect(shiftVolume(row)).toBe(880);
  });

  it('reports a downward shift as a negative delta', () => {
    expect(shiftDeltaPp(f({ recent_high_frac: 0.1, baseline_high_frac: 0.4 }))).toBeCloseTo(-30, 10);
  });
});

describe('talker shift', () => {
  it('reads addr / bytes / rank', () => {
    const row = f({ addr: '10.0.0.9', bytes: 4096, rank: 2 });
    expect(talkerAddr(row)).toBe('10.0.0.9');
    expect(talkerBytes(row)).toBe(4096);
    expect(talkerRank(row)).toBe(2);
  });

  it('defaults a missing rank to 99 so it sorts last and tones as info', () => {
    // `?? 1` would make every rank-less row the busiest talker, in colour and in order.
    expect(talkerRank(f({}))).toBe(99);
    expect(talkerAddr(f({}))).toBeUndefined();
  });
});
