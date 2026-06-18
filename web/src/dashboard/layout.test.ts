import { describe, expect, it } from 'vitest';
import {
  addInstance,
  clampSpan,
  countOfType,
  DASHBOARD_VERSION,
  moveItem,
  removeInstance,
  reorderByIds,
  sanitizeLayout,
  setSettingsById,
  setSpanById,
  type RegistryView,
} from './layout';
import type { WidgetInstance } from './types';

// Fake registry: type `a` allows spans 4/6 (default 4); `b` allows only 12 (default 12).
const reg: RegistryView = {
  isKnownType: (t) => t === 'a' || t === 'b',
  allowedSpansFor: (t) => (t === 'a' ? [4, 6] : t === 'b' ? [12] : []),
  defaultSpanFor: (t) => (t === 'a' ? 4 : 12),
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

describe('sanitizeLayout', () => {
  it('drops unknown types, clamps spans, and stamps the version', () => {
    const out = sanitizeLayout(
      {
        version: 0,
        widgets: [
          { instanceId: 'x', type: 'a', span: 99 }, // span clamped to 6
          { instanceId: 'y', type: 'gone', span: 4 }, // unknown type dropped
          { instanceId: 'z', type: 'b', span: 4 }, // span clamped to 12
        ],
      },
      reg,
    );
    expect(out.version).toBe(DASHBOARD_VERSION);
    expect(out.widgets.map((w) => w.instanceId)).toEqual(['x', 'z']);
    expect(out.widgets[0].span).toBe(6);
    expect(out.widgets[1].span).toBe(12);
  });

  it('repairs missing and duplicate instanceIds while preserving order', () => {
    const out = sanitizeLayout(
      { widgets: [{ type: 'a' }, { instanceId: 'dup', type: 'a' }, { instanceId: 'dup', type: 'a' }] },
      reg,
    );
    const ids = out.widgets.map((w) => w.instanceId);
    expect(ids.length).toBe(3);
    expect(new Set(ids).size).toBe(3); // all unique after repair
  });

  it('repairs repeated duplicate ids with clean incrementing suffixes (no cascade)', () => {
    const out = sanitizeLayout(
      {
        widgets: [
          { instanceId: 'dup', type: 'a' },
          { instanceId: 'dup', type: 'a' },
          { instanceId: 'dup', type: 'a' },
        ],
      },
      reg,
    );
    // base, base-1, base-2 — never base-1-1 / base-2-2.
    expect(out.widgets.map((w) => w.instanceId)).toEqual(['dup', 'dup-1', 'dup-2']);
  });

  it('returns an empty board for a non-object or missing widgets', () => {
    expect(sanitizeLayout(null, reg).widgets).toEqual([]);
    expect(sanitizeLayout({ widgets: 'nope' }, reg).widgets).toEqual([]);
    expect(sanitizeLayout(42, reg).widgets).toEqual([]);
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

  it('sets a span (clamped to the type) and merges settings', () => {
    const base = [inst('a1', 'a', 4)];
    expect(setSpanById(base, 'a1', 99, reg)[0].span).toBe(6); // clamped
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
