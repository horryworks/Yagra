// SPDX-License-Identifier: AGPL-3.0-only
// @vitest-environment jsdom
import { renderHook, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { EventKind, EventRow } from '../../types/api';
import { EVENT_PAGE_SIZE } from './useEventLog';

// Keyset paging over the passive-event firehose. The subtle parts are all here: the cursor is the
// last row's **event time** (not an offset, and not `recorded_at` — the two backends only agree on
// event time), "exhausted" is inferred from a short page, filters are primitives so an inline
// object can't retrigger the reload effect, and concurrent loadMore calls must not double-append.

const listEvents = vi.fn();
vi.mock('../../services/api', () => ({
  api: { listEvents: (opts: unknown) => listEvents(opts) },
}));

// `recorded_at` is deliberately set to a *different* instant from `at_unix_ms`: a cursor built from
// the wrong one is then a visible mismatch rather than an accidental pass.
const row = (id: string, at: string): EventRow =>
  ({
    id,
    at_unix_ms: Date.parse(at),
    recorded_at: '2099-01-01T00:00:00.000Z',
    kind: 'syslog',
    message: 'm',
  }) as unknown as EventRow;

/** A full page (so the hook does not consider itself exhausted). */
const fullPage = (prefix: string): EventRow[] =>
  Array.from({ length: EVENT_PAGE_SIZE }, (_, i) =>
    row(`${prefix}-${i}`, `2026-07-25T00:00:${String(i % 60).padStart(2, '0')}Z`),
  );

describe('useEventLog', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    listEvents.mockResolvedValue([row('e1', '2026-07-25T10:00:00Z')]);
  });
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it('loads the first page and reports a short page as exhausted', async () => {
    const { useEventLog } = await import('./useEventLog');
    const { result } = renderHook(() => useEventLog({}));

    expect(result.current.loading).toBe(true);
    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(result.current.rows).toHaveLength(1);
    expect(result.current.exhausted).toBe(true);
    expect(listEvents).toHaveBeenCalledWith({ limit: EVENT_PAGE_SIZE });
  });

  it('passes only the filters that are set', async () => {
    const { useEventLog } = await import('./useEventLog');
    renderHook(() =>
      useEventLog({ kind: 'trap', node_id: 'n1', matched: false, search: 'link', regex: true }),
    );

    await waitFor(() => expect(listEvents).toHaveBeenCalled());
    expect(listEvents).toHaveBeenCalledWith({
      limit: EVENT_PAGE_SIZE,
      kind: 'trap',
      node_id: 'n1',
      matched: false,
      q: 'link',
      regex: true,
    });
  });

  it('omits `regex` when there is no search term', async () => {
    const { useEventLog } = await import('./useEventLog');
    renderHook(() => useEventLog({ regex: true }));

    await waitFor(() => expect(listEvents).toHaveBeenCalled());
    expect(listEvents).toHaveBeenCalledWith({ limit: EVENT_PAGE_SIZE });
  });

  it('keeps `matched: false` (a meaningful filter) rather than dropping it as falsy', async () => {
    const { useEventLog } = await import('./useEventLog');
    renderHook(() => useEventLog({ matched: false }));

    await waitFor(() => expect(listEvents).toHaveBeenCalled());
    expect(listEvents.mock.calls[0][0]).toHaveProperty('matched', false);
  });

  it('pages with the last row event time as the cursor and appends', async () => {
    const first = fullPage('a');
    listEvents.mockResolvedValueOnce(first).mockResolvedValueOnce([row('b-0', '2026-07-24T00:00:00Z')]);

    const { useEventLog, eventCursor } = await import('./useEventLog');
    const { result } = renderHook(() => useEventLog({}));
    await waitFor(() => expect(result.current.rows).toHaveLength(EVENT_PAGE_SIZE));
    expect(result.current.exhausted).toBe(false);

    result.current.loadMore();
    await waitFor(() => expect(result.current.rows).toHaveLength(EVENT_PAGE_SIZE + 1));
    const last = first[first.length - 1];
    expect(listEvents).toHaveBeenLastCalledWith({
      limit: EVENT_PAGE_SIZE,
      before: eventCursor(last),
    });
    // Explicitly not the ingest time: the backends order by event time, so a `recorded_at` cursor
    // would skip and repeat rows.
    expect(listEvents.mock.calls.at(-1)?.[0].before).not.toBe(last.recorded_at);
    expect(result.current.exhausted).toBe(true);
  });

  it('ignores loadMore once exhausted', async () => {
    const { useEventLog } = await import('./useEventLog');
    const { result } = renderHook(() => useEventLog({}));
    await waitFor(() => expect(result.current.exhausted).toBe(true));

    result.current.loadMore();
    result.current.loadMore();
    expect(listEvents).toHaveBeenCalledTimes(1);
  });

  it('does not issue overlapping loadMore requests', async () => {
    let settle: (v: EventRow[]) => void = () => {};
    listEvents.mockResolvedValueOnce(fullPage('a')).mockImplementationOnce(
      () =>
        new Promise<EventRow[]>((res) => {
          settle = res;
        }),
    );

    const { useEventLog } = await import('./useEventLog');
    const { result } = renderHook(() => useEventLog({}));
    await waitFor(() => expect(result.current.rows).toHaveLength(EVENT_PAGE_SIZE));

    result.current.loadMore();
    result.current.loadMore();
    result.current.loadMore();
    // Second and third are dropped while the first is in flight — otherwise a fast scroll would
    // append the same page several times.
    await waitFor(() => expect(listEvents).toHaveBeenCalledTimes(2));
    settle([]);
  });

  it('reloads from the top when a filter changes', async () => {
    const { useEventLog } = await import('./useEventLog');
    const { rerender } = renderHook(({ kind }: { kind: EventKind }) => useEventLog({ kind }), {
      initialProps: { kind: 'syslog' },
    });
    await waitFor(() => expect(listEvents).toHaveBeenCalledTimes(1));

    rerender({ kind: 'trap' });
    await waitFor(() => expect(listEvents).toHaveBeenCalledTimes(2));
    expect(listEvents).toHaveBeenLastCalledWith({ limit: EVENT_PAGE_SIZE, kind: 'trap' });
  });

  it('does not reload when a re-render changes nothing', async () => {
    const { useEventLog } = await import('./useEventLog');
    const { rerender } = renderHook(() => useEventLog({ kind: 'syslog' }));
    await waitFor(() => expect(listEvents).toHaveBeenCalledTimes(1));

    // Filters are primitives precisely so an inline object at the call site can't retrigger this.
    rerender();
    rerender();
    expect(listEvents).toHaveBeenCalledTimes(1);
  });

  it('reload() refetches with the current filter', async () => {
    const { useEventLog } = await import('./useEventLog');
    const { result } = renderHook(() => useEventLog({ kind: 'syslog' }));
    await waitFor(() => expect(listEvents).toHaveBeenCalledTimes(1));

    result.current.reload();
    await waitFor(() => expect(listEvents).toHaveBeenCalledTimes(2));
    expect(listEvents).toHaveBeenLastCalledWith({ limit: EVENT_PAGE_SIZE, kind: 'syslog' });
  });

  it('leaves the rows intact and stops loading when a fetch fails', async () => {
    listEvents.mockRejectedValueOnce(new Error('503'));
    const { useEventLog } = await import('./useEventLog');
    const { result } = renderHook(() => useEventLog({}));

    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(result.current.rows).toEqual([]);
  });
});
