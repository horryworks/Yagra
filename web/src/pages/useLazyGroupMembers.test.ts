// SPDX-License-Identifier: AGPL-3.0-only
// @vitest-environment jsdom
import { renderHook, waitFor, act } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { NodeGroup, NodeSummary } from '../types/api';

// The inventory tree's lazy member cache. Its load-bearing property is *not* what it fetches but
// what it refuses to fetch twice: two effects (visible-and-open groups, and the selected group's
// subtree) can both want the same group on the same render, and the in-flight set is the only
// thing standing between that and a duplicated request per group at fleet scale.
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

  it('fetches nothing in filter mode, where the server-side search owns the tree', async () => {
    const { useLazyGroupMembers } = await import('./useLazyGroupMembers');
    renderHook(() => useLazyGroupMembers({ ...OPTS, browsing: false }));
    expect(getGroupNodes).not.toHaveBeenCalled();
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
