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
const getGroupNodesBatch = vi.fn();
vi.mock('../services/api', () => ({
  api: {
    getGroupNodes: (id: string | null) => getGroupNodes(id),
    getGroupNodesBatch: (ids: string[]) => getGroupNodesBatch(ids),
  },
}));

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
  // The folders the tree says are on screen and waiting (ADR-125). It used to be `collapsed: {}`
  // and the hook derived the set itself from "every folder with no collapsed ancestor" — which on
  // a real deployment meant all of them. Now the viewport decides, and a test says so outright.
  visibleGroupKeys: ['g1', 'g2'],
  ready: true,
  browsing: true,
  selectedGroupId: null as string | null,
  filterTerm: '',
};

describe('useLazyGroupMembers', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    getGroupNodes.mockResolvedValue({ nodes: [], truncated: false });
    // ⚠️ The default batch answer ECHOES what it was asked (`answered`), because that echo is what
    // says "this core understood the question". A mock that omitted it would put every test on the
    // N-1 fallback path without saying so.
    getGroupNodesBatch.mockImplementation((ids: string[]) =>
      Promise.resolve({ nodes: [], truncated: false, answered: ids }),
    );
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
    // Both folders leave in ONE request (ADR-125) — hence reading the batch's argument rather than
    // a call per folder.
    const asked = [
      ...getGroupNodesBatch.mock.calls.flatMap((c) => c[0] as string[]),
      ...getGroupNodes.mock.calls.map((c) => c[0] as string),
    ];
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
    // 🚨 The batch answers with ONE flat list for several folders, so the hook has to split it by
    // `group_id` — which means the fixture has to set it. A node with no `group_id` would be
    // dropped, and the folder would look loaded but empty.
    getGroupNodesBatch.mockImplementation((ids: string[]) =>
      Promise.resolve({
        nodes: ids.map((id) => ({ ...node(`${id}-a`), group_id: id })),
        truncated: false,
        answered: ids,
      }),
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
      initialProps: { ...OPTS, groups: [group('g1')], visibleGroupKeys: ['g1'] },
    });
    await waitFor(() => expect(getGroupNodes).toHaveBeenCalled());
    const afterFirst = getGroupNodes.mock.calls.length;

    // Select the same group: its subtree is {g1}, which the browse effect is already fetching.
    rerender({ ...OPTS, groups: [group('g1')], visibleGroupKeys: ['g1'], selectedGroupId: 'g1' });
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
      initialProps: { ...OPTS, groups: [group('g1')], visibleGroupKeys: ['g1'] },
    });
    await waitFor(() => expect(result.current.loadedGroups.has('g1')).toBe(true));
    const calls = getGroupNodes.mock.calls.filter((c) => c[0] === 'g1').length;

    rerender({ ...OPTS, groups: [group('g1')], visibleGroupKeys: ['g1'], selectedGroupId: 'g1' });
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
    getGroupNodesBatch.mockRejectedValue(new Error('boom'));
    const { useLazyGroupMembers } = await import('./useLazyGroupMembers');
    const { result } = renderHook(() => useLazyGroupMembers(OPTS));

    await waitFor(() => expect(getGroupNodes).toHaveBeenCalled());
    expect(result.current.loadedGroups.has('g1')).toBe(false);
    expect(result.current.nodes).toEqual([]);
  });

  it('stops after one attempt — a failed group is not retried on every render', async () => {
    // 🚨 The half the test above cannot see, and the reason ADR-125 exists. `toHaveBeenCalled()`
    // means "at least once" and `waitFor` returns on the first call, so an **unbounded retry loop
    // satisfies every assertion up there**. The loop was real: `.catch` left the key in neither
    // the loaded nor the in-flight set, and `.finally` published a fresh `loadingGroups` Set,
    // which changed `loadMissing`'s identity, which re-ran all three effects, which fetched the
    // key again — at whatever speed the server returns failures. So this one COUNTS.
    getGroupNodes.mockRejectedValue(new Error('boom'));
    getGroupNodesBatch.mockRejectedValue(new Error('boom'));
    const { useLazyGroupMembers } = await import('./useLazyGroupMembers');
    renderHook(() => useLazyGroupMembers({ ...OPTS, groups: [group('g1')], visibleGroupKeys: ['g1'] }));

    await waitFor(() => expect(getGroupNodes).toHaveBeenCalled());
    // Let every queued microtask and re-render settle. A loop runs straight through this.
    await act(async () => {
      await new Promise((r) => setTimeout(r, 50));
    });
    expect(getGroupNodes.mock.calls.filter((c) => c[0] === 'g1')).toHaveLength(1);
  });

  it('a whole screenful of folders leaves in one request', async () => {
    // The point of the batch form (ADR-125): twenty folders on screen is one round trip, not
    // twenty — and, crucially, ONE run of the server's five-read row builder rather than twenty.
    const many = Array.from({ length: 20 }, (_, i) => group(`g${i}`));

    const { useLazyGroupMembers } = await import('./useLazyGroupMembers');
    const { result } = renderHook(() =>
      useLazyGroupMembers({ ...OPTS, groups: many, visibleGroupKeys: many.map((g) => g.id) }),
    );

    await waitFor(() => expect(result.current.loadedGroups.has('g19')).toBe(true));
    expect(getGroupNodesBatch.mock.calls).toHaveLength(1);
    expect(getGroupNodesBatch.mock.calls[0][0]).toHaveLength(20);
    // ⚠️ The ungrouped bucket goes on its own, always: the batch matches `group_id = ANY(...)`
    // and SQL NULL is not a value ANY can match.
    expect(getGroupNodes.mock.calls).toEqual([[null]]);
  });

  it('falls back to one folder at a time against a core that does not know the batch form', async () => {
    // 🚨 **The N-1 trap this exists for.** An older core ignores the unknown `groups=`, finds no
    // `group=` either, and answers with the UNGROUPED bucket — a perfectly ordinary 200. Believing
    // it would file every ungrouped node under all thirty folders the tree asked about. The absent
    // `answered` echo is the only thing that distinguishes the two.
    getGroupNodesBatch.mockResolvedValue({ nodes: [node('stray')], truncated: false });
    const many = Array.from({ length: 20 }, (_, i) => group(`g${i}`));

    const { useLazyGroupMembers } = await import('./useLazyGroupMembers');
    const { result } = renderHook(() =>
      useLazyGroupMembers({ ...OPTS, groups: many, visibleGroupKeys: many.map((g) => g.id) }),
    );

    await waitFor(() => expect(result.current.loadedGroups.has('g19')).toBe(true));
    // The batch was tried once and never again.
    expect(getGroupNodesBatch.mock.calls).toHaveLength(1);
    // Every folder then went out singly — the shape that works against any core.
    const singly = getGroupNodes.mock.calls.map((c) => c[0] as string | null);
    for (const g of many) expect(singly).toContain(g.id);
    expect(singly).toContain(null);
    // 🚨 And the stray row from the misread answer is nowhere: no folder was given contents it
    // never had.
    expect(result.current.nodes.map((n) => n.id)).not.toContain('stray');
  });

  it('🚨 stops asking for a folder the echo does not claim to cover', async () => {
    // The loop this whole change began with, arriving through the door its own fix opened. A folder
    // named in the request but absent from `answered` is in neither the loaded set nor the failed
    // one — so the effects queue it again on the very next render, and again, at the speed the
    // server answers. Found by the browser suite (its generated mock echoes a set unrelated to the
    // request): 179 requests and climbing.
    getGroupNodesBatch.mockResolvedValue({
      nodes: [],
      truncated: false,
      answered: ['someone-else'], // a real echo, covering none of what was asked
    });
    const many = Array.from({ length: 5 }, (_, i) => group(`g${i}`));

    const { useLazyGroupMembers } = await import('./useLazyGroupMembers');
    const { result } = renderHook(() =>
      useLazyGroupMembers({ ...OPTS, groups: many, visibleGroupKeys: many.map((g) => g.id) }),
    );

    await waitFor(() => expect(getGroupNodesBatch).toHaveBeenCalled());
    await act(async () => {
      await new Promise((r) => setTimeout(r, 50));
    });
    // Tried once, then left alone — the operator gets a failed row with a retry, not a spinner
    // over a request storm.
    expect(getGroupNodesBatch.mock.calls).toHaveLength(1);
    for (const g of many) expect(result.current.failedGroups.has(g.id)).toBe(true);
    expect(result.current.loadedGroups.has('g0')).toBe(false);
  });

  it('never has more than the concurrency budget in flight, and frees one slot per answer', async () => {
    // 🚨 The browser used to enforce this for us and quietly stopped (ADR-125): over HTTP/1.1 it
    // opens at most 6 connections per origin, so a burst reached the server 6 at a time by
    // accident. ADR-044 made TLS — and so HTTP/2 multiplexing — the default, and the accident went
    // away.
    //
    // ⚠️ Measured on the FALLBACK path, because that is the only one that still sends a request per
    // folder — which is exactly the path an N-1 core puts a modern WebUI on, and therefore the one
    // where the budget still has work to do.
    getGroupNodesBatch.mockResolvedValue({ nodes: [], truncated: false }); // no echo ⇒ fall back
    const pending: Array<{ resolve: (v: { nodes: NodeSummary[]; truncated: boolean }) => void }> =
      [];
    getGroupNodes.mockImplementation(() => {
      const d = deferred<{ nodes: NodeSummary[]; truncated: boolean }>();
      pending.push(d);
      return d.promise;
    });
    const many = Array.from({ length: 20 }, (_, i) => group(`g${i}`));

    const { useLazyGroupMembers } = await import('./useLazyGroupMembers');
    renderHook(() =>
      useLazyGroupMembers({
        ...OPTS,
        groups: many,
        // A tall enough window that every folder is on screen at once — the burst this bounds.
        visibleGroupKeys: many.map((g) => g.id),
      }),
    );

    await waitFor(() => expect(getGroupNodes).toHaveBeenCalled());
    await act(async () => {
      await new Promise((r) => setTimeout(r, 20));
    });
    // 21 keys want fetching (20 folders + the ungrouped bucket). Six of them may.
    expect(getGroupNodes.mock.calls).toHaveLength(6);

    await act(async () => {
      pending[0].resolve({ nodes: [], truncated: false });
    });
    expect(getGroupNodes.mock.calls).toHaveLength(7);

    // Drain, so no unsettled promise outlives the test.
    await act(async () => {
      for (const d of pending) d.resolve({ nodes: [], truncated: false });
    });
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
