// SPDX-License-Identifier: AGPL-3.0-only
// @vitest-environment jsdom
import { renderHook, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { NodeGroup } from '../../types/api';

// Scope-picker data. The load-bearing property is what this hook does NOT do: it loads groups
// (cheap, bounded) and deliberately never full-loads the node inventory — Node mode goes through
// the server-side /nodes/search endpoint instead. A regression that reintroduced a `listNodes()`
// here would pull the whole fleet into the browser.

const listNodeGroups = vi.fn();
const listNodes = vi.fn();
const searchNodes = vi.fn();

vi.mock('../../services/api', () => ({
  api: {
    listNodeGroups: () => listNodeGroups(),
    listNodes: () => listNodes(),
    searchNodes: () => searchNodes(),
  },
}));

const group = (id: string, name: string): NodeGroup => ({ id, name }) as unknown as NodeGroup;

describe('useScopeData', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    listNodeGroups.mockResolvedValue([group('g1', 'tokyo'), group('g2', 'osaka')]);
  });
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it('loads the node groups once', async () => {
    const { useScopeData } = await import('./useScopeData');
    const { result } = renderHook(() => useScopeData());

    expect(result.current.groups).toEqual([]);
    await waitFor(() => expect(result.current.groups).toHaveLength(2));
    expect(result.current.groups[0].name).toBe('tokyo');
    expect(listNodeGroups).toHaveBeenCalledTimes(1);
  });

  it('never pulls the node inventory into the browser', async () => {
    const { useScopeData } = await import('./useScopeData');
    renderHook(() => useScopeData());

    await waitFor(() => expect(listNodeGroups).toHaveBeenCalled());
    expect(listNodes).not.toHaveBeenCalled();
    expect(searchNodes).not.toHaveBeenCalled();
  });

  it('degrades to an empty group list rather than throwing', async () => {
    listNodeGroups.mockRejectedValue(new Error('403'));
    const { useScopeData } = await import('./useScopeData');
    const { result } = renderHook(() => useScopeData());

    await waitFor(() => expect(listNodeGroups).toHaveBeenCalled());
    // All + Node scope modes still work without groups, so this is a soft failure by design.
    expect(result.current.groups).toEqual([]);
  });

  it('does not refetch on re-render', async () => {
    const { useScopeData } = await import('./useScopeData');
    const { rerender, result } = renderHook(() => useScopeData());
    await waitFor(() => expect(result.current.groups).toHaveLength(2));

    rerender();
    rerender();
    expect(listNodeGroups).toHaveBeenCalledTimes(1);
  });
});
