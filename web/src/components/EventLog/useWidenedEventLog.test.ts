// SPDX-License-Identifier: AGPL-3.0-only
// @vitest-environment jsdom
//
// The one automatic retry: on a deployment that matches a plain term from the start of a word, a
// term that finds nothing is asked again as a regex so it can match inside words (ADR-053 Inc.2d).
//
// `eventFilterSpec.test.ts` covers the *rule*. This file covers the thing the rule alone cannot:
// **when** it is evaluated. The rule was always right; the defect was that it ran against rows
// belonging to the previous query, so a term that matched perfectly well reported itself widened.
// Reproducing that needs two queries in sequence, which needs the hook.
//
// ⚠️ The reason this is worth a jsdom test rather than trusting the screen: a wrongly widened
// search looks entirely correct. Rows appear, the highlighting agrees with them, and the banner
// explains itself. Nobody looking at the page can tell.

import { renderHook, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { EventRow } from '../../types/api';

const listEvents = vi.fn();
vi.mock('../../services/api', () => ({
  api: { listEvents: (opts: unknown) => listEvents(opts), getSystemHealth: () => Promise.resolve({}) },
}));

const row = (id: string): EventRow =>
  ({ id, at_unix_ms: Date.parse('2026-08-13T10:00:00Z'), kind: 'syslog', message: 'm' }) as unknown as EventRow;

/** What the store answered, keyed by whether the request was the regex (widened) form. */
const answer = (narrow: EventRow[], wide: EventRow[]) => (opts: { msg_regex?: boolean }) =>
  Promise.resolve(opts.msg_regex ? wide : narrow);

describe('useWidenedEventLog', () => {
  beforeEach(() => vi.clearAllMocks());
  afterEach(() => vi.restoreAllMocks());

  it('widens a term that really did find nothing', async () => {
    listEvents.mockImplementation(answer([], [row('a')]));
    const { useWidenedEventLog } = await import('./useEventFilters');

    const { result } = renderHook(() => useWidenedEventLog({ msg: 'ermit' }, 'prefix'));

    await waitFor(() => expect(result.current.widened).toBe(true));
    expect(result.current.rows).toHaveLength(1);
    // Both forms were asked, in that order — the cheap one first is the whole point.
    expect(listEvents.mock.calls.map((c) => c[0].msg_regex)).toEqual([undefined, true]);
  });

  it('does not widen a new term just because the previous one found nothing', async () => {
    // 🚨 The regression. Reported on the test server 2026-08-13: `POLICY` matches `POLICYPERMIT` by
    // prefix, yet the widen banner appeared and the marks landed mid-word. The trigger had been
    // `!loading && rows.length === 0`, and `loading` is only raised *inside* the reload effect — so
    // for one commit after the filter changed, the hook was looking at the previous query's empty
    // result and calling it this query's miss.
    listEvents.mockImplementation(answer([], []));
    const { useWidenedEventLog } = await import('./useEventFilters');

    const { result, rerender } = renderHook(({ msg }) => useWidenedEventLog({ msg }, 'prefix'), {
      initialProps: { msg: 'nothingmatchesthis' },
    });
    await waitFor(() => expect(listEvents).toHaveBeenCalledTimes(2)); // narrow, then widened
    listEvents.mockClear();

    // Now the operator types a term that does match. The first request for it must be the narrow
    // one; widening before an answer arrives would search a broader question than they asked.
    listEvents.mockImplementation(answer([row('p')], [row('p'), row('q')]));
    rerender({ msg: 'POLICY' });

    await waitFor(() => expect(result.current.rows).toHaveLength(1));
    expect(listEvents.mock.calls.every((c) => !c[0].msg_regex)).toBe(true);
    expect(result.current.widened).toBe(false);
  });

  it('settles instead of oscillating when the widened form also finds nothing', async () => {
    listEvents.mockImplementation(answer([], []));
    const { useWidenedEventLog } = await import('./useEventFilters');

    const { result } = renderHook(() => useWidenedEventLog({ msg: 'zzz' }, 'prefix'));

    await waitFor(() => expect(listEvents).toHaveBeenCalledTimes(2));
    // Held for a while: a retry keyed on anything that changes per render would keep firing here.
    await new Promise((r) => setTimeout(r, 50));
    expect(listEvents).toHaveBeenCalledTimes(2);
    // No rows, so the screen must not claim a widened result — it shows the prefix-miss empty state.
    expect(result.current.widened).toBe(false);
  });

  it('never widens on a substring deployment, however empty the result', async () => {
    listEvents.mockImplementation(answer([], [row('a')]));
    const { useWidenedEventLog } = await import('./useEventFilters');

    const { result } = renderHook(() => useWidenedEventLog({ msg: 'ermit' }, 'substring'));

    await waitFor(() => expect(listEvents).toHaveBeenCalledTimes(1));
    await new Promise((r) => setTimeout(r, 50));
    expect(listEvents).toHaveBeenCalledTimes(1);
    expect(result.current.widened).toBe(false);
  });
});
