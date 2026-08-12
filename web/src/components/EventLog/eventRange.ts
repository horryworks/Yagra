// SPDX-License-Identifier: AGPL-3.0-only
// The event log's time range: the presets it offers, and the bounds each one becomes.
//
// ⚠️ **The default is bounded, and that is load-bearing rather than a preference.** The free-text
// search runs against VictoriaLogs where a case-insensitive term costs ~1.1× a case-sensitive one
// over 24 hours and ~10× — 9.4 seconds on 6.7M events — with no lower bound at all. Case-insensitive
// search is what an operator actually wants (a device that logs `SSH` should be found by `ssh`), so
// the range is what pays for it. Widening the default back to "all time" would silently turn every
// search on a busy deployment into a ten-second wait. See
// `logstore::a_plain_term_stays_a_phrase_filter_not_a_regex_scan` for the measurements.
//
// Here rather than in `EventFilterBar.tsx` because Vitest runs `environment: 'node'` and never
// executes a `.tsx` test (testing.md).

import { sinceIso } from '../../lib/filterQuery';

/** The presets, in the order the dropdown lists them. `custom` reveals the From/To inputs. */
export const EVENT_RANGES = ['24h', '7d', '30d', 'all', 'custom'] as const;
export type EventRange = (typeof EVENT_RANGES)[number];

/** The default view. Bounded — see the file header for why that is not negotiable. */
export const DEFAULT_EVENT_RANGE: EventRange = '24h';

const RANGE_SECS: Record<EventRange, number | null> = {
  '24h': 86_400,
  '7d': 7 * 86_400,
  '30d': 30 * 86_400,
  all: null,
  custom: null,
};

/** The bounds a filter sends: RFC 3339, or `undefined` for an unbounded side. */
export interface EventBounds {
  start: string | undefined;
  end: string | undefined;
}

/**
 * The bounds for a preset, or the operator's own two instants under `custom`.
 *
 * `nowMs` is a parameter so a relative range is testable without faking the clock — and callers
 * must resolve it **once, when the preset is chosen**, not per request. A lower bound recomputed on
 * every "load older" page would creep forward between pages and quietly drop rows the keyset cursor
 * was walking towards, which is the same class of bug the composite history cursor fixed.
 */
export function boundsFor(
  range: EventRange,
  custom: { from?: string; to?: string },
  nowMs: number,
): EventBounds {
  if (range === 'custom') {
    return { start: custom.from || undefined, end: custom.to || undefined };
  }
  return { start: sinceIso(RANGE_SECS[range], nowMs), end: undefined };
}

/** Whether the range is narrowing anything — for the mobile toggle's "N filters applied" badge.
 *
 *  `all` is the only range that narrows nothing. The default counts as a filter on purpose: it is
 *  hiding older events, and a badge that said "no filters" while a day-old event was missing would
 *  be exactly the lie the empty-state rule is about. */
export function rangeIsNarrowing(range: EventRange, custom: { from?: string; to?: string }): boolean {
  return range === 'custom' ? Boolean(custom.from || custom.to) : range !== 'all';
}
