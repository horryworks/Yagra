// SPDX-License-Identifier: AGPL-3.0-only
// The three things every filtered list's `*Query.ts` module was about to rewrite.
//
// Deliberately NOT a `makeFilterModule<T>()` factory: the screens' `queryFor` functions differ by
// their whole field set, not by a value, so the test extensibility.md §3 poses — "what actually
// differs? an id, a repo handle, some strings" — fails. What generalizes is these three helpers;
// the per-screen module (see `troubleshoot/findingsQuery.ts`) stays hand-written around them.

/**
 * A filter value for the wire: the value itself, or `undefined` when nothing is set.
 *
 * `''` is the one spelling of "no filter" across every screen (`FilterSelect` supplies that option
 * itself, so a caller cannot pick a different sentinel). It must not reach the query string:
 * `services/typedPaths.ts::buildUrl` drops `undefined` but *keeps* `''`, so `severity=` would
 * arrive at the backend as an empty string and be rejected as an unknown severity — a filter nobody
 * set turning into a 400.
 */
export function unset<T extends string>(v: T | '' | null | undefined): T | undefined {
  if (v === null || v === undefined) return undefined;
  const trimmed = v.trim();
  return trimmed === '' ? undefined : (trimmed as T);
}

/**
 * The `since` bound for a relative range, or `undefined` for "all time".
 *
 * `nowMs` is a parameter rather than a `Date.now()` call so a relative range is testable without
 * faking the clock — the shape `findingsQuery.ts` already uses.
 */
export function sinceIso(secs: number | null, nowMs: number): string | undefined {
  return secs === null ? undefined : new Date(nowMs - secs * 1000).toISOString();
}

/**
 * Whether anything is narrowing the list — which picks the empty state's wording.
 *
 * **Derived from the defaults, never written out by hand.** `findingsQuery.ts::isFiltered` spelled
 * the disjunction as an N-term boolean naming every field, and ui-conventions.md names exactly that
 * shape as the dangerous copy: a filter added without its clause makes the screen say "there is
 * nothing here at all" while a filter is hiding the rows. There is no list to forget an entry in
 * once the defaults object is the source.
 *
 * ⚠️ For a **server-side** list this is the only correct discriminator. Keying off `rows.length`
 * works only while everything is loaded — the moment the filter moves into the query, a filtered
 * query that legitimately returns zero is indistinguishable from an empty store.
 */
export function isFiltered<T extends Record<keyof T, string | number | boolean>>(
  filters: T,
  defaults: T,
): boolean {
  return (Object.keys(defaults) as (keyof T)[]).some((k) => filters[k] !== defaults[k]);
}
// The constraint is `Record<keyof T, primitive>` rather than `Record<string, primitive>` on purpose.
// The latter demands an index signature, which an `interface` never has — so every caller would have
// had to declare its filters as a type alias, a papercut with no upside. This spelling accepts both
// while still rejecting a nested object, where `!==` compares references and would report "filtered"
// on every render.
