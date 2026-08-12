// SPDX-License-Identifier: AGPL-3.0-only
// @vitest-environment jsdom
import { renderHook, waitFor, act } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { NodeGroup, NodeSummary } from '../types/api';

// The inventory tree's lazy member cache. Its load-bearing property is *not* what it fetches but
// what it refuses to fetch twice: three effects (visible-and-open groups, the selected group's
// subtree, and the subtree a filter revealed) can all want the same group on the same render, and
// the in-flight set is the only thing standing between that and a duplicated request per group at
// fleet scale.
//
// `getGroupNodes` is mocked with a manually-resolved promise so the window where a fetch is
// in-flight — the exact window the guard exists for — can be held open and asserted on.

const getGroupNodes = vi.fn();
vi.mock('../services/api', () => ({ api: { getGroupNodes: (id: string | null) => getGroupNodes(id) } }));

const group = (id: string, parent_id: string | null = null): NodeGroup =>
  ({ id, name: id, parent_id, group_type: 'generic', sort_order: 0 }) as unknown as NodeGroup;

const node = (id: string): NodeSummary => ({ id, name: id }) as unknown as NodeSummary;

/** A promise whose resolution this test controls, so "still in flight" is an observable state. */
function deferred<T>() {
  let resolve!: (v: T) => void;
  const promise = new Promise<T>((r) => {
    resolve = r;
  });
  return { promise, resolve };
}

const OPTS = {
  groups: [group('g1'), group('g2')],
  collapsed: {},
  ready: true,
  browsing: true,
  selectedGroupId: null as string | null,
  filterTerm: '',
};

describe('useLazyGroupMembers', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    getGroupNodes.mockResolvedValue({ nodes: [], truncated: false });
  });
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it('fetches nothing until the group skeleton is ready', async () => {
    const { useLazyGroupMembers } = await import('./useLazyGroupMembers');
    renderHook(() => useLazyGroupMembers({ ...OPTS, ready: false }));
    expect(getGroupNodes).not.toHaveBeenCalled();
  });

  it('fetches nothing in filter mode for a term that matches no group name', async () => {
    // The server-side search owns the tree here: every match it can find is already in its page.
    const { useLazyGroupMembers } = await import('./useLazyGroupMembers');
    renderHook(() => useLazyGroupMembers({ ...OPTS, browsing: false, filterTerm: 'zzz' }));
    expect(getGroupNodes).not.toHaveBeenCalled();
  });

  it("loads a name-matched group's whole subtree, even in filter mode", async () => {
    // The point of the whole feature: the search page matches node names/addresses and knows
    // nothing about groups, so a folder matched by name has to fetch its own contents.
    const { useLazyGroupMembers } = await import('./useLazyGroupMembers');
    const groups = [group('g1'), group('g1a', 'g1'), group('g2')];
    const { result } = renderHook(() =>
      useLazyGroupMembers({ ...OPTS, groups, browsing: false, filterTerm: 'g1' }),
    );

    await waitFor(() => expect(result.current.loadedGroups.has('g1a')).toBe(true));
    const asked = getGroupNodes.mock.calls.map((c) => c[0]);
    expect(asked).toContain('g1');
    expect(asked).toContain('g1a');
    // g2 did not match and is not under a match — browse mode is off, so nothing should have asked.
    expect(asked).not.toContain('g2');
  });

  it('reports the revealed set so the tree can place its loading rows', async () => {
    // Returned rather than re-derived by the page: the set that is fetched and the set the tree
    // draws placeholders for must be the same one.
    const { useLazyGroupMembers } = await import('./useLazyGroupMembers');
    const groups = [group('g1'), group('g1a', 'g1'), group('g2')];
    const { result } = renderHook(() =>
      useLazyGroupMembers({ ...OPTS, groups, browsing: false, filterTerm: 'g1' }),
    );
    expect([...result.current.revealedGroups].sort()).toEqual(['g1', 'g1a']);
    expect(result.current.revealTruncated).toBe(false);
  });

  it('never fetches a group twice when the filter and the selection both want it', async () => {
    // The third effect joins the same race the in-flight set already guards.
    const d = deferred<{ nodes: NodeSummary[]; truncated: boolean }>();
    getGroupNodes.mockReturnValue(d.promise);

    const { useLazyGroupMembers } = await import('./useLazyGroupMembers');
    const props = { ...OPTS, groups: [group('g1')], browsing: false, filterTerm: 'g1' };
    const { rerender } = renderHook((p: typeof props) => useLazyGroupMembers(p), {
      initialProps: props,
    });
    await waitFor(() => expect(getGroupNodes).toHaveBeenCalled());

    rerender({ ...props, selectedGroupId: 'g1' });
    expect(getGroupNodes.mock.calls.filter((c) => c[0] === 'g1')).toHaveLength(1);

    await act(async () => {
      d.resolve({ nodes: [node('n1')], truncated: false });
    });
  });

  it('flattens every loaded group into one node list', async () => {
    getGroupNodes.mockImplementation((id: string | null) =>
      Promise.resolve({ nodes: [node(`${id ?? 'ungrouped'}-a`)], truncated: false }),
    );
    const { useLazyGroupMembers } = await import('./useLazyGroupMembers');
    const { result } = renderHook(() => useLazyGroupMembers(OPTS));

    await waitFor(() => expect(result.current.nodes.length).toBeGreaterThan(0));
    const ids = result.current.nodes.map((n) => n.id).sort();
    expect(ids).toContain('g1-a');
    expect(ids).toContain('g2-a');
    // Each group is reported as loaded, which is what suppresses the tree's placeholder row.
    expect(result.current.loadedGroups.has('g1')).toBe(true);
    expect(result.current.loadedGroups.has('g2')).toBe(true);
  });

  it('never fetches the same group twice while its first fetch is still in flight', async () => {
    // The race this guards: the browse effect and the selected-subtree effect both want g1, and
    // neither has seen a result yet, so `loadedGroups` cannot tell them apart — only the
    // in-flight set can.
    const d = deferred<{ nodes: NodeSummary[]; truncated: boolean }>();
    getGroupNodes.mockReturnValue(d.promise);

    const { useLazyGroupMembers } = await import('./useLazyGroupMembers');
    const { rerender } = renderHook((props: typeof OPTS) => useLazyGroupMembers(props), {
      initialProps: { ...OPTS, groups: [group('g1')] },
    });
    await waitFor(() => expect(getGroupNodes).toHaveBeenCalled());
    const afterFirst = getGroupNodes.mock.calls.length;

    // Select the same group: its subtree is {g1}, which the browse effect is already fetching.
    rerender({ ...OPTS, groups: [group('g1')], selectedGroupId: 'g1' });
    expect(getGroupNodes.mock.calls.filter((c) => c[0] === 'g1')).toHaveLength(
      afterFirst === 0 ? 0 : 1,
    );

    await act(async () => {
      d.resolve({ nodes: [node('n1')], truncated: false });
    });
  });

  it('does not re-fetch a group it has already loaded', async () => {
    const { useLazyGroupMembers } = await import('./useLazyGroupMembers');
    const { result, rerender } = renderHook((props: typeof OPTS) => useLazyGroupMembers(props), {
      initialProps: { ...OPTS, groups: [group('g1')] },
    });
    await waitFor(() => expect(result.current.loadedGroups.has('g1')).toBe(true));
    const calls = getGroupNodes.mock.calls.filter((c) => c[0] === 'g1').length;

    rerender({ ...OPTS, groups: [group('g1')], selectedGroupId: 'g1' });
    await waitFor(() => expect(result.current.loadedGroups.has('g1')).toBe(true));
    expect(getGroupNodes.mock.calls.filter((c) => c[0] === 'g1')).toHaveLength(calls);
  });

  it('reports truncation so the page can say the list is capped', async () => {
    getGroupNodes.mockResolvedValue({ nodes: [node('n1')], truncated: true });
    const { useLazyGroupMembers } = await import('./useLazyGroupMembers');
    const { result } = renderHook(() => useLazyGroupMembers(OPTS));
    await waitFor(() => expect(result.current.anyTruncated).toBe(true));
  });

  it('leaves a failed group unloaded so opening it again retries', async () => {
    // A transient 500 must not poison the cache: marking it loaded would show an empty group
    // forever, which reads as "this group has no nodes".
    getGroupNodes.mockRejectedValue(new Error('boom'));
    const { useLazyGroupMembers } = await import('./useLazyGroupMembers');
    const { result } = renderHook(() => useLazyGroupMembers(OPTS));

    await waitFor(() => expect(getGroupNodes).toHaveBeenCalled());
    expect(result.current.loadedGroups.has('g1')).toBe(false);
    expect(result.current.nodes).toEqual([]);
  });

  it('invalidate drops the cache so open groups fetch again', async () => {
    // Called after any write that can change membership — without it the tree would keep showing
    // the pre-edit members until the page is reloaded.
    const { useLazyGroupMembers } = await import('./useLazyGroupMembers');
    const { result } = renderHook(() => useLazyGroupMembers(OPTS));
    await waitFor(() => expect(result.current.loadedGroups.size).toBeGreaterThan(0));
    const before = getGroupNodes.mock.calls.length;

    await act(async () => {
      result.current.invalidate();
    });
    await waitFor(() => expect(getGroupNodes.mock.calls.length).toBeGreaterThan(before));
  });
});
