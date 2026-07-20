// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import {
  addBoard,
  addInstance,
  clampRowSpan,
  clampSpan,
  countOfType,
  DASHBOARD_VERSION,
  moveItem,
  removeBoard,
  removeInstance,
  renameBoard,
  reorderByIds,
  sanitizeLayout,
  sanitizeWidgets,
  setBoardWidgets,
  setSettingsById,
  setSizeById,
  type RegistryView,
} from './layout';
import type { Board, WidgetInstance } from './types';

// Fake registry: type `a` allows spans 4/6 (default 4) and heights 1/2 (default 1); `b` allows only
// span 12 (default 12) and is fixed-height (only 1).
const reg: RegistryView = {
  isKnownType: (t) => t === 'a' || t === 'b',
  allowedSpansFor: (t) => (t === 'a' ? [4, 6] : t === 'b' ? [12] : []),
  defaultSpanFor: (t) => (t === 'a' ? 4 : 12),
  allowedRowSpansFor: (t) => (t === 'a' ? [1, 2] : t === 'b' ? [1] : []),
  defaultRowSpanFor: () => 1,
};

const inst = (id: string, type = 'a', span: 4 | 6 | 8 | 12 = 4): WidgetInstance => ({
  instanceId: id,
  type,
  span,
});

describe('clampSpan', () => {
  it('keeps an allowed span and snaps a disallowed one to the nearest', () => {
    expect(clampSpan('a', 6, reg)).toBe(6);
    expect(clampSpan('a', 12, reg)).toBe(6); // nearest of [4,6]
    expect(clampSpan('a', 5, reg)).toBe(4); // tie/closest
  });
  it('falls back to the default when the type has no allowed spans', () => {
    expect(clampSpan('unknown', 8, reg)).toBe(12);
  });
});

describe('clampRowSpan', () => {
  it('keeps an allowed height and snaps a disallowed one to the nearest', () => {
    expect(clampRowSpan('a', 2, reg)).toBe(2);
    expect(clampRowSpan('a', 3, reg)).toBe(2); // nearest of [1,2]
    expect(clampRowSpan('b', 2, reg)).toBe(1); // b is fixed-height ⇒ snaps to 1
  });
  it('falls back to the default when the type has no allowed heights', () => {
    expect(clampRowSpan('unknown', 3, reg)).toBe(1);
  });
});

describe('sanitizeLayout', () => {
  it('migrates a v1 doc (flat widgets) into a single board, clamping/dropping', () => {
    const out = sanitizeLayout(
      {
        version: 1,
        widgets: [
          { instanceId: 'x', type: 'a', span: 99 }, // span clamped to 6
          { instanceId: 'y', type: 'gone', span: 4 }, // unknown type dropped
          { instanceId: 'z', type: 'b', span: 4 }, // span clamped to 12
        ],
      },
      reg,
    );
    expect(out.version).toBe(DASHBOARD_VERSION);
    expect(out.boards).toHaveLength(1);
    expect(out.boards[0].name).toBe('Dashboard 1');
    const w = out.boards[0].widgets;
    expect(w.map((x) => x.instanceId)).toEqual(['x', 'z']);
    expect(w[0].span).toBe(6);
    expect(w[1].span).toBe(12);
  });

  it('sanitizes a v2 doc: dedupes board ids, defaults blank names, keeps ≥1 board', () => {
    const out = sanitizeLayout(
      {
        version: 2,
        boards: [
          { id: 'dup', name: 'One', widgets: [{ instanceId: 'x', type: 'a', span: 4 }] },
          { id: 'dup', name: '', widgets: [{ type: 'b' }] }, // duplicate id, blank name
          { id: 'gone', name: 'Empty', widgets: [{ type: 'gone' }] }, // unknown widget dropped
        ],
      },
      reg,
    );
    expect(out.version).toBe(DASHBOARD_VERSION);
    expect(out.boards.map((b) => b.id)).toEqual(['dup', 'dup-1', 'gone']);
    expect(out.boards[1].name).toBe('Dashboard 2'); // blank ⇒ positional default
    expect(out.boards[0].widgets).toHaveLength(1);
    expect(out.boards[2].widgets).toHaveLength(0);
  });

  it('returns a single empty board for a non-object or unrecognized shape', () => {
    for (const raw of [null, 42, { boards: 'nope', widgets: 'nope' }]) {
      const out = sanitizeLayout(raw, reg);
      expect(out.boards).toHaveLength(1);
      expect(out.boards[0].widgets).toEqual([]);
    }
  });

  it('repairs missing/duplicate widget ids within a board (no cascade)', () => {
    const out = sanitizeLayout(
      {
        version: 2,
        boards: [
          {
            id: 'b1',
            name: 'B',
            widgets: [
              { instanceId: 'dup', type: 'a' },
              { instanceId: 'dup', type: 'a' },
              { instanceId: 'dup', type: 'a' },
            ],
          },
        ],
      },
      reg,
    );
    // base, base-1, base-2 — never base-1-1 / base-2-2.
    expect(out.boards[0].widgets.map((w) => w.instanceId)).toEqual(['dup', 'dup-1', 'dup-2']);
  });
});

describe('sanitizeWidgets', () => {
  it('keeps known types, clamps spans, drops malformed/unknown, repairs ids', () => {
    const w = sanitizeWidgets(
      [
        { instanceId: 'x', type: 'a', span: 99 }, // clamped to 6
        { instanceId: 'y', type: 'gone' }, // unknown ⇒ dropped
        null,
        'nope',
        { type: 'b', span: 4 }, // missing id ⇒ repaired; span clamped to 12
      ],
      reg,
    );
    expect(w.map((x) => x.instanceId)).toEqual(['x', 'b-1']);
    expect(w[0].span).toBe(6);
    expect(w[1].span).toBe(12);
  });

  it('reads and clamps rowSpan, keeping standard (1) as an absent field', () => {
    const w = sanitizeWidgets(
      [
        { instanceId: 'x', type: 'a', span: 4, rowSpan: 2 }, // kept
        { instanceId: 'y', type: 'a', span: 4, rowSpan: 9 }, // clamped to 2 (nearest of [1,2])
        { instanceId: 'z', type: 'a', span: 4, rowSpan: 1 }, // standard ⇒ field dropped
        { instanceId: 'q', type: 'b', span: 12, rowSpan: 2 }, // b is fixed-height ⇒ dropped to 1
        { instanceId: 'r', type: 'a', span: 4 }, // no rowSpan ⇒ standard (absent)
      ],
      reg,
    );
    expect(w.map((x) => x.rowSpan)).toEqual([2, 2, undefined, undefined, undefined]);
    // the field is genuinely absent (not `rowSpan: 1`), so lean/pre-height docs round-trip unchanged
    expect('rowSpan' in w[2]).toBe(false);
  });

  it('returns [] for a non-array', () => {
    expect(sanitizeWidgets('nope', reg)).toEqual([]);
  });
});

describe('board helpers', () => {
  const board = (id: string, name = id, widgets: WidgetInstance[] = []): Board => ({
    id,
    name,
    widgets,
  });

  it('addBoard appends', () => {
    expect(addBoard([board('a')], board('c')).map((x) => x.id)).toEqual(['a', 'c']);
  });

  it('removeBoard drops by id but never empties the set', () => {
    const base = [board('a'), board('c')];
    expect(removeBoard(base, 'a').map((x) => x.id)).toEqual(['c']);
    expect(removeBoard([board('only')], 'only')).toHaveLength(1); // last board guarded
    expect(removeBoard(base, 'nope')).toBe(base); // unknown id ⇒ unchanged (same ref)
  });

  it('renameBoard renames the matching board (no-op if absent)', () => {
    expect(renameBoard([board('a', 'One')], 'a', 'Two')[0].name).toBe('Two');
    expect(renameBoard([board('a', 'One')], 'nope', 'Two')[0].name).toBe('One');
  });

  it('setBoardWidgets replaces one board widget list', () => {
    const base = [board('a'), board('c')];
    const next = setBoardWidgets(base, 'a', [inst('w1')]);
    expect(next[0].widgets).toHaveLength(1);
    expect(next[1].widgets).toHaveLength(0);
  });
});

describe('list mutators', () => {
  it('adds and removes by instanceId', () => {
    const base = [inst('a1'), inst('a2')];
    const added = addInstance(base, inst('a3'));
    expect(added.map((w) => w.instanceId)).toEqual(['a1', 'a2', 'a3']);
    expect(removeInstance(added, 'a2').map((w) => w.instanceId)).toEqual(['a1', 'a3']);
    // removing an absent id is a no-op
    expect(removeInstance(base, 'nope')).toHaveLength(2);
  });

  it('moves an item to a new index (clamped)', () => {
    const base = [inst('a1'), inst('a2'), inst('a3')];
    expect(moveItem(base, 0, 2).map((w) => w.instanceId)).toEqual(['a2', 'a3', 'a1']);
    expect(moveItem(base, 2, 0).map((w) => w.instanceId)).toEqual(['a3', 'a1', 'a2']);
    expect(moveItem(base, 0, 99).map((w) => w.instanceId)).toEqual(['a2', 'a3', 'a1']);
    expect(moveItem(base, 9, 0)).toBe(base); // out-of-range from ⇒ unchanged
  });

  it('reorders by id and keeps stragglers at the end', () => {
    const base = [inst('a1'), inst('a2'), inst('a3')];
    expect(reorderByIds(base, ['a3', 'a1', 'a2']).map((w) => w.instanceId)).toEqual([
      'a3',
      'a1',
      'a2',
    ]);
    // a missing id in the order list ⇒ that widget keeps its place at the end
    expect(reorderByIds(base, ['a2', 'a1']).map((w) => w.instanceId)).toEqual(['a2', 'a1', 'a3']);
  });

  it('setSizeById clamps span + rowSpan together and drops a standard height', () => {
    const base = [inst('a1', 'a', 4)];
    // span clamps to 6 (a allows [4,6]); rowSpan clamps to 2 (a allows [1,2])
    const sized = setSizeById(base, 'a1', 99, 9, reg);
    expect(sized[0].span).toBe(6);
    expect(sized[0].rowSpan).toBe(2);
    // back to standard height drops the rowSpan field entirely (absence = standard)
    const standard = setSizeById(sized, 'a1', 4, 1, reg);
    expect(standard[0].span).toBe(4);
    expect(standard[0].rowSpan).toBeUndefined();
    expect('rowSpan' in standard[0]).toBe(false);
    // a fixed-height type (b allows only rowSpan 1) can never gain a taller row span
    expect(setSizeById([inst('b1', 'b', 12)], 'b1', 12, 3, reg)[0].rowSpan).toBeUndefined();
  });

  it('merges settings', () => {
    const base = [inst('a1', 'a', 4)];
    const withSettings = setSettingsById(base, 'a1', { agg: 'max_1h' });
    expect(withSettings[0].settings).toEqual({ agg: 'max_1h' });
    const merged = setSettingsById(withSettings, 'a1', { nodeId: 'n1' });
    expect(merged[0].settings).toEqual({ agg: 'max_1h', nodeId: 'n1' });
  });

  it('counts instances of a type (for maxInstances)', () => {
    const base = [inst('a1', 'a'), inst('b1', 'b'), inst('a2', 'a')];
    expect(countOfType(base, 'a')).toBe(2);
    expect(countOfType(base, 'b')).toBe(1);
    expect(countOfType(base, 'c')).toBe(0);
  });
});
