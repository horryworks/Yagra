// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import {
  CLUSTER_PX,
  clusterPins,
  splitsAt,
  sumCounts,
  worstFirst,
  type PinPoint,
} from './geoCluster';
import { MAP_WIDTH, MAX_GEO_SCALE } from './geoProjection';
import { pinLook } from '../dashboard/widgets/util';
import { emptyStateCounts } from '../lib/nodeState';
import type { NodeState } from '../types/api';

const counts = (c: Partial<Record<NodeState, number>>) => ({
  ...emptyStateCounts(),
  ...c,
});
const pin = (
  id: string,
  x: number,
  y: number,
  c?: Partial<Record<NodeState, number>>,
): PinPoint => ({
  id,
  name: id,
  x,
  y,
  counts: c ? counts(c) : undefined,
});

describe('clusterPins', () => {
  it('keeps sites at one coordinate together even at the zoom ceiling', () => {
    const out = clusterPins(
      [pin('a', 100, 50, { ok: 1 }), pin('b', 100, 50, { ok: 1 }), pin('c', 100, 50, { ok: 1 })],
      MAX_GEO_SCALE,
    );
    expect(out).toHaveLength(1);
    expect(out[0].members.map((m) => m.id).sort()).toEqual(['a', 'b', 'c']);
  });

  it('separates two nearby sites once the zoom puts them far enough apart', () => {
    const pts = [pin('a', 100, 50), pin('b', 100.5, 50)];
    // 0.5 map units: 5 px at scale 10, 50 px at scale 100.
    expect(clusterPins(pts, 10)).toHaveLength(1);
    expect(clusterPins(pts, 100)).toHaveLength(2);
  });

  it('measures against the first member, so a chain does not pull in a far site', () => {
    // At scale 1 the reach is CLUSTER_PX map units. a–b and b–c are inside it, a–c is not.
    const step = CLUSTER_PX * 0.8;
    const out = clusterPins([pin('a', 0, 0), pin('b', step, 0), pin('c', 2 * step, 0)], 1);
    expect(out.map((c) => c.members.map((m) => m.id).sort())).toEqual([['a', 'b'], ['c']]);
  });

  it('places a group at the mean of its members and keys it by its first member', () => {
    const [c] = clusterPins([pin('a', 10, 20), pin('b', 12, 22)], 1);
    expect(c.key).toBe('a');
    expect(c.x).toBe(11);
    expect(c.y).toBe(21);
  });

  it('gives a group the worst state among its members', () => {
    const [c] = clusterPins([pin('a', 0, 0, { ok: 5 }), pin('b', 0, 0, { critical: 1 })], 1);
    expect(pinLook(c.counts)).toBe('critical');
    expect(c.counts?.ok).toBe(5);
  });

  it('reads as empty when no member monitors anything', () => {
    const [c] = clusterPins([pin('a', 0, 0), pin('b', 0, 0)], 1);
    expect(pinLook(c.counts)).toBe('empty');
  });

  it('answers a group per pin at a scale it cannot use', () => {
    expect(clusterPins([pin('a', 0, 0), pin('b', 0, 0)], 0)).toHaveLength(2);
  });
});

describe('clusterPins across the date line (ADR-218)', () => {
  // The map repeats east-west, so a site at 179.5°E and one at 179.5°W sit side by side on screen.
  it('groups two sites either side of the date line', () => {
    const out = clusterPins([pin('east', MAP_WIDTH - 1, 100), pin('west', 1, 100)], 1);
    expect(out).toHaveLength(1);
    expect(out[0].members.map((m) => m.id).sort()).toEqual(['east', 'west']);
  });

  it('places that group on the date line, not in the middle of the Atlantic', () => {
    const [c] = clusterPins([pin('east', MAP_WIDTH - 1, 100), pin('west', 1, 100)], 1);
    expect(Math.min(c.x, MAP_WIDTH - c.x)).toBeCloseTo(0, 9);
  });

  it('keeps the mean inside one world', () => {
    const [c] = clusterPins([pin('west', 1, 100), pin('east', MAP_WIDTH - 2, 100)], 1);
    expect(c.x).toBeGreaterThanOrEqual(0);
    expect(c.x).toBeLessThan(MAP_WIDTH);
    expect(c.x).toBeCloseTo(MAP_WIDTH - 0.5, 9);
  });

  it('still separates sites half a world apart', () => {
    expect(clusterPins([pin('a', 0, 100), pin('b', MAP_WIDTH / 2, 100)], 1)).toHaveLength(2);
  });
});

describe('sumCounts', () => {
  it('ignores sites with no tally and answers undefined when none has one', () => {
    expect(sumCounts([undefined, undefined])).toBeUndefined();
    expect(sumCounts([undefined, counts({ warning: 2 })])?.warning).toBe(2);
  });
});

describe('worstFirst', () => {
  it('orders by state, then size, then name, with empty sites last', () => {
    const order = worstFirst([
      pin('empty', 0, 0),
      pin('ok-small', 0, 0, { ok: 1 }),
      pin('ok-big', 0, 0, { ok: 9 }),
      pin('crit', 0, 0, { critical: 1, ok: 1 }),
      pin('warn', 0, 0, { warning: 1 }),
    ]).map((p) => p.id);
    expect(order).toEqual(['crit', 'warn', 'ok-big', 'ok-small', 'empty']);
  });
});

describe('splitsAt', () => {
  it('is true only when the sites would be more than one marker at that scale', () => {
    // One coordinate: never.
    expect(splitsAt([pin('a', 1, 2), pin('b', 1, 2)], MAX_GEO_SCALE)).toBe(false);
    // 0.001 map units (about 55 m) is a quarter of a pixel at the ceiling: still one marker.
    expect(splitsAt([pin('a', 1, 2), pin('b', 1, 2.001)], MAX_GEO_SCALE)).toBe(false);
    // 0.1 map units is 24 px at the ceiling: two markers.
    expect(splitsAt([pin('a', 1, 2), pin('b', 1, 2.1)], MAX_GEO_SCALE)).toBe(true);
  });
});
