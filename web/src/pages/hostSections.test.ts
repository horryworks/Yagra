// SPDX-License-Identifier: AGPL-3.0-only
// The Host-resources sections: grouping, the shared timestamp axis, and the two rules that decide
// what a chart means.
//
// The multi-host cases matter most here because **they cannot be reproduced on the test server** —
// it runs one poller in one pool, so the overlay path, the palette wrap and the load-series
// collapse have no other gate than this file.

import { describe, expect, it } from 'vitest';
import {
  groupHosts,
  memPctPoints,
  mountPct,
  mountUnion,
  overlaySeries,
  peakHeadline,
  peakOf,
  sectionCharts,
} from './hostSections';
import type { HostInfo, HostMetricRange, MetricPoint } from '../types/api';

const pts = (...pairs: [number, number][]): MetricPoint[] => pairs.map(([t, v]) => ({ t, v }));

const host = (instance: string, role: 'core' | 'poller', pool: string | null): HostInfo => ({
  instance,
  role,
  pool,
  online: true,
  disks: [],
});

/** A range with only the fields a given test cares about; the rest are empty, which is also the
 *  shape a host that has just come up actually returns. */
const range = (instance: string, over: Partial<HostMetricRange> = {}): HostMetricRange => ({
  instance,
  cpu_pct: [],
  load1: [],
  load5: [],
  load15: [],
  mem_used_bytes: [],
  mem_total_bytes: [],
  disks: [],
  ...over,
});

describe('groupHosts', () => {
  it('puts core first and then the pools by name', () => {
    const sections = groupHosts([
      host('p-tokyo', 'poller', 'tokyo'),
      host('p-b', 'poller', 'default'),
      host('core', 'core', null),
      host('p-a', 'poller', 'default'),
    ]);
    expect(sections.map((s) => s.key)).toEqual(['core', 'pool:default', 'pool:tokyo']);
    expect(sections[0].kind).toBe('core');
    expect(sections[1].pool).toBe('default');
  });

  it('orders hosts inside a section by instance id', () => {
    // Load-bearing: the colour a host gets is its index here, so an unstable order would recolour
    // every chart on each 15s refresh.
    const [pool] = groupHosts([
      host('zeta', 'poller', 'default'),
      host('alpha', 'poller', 'default'),
      host('mid', 'poller', 'default'),
    ]);
    expect(pool.hosts.map((h) => h.instance)).toEqual(['alpha', 'mid', 'zeta']);
  });

  it('emits no core section when core is absent, rather than an empty one', () => {
    const sections = groupHosts([host('p-a', 'poller', 'default')]);
    expect(sections.map((s) => s.kind)).toEqual(['pool']);
  });

  it('still groups a poller whose pool is missing', () => {
    // Shouldn't happen — the coordinator always carries a pool — but disappearing from the page is
    // a worse failure than an oddly-labelled section.
    const sections = groupHosts([host('p-a', 'poller', null)]);
    expect(sections).toHaveLength(1);
    expect(sections[0].hosts.map((h) => h.instance)).toEqual(['p-a']);
  });
});

describe('mountUnion', () => {
  it('unions the mounts across the section in first-seen order', () => {
    // The real shape on the test server: core carries three, its poller carries one.
    const mounts = mountUnion([
      {
        label: 'core',
        range: range('core', {
          disks: [
            { mount: 'root', used_bytes: [], size_bytes: [] },
            { mount: 'metrics', used_bytes: [], size_bytes: [] },
            { mount: 'database', used_bytes: [], size_bytes: [] },
          ],
        }),
      },
      {
        label: 'p-a',
        range: range('p-a', { disks: [{ mount: 'root', used_bytes: [], size_bytes: [] }] }),
      },
    ]);
    expect(mounts).toEqual(['root', 'metrics', 'database']);
  });

  it('ignores hosts whose trends have not arrived', () => {
    expect(mountUnion([{ label: 'p-a', range: null }])).toEqual([]);
  });
});

describe('overlaySeries', () => {
  it('aligns hosts onto the union axis and gaps what one of them did not report', () => {
    const { timestamps, series } = overlaySeries(
      [
        { label: 'core', points: pts([10, 1], [30, 3]) },
        { label: 'p-a', points: pts([20, 2], [30, 9]) },
      ],
      ['c1', 'c2'],
    );
    expect(timestamps).toEqual([10, 20, 30]);
    // `null`, never 0 — a zero would draw a cliff to the floor and read as an outage.
    expect(series[0].values).toEqual([1, null, 3]);
    expect(series[1].values).toEqual([null, 2, 9]);
  });

  it('wraps the palette rather than dropping a host past its end', () => {
    const { series } = overlaySeries(
      [
        { label: 'a', points: pts([1, 1]) },
        { label: 'b', points: pts([1, 1]) },
        { label: 'c', points: pts([1, 1]) },
      ],
      ['c1', 'c2'],
    );
    expect(series).toHaveLength(3);
    expect(series.map((s) => s.color)).toEqual(['c1', 'c2', 'c1']);
  });
});

describe('memPctPoints', () => {
  it('derives used-% and drops a reading with no usable total', () => {
    const points = memPctPoints(
      range('core', {
        mem_used_bytes: pts([10, 50], [20, 25], [30, 10]),
        // t=20 has no total at all; t=30's is zero. Both would invent a percentage.
        mem_total_bytes: pts([10, 100], [30, 0]),
      }),
    );
    expect(points).toEqual([{ t: 10, v: 50 }]);
  });

  it('is empty for a host whose trends have not arrived', () => {
    expect(memPctPoints(null)).toEqual([]);
  });
});

describe('sectionCharts — the load rule', () => {
  it('keeps 1m/5m/15m when the section holds a single host', () => {
    const charts = sectionCharts(
      [
        {
          label: 'core',
          range: range('core', {
            load1: pts([10, 1]),
            load5: pts([10, 2]),
            load15: pts([10, 3]),
          }),
        },
      ],
      ['c1', 'c2', 'c3'],
    );
    expect(charts.load.multi).toBe(false);
    expect(charts.load.series.map((s) => s.label)).toEqual(['1m', '5m', '15m']);
    expect(charts.load.series.map((s) => s.values[0])).toEqual([1, 2, 3]);
  });

  it('collapses to load1 per host once there is something to compare against', () => {
    const charts = sectionCharts(
      [
        { label: 'p-a', range: range('p-a', { load1: pts([10, 3]), load5: pts([10, 9]) }) },
        { label: 'p-b', range: range('p-b', { load1: pts([10, 1]), load5: pts([10, 9]) }) },
      ],
      ['c1', 'c2'],
    );
    expect(charts.load.multi).toBe(true);
    expect(charts.load.series.map((s) => s.label)).toEqual(['p-a', 'p-b']);
    // The 5m series is deliberately not drawn — the colour now means the host.
    expect(charts.load.series.map((s) => s.values[0])).toEqual([3, 1]);
  });
});

describe('sectionCharts — disks', () => {
  it('reads a mount as a percentage when any host reports a capacity for it', () => {
    const charts = sectionCharts(
      [
        {
          label: 'core',
          range: range('core', {
            disks: [{ mount: 'root', used_bytes: pts([10, 25]), size_bytes: pts([10, 100]) }],
          }),
        },
        {
          label: 'p-a',
          range: range('p-a', {
            disks: [{ mount: 'root', used_bytes: pts([10, 60]), size_bytes: pts([10, 100]) }],
          }),
        },
      ],
      ['c1', 'c2'],
    );
    expect(charts.disks).toHaveLength(1);
    expect(charts.disks[0].known).toBe(true);
    expect(charts.disks[0].series.map((s) => s.values[0])).toEqual([25, 60]);
  });

  it('falls back to bare bytes for a store that reports no capacity', () => {
    // The PostgreSQL `database` proxy: size_bytes is 0, so a percentage would be an invention.
    const charts = sectionCharts(
      [
        {
          label: 'core',
          range: range('core', {
            disks: [{ mount: 'database', used_bytes: pts([10, 4096]), size_bytes: pts([10, 0]) }],
          }),
        },
      ],
      ['c1'],
    );
    expect(charts.disks[0].known).toBe(false);
    expect(charts.disks[0].series[0].values).toEqual([4096]);
  });

  it('keeps a host that does not report the mount as an empty line, not a missing one', () => {
    // Dropping it would shift the colours of every host after it.
    const charts = sectionCharts(
      [
        {
          label: 'core',
          range: range('core', {
            disks: [{ mount: 'metrics', used_bytes: pts([10, 25]), size_bytes: pts([10, 100]) }],
          }),
        },
        { label: 'p-a', range: range('p-a', { disks: [] }) },
      ],
      ['c1', 'c2'],
    );
    expect(charts.disks[0].series).toHaveLength(2);
    expect(charts.disks[0].series[1].values).toEqual([null]);
    expect(charts.disks[0].series[1].color).toBe('c2');
  });
});

describe('peakOf', () => {
  const withCpu = (instance: string, cpu: number | null): HostInfo => ({
    ...host(instance, 'poller', 'default'),
    cpu_pct: cpu,
  });

  it('names the worst reader in the section', () => {
    expect(peakOf([withCpu('a', 12), withCpu('b', 68), withCpu('c', 30)], (h) => h.cpu_pct)).toEqual(
      { instance: 'b', value: 68 },
    );
  });

  it('skips hosts with no reading and answers null when none has one', () => {
    expect(peakOf([withCpu('a', null), withCpu('b', 4)], (h) => h.cpu_pct)).toEqual({
      instance: 'b',
      value: 4,
    });
    expect(peakOf([withCpu('a', null)], (h) => h.cpu_pct)).toBeNull();
    expect(peakOf([], (h) => h.cpu_pct)).toBeNull();
  });
});

describe('mountPct', () => {
  it('derives the current percentage and refuses one for an unmeasurable store', () => {
    const h: HostInfo = {
      ...host('core', 'core', null),
      disks: [
        { mount: 'root', used_bytes: 25, size_bytes: 100 },
        { mount: 'database', used_bytes: 4096, size_bytes: 0 },
      ],
    };
    expect(mountPct(h, 'root')).toBe(25);
    expect(mountPct(h, 'database')).toBeNull();
    expect(mountPct(h, 'nope')).toBeNull();
  });
});

describe('peakHeadline', () => {
  const h = (instance: string, cpu: number | null) =>
    ({ instance, cpu_pct: cpu }) as unknown as HostInfo;

  it('names the peak’s value and the host it was on', () => {
    const out = peakHeadline(
      [h('core-1', 12), h('poller-2', 68)],
      (x) => x.cpu_pct,
      (v) => `${v}%`,
    );
    expect(out).toBe('68% · poller-2');
  });

  it('shows an em dash when nothing reported, rather than 0', () => {
    // "0%" would be a claim about the fleet; the dash says the sample is missing.
    expect(peakHeadline([], (x) => x.cpu_pct, (v) => `${v}%`)).toBe('—');
    expect(peakHeadline([h('core-1', null)], (x) => x.cpu_pct, (v) => `${v}%`)).toBe('—');
  });
});
