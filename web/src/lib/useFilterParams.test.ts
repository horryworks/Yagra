// SPDX-License-Identifier: AGPL-3.0-only
// @vitest-environment jsdom
//
// The URL binding for a filter row (ADR-053). The codec underneath is pure and covered by
// `columnFilter.test.ts`; what this file covers is the two things only the hook can get wrong, and
// both have shipped as bugs.
//
//  1. **One handler, one write.** `setFilters(next, also)` exists because two `setSearchParams`
//     calls in one handler are both built from this render's snapshot, React batches them, and the
//     second silently discards the first. That is not a hypothesis — "clear all filters" on the
//     Events page cleared the columns and then restored them, because clearing the node picker was
//     a second write. A test that only checks `next` lands would pass with the broken shape.
//  2. **The clock is pinned.** A relative range resolves to an absolute lower bound once, when the
//     range is chosen. Recompute it per render and it creeps forward between "load older" pages, so
//     the keyset cursor walks towards rows that are no longer in the window — a list that silently
//     stops short. Nothing on screen says so.
//
// The `replace` case is here too, because Back is the only way to notice it and nobody tests Back.

import { createElement, type ReactNode } from 'react';
import { act, renderHook } from '@testing-library/react';
import { MemoryRouter, useLocation, useNavigate } from 'react-router-dom';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { specColumns } from './columnFilter';
import { useFilterParams } from './useFilterParams';

interface Row {
  at: number;
}

// Module scope on purpose: the hook memoizes on `columns`, so a fixture rebuilt per render would
// make every memo a fresh object and hide exactly the churn this hook is supposed to avoid.
const COLS = specColumns<Row>({
  range: {
    kind: 'range',
    presets: [
      { value: '24h', label: '24h', seconds: 86_400 },
      { value: '7d', label: '7d', seconds: 604_800 },
    ],
    defaultPreset: '24h',
  },
  severity: {
    kind: 'enum',
    options: [
      { value: 'critical', label: 'Critical' },
      { value: 'warning', label: 'Warning' },
    ],
    allLabel: 'All severities',
  },
  q: { kind: 'text', modes: ['contains'] },
});

const T0 = Date.parse('2026-08-14T10:00:00Z');

/** The hook plus the router handles the assertions need. */
const useProbe = () => ({
  ...useFilterParams(COLS),
  navigate: useNavigate(),
  search: useLocation().search,
  path: useLocation().pathname,
});

/** Render inside a memory history. `entries`'s last element is where we start. */
function mount(entries: string[]) {
  const wrapper = ({ children }: { children: ReactNode }) =>
    createElement(MemoryRouter, { initialEntries: entries, initialIndex: entries.length - 1 }, children);
  return renderHook(useProbe, { wrapper });
}

describe('useFilterParams', () => {
  beforeEach(() => {
    vi.useFakeTimers();
    vi.setSystemTime(T0);
  });
  afterEach(() => vi.useRealTimers());

  it('reads the state out of the query string, defaulting what is absent', () => {
    const { result } = mount(['/alerts/history?severity=critical&q=sw-core']);
    expect(result.current.filters).toEqual({ range: '24h', severity: 'critical', q: 'sw-core' });
  });

  it('leaves a value this build does not understand alone rather than rejecting it', () => {
    // `readFilterParams`'s rule, asserted through the hook because it is the hook a stale bookmark
    // actually lands in. The token dies later, in `normalizeSets`, on the way to the API — the
    // control must not blank the screen for it.
    const { result } = mount(['/alerts/history?severity=bogus']);
    expect(result.current.filters.severity).toBe('bogus');
  });

  it('writes through the codec, deleting anything at its default', () => {
    const { result } = mount(['/alerts/history']);
    act(() => {
      result.current.setFilters({ ...result.current.filters, severity: 'warning', range: '7d' });
    });
    expect(result.current.search).toBe('?range=7d&severity=warning');

    // Back to defaults ⇒ a bare URL. `?` always meaning "something is narrowing this" is what the
    // empty state and the clear-all affordance both read.
    act(() => {
      result.current.setFilters({ range: '24h', severity: '', q: '' });
    });
    expect(result.current.search).toBe('');
  });

  it('commits `also` and the filter row as ONE write', () => {
    // The regression, in its own shape: a screen with an extra URL key (alert history's `node_id`)
    // sets both in one handler. Two `setSearchParams` calls here would land only the second.
    const { result } = mount(['/alerts/history']);
    act(() => {
      result.current.setFilters({ ...result.current.filters, severity: 'critical' }, (p) =>
        p.set('node_id', 'n-1'),
      );
    });
    const params = new URLSearchParams(result.current.search);
    expect(params.get('severity')).toBe('critical');
    expect(params.get('node_id')).toBe('n-1');
  });

  it('clears the filter row and the extra key together', () => {
    // "Clear all" is the direction the bug was actually reported in: the columns cleared, and then
    // the node picker's write put them back.
    const { result } = mount(['/alerts/history?severity=critical&node_id=n-1']);
    act(() => {
      result.current.setFilters({ range: '24h', severity: '', q: '' }, (p) => p.delete('node_id'));
    });
    expect(result.current.search).toBe('');
  });

  it('replaces rather than pushes, so Back means the previous screen', () => {
    // Every settled keystroke is a `setFilters`. Pushing would make Back mean "the previous
    // character", and an operator would press it a dozen times to leave one page.
    const { result } = mount(['/nodes', '/alerts/history']);
    act(() => {
      result.current.setFilters({ ...result.current.filters, q: 's' });
    });
    act(() => {
      result.current.setFilters({ ...result.current.filters, q: 'sw' });
    });
    act(() => {
      result.current.setFilters({ ...result.current.filters, q: 'sw-' });
    });
    expect(result.current.search).toBe('?q=sw-');

    act(() => {
      result.current.navigate(-1);
    });
    expect(result.current.path).toBe('/nodes');
  });

  // ── The pinned clock ────────────────────────────────────────────────────────────────────────

  it('holds one instant across re-renders', () => {
    const { result, rerender } = mount(['/alerts/history']);
    const first = result.current.nowMs;
    expect(first).toBe(T0);

    vi.setSystemTime(T0 + 60_000);
    rerender();
    rerender();
    expect(result.current.nowMs).toBe(first);
  });

  it('holds that instant across a change to a NON-range column', () => {
    // The one that matters. Typing in the search box while paging through history must not move the
    // window's lower bound — `since` is a request parameter, and moving it mid-walk drops the rows
    // the cursor was heading for. There is no error and no empty state; the list just ends early.
    const { result } = mount(['/alerts/history']);
    const pinned = result.current.nowMs;

    vi.setSystemTime(T0 + 5 * 60_000);
    act(() => {
      result.current.setFilters({ ...result.current.filters, q: 'sw' });
    });
    expect(result.current.nowMs).toBe(pinned);

    vi.setSystemTime(T0 + 10 * 60_000);
    act(() => {
      result.current.setFilters({ ...result.current.filters, severity: 'critical' });
    });
    expect(result.current.nowMs).toBe(pinned);
  });

  it('re-reads the clock when the range itself changes', () => {
    // The other half: choosing "last 7 days" must mean seven days back from *now*, not from
    // whenever the screen was opened. A tab left open overnight would otherwise ask for a window
    // that ended hours ago.
    const { result } = mount(['/alerts/history']);
    expect(result.current.nowMs).toBe(T0);

    vi.setSystemTime(T0 + 3_600_000);
    act(() => {
      result.current.setFilters({ ...result.current.filters, range: '7d' });
    });
    expect(result.current.nowMs).toBe(T0 + 3_600_000);
  });

  it('pins the clock even on a screen with no range column', () => {
    // `useClientFilters` calls this unconditionally, and several of its screens have no range at
    // all. `rangeValue` is then the empty string for every render — which must still pin once,
    // not read `Date.now()` per render.
    const noRange = specColumns<Row>({ q: { kind: 'text', modes: ['contains'] } });
    const wrapper = ({ children }: { children: ReactNode }) =>
      createElement(MemoryRouter, { initialEntries: ['/nodes'] }, children);
    const { result, rerender } = renderHook(() => useFilterParams(noRange), { wrapper });

    const first = result.current.nowMs;
    vi.setSystemTime(T0 + 60_000);
    rerender();
    expect(result.current.nowMs).toBe(first);
  });
});
