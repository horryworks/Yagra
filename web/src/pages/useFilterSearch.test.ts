// SPDX-License-Identifier: AGPL-3.0-only
// @vitest-environment jsdom
import { act, renderHook } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { NodeSummary } from '../types/api';
import { useFilterSearch } from './useFilterSearch';
import { inventoryColumns } from './inventoryFilters';
import { defaultFilters, type FilterState } from '../lib/columnFilter';
import type { TFunction } from 'i18next';

// The three tree filters became sets in ADR-053 Inc.6; what this file tests is unchanged by that,
// so the fixture is simply the derived default state rather than a hand-written object.
const COLS = inventoryColumns(((k: string) => k) as unknown as TFunction, []);
const DEFAULT_INVENTORY_FILTERS = defaultFilters(COLS);

// The inventory tree's filter-mode search. What is worth testing here is not the request — it is
// WHEN one is issued: once per burst of keystrokes, again when the caller says the answer is stale,
// and never for a term the operator has cleared. That rule lives in an effect's dependency list,
// which is exactly the kind of thing that can be wrong for a whole release without anything failing
// to compile: `reload()` used to drop the search page without re-issuing the search, so editing a
// node while the box had a term in it emptied the tree until the term was retyped.
//
// Fake timers rather than real ones so the 200 ms debounce is stepped over deliberately: an
// assertion that no request went out has to mean "the debounce has not elapsed", not "the test was
// quick enough".

const listNodesPage = vi.fn();
vi.mock('../services/api', () => ({ api: { listNodesPage: (o: unknown) => listNodesPage(o) } }));

const node = (id: string): NodeSummary => ({ id, name: id }) as unknown as NodeSummary;
const ids = (ns: NodeSummary[]) => ns.map((n) => n.id);

/** A promise whose resolution this test controls, so "still in flight" is an observable state. */
function deferred<T>() {
  let resolve!: (v: T) => void;
  const promise = new Promise<T>((r) => {
    resolve = r;
  });
  return { promise, resolve };
}

/** Step the debounce (and let the resulting promise chain settle) inside `act`. */
const tick = async (ms: number) => {
  await act(async () => {
    await vi.advanceTimersByTimeAsync(ms);
  });
};

describe('useFilterSearch', () => {
  beforeEach(() => {
    vi.useFakeTimers();
    vi.clearAllMocks();
    listNodesPage.mockResolvedValue({ nodes: [], truncated: false });
  });
  afterEach(() => {
    vi.useRealTimers();
    vi.restoreAllMocks();
  });

  it('searches for nothing while the box is empty or blank', async () => {
    const { result, rerender } = renderHook((f: string) => useFilterSearch(f, DEFAULT_INVENTORY_FILTERS), {
      initialProps: '',
    });
    await tick(1000);
    rerender('   ');
    await tick(1000);

    expect(listNodesPage).not.toHaveBeenCalled();
    expect(result.current).toMatchObject({ nodes: [], loading: false, appliedTerm: '' });
  });

  it('fires one request per burst of keystrokes, for the last term typed', async () => {
    // Mounts empty, as the screen does — the box is local state that starts blank. A term present
    // at mount is NOT debounced (`useDebouncedValue` seeds itself with its initial value), which is
    // right for a filter restored from a URL and is simply not this box's case.
    const { rerender } = renderHook((f: string) => useFilterSearch(f, DEFAULT_INVENTORY_FILTERS), { initialProps: '' });
    rerender('M');
    rerender('Ma');
    rerender('Mat');
    await tick(200);

    expect(listNodesPage).toHaveBeenCalledTimes(1);
    expect(listNodesPage).toHaveBeenCalledWith({ search: 'Mat', limit: 500, state: undefined, kind: undefined, pool: undefined });
  });

  it('publishes the applied term when the request is issued, not when it answers', async () => {
    // The page feeds this term to the per-group member cache, which loads the folders the term
    // revealed. Publishing it on the answer would put those loads a round-trip behind the search.
    const d = deferred<{ nodes: NodeSummary[] }>();
    listNodesPage.mockReturnValue(d.promise);

    const { result } = renderHook((f: string) => useFilterSearch(f, DEFAULT_INVENTORY_FILTERS), { initialProps: 'Mat' });
    await tick(200);
    expect(result.current).toMatchObject({ appliedTerm: 'Mat', loading: true, nodes: [] });

    await act(async () => {
      d.resolve({ nodes: [node('n1')] });
    });
    expect(ids(result.current.nodes)).toEqual(['n1']);
    expect(result.current.loading).toBe(false);
  });

  it('drops the answer to a term the operator has already moved on from', async () => {
    const first = deferred<{ nodes: NodeSummary[] }>();
    const second = deferred<{ nodes: NodeSummary[] }>();
    listNodesPage.mockReturnValueOnce(first.promise).mockReturnValueOnce(second.promise);

    const { result, rerender } = renderHook((f: string) => useFilterSearch(f, DEFAULT_INVENTORY_FILTERS), {
      initialProps: 'a',
    });
    await tick(200);
    rerender('ab');
    await tick(200);

    await act(async () => {
      second.resolve({ nodes: [node('ab-hit')] });
    });
    await act(async () => {
      first.resolve({ nodes: [node('a-hit')] });
    });
    expect(ids(result.current.nodes)).toEqual(['ab-hit']);
  });

  it('re-issues the search for the same term when the caller invalidates it', async () => {
    // The regression. Every write on the Nodes page ends in `reload()`, which drops both member
    // caches; the per-group one re-fetches because its effects read the state that was cleared, and
    // the search page has nothing equivalent — so without this it stayed cleared, and the tree read
    // as "nothing matches" until the operator touched the box.
    listNodesPage.mockResolvedValueOnce({ nodes: [node('n1'), node('n2')] });
    const { result } = renderHook((f: string) => useFilterSearch(f, DEFAULT_INVENTORY_FILTERS), { initialProps: 'Mat' });
    await tick(200);
    expect(ids(result.current.nodes)).toEqual(['n1', 'n2']);

    const again = deferred<{ nodes: NodeSummary[] }>();
    listNodesPage.mockReturnValueOnce(again.promise);
    act(() => {
      result.current.refetch();
    });
    await tick(200);

    expect(listNodesPage).toHaveBeenCalledTimes(2);
    expect(listNodesPage).toHaveBeenLastCalledWith({ search: 'Mat', limit: 500, state: undefined, kind: undefined, pool: undefined });
    // …and the page it already had is still on screen while the new one is in flight: a write must
    // not blink the tree to empty and back.
    expect(ids(result.current.nodes)).toEqual(['n1', 'n2']);

    await act(async () => {
      again.resolve({ nodes: [node('n1')] });
    });
    expect(ids(result.current.nodes)).toEqual(['n1']);
  });

  it('clearing the box clears the applied term and stops searching', async () => {
    const { result, rerender } = renderHook((f: string) => useFilterSearch(f, DEFAULT_INVENTORY_FILTERS), {
      initialProps: 'Mat',
    });
    await tick(200);
    expect(listNodesPage).toHaveBeenCalledTimes(1);

    rerender('');
    await tick(1000);
    expect(result.current.appliedTerm).toBe('');
    expect(listNodesPage).toHaveBeenCalledTimes(1);
  });

  it('leaves nothing loading when the box is cleared mid-request', async () => {
    // The cancelled request never resolves its own `loading`, so the not-filtering branch has to.
    // Harmless today (every reader gates on the filter being non-empty) and a stuck spinner the
    // moment one does not.
    const d = deferred<{ nodes: NodeSummary[] }>();
    listNodesPage.mockReturnValue(d.promise);

    const { result, rerender } = renderHook((f: string) => useFilterSearch(f, DEFAULT_INVENTORY_FILTERS), {
      initialProps: 'Mat',
    });
    await tick(200);
    expect(result.current.loading).toBe(true);

    rerender('');
    expect(result.current.loading).toBe(false);
    await act(async () => {
      d.resolve({ nodes: [node('n1')] });
    });
    expect(result.current.loading).toBe(false);
  });

  it('takes the truncation flag from the server rather than measuring the page', async () => {
    // This used to be `nodes.length >= cap`, which was right while the only filter was a text
    // search the SQL served whole. With state / kind / pool the server rejects candidates AFTER
    // the query, so a three-row answer can be incomplete — the inference would have called that
    // page complete precisely when it was least so.
    listNodesPage.mockResolvedValue({ nodes: [node('n1')], truncated: true });
    const { result } = renderHook((f: string) => useFilterSearch(f, DEFAULT_INVENTORY_FILTERS), {
      initialProps: 'a',
    });
    await tick(200);
    expect(result.current.truncated).toBe(true);
  });

  it('does not report truncation for a complete answer, however long', async () => {
    listNodesPage.mockResolvedValue({
      nodes: Array.from({ length: 500 }, (_, i) => node(`n${i}`)),
      truncated: false,
    });
    const { result } = renderHook((f: string) => useFilterSearch(f, DEFAULT_INVENTORY_FILTERS), {
      initialProps: 'a',
    });
    await tick(200);
    expect(result.current.truncated).toBe(false);
  });

  it('shows an empty result rather than the previous one when the search fails', async () => {
    listNodesPage.mockResolvedValueOnce({ nodes: [node('n1')], truncated: true });
    const { result, rerender } = renderHook((f: string) => useFilterSearch(f, DEFAULT_INVENTORY_FILTERS), {
      initialProps: 'a',
    });
    await tick(200);
    expect(ids(result.current.nodes)).toEqual(['n1']);

    listNodesPage.mockRejectedValueOnce(new Error('boom'));
    rerender('ab');
    await tick(200);
    expect(result.current.nodes).toEqual([]);
    expect(result.current.loading).toBe(false);
    // A failed search must also drop the previous answer's truncation notice, or the page keeps
    // warning that matches are missing from a list that is now empty for an unrelated reason.
    expect(result.current.truncated).toBe(false);
  });

  // ── The state / kind / pool filters ─────────────────────────────────────────────────────────

  const withFilters = (over: FilterState): FilterState => ({
    ...DEFAULT_INVENTORY_FILTERS,
    ...over,
  });

  it('searches on a filter alone, with no term typed', async () => {
    // The case an empty-term guard would swallow: "show me the URL monitors" is a whole question,
    // and answering it with the unfiltered tree would look like the control did nothing.
    const { result } = renderHook(
      (f: FilterState) => useFilterSearch('', f),
      { initialProps: withFilters({ kind: 'url' }) },
    );
    await tick(200);
    expect(listNodesPage).toHaveBeenCalledWith({
      search: '',
      limit: 500,
      state: undefined,
      kind: 'url',
      pool: undefined,
    });
    expect(result.current.appliedTerm).toBe('');
  });

  it('sends each filter, and sends nothing for one left unset', async () => {
    // `''` must become `undefined`, never an empty string: `?state=` reaches the API edge as a
    // value it cannot parse, which is a 400.
    renderHook((f: FilterState) => useFilterSearch('sw', f), {
      initialProps: withFilters({ state: 'critical', pool: 'tokyo' }),
    });
    await tick(200);
    expect(listNodesPage).toHaveBeenCalledWith({
      search: 'sw',
      limit: 500,
      state: 'critical',
      kind: undefined,
      pool: 'tokyo',
    });
  });

  it('re-issues when a filter changes, and not when an unrelated re-render happens', async () => {
    // The filters are re-derived from the URL every render, so the object identity changes
    // constantly. Keying the effect on the object would fire a request per render.
    const { rerender } = renderHook((f: FilterState) => useFilterSearch('sw', f), {
      initialProps: withFilters({ kind: 'device' }),
    });
    await tick(200);
    expect(listNodesPage).toHaveBeenCalledTimes(1);

    rerender(withFilters({ kind: 'device' })); // same values, new object
    await tick(1000);
    expect(listNodesPage).toHaveBeenCalledTimes(1);

    rerender(withFilters({ kind: 'dns' }));
    await tick(200);
    expect(listNodesPage).toHaveBeenCalledTimes(2);
  });

  it('stops searching only when the term AND every filter are clear', async () => {
    const { result, rerender } = renderHook((f: FilterState) => useFilterSearch('', f), {
      initialProps: withFilters({ state: 'warning' }),
    });
    await tick(200);
    expect(listNodesPage).toHaveBeenCalledTimes(1);

    rerender(DEFAULT_INVENTORY_FILTERS);
    await tick(1000);
    expect(listNodesPage).toHaveBeenCalledTimes(1);
    expect(result.current.loading).toBe(false);
    expect(result.current.truncated).toBe(false);
  });
});
