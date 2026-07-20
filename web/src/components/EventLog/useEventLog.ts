// SPDX-License-Identifier: AGPL-3.0-only
// Keyset-paginated passive-event log fetch, shared by the Events page and the NodeDetail
// Events tab. Owns the rows / loading / exhausted state machine and the load-more cursor
// (last row's recorded_at). Filters are passed as PRIMITIVES (kind / node_id / matched) so an
// inline filter object at the call site can't retrigger the reload effect every render.

import { useCallback, useEffect, useRef, useState } from 'react';
import { api } from '../../services/api';
import type { EventKind, EventRow } from '../../types/api';

export const EVENT_PAGE_SIZE = 100;

export interface EventLogFilter {
  kind?: EventKind;
  node_id?: string;
  /** Already resolved to a boolean (the UI string ↔ boolean mapping stays at the call site). */
  matched?: boolean;
  /** Free-text search. Substring over source (node name / IP) or message, or — when `regex` is
   *  set — a regular expression matched against the message only. */
  search?: string;
  /** Interpret `search` as a regular expression (message-only) rather than a substring. */
  regex?: boolean;
  /** Time-range lower bound (inclusive, RFC 3339), or undefined for unbounded. */
  start?: string;
  /** Time-range upper bound (inclusive, RFC 3339), or undefined for unbounded. */
  end?: string;
}

export interface EventLog {
  rows: EventRow[];
  loading: boolean;
  exhausted: boolean;
  loadMore: () => void;
  /** Force a reload from the top (keeps the current filter). */
  reload: () => void;
}

export function useEventLog({
  kind,
  node_id,
  matched,
  search,
  regex,
  start,
  end,
}: EventLogFilter): EventLog {
  const [rows, setRows] = useState<EventRow[]>([]);
  const [loading, setLoading] = useState(true);
  const [exhausted, setExhausted] = useState(false);
  const [nonce, setNonce] = useState(0);
  const loadingMore = useRef(false);

  const filterOpts = useCallback(
    (before?: string) => ({
      limit: EVENT_PAGE_SIZE,
      ...(before ? { before } : {}),
      ...(kind ? { kind } : {}),
      ...(node_id ? { node_id } : {}),
      ...(matched != null ? { matched } : {}),
      ...(search ? { q: search } : {}),
      ...(search && regex ? { regex: true } : {}),
      ...(start ? { start } : {}),
      ...(end ? { end } : {}),
    }),
    [kind, node_id, matched, search, regex, start, end],
  );

  // Reload from the top whenever a filter changes (or reload() bumps the nonce).
  useEffect(() => {
    setLoading(true);
    api
      .listEvents(filterOpts())
      .then((page) => {
        setRows(page);
        setExhausted(page.length < EVENT_PAGE_SIZE);
      })
      .catch(() => undefined)
      .finally(() => setLoading(false));
  }, [filterOpts, nonce]);

  const loadMore = useCallback(() => {
    if (loadingMore.current || exhausted) return;
    const last = rows[rows.length - 1];
    if (!last) return;
    loadingMore.current = true;
    api
      .listEvents(filterOpts(last.recorded_at))
      .then((page) => {
        setRows((cur) => [...cur, ...page]);
        setExhausted(page.length < EVENT_PAGE_SIZE);
      })
      .catch(() => undefined)
      .finally(() => {
        loadingMore.current = false;
      });
  }, [rows, exhausted, filterOpts]);

  const reload = useCallback(() => setNonce((n) => n + 1), []);
  return { rows, loading, exhausted, loadMore, reload };
}
