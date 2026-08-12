// SPDX-License-Identifier: AGPL-3.0-only
// Unit tests for alert-history keyset paging (no DOM — Vitest node env).

import { describe, expect, it } from 'vitest';
import type { AlertHistoryRow } from '../types/api';
import { appendPage, nextCursor, PAGE_SIZE } from './historyCursor';

/** A row carrying only the fields paging reads. */
const row = (id: string, recorded_at: string) => ({ id, recorded_at }) as AlertHistoryRow;

/** `n` rows that all share one `recorded_at` — what one flush of alerts actually looks like. */
const flush = (n: number, at: string, from = 0) =>
  Array.from({ length: n }, (_, i) => row(`r${from + i}`, at));

describe('nextCursor', () => {
  it('takes both halves from the same row', () => {
    const rows = [...flush(PAGE_SIZE - 1, 't1'), row('last', 't2')];
    expect(nextCursor(rows)).toEqual({ before: 't2', before_id: 'last' });
  });

  it('stops on a short page', () => {
    expect(nextCursor(flush(PAGE_SIZE - 1, 't1'))).toBeNull();
    expect(nextCursor([])).toBeNull();
  });

  it('carries the id even when the whole page shares one timestamp', () => {
    // The case the composite cursor exists for. A fleet-wide event writes its alerts in one
    // transaction, and PostgreSQL's now() is the transaction timestamp — so every row of that
    // flush has an identical recorded_at. With the timestamp alone the next request would ask for
    // rows strictly older than it and skip every sibling still unread.
    const c = nextCursor(flush(PAGE_SIZE, '2026-08-12T01:00:00Z'));
    expect(c).toEqual({ before: '2026-08-12T01:00:00Z', before_id: `r${PAGE_SIZE - 1}` });
    expect(c?.before_id).toBeTruthy();
  });
});

describe('appendPage', () => {
  it('appends in order', () => {
    expect(appendPage([row('a', 't1')], [row('b', 't2')]).map((r) => r.id)).toEqual(['a', 'b']);
  });

  it('drops a row already held', () => {
    const have = [row('a', 't1'), row('b', 't1')];
    expect(appendPage(have, [row('b', 't1'), row('c', 't1')]).map((r) => r.id)).toEqual([
      'a',
      'b',
      'c',
    ]);
  });

  it('keeps rows that differ only by id', () => {
    // Rows from one flush are identical except for the id, so a dedup keyed on anything else would
    // throw away real transitions.
    const have = flush(2, 't1');
    const page = flush(2, 't1', 2);
    expect(appendPage(have, page)).toHaveLength(4);
  });
});
