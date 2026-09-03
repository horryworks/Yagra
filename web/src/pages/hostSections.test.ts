// SPDX-License-Identifier: AGPL-3.0-only
// The Host-resources sections: one per host, the shared timestamp axis, and the one rule that
// decides what a colour means.
//
// 🚨 **This file is the only automated gate on the split.** The Tier1 walk builds its fixtures from
// the OpenAPI document and `tests/support/openapi.ts` emits arrays with exactly one element, so
// `/system/hosts` there returns a single host — the browser walk cannot tell a page that splits two
// pollers apart from one that overlays them. Multi-host behaviour is checked here or nowhere
// (ADR-118); the rendered page is checked by a person on the two-poller box.

import { describe, expect, it } from 'vitest';
import { groupHosts, hostCharts, memPctPoints, overlaySeries } from './hostSections';
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
  it('emits one section per host, never one per pool', () => {
    // The defect ADR-118 fixed: two pollers in one pool used to share a section, and therefore a
    // chart and a headline. Counting sections is what tells the two layouts apart.
    const sections = groupHosts([
      host('p2', 'poller', 'test1'),
      host('core', 'core', null),
      host('p1', 'poller', 'test1'),
    ]);
    expect(sections).toHaveLength(3);
    expect(sections.map((s) => s.host.instance)).toEqual(['core', 'p1', 'p2']);
    expect(sections.map((s) => s.key)).toEqual(['host:core', 'host:p1', 'host:p2']);
  });

  it('puts cores first, then pollers by pool name and by instance inside a pool', () => {
    const sections = groupHosts([
      host('b1', 'poller', 'zulu'),
      host('a2', 'poller', 'alpha'),
      host('core', 'core', null),
      host('a1', 'poller', 'alpha'),
    ]);
    expect(sections.map((s) => `${s.pool ?? 'core'}/${s.host.instance}`)).toEqual([
      'core/core',
      'alpha/a1',
      'alpha/a2',
      'zulu/b1',
    ]);
  });

  it('names the instance only when more than one core reports', () => {
    // `Core / Web · core` stutters on the single-core deployment everyone runs; an HA pair has no
    // other way to say which of the two a section is.
    const single = groupHosts([host('core', 'core', null), host('p1', 'poller', 'default')]);
    expect(single.map((s) => s.coreAmbiguous)).toEqual([false, false]);
    const pair = groupHosts([host('core-a', 'core', null), host('core-b', 'core', null)]);
    expect(pair.map((s) => s.coreAmbiguous)).toEqual([true, true]);
  });

  it('emits no core section when core is absent, rather than an empty one', () => {
    expect(groupHosts([host('p1', 'poller', 'default')]).map((s) => s.kind)).toEqual(['pool']);
  });

  it('still shows a poller whose pool is missing', () => {
    // A malformed row has to land somewhere visible: silently dropping it would make a poller that
    // *is* reporting look like one that never came up.
    const sections = groupHosts([host('p1', 'poller', null)]);
    expect(sections).toHaveLength(1);
    expect(sections[0].pool).toBe('');
  });
});

describe('overlaySeries', () => {
  it('aligns series onto the union axis and gaps what one of them did not report', () => {
    const { timestamps, series } = overlaySeries(
      [
        { label: '1m', points: pts([1, 10], [3, 30]) },
        { label: '5m', points: pts([2, 20], [3, 33]) },
      ],
      ['#a', '#b'],
    );
    expect(timestamps).toEqual([1, 2, 3]);
    expect(series[0].values).toEqual([10, null, 30]);
    expect(series[1].values).toEqual([null, 20, 33]);
  });

  it('wraps the palette rather than dropping a series past its end', () => {
    const { series } = overlaySeries(
      [
        { label: '1m', points: pts([1, 1]) },
        { label: '5m', points: pts([1, 2]) },
        { label: '15m', points: pts([1, 3]) },
      ],
      ['#a', '#b'],
    );
    expect(series).toHaveLength(3);
    expect(series.map((s) => s.color)).toEqual(['#a', '#b', '#a']);
  });
});

describe('memPctPoints', () => {
  it('derives used-% and drops a reading with no usable total', () => {
    const r = range('core', {
      mem_used_bytes: pts([1, 512], [2, 256], [3, 128]),
      // t=2 has no matching total; t=3's total is zero.
      mem_total_bytes: pts([1, 1024], [3, 0]),
    });
    expect(memPctPoints(r)).toEqual([{ t: 1, v: 50 }]);
  });

  it('is empty for a host whose trends have not arrived', () => {
    expect(memPctPoints(null)).toEqual([]);
  });
});

describe('hostCharts', () => {
  it('always draws all three load averages, whatever pool the host is in', () => {
    // The per-pool layout collapsed to load1 as soon as a pool held two pollers, because the colour
    // had to mean the host. One host per section gives the colour back to the window (ADR-118).
    const charts = hostCharts(
      range('p1', { load1: pts([1, 0.1]), load5: pts([1, 0.2]), load15: pts([1, 0.3]) }),
      ['#a', '#b', '#c'],
    );
    expect(charts.load.series.map((s) => s.label)).toEqual(['1m', '5m', '15m']);
    expect(charts.load.series.map((s) => s.values[0])).toEqual([0.1, 0.2, 0.3]);
  });

  it('draws a single line for cpu and memory', () => {
    const charts = hostCharts(
      range('p1', {
        cpu_pct: pts([1, 12]),
        mem_used_bytes: pts([1, 512]),
        mem_total_bytes: pts([1, 1024]),
      }),
      ['#a'],
    );
    expect(charts.cpu.series).toHaveLength(1);
    expect(charts.cpu.series[0].values).toEqual([12]);
    expect(charts.mem.series).toHaveLength(1);
    expect(charts.mem.series[0].values).toEqual([50]);
  });

  it('reads a mount as a percentage when the host reports a capacity for it', () => {
    const charts = hostCharts(
      range('p1', {
        disks: [{ mount: 'root', used_bytes: pts([1, 250]), size_bytes: pts([1, 1000]) }],
      }),
      ['#a'],
    );
    expect(charts.disks).toHaveLength(1);
    expect(charts.disks[0].known).toBe(true);
    expect(charts.disks[0].series[0].values).toEqual([25]);
  });

  it('falls back to bare bytes for a store that reports no capacity', () => {
    // The PostgreSQL `database` proxy: a percentage of an unknown capacity would be an invention.
    const charts = hostCharts(
      range('core', {
        disks: [{ mount: 'database', used_bytes: pts([1, 2048]), size_bytes: pts([1, 0]) }],
      }),
      ['#a'],
    );
    expect(charts.disks[0].known).toBe(false);
    expect(charts.disks[0].series[0].values).toEqual([2048]);
  });

  it('draws empty cards while the fetch is outstanding, rather than none at all', () => {
    // A loading section has to stay distinguishable from a host that reports nothing: the cards are
    // there, the lines are not.
    const charts = hostCharts(null, ['#a', '#b', '#c']);
    expect(charts.cpu.series).toHaveLength(1);
    expect(charts.cpu.timestamps).toEqual([]);
    expect(charts.load.series.map((s) => s.label)).toEqual(['1m', '5m', '15m']);
    expect(charts.disks).toEqual([]);
  });
});
