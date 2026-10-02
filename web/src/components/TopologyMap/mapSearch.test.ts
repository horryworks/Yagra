// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import type { MapLevel, MapNode } from '../../types/api';
import { decodeCondition, EMPTY_CONDITION } from '../../lib/filterCondition';
import { bundleId, layoutGraph } from './graphLayout';
import { centerOn } from './fitView';
import { boxEmphasis, edgeDimmed, edgeEnds, searchMap, stepTermOf, stepThrough } from './mapSearch';
import { GROUP_MAP_SEARCH_KEY, levelToGraph, withSearch } from './topologyLevel';
import { TREE_SEARCH_KEY } from '../../pages/inventoryFilters';

const id = (n: number) => `00000000-0000-4000-8000-${String(n).padStart(12, '0')}`;

function mapNode(n: number, name: string, ap = false): MapNode {
  return {
    id: id(n),
    name,
    state: 'ok',
    root_cause: null,
    folder_path: [],
    access_point: ap,
    role: ap ? 'access_point' : 'l2_switch',
    role_reason: 'default',
    subnet_count: null,
  };
}

/** A switch with three access points (one bundle), a second switch, and one node with no line. */
function level(): MapLevel {
  const nodes = [
    mapNode(1, 'core-sw'),
    mapNode(2, 'floor-sw'),
    mapNode(3, 'ap-lobby', true),
    mapNode(4, 'ap-office', true),
    mapNode(5, 'ap-store', true),
    mapNode(6, 'printer-lobby'),
  ];
  const edge = (a: number, b: number) => ({
    id: `node:${id(a)}|node:${id(b)}`,
    a: { kind: 'node' as const, id: id(a) },
    b: { kind: 'node' as const, id: id(b) },
    count: 1,
    source: 'cdp' as const,
    sources: ['cdp' as const],
    members: [],
  });
  return {
    nodes,
    edges: [edge(1, 2), edge(2, 3), edge(2, 4), edge(2, 5)],
    folders: [],
    stubs: [],
  } as unknown as MapLevel;
}

const captions = { folder: () => '', stub: () => '' };

function run(q: string) {
  const lv = level();
  const layout = layoutGraph(levelToGraph(lv, captions));
  return { layout, s: searchMap(lv, layout, decodeCondition(q)) };
}

describe('searching a level of the map (ADR-191 Inc.11)', () => {
  const bundle = bundleId(`node:${id(2)}`);

  it('picks nothing out while the condition is empty', () => {
    const lv = level();
    const layout = layoutGraph(levelToGraph(lv, captions));
    const s = searchMap(lv, layout, EMPTY_CONDITION);
    expect(s.matched).toBeNull();
    expect(boxEmphasis(`node:${id(1)}`, s.matched)).toBeNull();
    expect(edgeDimmed(`node:${id(1)}|node:${id(2)}`, s.matched)).toBe(false);
  });

  it('matches a part of a host name, ignoring case, and counts the hits inside a bundle', () => {
    const { s } = run('LOBBY');
    expect(s.total).toBe(2);
    expect([...s.matched!].sort()).toEqual([bundle]);
    expect(s.bundleHits.get(bundle)).toBe(1);
    // The printer has no line, so it is a hit the map cannot show.
    expect(s.undrawn).toBe(1);
  });

  it('reads a regular expression the way the column filters do', () => {
    const { s } = run('~^ap-(office|store)$');
    expect(s.total).toBe(2);
    expect(s.bundleHits.get(bundle)).toBe(2);
    expect(s.undrawn).toBe(0);
  });

  it('turns NOT around, so everything but the hits is picked out', () => {
    const { s } = run('!sw');
    expect(s.matched!.has(`node:${id(1)}`)).toBe(false);
    expect(s.bundleHits.get(bundle)).toBe(3);
    expect(s.total).toBe(4);
  });

  it('matches nothing on a regular expression that does not compile, and does not throw', () => {
    const { s } = run('~ap-[');
    expect(s.matched!.size).toBe(0);
    expect(s.total).toBe(0);
  });

  it('fades a line only when neither end is picked out', () => {
    const { s, layout } = run('core');
    const backbone = layout.edges.find((e) => !e.box)!;
    const toBundle = layout.edges.find((e) => e.box === bundle)!;
    expect(edgeDimmed(backbone.id, s.matched)).toBe(false);
    expect(edgeDimmed(toBundle.id, s.matched)).toBe(true);
    expect(edgeEnds(toBundle.id)).toEqual([`node:${id(2)}`, bundle].sort() as [string, string]);
  });

  it('steps through the hits top to bottom, wrapping both ways', () => {
    const { s } = run('~.');
    expect(s.order[0]).toBe(`node:${id(1)}`);
    const last = s.order[s.order.length - 1];
    expect(stepThrough(s.order, null, 1)).toBe(s.order[0]);
    expect(stepThrough(s.order, null, -1)).toBe(last);
    expect(stepThrough(s.order, last, 1)).toBe(s.order[0]);
    expect(stepThrough(s.order, s.order[0], -1)).toBe(last);
    expect(stepThrough([], null, 1)).toBeNull();
  });

  it('centres a point at the scale in use', () => {
    expect(centerOn(100, 50, 800, 600, 2)).toEqual({ tx: 200, ty: 200, scale: 2 });
  });

  it('carries the search to the next level, and adds nothing when there is none', () => {
    expect(withSearch('/topology/map?group=g1', '!~^ap')).toBe('/topology/map?group=g1&q=%21%7E%5Eap');
    expect(withSearch('/topology/map', 'x')).toBe('/topology/map?q=x');
    expect(withSearch('/topology/map?group=g1', '')).toBe('/topology/map?group=g1');
  });

  // ADR-191 Inc.13: the folder pane's search shares /nodes with the tree's own, so it needs a key
  // of its own; and the link out hands that condition to the full map unchanged, under its key.
  it('the folder pane keeps its search apart from the tree and hands it to the full map', () => {
    expect(GROUP_MAP_SEARCH_KEY).not.toBe(TREE_SEARCH_KEY);
    const encoded = new URLSearchParams(`${GROUP_MAP_SEARCH_KEY}=%21%7E%5Eap`).get(GROUP_MAP_SEARCH_KEY) ?? '';
    expect(withSearch('/topology/map?group=g1', encoded)).toBe('/topology/map?group=g1&q=%21%7E%5Eap');
  });
});

describe('which key press steps through the hits (ADR-191 Inc.13)', () => {
  it('steps on Enter in the term field, with the text in the box', () => {
    expect(stepTermOf({ tagName: 'INPUT', type: 'search', value: 'core-' })).toBe('core-');
  });

  it('does not step on Enter on the Regex switch (value "on") or the Exclude button', () => {
    expect(stepTermOf({ tagName: 'INPUT', type: 'checkbox', value: 'on' })).toBeNull();
    expect(stepTermOf({ tagName: 'BUTTON', type: 'button', value: '' })).toBeNull();
  });
});
