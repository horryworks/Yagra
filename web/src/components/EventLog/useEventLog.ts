// SPDX-License-Identifier: AGPL-3.0-only
// Keyset-paginated passive-event log fetch, shared by the Events page and the NodeDetail
// Events tab. Owns the rows / loading / exhausted state machine and the load-more cursor
// (last row's event time). Filters are passed as PRIMITIVES (kind / node_id / matched) so an
// inline filter object at the call site can't retrigger the reload effect every render.

import { useCallback, useEffect, useRef, useState } from 'react';
import { api } from '../../services/api';
import type { EventRow } from '../../types/api';

export const EVENT_PAGE_SIZE = 100;

/** The `before` cursor for the next (older) page: the row's own **event time**, as RFC 3339.
 *
 *  It used to be `recorded_at` (ingest time), which only worked because the SQL backend also
 *  ordered by ingest time — the VictoriaLogs backend never did, so the two returned different
 *  pages for the same request. Both now order by event time, so the cursor has to be event time
 *  or paging skips and repeats rows. */
export function eventCursor(row: EventRow): string {
  return new Date(row.at_unix_ms).toISOString();
}

export interface EventLogFilter {
  /** One kind or several, comma-joined (ADR-053). */
  kind?: string;
  /** Rule outcomes, comma-joined. */
  action?: string;
  node_id?: string;
  /** Already resolved to a boolean (the UI string ↔ boolean mapping stays at the call site). */
  matched?: boolean;
  /** Free-text search over source (node name / IP) or message.
   *
   *  Matching depends on the backend: substring and case-insensitive on PostgreSQL, whole-token
   *  and case-sensitive on a VictoriaLogs deployment (an inverted word index cannot do either
   *  cheaply — see `logstore::build_filter_part`). `regex` is the escape hatch that behaves the
   *  same on both.
   *
   *  ⚠️ **The filter row does not use this.** It sends `msg`/`src`, which say *which column* the
   *  term is about; `search` remains for MCP and for clients written against it, and the two are
   *  ANDed if both arrive. */
  search?: string;
  /** Interpret `search` as a regular expression (message-only). Case-insensitive on both
   *  backends, and unlike a plain term it matches inside tokens — at a scan cost. */
  regex?: boolean;
  /** The Message column's condition. */
  msg?: string;
  msg_regex?: boolean;
  msg_not?: boolean;
  /** The Source column's condition (source IP or attributed node name). No regex form. */
  src?: string;
  src_not?: boolean;
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
  action,
  node_id,
  matched,
  search,
  regex,
  msg,
  msg_regex,
  msg_not,
  src,
  src_not,
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
      ...(action ? { action } : {}),
      ...(node_id ? { node_id } : {}),
      ...(matched != null ? { matched } : {}),
      ...(search ? { q: search } : {}),
      ...(search && regex ? { regex: true } : {}),
      ...(msg ? { msg } : {}),
      ...(msg && msg_regex ? { msg_regex: true } : {}),
      ...(msg && msg_not ? { msg_not: true } : {}),
      ...(src ? { src } : {}),
      ...(src && src_not ? { src_not: true } : {}),
      ...(start ? { start } : {}),
      ...(end ? { end } : {}),
    }),
    // Primitives only — an inline object here would be a new identity every render and would
    // re-fire the reload effect below on every keystroke anywhere on the page.
    [kind, action, node_id, matched, search, regex, msg, msg_regex, msg_not, src, src_not, start, end],
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
      .listEvents(filterOpts(eventCursor(last)))
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
