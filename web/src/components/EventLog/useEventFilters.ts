// SPDX-License-Identifier: AGPL-3.0-only
// The two things both Events screens need from the server to make their filter row honest: how a
// plain term matches here, and the per-value counts a multi-select shows.
//
// Both are deliberately *lazy*. ADR-023 puts UI load third, behind polling and alert evaluation,
// and neither of these is on the path to seeing the log.

import { useCallback, useRef, useState } from 'react';
import type { TFunction } from 'i18next';
import { api } from '../../services/api';
import { clearFilter, type FilterState, type FilterableColumn } from '../../lib/columnFilter';
import { eventFilterQuery, EVENT_FILTER_KEYS, type SearchSemantics } from './eventFilterSpec';
import type { EventRow } from '../../types/api';

/** Column names for the mobile sheet, which has room for them where a 90px grid track does not.
 *  Read off the same keys the columns use, so a column added to one is missing from neither. */
export function eventColumnLabels(t: TFunction): Record<string, string> {
  const header: Record<(typeof EVENT_FILTER_KEYS)[number], string> = {
    kind: 'alerts:eventLog.cols.kind',
    source: 'alerts:eventLog.cols.source',
    message: 'alerts:eventLog.cols.message',
    action: 'alerts:eventLog.cols.result',
    at: 'alerts:eventLog.cols.when',
  };
  return Object.fromEntries(EVENT_FILTER_KEYS.map((k) => [k, t(header[k])]));
}

/**
 * How a plain search term matches on this deployment, fetched once per browser session.
 *
 * Memoized at module scope rather than per component: the answer is a property of the deployment,
 * it cannot change without a core restart, and `/system-health` pings every backing store — asking
 * again on each navigation to Events would be a health probe per page view.
 *
 * `undefined` survives as a real answer. An N-1 core does not report the field, and axum drops
 * unknown query parameters **silently**, so on that core the new filters would appear to work and
 * quietly do nothing. The screens use the absence to say less rather than to guess.
 */
let semanticsPromise: Promise<SearchSemantics> | null = null;

export function loadSearchSemantics(): Promise<SearchSemantics> {
  semanticsPromise ??= api
    .getSystemHealth()
    .then((h) => (h as { search_semantics?: SearchSemantics }).search_semantics)
    .catch(() => undefined);
  return semanticsPromise;
}

/** Test seam: forget the memoized answer. */
export function resetSearchSemantics(): void {
  semanticsPromise = null;
}

export function useSearchSemantics(): SearchSemantics {
  const [value, setValue] = useState<SearchSemantics>(undefined);
  const started = useRef(false);
  if (!started.current) {
    started.current = true;
    void loadSearchSemantics().then(setValue);
  }
  return value;
}

/** Facet counts per column, and the loader a popover calls when it opens. */
export interface EventFacets {
  counts: Record<string, Record<string, number>>;
  load: (key: string) => void;
}

/**
 * Per-value counts for the enum columns, fetched when a popover opens.
 *
 * ⚠️ **A column's counts exclude that column's own filter.** Selecting `syslog` must not make
 * `trap` read `0`; an autofilter answers "how many would I get if I picked this *instead*", and the
 * version that looks right while being wrong is the one that passes its own filter through. The
 * exclusion lives in `lib/filterCounts.ts` for client-side lists and here for the server-side ones,
 * where it is `clearFilter` before building the query rather than a second pass over rows.
 */
export function useEventFacets(
  columns: readonly FilterableColumn<EventRow>[],
  state: FilterState,
  nowMs: number,
  extra?: { node_id?: string },
): EventFacets {
  const [counts, setCounts] = useState<Record<string, Record<string, number>>>({});
  // The current inputs, read through a ref so `load` keeps a stable identity — it is passed to
  // every filter cell, and a new function each render would be a new prop on all of them.
  const latest = useRef({ columns, state, nowMs, extra });
  latest.current = { columns, state, nowMs, extra };

  const load = useCallback((key: string) => {
    if (key !== 'kind' && key !== 'action') return; // the two dimensions /events/stats groups by
    const cur = latest.current;
    const without = clearFilter(cur.columns, cur.state, key);
    api
      .getEventStats(key, { ...eventFilterQuery(without, cur.nowMs), ...cur.extra, limit: 50 })
      .then((buckets) => {
        const next: Record<string, number> = {};
        for (const b of buckets) if (b.key) next[b.key] = b.count;
        setCounts((prev) => ({ ...prev, [key]: next }));
      })
      .catch(() => undefined);
  }, []);

  return { counts, load };
}
