// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import type { MapLevel } from '../../types/api';
import {
  activateOnKey,
  edgeShowsChip,
  edgesOf,
  fitLabel,
  folderHref,
  graphId,
  groupMapTarget,
  isLevelFor,
  levelNodesHref,
  levelToGraph,
  levelTrail,
  liveByGraphId,
  mapEscapeTarget,
  memberPorts,
  nodePlace,
  selectedGraphId,
  selectionFromParam,
  splitGraphId,
  stubHref,
  unresolvedCount,
  wheelZooms,
} from './topologyLevel';
import { layoutGraph } from './graphLayout';

const SITE = '11111111-1111-4111-8111-111111111111';
const FLOOR = '22222222-2222-4222-8222-222222222222';
const WEST = '33333333-3333-4333-8333-333333333333';
const SW1 = '44444444-4444-4444-8444-444444444444';
const SW2 = '55555555-5555-4555-8555-555555555555';
const FAR = '66666666-6666-4666-8666-666666666666';

const counts = (over: Partial<MapLevel['folders'][number]['counts']> = {}) => ({
  ok: 0,
  warning: 0,
  critical: 0,
  unknown: 0,
  unreachable: 0,
  maintenance: 0,
  ...over,
});

function level(): MapLevel {
  return {
    group: { id: SITE, name: 'site-a' },
    breadcrumbs: [{ id: 'east', name: 'east' }],
    folders: [
      {
        id: FLOOR,
        name: 'floor-1',
        group_type: 'site',
        node_count: 3,
        counts: counts({ ok: 2, critical: 1 }),
      },
    ],
    subfolder_count: 1,
    nodes: [
      { id: SW1, name: 'sw-01', state: 'ok', root_cause: null, folder_path: [], access_point: false },
      { id: SW2, name: 'sw-02', state: 'warning', root_cause: SW1, folder_path: [], access_point: false },
    ],
    stubs: [{ kind: 'folder', id: WEST, name: 'west', level_group: null }],
    edges: [
      {
        id: `node:${SW1}|folder:${FLOOR}`,
        a: { kind: 'node', id: SW1 },
        b: { kind: 'folder', id: FLOOR },
        count: 2,
        source: 'lldp',
        sources: ['lldp'],
        members: [
          {
            link_id: 1,
            a_node: SW1,
            b_node: FAR,
            a_if_name: 'ge-0/0/1',
            b_if_name: null,
            source: 'lldp',
            subnet: null,
          },
        ],
      },
      {
        id: `node:${SW2}|external:${WEST}`,
        a: { kind: 'node', id: SW2 },
        b: { kind: 'external', id: WEST },
        count: 1,
        source: 'cdp',
        sources: ['cdp'],
        members: [],
      },
    ],
    direct_node_count: 5,
    linked_node_count: 2,
    edge_count: 2,
    isolated_count: 3,
    overflow: false,
    flattened: false,
    node_limit: 2000,
    edge_limit: 4000,
    derived_at: null,
    summary: {},
  };
}

const captions = { folder: () => 'folder caption', stub: () => 'stub caption' };

describe('levelToGraph', () => {
  it('gives every node, folder and stub its own box, prefixed by kind', () => {
    const g = levelToGraph(level(), captions);
    expect(g.nodes.map((n) => n.id).sort()).toEqual(
      [graphId('node', SW1), graphId('node', SW2), graphId('folder', FLOOR), graphId('external', WEST)].sort(),
    );
    expect(g.links.map((l) => [l.a, l.b, l.count])).toEqual([
      [graphId('node', SW1), graphId('folder', FLOOR), 2],
      [graphId('node', SW2), graphId('external', WEST), 1],
    ]);
  });

  it('colours a folder by its worst member and captions folders and stubs', () => {
    const g = levelToGraph(level(), captions);
    const folder = g.nodes.find((n) => n.kind === 'folder')!;
    expect(folder.state).toBe('critical');
    expect(folder.sub).toBe('folder caption');
    expect(g.nodes.find((n) => n.kind === 'external')!.sub).toBe('stub caption');
    expect(g.nodes.find((n) => n.id === graphId('node', SW2))!.rootCause).toBe(SW1);
  });

  it('puts the folders down to a node’s own on its second line on a level drawn flat', () => {
    const flat = level();
    flat.flattened = true;
    flat.nodes[1] = {
      ...flat.nodes[1],
      folder_path: [
        { id: FLOOR, name: 'floor-1' },
        { id: WEST, name: 'room-1' },
      ],
    };
    const g = levelToGraph(flat, captions);
    expect(g.nodes.find((n) => n.id === graphId('node', SW2))!.sub).toBe('▤ floor-1 › room-1');
    expect(g.nodes.find((n) => n.id === graphId('node', SW1))!.sub).toBeNull();
  });

  it('carries the access-point mark to the node, and never to a folder or a stub', () => {
    const l = level();
    l.nodes[1] = { ...l.nodes[1], access_point: true };
    const g = levelToGraph(l, captions);
    expect(g.nodes.find((n) => n.id === graphId('node', SW2))!.ap).toBe(true);
    expect(g.nodes.find((n) => n.id === graphId('node', SW1))!.ap).toBe(false);
    expect(g.nodes.filter((n) => n.kind !== 'node').every((n) => !n.ap)).toBe(true);
  });
});

describe('nodePlace', () => {
  it('is the breadcrumbs and the level for a node directly on it', () => {
    const l = { ...level(), breadcrumbs: [{ id: WEST, name: 'east' }] };
    expect(nodePlace(l, l.nodes[0], 'site-a')).toBe('east › site-a');
  });

  it('goes on down to the node’s own folder on a level drawn flat', () => {
    const l = { ...level(), flattened: true, breadcrumbs: [{ id: WEST, name: 'east' }] };
    const n = {
      ...l.nodes[1],
      folder_path: [
        { id: FLOOR, name: 'floor-1' },
        { id: FAR, name: 'room-1' },
      ],
    };
    expect(nodePlace(l, n, 'site-a')).toBe('east › site-a › floor-1 › room-1');
  });

  it('starts at the level for the whole network', () => {
    const whole = { ...level(), group: null, breadcrumbs: [] };
    expect(nodePlace(whole, whole.nodes[0], 'Whole network')).toBe('Whole network');
  });

  it('lays out as a connected level with no loose node', () => {
    const out = layoutGraph(levelToGraph(level(), captions));
    expect(out.nodes).toHaveLength(4);
    expect(out.edges).toHaveLength(2);
    expect(out.isolatedCount).toBe(0);
  });
});

describe('liveByGraphId', () => {
  it('keys live states by box id, for the level’s own nodes only', () => {
    const live = new Map([
      [SW1, 'critical' as const],
      [FAR, 'critical' as const],
    ]);
    const out = liveByGraphId(level(), live);
    expect([...out.entries()]).toEqual([[graphId('node', SW1), 'critical']]);
    expect(liveByGraphId(null, live).size).toBe(0);
  });
});

describe('selection', () => {
  it('reads the Nodes page spelling of sel=', () => {
    expect(selectionFromParam({ kind: 'node', id: SW1 })).toEqual({ kind: 'node', id: SW1 });
    expect(selectionFromParam({ kind: 'group', id: FLOOR })).toEqual({ kind: 'folder', id: FLOOR });
    expect(selectionFromParam(null)).toBeNull();
  });

  it('names the selected box, and no box for an edge', () => {
    expect(selectedGraphId({ kind: 'node', id: SW1 })).toBe(graphId('node', SW1));
    expect(selectedGraphId({ kind: 'folder', id: FLOOR })).toBe(graphId('folder', FLOOR));
    expect(selectedGraphId({ kind: 'edge', id: 'x' })).toBeNull();
    expect(selectedGraphId(null)).toBeNull();
  });

  it('unwinds the edge first, then the URL selection', () => {
    expect(mapEscapeTarget(true, true)).toBe('edge');
    expect(mapEscapeTarget(false, true)).toBe('selection');
    expect(mapEscapeTarget(false, false)).toBeNull();
  });
});

describe('where things lead', () => {
  it('opens a folder stub on the common level with the folder selected', () => {
    expect(stubHref(level().stubs[0])).toBe(
      `/topology/map?sel=${encodeURIComponent(`group:${WEST}`)}`,
    );
    expect(stubHref({ kind: 'node', id: FAR, name: 'x', level_group: SITE })).toBe(
      `/topology/map?group=${SITE}&sel=${encodeURIComponent(`node:${FAR}`)}`,
    );
  });

  it('enters a folder box, and opens the level in the Nodes page', () => {
    expect(folderHref(FLOOR)).toBe(`/topology/map?group=${FLOOR}`);
    expect(levelNodesHref(level())).toBe(
      `/nodes?sel=${encodeURIComponent(`group:${SITE}`)}`,
    );
    expect(levelNodesHref({ ...level(), group: null })).toBe('/nodes');
  });
});

describe('isLevelFor', () => {
  it('refuses the previous level while the next one loads', () => {
    expect(isLevelFor(level(), SITE)).toBe(true);
    expect(isLevelFor(level(), FLOOR)).toBe(false);
    expect(isLevelFor(level(), null)).toBe(false);
    expect(isLevelFor({ ...level(), group: null }, null)).toBe(true);
    expect(isLevelFor(null, null)).toBe(false);
  });
});

describe('edgesOf and memberPorts', () => {
  it('finds the edges touching a node and fills a missing port', () => {
    expect(edgesOf(level(), SW1).map((e) => e.count)).toEqual([2]);
    expect(edgesOf(level(), FAR)).toEqual([]);
    expect(memberPorts(level().edges[0].members[0], '—')).toEqual({ a: 'ge-0/0/1', b: '—' });
  });
});

describe('fitLabel', () => {
  it('keeps a short label and cuts a long one with an ellipsis', () => {
    expect(fitLabel('sw-01', 150, 34)).toBe('sw-01');
    const cut = fitLabel('a-very-long-device-name-that-will-not-fit', 150, 34);
    expect(cut.endsWith('…')).toBe(true);
    expect(cut.length).toBe(Math.floor((150 - 34 - 8) / 7));
  });
});

describe('activateOnKey', () => {
  it('acts on Enter and Space only', () => {
    let acted = 0;
    let prevented = 0;
    const press = (key: string) =>
      activateOnKey({ key, preventDefault: () => prevented++ }, () => acted++);
    press('Enter');
    press(' ');
    press('Tab');
    expect([acted, prevented]).toEqual([2, 2]);
  });
});

describe('levelTrail', () => {
  it('links every level above this one and ends on this one', () => {
    expect(levelTrail(level(), 'Whole network')).toEqual([
      { label: 'Whole network', to: '/topology/map' },
      { label: 'east', to: '/topology/map?group=east' },
      { label: 'site-a' },
    ]);
    expect(levelTrail({ ...level(), group: null, breadcrumbs: [] }, 'Whole network')).toEqual([
      { label: 'Whole network', to: undefined },
    ]);
  });
});

describe('splitGraphId and edgeShowsChip', () => {
  it('reads a box id back', () => {
    expect(splitGraphId(graphId('folder', FLOOR))).toEqual({ kind: 'folder', id: FLOOR });
    expect(splitGraphId('bogus')).toBeNull();
    expect(splitGraphId('site:x')).toBeNull();
  });

  it('shows a chip for a bundle or a line to a box, not for one node-to-node link', () => {
    expect(edgeShowsChip(`node:${SW1}|node:${SW2}`, 1)).toBe(false);
    expect(edgeShowsChip(`node:${SW1}|node:${SW2}`, 2)).toBe(true);
    expect(edgeShowsChip(`node:${SW1}|folder:${FLOOR}`, 1)).toBe(true);
    expect(edgeShowsChip(`node:${SW2}|external:${WEST}`, 1)).toBe(true);
  });
});

describe('groupMapTarget', () => {
  // The map in a folder's pane never descends by itself: a press selects in the tree (ADR-191 Inc.2).
  it('selects a node or a folder for what its box is', () => {
    expect(groupMapTarget(graphId('node', SW1), level())).toEqual({ kind: 'node', id: SW1 });
    expect(groupMapTarget(graphId('folder', FLOOR), level())).toEqual({ kind: 'group', id: FLOOR });
  });

  it('selects what a stub stands for', () => {
    expect(groupMapTarget(graphId('external', WEST), level())).toEqual({ kind: 'group', id: WEST });
    const l = level();
    l.stubs = [{ kind: 'node', id: FAR, name: 'sw-far', level_group: null }];
    expect(groupMapTarget(graphId('external', FAR), l)).toEqual({ kind: 'node', id: FAR });
  });

  it('selects nothing for a stub the level no longer has or a malformed id', () => {
    expect(groupMapTarget(graphId('external', FAR), level())).toBeNull();
    expect(groupMapTarget('nonsense', level())).toBeNull();
    expect(groupMapTarget(graphId('external', WEST), null)).toBeNull();
  });
});

describe('wheelZooms', () => {
  it('zooms on any wheel where the map owns the wheel', () => {
    expect(wheelZooms({ ctrlKey: false, metaKey: false }, false)).toBe(true);
  });

  it('leaves the plain wheel to the page inside a pane, and zooms with Ctrl or Cmd', () => {
    expect(wheelZooms({ ctrlKey: false, metaKey: false }, true)).toBe(false);
    expect(wheelZooms({ ctrlKey: true, metaKey: false }, true)).toBe(true);
    expect(wheelZooms({ ctrlKey: false, metaKey: true }, true)).toBe(true);
  });
});

describe('unresolvedCount', () => {
  // The four counters the derivation declined to turn into links; the link counts are not in it.
  it('adds what could not be resolved and nothing else', () => {
    const l = level();
    expect(unresolvedCount(l)).toBe(0);
    l.summary = {
      unmatched_lldp_rows: 2,
      unmatched_cdp_rows: 1,
      ambiguous_mgmt_addrs: 3,
      oversized_segments: 4,
      lldp_links: 50,
      unmatched_routing_peers: 7,
    };
    expect(unresolvedCount(l)).toBe(10);
  });
});
