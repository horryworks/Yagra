// SPDX-License-Identifier: AGPL-3.0-only
// @vitest-environment jsdom
// The wiring half of the "muted until" mark (ADR-209 decision 3). Which mute covers an alert is
// `suppression.ts::muteCovering`, tested without a DOM; this pins when the two lists are read.
import { act, cleanup, renderHook, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { Mute } from '../types/api';
import { useConfigChangeStore } from '../lib/configChanges';

const listMutes = vi.fn();
const listSuppressionExemptions = vi.fn();
const listNodeGroups = vi.fn();
vi.mock('../services/api', () => ({
  api: {
    listMutes: () => listMutes(),
    listSuppressionExemptions: () => listSuppressionExemptions(),
    listNodeGroups: () => listNodeGroups(),
  },
}));

const mute = (id: string, untilMs: number): Mute =>
  ({
    id,
    scope_kind: 'node',
    node_id: `node-${id}`,
    group_id: null,
    metric_name: null,
    until_at: new Date(untilMs).toISOString(),
    reason: null,
  }) as Mute;

describe('useMuteLists', () => {
  beforeEach(() => {
    listMutes.mockReset();
    listSuppressionExemptions.mockReset().mockResolvedValue([]);
    listNodeGroups.mockReset().mockResolvedValue([]);
  });
  // Unmount between cases: a hook left mounted would answer the next case's configuration change.
  afterEach(() => {
    cleanup();
    vi.useRealTimers();
  });

  it('reads the mutes and the folder tree on mount and again on a configuration change', async () => {
    const later = Date.now() + 3_600_000;
    listMutes.mockResolvedValueOnce([mute('a', later)]).mockResolvedValueOnce([
      mute('a', later),
      mute('b', later),
    ]);
    const { useMuteLists } = await import('./useMuteLists');
    const { result } = renderHook(() => useMuteLists());
    await waitFor(() => expect(result.current.mutes).toHaveLength(1));
    expect(listSuppressionExemptions).toHaveBeenCalledTimes(1);

    act(() => useConfigChangeStore.setState((s) => ({ ...s, changes: s.changes + 1 })));
    await waitFor(() => expect(result.current.mutes).toHaveLength(2));
    expect(listMutes).toHaveBeenCalledTimes(2);
    // A folder re-parented under a muted one changes the answer the same way a new mute does.
    expect(listNodeGroups).toHaveBeenCalledTimes(2);
  });

  it('keeps what it had when a re-read is refused', async () => {
    listMutes
      .mockResolvedValueOnce([mute('a', Date.now() + 3_600_000)])
      .mockRejectedValueOnce(new Error('403'));
    const { useMuteLists } = await import('./useMuteLists');
    const { result } = renderHook(() => useMuteLists());
    await waitFor(() => expect(result.current.mutes).toHaveLength(1));

    act(() => useConfigChangeStore.setState((s) => ({ ...s, changes: s.changes + 1 })));
    await waitFor(() => expect(listMutes).toHaveBeenCalledTimes(2));
    expect(result.current.mutes).toHaveLength(1);
  });

  it('reads again once the next mute runs out', async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    listMutes.mockResolvedValueOnce([mute('a', Date.now() + 60_000)]).mockResolvedValue([]);
    const { useMuteLists } = await import('./useMuteLists');
    const { result } = renderHook(() => useMuteLists());
    await waitFor(() => expect(result.current.mutes).toHaveLength(1));

    await act(async () => {
      await vi.advanceTimersByTimeAsync(120_000);
    });
    await waitFor(() => expect(result.current.mutes).toHaveLength(0));
    expect(listMutes).toHaveBeenCalledTimes(2);
  });
});
