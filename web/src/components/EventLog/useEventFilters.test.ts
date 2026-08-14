// SPDX-License-Identifier: AGPL-3.0-only
// @vitest-environment jsdom
//
// The two things the Events screens ask the server for so their filter row can be honest: how a
// plain term matches on this deployment, and the per-value counts a multi-select shows.
//
// `useWidenedEventLog` — the third export of this module — already has its own file, because it is
// about *when* a rule is evaluated. This one covers the rest, and the two properties worth the
// file are both "expensive when wrong, invisible when wrong":
//
//  - **The semantics answer is memoized at module scope**, because `/system-health` pings every
//    backing store. Losing the memo turns each navigation to Events into a health probe per page
//    view, which nothing on screen would show — the page renders identically either way. (Its
//    `undefined` case is load-bearing too: an N-1 core does not report the field, and axum drops
//    unknown query parameters *silently*, so the screens use the absence to say less rather than
//    to claim a behaviour the core does not have.)
//  - **A facet's counts exclude that column's own filter.** Selecting `syslog` must not make `trap`
//    read `0`. The version that passes its own filter through looks entirely correct: numbers
//    appear, they are internally consistent, and they answer the wrong question.

import { act, renderHook } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { TFunction } from 'i18next';
import { defaultFilters, type FilterState } from '../../lib/columnFilter';
import { EVENT_FILTER_KEYS, eventFilterColumns } from './eventFilterSpec';

const getSystemHealth = vi.fn();
const getEventStats = vi.fn();
vi.mock('../../services/api', () => ({
  api: {
    getSystemHealth: () => getSystemHealth(),
    getEventStats: (groupBy: string, opts: unknown) => getEventStats(groupBy, opts),
  },
}));

const t = ((k: string) => k) as unknown as TFunction;
const COLUMNS = eventFilterColumns(t);
const DEFAULTS = defaultFilters(COLUMNS);
const NOW = Date.parse('2026-08-14T10:00:00Z');

/** Let the microtask queue drain inside `act`, so a settled promise's state update is flushed. */
const settle = () => act(async () => {});

describe('useEventFilters', () => {
  beforeEach(async () => {
    vi.clearAllMocks();
    getSystemHealth.mockResolvedValue({ search_semantics: 'prefix' });
    getEventStats.mockResolvedValue([]);
    // The memo lives at module scope, so it survives between tests in this file. This is what
    // `resetSearchSemantics` is for — without it only the first test could observe a fetch.
    const { resetSearchSemantics } = await import('./useEventFilters');
    resetSearchSemantics();
  });
  afterEach(() => vi.restoreAllMocks());

  // ── Column labels for the mobile sheet ──────────────────────────────────────────────────────

  it('labels every filter key, so a new column is missing from neither surface', async () => {
    // The sheet reads the same keys the columns do. A column added to `EVENT_FILTER_KEYS` with no
    // label here would render a blank heading on mobile and nothing anywhere else.
    const { eventColumnLabels } = await import('./useEventFilters');
    expect(Object.keys(eventColumnLabels(t)).sort()).toEqual([...EVENT_FILTER_KEYS].sort());
    expect(Object.values(eventColumnLabels(t)).every((v) => v.length > 0)).toBe(true);
  });

  // ── How a plain term matches here ───────────────────────────────────────────────────────────

  it('asks the deployment once, however many callers there are', async () => {
    const { loadSearchSemantics } = await import('./useEventFilters');
    const [a, b, c] = await Promise.all([
      loadSearchSemantics(),
      loadSearchSemantics(),
      loadSearchSemantics(),
    ]);
    expect(getSystemHealth).toHaveBeenCalledTimes(1);
    expect([a, b, c]).toEqual(['prefix', 'prefix', 'prefix']);
  });

  it('reports undefined on a core that does not answer the question', async () => {
    // An N-1 core has no `search_semantics`. The screens must be able to tell "matches whole words"
    // from "nobody said", because the wording of the empty state differs.
    getSystemHealth.mockResolvedValue({});
    const { loadSearchSemantics } = await import('./useEventFilters');
    expect(await loadSearchSemantics()).toBeUndefined();
  });

  it('reports undefined rather than rejecting when the health probe fails', async () => {
    // `/system-health` touches every store, so it is one of the likelier calls to fail. A rejected
    // promise here would take down the Events page over a question it only asks to word a hint.
    getSystemHealth.mockRejectedValue(new Error('victorialogs unreachable'));
    const { loadSearchSemantics } = await import('./useEventFilters');
    await expect(loadSearchSemantics()).resolves.toBeUndefined();
  });

  it('publishes the answer to the hook once it arrives', async () => {
    const { useSearchSemantics } = await import('./useEventFilters');
    const { result } = renderHook(() => useSearchSemantics());
    expect(result.current).toBeUndefined(); // nothing known yet — not "substring"
    await settle();
    expect(result.current).toBe('prefix');
  });

  it('does not re-ask when a second screen mounts', async () => {
    // Both Events screens use this, and the operator moves between them. The answer is a property
    // of the deployment and cannot change without a core restart.
    const { useSearchSemantics } = await import('./useEventFilters');
    const first = renderHook(() => useSearchSemantics());
    await settle();
    first.unmount();

    const second = renderHook(() => useSearchSemantics());
    await settle();
    expect(second.result.current).toBe('prefix');
    expect(getSystemHealth).toHaveBeenCalledTimes(1);
  });

  // ── Facet counts ────────────────────────────────────────────────────────────────────────────

  const filtered: FilterState = { ...DEFAULTS, kind: 'syslog', action: 'fired' };

  it("drops a column's own filter before counting it", async () => {
    // The rule. Counting with `kind` still applied would answer "how many syslog rows are syslog",
    // so `trap` reads 0 and the operator is told the option they might switch to is empty.
    const { useEventFacets } = await import('./useEventFilters');
    const { result } = renderHook(() => useEventFacets(COLUMNS, filtered, NOW));
    act(() => result.current.load('kind'));

    const [groupBy, opts] = getEventStats.mock.calls[0];
    expect(groupBy).toBe('kind');
    expect(opts.kind).toBeUndefined(); // its own filter is gone
    expect(opts.action).toBe('fired'); // every other one is not
  });

  it('keeps the other columns when counting a different one', async () => {
    const { useEventFacets } = await import('./useEventFilters');
    const { result } = renderHook(() => useEventFacets(COLUMNS, filtered, NOW));
    act(() => result.current.load('action'));

    const [groupBy, opts] = getEventStats.mock.calls[0];
    expect(groupBy).toBe('action');
    expect(opts.action).toBeUndefined();
    expect(opts.kind).toBe('syslog');
  });

  it('asks only for the two dimensions the endpoint groups by', async () => {
    // `/events/stats` groups by kind or action. A popover on the message or range column opening a
    // request the endpoint cannot answer would be a 400 per click, with nothing on screen.
    const { useEventFacets } = await import('./useEventFilters');
    const { result } = renderHook(() => useEventFacets(COLUMNS, filtered, NOW));
    act(() => {
      result.current.load('message');
      result.current.load('at');
      result.current.load('source');
    });
    expect(getEventStats).not.toHaveBeenCalled();
  });

  it("carries the screen's extra scope into the count query", async () => {
    // The node-detail Events tab counts within one node. Without this the popover would decorate
    // the checkboxes with fleet-wide totals beside a per-node list.
    const { useEventFacets } = await import('./useEventFilters');
    const { result } = renderHook(() =>
      useEventFacets(COLUMNS, filtered, NOW, { node_id: 'n-1' }),
    );
    act(() => result.current.load('kind'));
    expect(getEventStats.mock.calls[0][1]).toMatchObject({ node_id: 'n-1', limit: 50 });
  });

  it('files each answer under its own key, keeping the ones already fetched', async () => {
    const { useEventFacets } = await import('./useEventFilters');
    getEventStats
      .mockResolvedValueOnce([{ key: 'syslog', count: 12 }, { key: 'trap', count: 3 }])
      .mockResolvedValueOnce([{ key: 'fired', count: 5 }]);

    const { result } = renderHook(() => useEventFacets(COLUMNS, filtered, NOW));
    act(() => result.current.load('kind'));
    await settle();
    act(() => result.current.load('action'));
    await settle();

    expect(result.current.counts).toEqual({
      kind: { syslog: 12, trap: 3 },
      action: { fired: 5 },
    });
  });

  it('leaves the counts alone when the aggregate query fails', async () => {
    // A count is decoration. Losing it must not blank the numbers already on screen, and must not
    // reject into the render tree.
    const { useEventFacets } = await import('./useEventFilters');
    getEventStats
      .mockResolvedValueOnce([{ key: 'syslog', count: 12 }])
      .mockRejectedValueOnce(new Error('clickhouse timeout'));

    const { result } = renderHook(() => useEventFacets(COLUMNS, filtered, NOW));
    act(() => result.current.load('kind'));
    await settle();
    act(() => result.current.load('action'));
    await settle();

    expect(result.current.counts).toEqual({ kind: { syslog: 12 } });
  });

  it('keeps one identity for `load` across renders', async () => {
    // It is a prop on every filter cell. A new function each render would be a changed prop on all
    // of them — which is why the current inputs are read through a ref rather than closed over.
    const { useEventFacets } = await import('./useEventFilters');
    const { result, rerender } = renderHook(
      ({ s }: { s: FilterState }) => useEventFacets(COLUMNS, s, NOW),
      { initialProps: { s: filtered } },
    );
    const first = result.current.load;
    rerender({ s: { ...filtered, kind: 'trap' } });
    expect(result.current.load).toBe(first);
  });

  it('counts against the CURRENT filters, not the ones `load` was created with', async () => {
    // The other half of that ref: a stable identity is only safe if it does not also freeze the
    // inputs. Closing over them would make every popover count against the state as it was when
    // the screen mounted.
    const { useEventFacets } = await import('./useEventFilters');
    const { result, rerender } = renderHook(
      ({ s }: { s: FilterState }) => useEventFacets(COLUMNS, s, NOW),
      { initialProps: { s: filtered } },
    );
    rerender({ s: { ...DEFAULTS, kind: 'trap', action: 'cleared' } });
    act(() => result.current.load('kind'));
    expect(getEventStats.mock.calls[0][1].action).toBe('cleared');
  });
});
