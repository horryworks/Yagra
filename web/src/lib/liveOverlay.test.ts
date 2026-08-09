// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import { overlayLiveStates, type LiveOverlay, type LiveStated } from './liveOverlay';
import type { NodeState } from '../types/api';

interface Row extends LiveStated {
  name: string;
}

const base = (): Row[] => [
  { id: 'a', state: 'ok', name: 'alpha' },
  { id: 'b', state: 'ok', name: 'bravo' },
  { id: 'c', state: 'warning', name: 'charlie' },
];

const live = (entries: Array<[string, NodeState]>) => new Map<string, NodeState>(entries);

/** Feed a sequence of live maps through the cache the way a component's ref does. */
function run(rows: Row[], maps: Array<Map<string, NodeState>>): Row[][] {
  let cache: LiveOverlay<Row> | null = null;
  return maps.map((m) => {
    cache = overlayLiveStates(rows, m, cache);
    return cache.out;
  });
}

describe('overlayLiveStates', () => {
  it('returns the base array itself when nothing is overridden', () => {
    const rows = base();
    const r = overlayLiveStates(rows, live([]), null);
    expect(r.out).toBe(rows);
  });

  it('treats a live entry equal to the fetched state as no override', () => {
    const rows = base();
    const r = overlayLiveStates(rows, live([['a', 'ok']]), null);
    expect(r.out).toBe(rows);
  });

  it('overlays a changed state without mutating the base row', () => {
    const rows = base();
    const r = overlayLiveStates(rows, live([['b', 'critical']]), null);
    expect(r.out).not.toBe(rows);
    expect(r.out.map((n) => n.state)).toEqual(['ok', 'critical', 'warning']);
    expect(rows[1].state).toBe('ok');
    // Untouched rows keep their original object identity, so a consumer comparing per-row can tell
    // which ones moved.
    expect(r.out[0]).toBe(rows[0]);
    expect(r.out[2]).toBe(rows[2]);
    expect(r.out[1].name).toBe('bravo');
  });

  it('returns the previous array when a later flush changes nothing visible', () => {
    const rows = base();
    // Flush 2 carries a state for a node this view does not show (the map is fleet-wide), flush 3
    // repeats what flush 1 already applied.
    const [r1, r2, r3] = run(rows, [
      live([['b', 'critical']]),
      live([
        ['b', 'critical'],
        ['zz', 'unreachable'],
      ]),
      live([
        ['b', 'critical'],
        ['zz', 'unreachable'],
        ['yy', 'ok'],
      ]),
    ]);
    expect(r2).toBe(r1);
    expect(r3).toBe(r1);
  });

  it('produces a new array as soon as a visible row does change', () => {
    const rows = base();
    const [r1, r2] = run(rows, [live([['b', 'critical']]), live([['b', 'unreachable']])]);
    expect(r2).not.toBe(r1);
    expect(r2.map((n) => n.state)).toEqual(['ok', 'unreachable', 'warning']);
  });

  it('drops the cache when the base list is replaced (a structural refetch)', () => {
    const first = base();
    let cache = overlayLiveStates(first, live([['b', 'critical']]), null);
    const renamed: Row[] = [
      { id: 'a', state: 'ok', name: 'alpha' },
      { id: 'b', state: 'ok', name: 'bravo-renamed' },
      { id: 'c', state: 'warning', name: 'charlie' },
    ];
    cache = overlayLiveStates(renamed, live([['b', 'critical']]), cache);
    expect(cache.out[1].name).toBe('bravo-renamed');
    expect(cache.out[1].state).toBe('critical');
  });

  it('is idempotent — a second call with the same inputs returns the same record', () => {
    const rows = base();
    const m = live([['c', 'critical']]);
    const first = overlayLiveStates(rows, m, null);
    expect(overlayLiveStates(rows, m, first)).toBe(first);
  });

  it('handles an empty list', () => {
    const rows: Row[] = [];
    const r = overlayLiveStates(rows, live([['a', 'ok']]), null);
    expect(r.out).toBe(rows);
  });
});
