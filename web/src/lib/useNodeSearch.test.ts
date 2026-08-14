// SPDX-License-Identifier: AGPL-3.0-only
// @vitest-environment jsdom
//
// The node typeahead behind `NodePicker`, `GlobalSearch` and `ScopePicker`.
//
// Those three held byte-identical copies of this effect, which is why it was extracted — and it is
// also why it needs a test rather than three screen tests: every property here fails *silently*.
// A picker that overwrites a fresh result with a stale one still shows nodes. A picker that keeps
// the previous list after a failed search still shows nodes. A picker that refetches while closed
// still shows nodes. The screen looks right in all three cases; only the list is wrong, and only
// for the term the operator actually typed.
//
// Fake timers rather than real ones so "no request went out" means "the debounce has not elapsed"
// and not "the test was quick enough".

import { act, renderHook } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { NodeSearchResult } from '../types/api';
import { SEARCH_DEBOUNCE_MS } from './useDebouncedValue';
import { useNodeSearch } from './useNodeSearch';

const searchNodes = vi.fn();
vi.mock('../services/api', () => ({
  api: { searchNodes: (q: string, limit: number) => searchNodes(q, limit) },
}));

const node = (id: string): NodeSearchResult => ({ id, name: id }) as unknown as NodeSearchResult;
const ids = (rs: NodeSearchResult[]) => rs.map((r) => r.id);

/** A promise this test resolves by hand, so "still in flight" is an observable state. */
function deferred<T>() {
  let resolve!: (v: T) => void;
  const promise = new Promise<T>((r) => {
    resolve = r;
  });
  return { promise, resolve };
}

const tick = async (ms: number) => {
  await act(async () => {
    await vi.advanceTimersByTimeAsync(ms);
  });
};

interface Props {
  active: boolean;
  query: string;
  max?: number;
}
const mount = (initialProps: Props) =>
  renderHook(({ active, query, max = 50 }: Props) => useNodeSearch(active, query, max), {
    initialProps,
  });

describe('useNodeSearch', () => {
  beforeEach(() => {
    vi.useFakeTimers();
    vi.clearAllMocks();
    searchNodes.mockResolvedValue([]);
  });
  afterEach(() => {
    vi.useRealTimers();
    vi.restoreAllMocks();
  });

  it('asks nothing while the picker is closed', async () => {
    const { result } = mount({ active: false, query: 'sw' });
    await tick(1000);
    expect(searchNodes).not.toHaveBeenCalled();
    expect(result.current).toEqual({ results: [], loading: false });
  });

  it('leaves the previous matches alone when the picker closes', async () => {
    // Reopening shows what was there rather than blinking to empty and back. Clearing on close
    // would be a visible flash on every open, for no gain — the list is re-fetched anyway.
    searchNodes.mockResolvedValue([node('sw-core')]);
    const { result, rerender } = mount({ active: true, query: 'sw' });
    await tick(SEARCH_DEBOUNCE_MS);
    expect(ids(result.current.results)).toEqual(['sw-core']);

    rerender({ active: false, query: 'rt' });
    await tick(1000);
    expect(ids(result.current.results)).toEqual(['sw-core']);
    expect(searchNodes).toHaveBeenCalledTimes(1);
  });

  it('treats an empty term as a real query, so the list is populated before anything is typed', async () => {
    // The case an empty-term guard would swallow. A picker that opens blank until you type is the
    // shape this hook exists to prevent.
    mount({ active: true, query: '' });
    await tick(0);
    expect(searchNodes).toHaveBeenCalledWith('', 50);
  });

  it('settles a cleared box with no delay', async () => {
    // `useDebouncedValue(term, term ? 200 : 0)`. Clearing is one keystroke, not a burst — there is
    // nothing to wait for, and waiting would leave the old term's matches on screen.
    const { rerender } = mount({ active: true, query: 'sw' });
    await tick(SEARCH_DEBOUNCE_MS);
    expect(searchNodes).toHaveBeenCalledTimes(1);

    rerender({ active: true, query: '   ' }); // whitespace trims to empty
    await tick(0);
    expect(searchNodes).toHaveBeenLastCalledWith('', 50);
  });

  it('fires one request per burst, for the last term typed', async () => {
    const { rerender } = mount({ active: true, query: '' });
    await tick(0);
    searchNodes.mockClear();

    rerender({ active: true, query: 's' });
    rerender({ active: true, query: 'sw' });
    rerender({ active: true, query: 'sw-' });
    await tick(SEARCH_DEBOUNCE_MS);

    expect(searchNodes).toHaveBeenCalledTimes(1);
    expect(searchNodes).toHaveBeenCalledWith('sw-', 50);
  });

  it('reports loading from the keystroke, not from the request', async () => {
    // The operator is typing and the list on screen is already stale. Showing it as settled for
    // 200ms would be exactly the lie the spinner exists to prevent.
    const { result, rerender } = mount({ active: true, query: '' });
    await tick(0);
    await act(async () => {});
    expect(result.current.loading).toBe(false);

    rerender({ active: true, query: 'sw' });
    expect(result.current.loading).toBe(true); // inside the debounce, before any request
    await tick(SEARCH_DEBOUNCE_MS);
    await act(async () => {});
    expect(result.current.loading).toBe(false);
  });

  it('drops the answer to a term the operator has moved on from', async () => {
    // The guard `useDebouncedValue` deliberately does not provide: debouncing the *value* does
    // nothing about two in-flight requests answering in either order. Without the `cancelled` flag
    // the slower earlier one wins, and the picker shows matches for a term nobody typed.
    const first = deferred<NodeSearchResult[]>();
    const second = deferred<NodeSearchResult[]>();
    searchNodes.mockReturnValueOnce(first.promise).mockReturnValueOnce(second.promise);

    const { result, rerender } = mount({ active: true, query: 'a' });
    await tick(SEARCH_DEBOUNCE_MS);
    rerender({ active: true, query: 'ab' });
    await tick(SEARCH_DEBOUNCE_MS);

    await act(async () => {
      second.resolve([node('ab-hit')]);
    });
    await act(async () => {
      first.resolve([node('a-hit')]);
    });
    expect(ids(result.current.results)).toEqual(['ab-hit']);
  });

  it('shows nothing rather than the previous matches when a search fails', async () => {
    // Keeping them invites selecting a node that does not match what was typed — and the operator
    // has no way to tell, because a stale list and a fresh one look identical.
    searchNodes.mockResolvedValueOnce([node('sw-core')]);
    const { result, rerender } = mount({ active: true, query: 'sw' });
    await tick(SEARCH_DEBOUNCE_MS);
    expect(ids(result.current.results)).toEqual(['sw-core']);

    searchNodes.mockRejectedValueOnce(new Error('boom'));
    rerender({ active: true, query: 'rt' });
    await tick(SEARCH_DEBOUNCE_MS);
    await act(async () => {});
    expect(result.current.results).toEqual([]);
    expect(result.current.loading).toBe(false);
  });

  it('re-asks when the cap changes, and not when an unrelated re-render happens', async () => {
    const { rerender } = mount({ active: true, query: 'sw' });
    await tick(SEARCH_DEBOUNCE_MS);
    expect(searchNodes).toHaveBeenCalledTimes(1);

    rerender({ active: true, query: 'sw' });
    await tick(1000);
    expect(searchNodes).toHaveBeenCalledTimes(1);

    rerender({ active: true, query: 'sw', max: 200 });
    await tick(SEARCH_DEBOUNCE_MS);
    expect(searchNodes).toHaveBeenLastCalledWith('sw', 200);
  });
});
