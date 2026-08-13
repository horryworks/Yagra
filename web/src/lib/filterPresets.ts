// SPDX-License-Identifier: AGPL-3.0-only
// The filter-row pieces that are the same on every screen (ADR-053 Inc.3/Inc.4).
//
// Inc.2 built one screen's filters, and one screen cannot tell you what generalizes. Thirteen can:
// almost every list wants "the last day / week / month", and every one of them would otherwise
// declare its own preset array with its own seconds arithmetic and its own labels. That is the
// shape `extensibility.md` opens with — a fact repeated in N places ends up repeated in N-1 — and
// the failure here is quiet, because a screen whose `7d` is 7×24×60×60 minus a typo still filters,
// just to the wrong window.
//
// What is deliberately NOT here: the specs themselves. A column's `readValue`/`readText` is the one
// thing that genuinely differs per screen, which is `filterQuery.ts`'s standing argument and the
// reason `buildPredicate` takes accessors rather than a table name.

import type { TFunction } from 'i18next';
import type { FilterOption, RangePreset } from './columnFilter';

const DAY = 24 * 60 * 60;

/** The relative windows a client-side list offers, newest-first, ending in "all time".
 *
 *  ⚠️ The default preset is the caller's decision, not this module's, and it is not always `all`.
 *  A list whose rows come from a bounded query (Events) treats its window as a performance
 *  contract; a list that is fully in the browser (API tokens) has no such constraint and defaults
 *  to `all` because narrowing by default would hide rows nobody asked to hide. */
export const CLIENT_RANGES = ['24h', '7d', '30d', '90d', 'all'] as const;
export type ClientRange = (typeof CLIENT_RANGES)[number];

const SECONDS: Record<ClientRange, number | null> = {
  '24h': DAY,
  '7d': 7 * DAY,
  '30d': 30 * DAY,
  '90d': 90 * DAY,
  all: null,
};

/** The presets, localized. `Record<ClientRange, …>` above is what makes a new window a compile
 *  error rather than a preset that renders its own key (extensibility.md §1). */
export function clientRangePresets(t: TFunction): RangePreset[] {
  return CLIENT_RANGES.map((value) => ({
    value,
    label: t(`common:filter.range.${value}`),
    seconds: SECONDS[value],
  }));
}

/** Options for a column whose values are an `as const` enum with `t()` labels under one prefix.
 *
 *  The array is the source, so a backend variant added without its label shows up in
 *  `i18nEnumKeys.test.ts` rather than as a checkbox captioned with its own key. */
export function enumOptions<T extends string>(
  values: readonly T[],
  t: TFunction,
  prefix: string,
): FilterOption[] {
  return values.map((value) => ({ value, label: t(`${prefix}${value}`) }));
}

/** Options for a column whose values are discovered from the rows rather than declared — a profile
 *  name, a destination host, a rule id.
 *
 *  Sorted, deduplicated, and capped: an unbounded value space must not become a value list
 *  (ADR-053 decision 6), and a column that would exceed the cap is one whose spec should be a text
 *  condition instead. The cap returning a *short* list rather than a wrong one is the point —
 *  `MultiSelectList` has its own search box, so a long list is usable, but a list of ten thousand
 *  distinct strings is a different control. */
export const MAX_DISCOVERED_OPTIONS = 200;

export function discoveredOptions<T>(
  rows: readonly T[],
  read: (row: T) => string | null | undefined,
  label?: (value: string) => string,
): FilterOption[] {
  const seen = new Set<string>();
  for (const r of rows) {
    const v = read(r);
    if (v != null && v !== '') seen.add(v);
    if (seen.size > MAX_DISCOVERED_OPTIONS) break;
  }
  return [...seen]
    .sort((a, b) => a.localeCompare(b))
    .map((value) => ({ value, label: label ? label(value) : value }));
}
