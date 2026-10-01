// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import type { MapEdge, MapLevel, NodeState } from '../../types/api';
import { membersByPort, overlayBundleStates, rimArcs, stateCounts, troubleCounts } from './apBundle';
import { bundleId, type PlacedNode } from './graphLayout';
import { groupMapTarget, selectedGraphId } from './topologyLevel';

const PARENT = '11111111-1111-4111-8111-111111111111';
const ap = (n: number) => `22222222-2222-4222-8222-${String(n).padStart(12, '0')}`;

const states = (...s: NodeState[]) => s.map((state) => ({ state }));

describe('a bundle of access points (ADR-191 Inc.9)', () => {
  it('counts the states worst first and leaves out the ones nobody is in', () => {
    const m = states('ok', 'critical', 'ok', 'warning', 'ok');
    expect(stateCounts(m)).toEqual([
      ['critical', 1],
      ['warning', 1],
      ['ok', 3],
    ]);
    expect(troubleCounts(m)).toEqual([
      ['critical', 1],
      ['warning', 1],
    ]);
    expect(troubleCounts(states('ok', 'ok'))).toEqual([]);
  });

  it('cuts the rim into arcs that follow one another and add up to the whole circle', () => {
    const c = 2 * Math.PI * 24;
    const arcs = rimArcs(states('ok', 'critical', 'ok', 'ok'), c);
    expect(arcs.map((a) => a.state)).toEqual(['critical', 'ok']);
    expect(arcs[0].length).toBeCloseTo(c / 4);
    expect(arcs[1].offset).toBeCloseTo(arcs[0].length);
    expect(arcs.reduce((s, a) => s + a.length, 0)).toBeCloseTo(c);
    expect(rimArcs([], c)).toEqual([]);
  });

  const bundle = (memberStates: NodeState[]): PlacedNode => ({
    id: bundleId(`node:${PARENT}`),
    kind: 'node',
    name: 'sw-01',
    state: 'ok',
    sub: null,
    rootCause: null,
    ap: true,
    role: 'access_point',
    bundle: {
      parent: `node:${PARENT}`,
      members: memberStates.map((state, i) => ({ id: `node:${ap(i)}`, name: `ap-${i}`, state })),
    },
    cx: 0,
    cy: 0,
    w: 48,
    h: 48,
    suppressed: false,
  });

  it('recolours a bundle from its members’ live states, and keeps the array when nothing moved', () => {
    const nodes = [bundle(['ok', 'ok'])];
    expect(overlayBundleStates(nodes, new Map())).toBe(nodes);
    expect(overlayBundleStates(nodes, new Map([[`node:${ap(0)}`, 'ok']]))).toBe(nodes);
    const out = overlayBundleStates(nodes, new Map([[`node:${ap(1)}`, 'unreachable']]));
    expect(out).not.toBe(nodes);
    expect(out[0].state).toBe('unreachable');
    expect(out[0].bundle!.members.map((m) => m.state)).toEqual(['ok', 'unreachable']);
  });

  it('lists the members port by port, troubled and busy ports first', () => {
    const edge = (i: number, port: string | null, parentIsA: boolean): MapEdge => ({
      id: `e${i}`,
      a: { kind: 'node', id: parentIsA ? PARENT : ap(i) },
      b: { kind: 'node', id: parentIsA ? ap(i) : PARENT },
      count: 1,
      source: 'cdp',
      sources: ['cdp'],
      members: [
        {
          link_id: i,
          a_node: parentIsA ? PARENT : ap(i),
          b_node: parentIsA ? ap(i) : PARENT,
          a_if_name: parentIsA ? port : null,
          b_if_name: parentIsA ? null : port,
          source: 'cdp',
          subnet: null,
        },
      ],
    });
    const level = {
      edges: [
        edge(0, 'Fa0/11', true),
        edge(1, 'Fa0/11', false),
        edge(2, 'Fa0/11', true),
        edge(3, 'Fa0/2', true),
        edge(4, 'Fa0/9', false),
        edge(5, null, true),
      ],
    } as unknown as MapLevel;
    const b = bundle(['ok', 'ok', 'ok', 'ok', 'critical', 'ok']).bundle!;
    const groups = membersByPort(level, b);
    expect(groups.map((g) => [g.port, g.members.length])).toEqual([
      ['Fa0/9', 1],
      ['Fa0/11', 3],
      ['Fa0/2', 1],
      [null, 1],
    ]);
    expect(groups[1].members.map((m) => m.id)).toEqual([ap(0), ap(1), ap(2)]);
  });

  it('is selected by its box id, and in a folder pane selects its parent in the tree', () => {
    const id = bundleId(`node:${PARENT}`);
    expect(selectedGraphId({ kind: 'bundle', id })).toBe(id);
    expect(groupMapTarget(id, null)).toEqual({ kind: 'node', id: PARENT });
  });
});
