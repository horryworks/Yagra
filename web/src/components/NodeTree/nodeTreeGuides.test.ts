// SPDX-License-Identifier: AGPL-3.0-only
// The tree's branch lines (ADR-171). Every case names the exact cell it expects — a guide drawn in
// the wrong column still "draws a guide".
import { describe, expect, it } from 'vitest';
import {
  cellTones,
  checkedPerGroup,
  checkedRowIndices,
  litGuides,
  parentRows,
  stickyParents,
  treeGuides,
  type LitGuides,
} from './nodeTreeGuides';
import type { FlatRow } from '../../lib/nodeTree';
import type { NodeSummary } from '../../types/api';

const node = (id: string): NodeSummary => ({ id, name: id }) as NodeSummary;
const n = (id: string, depth: number): FlatRow => ({
  kind: 'node',
  depth,
  node: node(id),
});
const g = (id: string, depth: number): FlatRow =>
  ({
    kind: 'group',
    depth,
    group: { id },
    isOpen: true,
    hasChildren: true,
    tally: null,
  }) as unknown as FlatRow;
const loading = (depth: number): FlatRow => ({
  kind: 'group-loading',
  depth,
  groupId: 'x',
});
const slot = (depth: number): FlatRow => ({ kind: 'drop-slot', depth });
const head = (): FlatRow => ({ kind: 'ungrouped-head', count: 2 });
const loose = (id: string): FlatRow => ({
  kind: 'ungrouped-node',
  depth: 1,
  node: node(id),
});

/**
 *   0  APAC
 *   1  ├ India
 *   2  │ └ mum-01
 *   3  ├ Japan
 *   4  │ ├ Tokyo
 *   5  │ │ ├ tyo-01
 *   6  │ │ └ tyo-02
 *   7  │ └ osa-01
 *   8  └ Vietnam
 *   9  Africa
 *  10  └ jnb-01
 *  11  Ungrouped
 *  12  ├ loose-1
 *  13  └ loose-2
 */
const TREE: FlatRow[] = [
  g('apac', 0),
  g('india', 1),
  n('mum-01', 2),
  g('japan', 1),
  g('tokyo', 2),
  n('tyo-01', 3),
  n('tyo-02', 3),
  n('osa-01', 2),
  g('vietnam', 1),
  g('africa', 0),
  n('jnb-01', 1),
  head(),
  loose('loose-1'),
  loose('loose-2'),
];

describe('treeGuides', () => {
  const guides = treeGuides(TREE);

  it('draws nothing on a top-level row or the Ungrouped header', () => {
    expect(guides[0]).toEqual([]);
    expect(guides[9]).toEqual([]);
    expect(guides[11]).toEqual([]);
  });

  it('joins a child with a tee while a sibling follows and an elbow on the last one', () => {
    expect(guides[1]).toEqual(['tee']);
    expect(guides[3]).toEqual(['tee']);
    expect(guides[8]).toEqual(['elbow']);
    expect(guides[5]).toEqual(['pipe', 'pipe', 'tee']);
    expect(guides[6]).toEqual(['pipe', 'pipe', 'elbow']);
  });

  it('carries an ancestor line only while that ancestor has a sibling below', () => {
    // India's only child: APAC's line continues (Japan follows India), India's own line ends.
    expect(guides[2]).toEqual(['pipe', 'elbow']);
    // Osaka is Japan's last child and Vietnam follows Japan, so column 0 continues.
    expect(guides[7]).toEqual(['pipe', 'elbow']);
  });

  it('does not carry a line from one top-level folder into the next', () => {
    expect(guides[10]).toEqual(['elbow']);
  });

  it('hangs the Ungrouped nodes off the header', () => {
    expect(guides[12]).toEqual(['tee']);
    expect(guides[13]).toEqual(['elbow']);
  });

  it('treats placeholder and drop-slot rows as siblings at their depth', () => {
    const rows = [g('a', 0), loading(1)];
    expect(treeGuides(rows)[1]).toEqual(['elbow']);
    const withSlot = [g('a', 0), n('x', 1), slot(1), n('y', 1)];
    expect(treeGuides(withSlot).slice(1)).toEqual([['tee'], ['tee'], ['elbow']]);
  });

  it('answers an empty list with nothing', () => {
    expect(treeGuides([])).toEqual([]);
  });
});

describe('parentRows', () => {
  it('names each row’s folder, and the header for an Ungrouped node', () => {
    expect(parentRows(TREE)).toEqual([-1, 0, 1, 0, 3, 4, 4, 3, 0, -1, 9, -1, 11, 11]);
  });
});

/** The lit parts of one cell as a short string, `up/down/stub` per layer: `S:ud-` etc. */
function at(lit: LitGuides, row: number, col: number): string {
  const c = lit.get(row)?.get(col);
  if (!c) return '';
  const f = (p: { up: boolean; down: boolean; stub: boolean }) =>
    `${p.up ? 'u' : '-'}${p.down ? 'd' : '-'}${p.stub ? 's' : '-'}`;
  return `S:${f(c.strong)} W:${f(c.soft)}`;
}

describe('litGuides', () => {
  const parent = parentRows(TREE);

  it('lights the selected row’s branch up to the root and nothing else', () => {
    const lit = litGuides(TREE, parent, 6, []); // tyo-02
    // Its own connector: the upper half and the stub.
    expect(at(lit, 6, 2)).toBe('S:u-s W:---');
    // tyo-01 sits between Tokyo and tyo-02: the line passes straight through.
    expect(at(lit, 5, 2)).toBe('S:ud- W:---');
    // Tokyo's own connector into Japan, then Japan's into APAC.
    expect(at(lit, 4, 1)).toBe('S:u-s W:---');
    expect(at(lit, 3, 0)).toBe('S:u-s W:---');
    // India's rows sit between APAC and Japan on column 0.
    expect(at(lit, 1, 0)).toBe('S:ud- W:---');
    expect(at(lit, 2, 0)).toBe('S:ud- W:---');
    // Nothing below the branch, and nothing in another column.
    expect(at(lit, 7, 1)).toBe('');
    expect(at(lit, 8, 0)).toBe('');
    expect(at(lit, 5, 1)).toBe('');
    expect([...lit.keys()].sort((a, b) => a - b)).toEqual([1, 2, 3, 4, 5, 6]);
  });

  it('merges branches from different folders where they meet, without dropping either', () => {
    const lit = litGuides(TREE, parent, -1, [2, 5, 7]); // mum-01, tyo-01, osa-01
    // Each start keeps its own connector.
    expect(at(lit, 2, 1)).toBe('S:--- W:u-s');
    expect(at(lit, 5, 2)).toBe('S:--- W:u-s');
    expect(at(lit, 7, 1)).toBe('S:--- W:u-s');
    // Japan's column runs from Japan down to Osaka, through Tokyo's rows.
    expect(at(lit, 4, 1)).toBe('S:--- W:uds');
    expect(at(lit, 6, 1)).toBe('S:--- W:ud-');
    // APAC's column: India joins (its connector), and the line runs on down to Japan.
    expect(at(lit, 1, 0)).toBe('S:--- W:uds');
    expect(at(lit, 2, 0)).toBe('S:--- W:ud-');
    expect(at(lit, 3, 0)).toBe('S:--- W:u-s');
  });

  it('keeps the two layers apart on the same cell', () => {
    // Pane on tyo-01, working set holds tyo-02 below it: the strong branch stops at tyo-01's
    // connector, and the soft one carries on down through it.
    const lit = litGuides(TREE, parent, 5, [6]);
    expect(at(lit, 5, 2)).toBe('S:u-s W:ud-');
    expect(at(lit, 6, 2)).toBe('S:--- W:u-s');
  });

  it('lights a whole folder once however many of its rows are checked', () => {
    const rows: FlatRow[] = [g('big', 0)];
    for (let i = 0; i < 3000; i++) rows.push(n(`n${i}`, 1));
    const all = rows.map((_, i) => i).slice(1);
    const lit = litGuides(rows, parentRows(rows), -1, all);
    expect(at(lit, 1, 0)).toBe('S:--- W:uds');
    expect(at(lit, 3000, 0)).toBe('S:--- W:u-s');
  });

  it('lights nothing for a top-level selection, and nothing with nothing selected', () => {
    expect(litGuides(TREE, parent, 0, []).size).toBe(0);
    expect(litGuides(TREE, parent, -1, []).size).toBe(0);
  });
});

describe('cellTones', () => {
  it('lets the pane’s branch win where both layers pass, and the batch show where only it does', () => {
    const lit = litGuides(TREE, parentRows(TREE), 5, [6]);
    expect(cellTones(lit.get(5)?.get(2))).toEqual({
      up: 'strong',
      down: 'soft',
      stub: 'strong',
    });
    expect(cellTones(lit.get(6)?.get(2))).toEqual({
      up: 'soft',
      down: 'plain',
      stub: 'soft',
    });
  });

  it('draws an unlit cell plain', () => {
    expect(cellTones(undefined)).toEqual({
      up: 'plain',
      down: 'plain',
      stub: 'plain',
    });
  });
});

describe('checkedRowIndices', () => {
  it('finds the drawn rows of the working set, Ungrouped included', () => {
    const checked = new Map([
      ['tyo-02', {}],
      ['loose-2', {}],
      ['not-drawn', {}],
    ]);
    expect(checkedRowIndices(TREE, checked)).toEqual([6, 13]);
  });
});

describe('checkedPerGroup', () => {
  const groups = [
    { id: 'apac', parent_id: null },
    { id: 'india', parent_id: 'apac' },
    { id: 'mumbai', parent_id: 'india' },
    { id: 'japan', parent_id: 'apac' },
  ];

  it('counts each checked node in every folder above it, closed folders included', () => {
    const checked = new Map([
      ['a', { group_id: 'mumbai' }],
      ['b', { group_id: 'japan' }],
      ['c', { group_id: 'japan' }],
      ['d', { group_id: null }],
    ]);
    const out = checkedPerGroup(checked, groups);
    expect(Object.fromEntries(out)).toEqual({
      apac: 3,
      india: 1,
      mumbai: 1,
      japan: 2,
    });
  });

  it('stops on a folder cycle instead of looping', () => {
    const loop = [
      { id: 'x', parent_id: 'y' },
      { id: 'y', parent_id: 'x' },
    ];
    expect(Object.fromEntries(checkedPerGroup(new Map([['a', { group_id: 'x' }]]), loop))).toEqual({
      x: 1,
      y: 1,
    });
  });
});

describe('stickyParents', () => {
  const parent = parentRows(TREE);
  const H = 30;

  it('pins nothing at the top of the list', () => {
    expect(stickyParents(TREE, parent, 0, H)).toEqual([]);
  });

  it('pins the folders that have scrolled off above the row under the band', () => {
    // Top row is Tokyo (4). A band of 3 would put osa-01 (7) under it, which is not in Tokyo and
    // has only APAC and Japan scrolled off — two, so the band is two: APAC and Japan.
    expect(stickyParents(TREE, parent, 4 * H, H)).toEqual([0, 3]);
  });

  it('pins three levels when all three have scrolled off', () => {
    const rows: FlatRow[] = [g('apac', 0), g('japan', 1), g('tokyo', 2)];
    for (let i = 0; i < 20; i++) rows.push(n(`t${i}`, 3));
    expect(stickyParents(rows, parentRows(rows), 5 * H, H)).toEqual([0, 1, 2]);
  });

  it('keeps only the deepest folders when there are more than the band holds', () => {
    const rows: FlatRow[] = [g('apac', 0), g('japan', 1), g('tokyo', 2)];
    for (let i = 0; i < 20; i++) rows.push(n(`t${i}`, 3));
    expect(stickyParents(rows, parentRows(rows), 5 * H, H, 2)).toEqual([1, 2]);
  });

  it('never pins a folder whose own row is still on screen', () => {
    // Top row is India (1): APAC is gone, India is not.
    expect(stickyParents(TREE, parent, 1 * H, H)).toEqual([0]);
  });

  it('pins nothing once the rows below the top have left every scrolled-off folder', () => {
    // Top row is jnb-01, Africa's only child; everything below is Ungrouped. An iterate-to-a-
    // fixed-point version flipped between [] and [Africa] here, depending on the pass count.
    expect(stickyParents(TREE, parent, 10 * H, H)).toEqual([]);
  });

  it('pins the Ungrouped header over its nodes', () => {
    expect(stickyParents(TREE, parent, 12 * H, H)).toEqual([11]);
  });
});
