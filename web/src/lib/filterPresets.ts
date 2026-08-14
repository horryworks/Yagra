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

/**
 * Every relative window any list offers, and **the only place their lengths are written down**
 * (ADR-053 Inc.10, 決定 X).
 *
 * ⚠️ There were four copies of this arithmetic before Inc.10 — here, `eventRange.ts`,
 * `auditQuery.ts` and `historyQuery.ts` — which is the exact failure this module's header opens by
 * naming. Worth recording *why* it happened, because the cause was not carelessness:
 *
 * A server-side range column writes `readTime: undefined`, because a browser-side pass must not
 * re-apply a window the query already used. Each of those three screens *also* wrote
 * `seconds: null` on every preset, as a second belt for the same trouser. But
 * `filterPredicate.ts` returns at `if (!spec.readTime)` — **before it ever looks at `seconds`** —
 * so the second guard was inert. Its only real effect was to make the spec unable to state the
 * window, which forced each screen to keep a private table for its query to read.
 *
 * ⇒ **A "just in case" null is not free: it takes the value's home away from it.** Before adding
 * a defensive default, read the consumer and check whether the first guard already returned.
 */
export const RANGE_TOKENS = ['24h', '7d', '30d', '90d', 'all', 'custom'] as const;
export type RangeToken = (typeof RANGE_TOKENS)[number];

/** Seconds per window; `null` means "no lower bound". `custom` is `null` because its two instants
 *  are typed by the operator and carried inside the column's value, not derived from a length. */
const SECONDS: Record<RangeToken, number | null> = {
  '24h': DAY,
  '7d': 7 * DAY,
  '30d': 30 * DAY,
  '90d': 90 * DAY,
  all: null,
  custom: null,
};

/**
 * How far back a window reaches, in seconds, or `null` for "no lower bound".
 *
 * Typed against [`RangeToken`] rather than `string` on purpose: a screen's own range union is a
 * subset of it, so passing one is checked at compile time and an unknown token cannot reach here.
 * A `string` parameter would have to answer *something* for a typo, and the only available answer
 * is `null` — which widens the query to all time. On Events that is the difference between a 24h
 * search and a ten-second one (`eventRange.ts`).
 */
export function rangeSeconds(token: RangeToken): number | null {
  return SECONDS[token];
}

/** Localized presets for a screen's chosen subset of the windows.
 *
 *  `Record<RangeToken, …>` above is what makes a new window a compile error rather than a preset
 *  that renders its own key (extensibility.md §1), and `satisfies` on each screen's array is what
 *  keeps the subset relation checked in the other direction. */
export function rangePresets<T extends RangeToken>(
  values: readonly T[],
  t: TFunction,
  prefix: string,
): RangePreset[] {
  return values.map((value) => ({
    value,
    label: t(`${prefix}${value}`),
    seconds: SECONDS[value],
  }));
}

/** The relative windows a client-side list offers, newest-first, ending in "all time".
 *
 *  ⚠️ The default preset is the caller's decision, not this module's, and it is not always `all`.
 *  A list whose rows come from a bounded query (Events) treats its window as a performance
 *  contract; a list that is fully in the browser (API tokens) has no such constraint and defaults
 *  to `all` because narrowing by default would hide rows nobody asked to hide. */
export const CLIENT_RANGES = ['24h', '7d', '30d', '90d', 'all'] as const satisfies readonly RangeToken[];
export type ClientRange = (typeof CLIENT_RANGES)[number];

/** The presets a client-side list offers, localized. */
export function clientRangePresets(t: TFunction): RangePreset[] {
  return rangePresets(CLIENT_RANGES, t, 'common:filter.range.');
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
    // `>=`, not `>`: the check runs after the insert, so `>` let the 201st value in and returned a
    // list one longer than the constant naming it. Nothing broke — it is one extra checkbox — but a
    // cap that does not hold at its own number is a cap nobody can reason about.
    if (seen.size >= MAX_DISCOVERED_OPTIONS) break;
  }
  return [...seen]
    .sort((a, b) => a.localeCompare(b))
    .map((value) => ({ value, label: label ? label(value) : value }));
}
