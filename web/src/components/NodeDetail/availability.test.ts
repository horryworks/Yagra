// SPDX-License-Identifier: AGPL-3.0-only
// The availability graph's bars (ADR-223). The arithmetic that can be quietly wrong: bars aligned
// to the display zone's clock (a 23-hour day included), time the server dropped read as not
// recorded rather than up, and the future part of the last bar counted as nothing at all.

import { describe, expect, it } from 'vitest';
import {
  AVAILABILITY_PERIODS,
  availabilityBars,
  barBounds,
  barIndexAt,
  nextBarIndex,
  barSegments,
  barsAreDays,
  formatAvailabilityPct,
} from './availability';
import type { NodeAvailability } from '../../types/api';

const H = 3_600_000;
// 2026-11-01 09:15 UTC.
const NOW = Date.UTC(2026, 10, 1, 9, 15);

function answer(over: Partial<NodeAvailability>): NodeAvailability {
  return {
    node_id: '00000000-0000-0000-0000-000000000000',
    period: '24h',
    from_unix_ms: NOW - 24 * H,
    to_unix_ms: NOW,
    spans: [],
    up_secs: 0,
    down_secs: 0,
    maintenance_secs: 0,
    no_record_secs: 0,
    outages: 0,
    availability_pct: null,
    ...over,
  };
}

describe('barBounds', () => {
  it('cuts each period into its number of bars, the last one holding now', () => {
    const counts = { '24h': 48, '7d': 42, '30d': 30, '90d': 90 } as const;
    for (const p of AVAILABILITY_PERIODS) {
      const bars = barBounds(p, NOW, 'UTC');
      expect(bars).toHaveLength(counts[p]);
      const last = bars[bars.length - 1];
      expect(last.fromMs <= NOW && NOW < last.toMs).toBe(true);
      for (let i = 1; i < bars.length; i++) expect(bars[i].fromMs).toBe(bars[i - 1].toMs);
    }
  });

  it('aligns half-hour bars to the clock', () => {
    const last = barBounds('24h', NOW, 'UTC').at(-1)!;
    expect(last.fromMs).toBe(Date.UTC(2026, 10, 1, 9, 0));
    expect(last.toMs).toBe(Date.UTC(2026, 10, 1, 9, 30));
  });

  it('starts a day bar at midnight in the display zone, not in UTC', () => {
    // 09:15 UTC is 18:15 in Tokyo, so the day bar began at 15:00 UTC the day before.
    const last = barBounds('30d', NOW, 'Asia/Tokyo').at(-1)!;
    expect(last.fromMs).toBe(Date.UTC(2026, 9, 31, 15, 0));
  });

  it('gives the day the clocks go back its 25 hours', () => {
    // New York leaves daylight time on 2026-11-01 at 02:00 local.
    const bars = barBounds('30d', Date.UTC(2026, 10, 2, 12, 0), 'America/New_York');
    const sunday = bars.find((b) => b.fromMs === Date.UTC(2026, 10, 1, 4, 0))!;
    expect((sunday.toMs - sunday.fromMs) / H).toBe(25);
  });

  it('says which periods are labelled by date', () => {
    expect(barsAreDays('24h')).toBe(false);
    expect(barsAreDays('7d')).toBe(false);
    expect(barsAreDays('30d')).toBe(true);
  });
});

describe('availabilityBars', () => {
  it('puts an outage into the bars it covers and leaves the rest up', () => {
    const bars = availabilityBars(
      answer({
        // 07:00-07:30 UTC: exactly one half-hour bar.
        spans: [
          { kind: 'down', from_unix_ms: Date.UTC(2026, 10, 1, 7), to_unix_ms: Date.UTC(2026, 10, 1, 7, 30) },
        ],
      }),
      '24h',
      'UTC',
    );
    const hit = bars.filter((b) => b.downSecs > 0);
    expect(hit).toHaveLength(1);
    expect(hit[0].downSecs).toBe(30 * 60);
    expect(hit[0].upSecs).toBe(0);
    expect(hit[0].downCount).toBe(1);
  });

  it('counts only the elapsed part of the last bar', () => {
    const last = availabilityBars(answer({}), '24h', 'UTC').at(-1)!;
    expect(last.elapsedSecs).toBe(15 * 60);
    expect(last.upSecs).toBe(15 * 60);
  });

  it('reads time the server dropped as not recorded, never as up', () => {
    const bars = availabilityBars(
      answer({ truncated_before_unix_ms: NOW - 12 * H }),
      '24h',
      'UTC',
    );
    const early = bars.find((b) => b.toMs <= NOW - 12 * H && b.fromMs >= NOW - 23 * H)!;
    expect(early.upSecs).toBe(0);
    expect(early.noRecordSecs).toBe(early.elapsedSecs);
  });
});

describe('barSegments', () => {
  it('stacks the kinds as shares of the elapsed time, and draws nothing for the future', () => {
    const segs = barSegments({
      fromMs: 0,
      toMs: 0,
      elapsedSecs: 100,
      upSecs: 70,
      downSecs: 20,
      maintenanceSecs: 10,
      noRecordSecs: 0,
      downCount: 1,
    });
    expect(segs).toEqual([
      { kind: 'up', pct: 70 },
      { kind: 'maintenance', pct: 10 },
      { kind: 'down', pct: 20 },
    ]);
    expect(
      barSegments({
        fromMs: 0,
        toMs: 0,
        elapsedSecs: 0,
        upSecs: 0,
        downSecs: 0,
        maintenanceSecs: 0,
        noRecordSecs: 0,
        downCount: 0,
      }),
    ).toEqual([]);
  });
});

describe('formatAvailabilityPct', () => {
  it('never rounds a period with an outage up to 100%', () => {
    expect(formatAvailabilityPct(99.999, 3)).toBe('99.99%');
    expect(formatAvailabilityPct(100, 0)).toBe('100%');
    expect(formatAvailabilityPct(97.5, 100)).toBe('97.50%');
    expect(formatAvailabilityPct(null, 0)).toBeNull();
  });
});

describe('barIndexAt and nextBarIndex', () => {
  it('finds the bar under a point and refuses one outside the strip', () => {
    expect(barIndexAt(0, 300, 30)).toBe(0);
    expect(barIndexAt(299.9, 300, 30)).toBe(29);
    expect(barIndexAt(300, 300, 30)).toBe(29);
    expect(barIndexAt(-1, 300, 30)).toBeNull();
    expect(barIndexAt(10, 0, 30)).toBeNull();
  });

  it('starts the keyboard at the newest bar and stops at both ends', () => {
    expect(nextBarIndex('ArrowLeft', null, 30)).toBe(29);
    expect(nextBarIndex('ArrowRight', null, 30)).toBe(29);
    expect(nextBarIndex('ArrowLeft', 0, 30)).toBe(0);
    expect(nextBarIndex('ArrowRight', 29, 30)).toBe(29);
    expect(nextBarIndex('Home', 12, 30)).toBe(0);
    expect(nextBarIndex('Enter', 12, 30)).toBeNull();
  });
});
