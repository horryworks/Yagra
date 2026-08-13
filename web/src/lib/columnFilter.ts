// SPDX-License-Identifier: AGPL-3.0-only
// The types and the derived facts behind the column filter row (ADR-053, design-system §4.1).
//
// **Why the state is a flat `Record<string, string>` and not `{ [column]: Condition }`.** The
// obvious nested shape cannot be used: `filterQuery.ts::isFiltered` is the only correct empty-state
// discriminator for a server-side list, it compares with `!==`, and a nested object compares by
// reference — so every render would report "filtered" and all eleven server-side screens would show
// the wrong empty state. The flat shape is forced by that, not chosen for taste. Everything a column
// needs to say therefore has to fit in one primitive string: a comma-joined token set, a prefix-coded
// text condition (`filterCondition.ts`), or a preset name.
//
// The same shape is what makes the URL codec trivial — one key per column, delete on default — and
// what keeps `useEventLog`'s primitive-props contract (an inline object re-fires its reload effect
// every render) intact.

import { isFiltered } from './filterQuery';

/** One column's filter value, keyed by `Column.key`. Always primitive — see the header. */
export type FilterState = Record<string, string>;

/** How a text condition matches. An `as const` array because `i18nEnumKeys.test.ts` iterates it:
 *  the mode labels are built at runtime (`t(\`filter.mode.${m}\`)`), so EN/JA parity alone cannot
 *  prove either locale has them (extensibility.md §4). */
export const TEXT_MODES = ['contains', 'regex'] as const;
export type TextMode = (typeof TEXT_MODES)[number];

/** A choice in a closed set. */
export interface FilterOption {
  value: string;
  label: string;
}

/** A relative-time choice. `seconds: null` is "all time" — the one preset that widens rather than
 *  narrows, which is why `defaultPreset` must not be it on a list whose range default is a
 *  performance contract (`components/EventLog/eventRange.ts`). */
export interface RangePreset extends FilterOption {
  seconds: number | null;
}

// ⚠️ The row accessors are `readValue` / `readText` / `readTime`, and NOT the obvious `valueOf`.
// `valueOf` is inherited from `Object.prototype` by every object literal, so an *optional* accessor
// spelled that way is never absent: `if (!spec.valueOf)` reads the built-in, which is truthy, and
// then calls it — handing the predicate the row object where it expected a timestamp, with no type
// error at the call site and no crash. TypeScript caught it here as an assignability failure on the
// spec literal, one layer away from where it would have gone wrong. Do not rename these back.

/** A column whose values come from a closed set: multi-select with counts. */
export interface EnumFilterSpec<T> {
  kind: 'enum';
  options: readonly FilterOption[];
  /** The row's value(s). An array for a row that legitimately carries several (a tag list); a
   *  single value is the common case. `null`/`undefined` matches nothing once a selection exists. */
  readValue: (row: T) => string | readonly string[] | null | undefined;
  /** Trigger label when nothing is selected ("All kinds"). */
  allLabel: string;
  /** Where the option counts come from. `'client'` counts the rows in the browser (exact and free);
   *  `'server'` asks an endpoint when the popover opens. Absent = no counts at all, which is the
   *  honest answer for a keyset-paged list with no aggregate endpoint. */
  counts?: 'client' | 'server';
}

/** A column matched by free text. */
export interface TextFilterSpec<T> {
  kind: 'text';
  /** Which modes this column offers, in the order they appear. A column the backend cannot run a
   *  regex against ships `['contains']` and the toggle does not render. */
  modes: readonly TextMode[];
  /** Whether the NOT toggle renders. Measured 2026-08-13: negation costs the same as the form it
   *  negates on both backends (ADR-053), so this is about whether *excluding* is meaningful for the
   *  column, never about what it costs. */
  not?: boolean;
  /** The strings this column matches against. `null`/`undefined` parts are simply not candidates. */
  readText: (row: T) => readonly (string | null | undefined)[];
  /** What a plain term means on THIS list. Client-side lists are always `'substring'`. A server-side
   *  list must say which the deployment does — PostgreSQL substrings, VictoriaLogs matches whole
   *  tokens, and the divergence is deliberate and measured (ADR-024). `undefined` = unknown, which
   *  is what an N-1 core produces and which the empty state has to word differently. */
  containsSemantics?: 'substring' | 'token';
  placeholder?: string;
}

/** A column filtered by a relative time window. */
export interface RangeFilterSpec<T> {
  kind: 'range';
  presets: readonly RangePreset[];
  /** The preset that is "no filter" for URL purposes. ⚠️ Not necessarily a *widening* value: the
   *  Events page defaults to 24h precisely because an unbounded default made case-insensitive
   *  search unaffordable. Narrowing-by-default is why the empty state must name the window. */
  defaultPreset: string;
  /** Epoch ms for the row, when the list is filtered in the browser. **A server-side list omits
   *  it** — the range becomes `start`/`end` in the query and the client predicate must not
   *  re-apply it against a clock the server already used. */
  readTime?: (row: T) => number | null | undefined;
}

export type ColumnFilterSpec<T> = EnumFilterSpec<T> | TextFilterSpec<T> | RangeFilterSpec<T>;

/** The shape `filterPredicate` / `filterCounts` walk: a column key paired with its spec. Decoupled
 *  from `Column<T>` so the pure modules never import a component. */
export interface FilterableColumn<T> {
  key: string;
  filter: ColumnFilterSpec<T>;
}

/** Pull the filterable columns out of a `Column<T>[]`-shaped list, narrowing `filter` to non-null. */
export function filterableColumns<T, C extends { key: string; filter?: ColumnFilterSpec<T> }>(
  columns: readonly C[],
): FilterableColumn<T>[] {
  return columns.flatMap((c) => (c.filter ? [{ key: c.key, filter: c.filter }] : []));
}

/** The value a column has when nothing is set. */
export function defaultValue<T>(spec: ColumnFilterSpec<T>): string {
  return spec.kind === 'range' ? spec.defaultPreset : '';
}

/**
 * The whole default state, **derived from the specs**.
 *
 * Never write this object by hand. `filterQuery.ts::isFiltered` documents why: a filter added
 * without its clause in a hand-written defaults object makes the screen say "there is nothing here"
 * while a filter is hiding the rows. There is no list to forget an entry in once this is the source.
 */
export function defaultFilters<T>(columns: readonly FilterableColumn<T>[]): FilterState {
  return Object.fromEntries(columns.map((c) => [c.key, defaultValue(c.filter)]));
}

/** Whether anything narrows the list — the empty state's wording turns on this. */
export function isAnyFiltered<T>(
  columns: readonly FilterableColumn<T>[],
  state: FilterState,
): boolean {
  const defaults = defaultFilters(columns);
  // Read every key through the defaults so a state object missing a key reads as "not set" rather
  // than as `undefined !== ''` = filtered.
  const filled: FilterState = { ...defaults, ...state };
  return isFiltered(filled, defaults);
}

/** How many columns are narrowing — the `フィルタ (N)` badge on mobile, and the clear-all affordance.
 *
 *  ⚠️ One column counts once however many dimensions its condition carries. `EventFilterBar` counts
 *  `regex` as a filter of its own today, so a regex search reads as two. A mode is not a filter. */
export function activeFilterCount<T>(
  columns: readonly FilterableColumn<T>[],
  state: FilterState,
): number {
  return columns.filter((c) => (state[c.key] ?? '') !== defaultValue(c.filter)).length;
}

/** Column keys whose filter is not at its default. */
export function activeFilterKeys<T>(
  columns: readonly FilterableColumn<T>[],
  state: FilterState,
): string[] {
  return columns.filter((c) => (state[c.key] ?? '') !== defaultValue(c.filter)).map((c) => c.key);
}

/** Clear one column, leaving the rest alone. */
export function clearFilter<T>(
  columns: readonly FilterableColumn<T>[],
  state: FilterState,
  key: string,
): FilterState {
  const col = columns.find((c) => c.key === key);
  return { ...state, [key]: col ? defaultValue(col.filter) : '' };
}

/** Query-string keys the list screens already own, which a column key must not collide with.
 *
 *  The column key IS the URL key — no prefix — because the existing screens spell `severity`,
 *  `state` and `q` bare and a prefix would break every bookmark taken before this shipped. The
 *  cost of that choice is this list, and `reservedKeyCollisions` is what makes the cost checkable
 *  instead of a surprise at runtime. */
export const RESERVED_URL_KEYS = [
  'tab',
  'sub',
  'limit',
  'before',
  'after',
  'cursor',
  'page',
  'sort',
  'dir',
  'start',
  'end',
  'from',
  'to',
] as const;

/** Column keys that would fight the page's own query params, plus any duplicated key. Empty is the
 *  only acceptable answer; a test on each screen's spec asserts that. */
export function reservedKeyCollisions<T>(columns: readonly FilterableColumn<T>[]): string[] {
  const bad = new Set<string>();
  const seen = new Set<string>();
  for (const c of columns) {
    if ((RESERVED_URL_KEYS as readonly string[]).includes(c.key)) bad.add(c.key);
    if (seen.has(c.key)) bad.add(c.key);
    seen.add(c.key);
  }
  return [...bad].sort();
}

// ---------------------------------------------------------------------------
// Token sets — the `enum` kind's transport.

/** Split a comma-joined token set. Blank entries are dropped, so `''` decodes to `[]`. */
export function decodeSet(value: string): string[] {
  return value
    .split(',')
    .map((s) => s.trim())
    .filter((s) => s !== '');
}

/**
 * Join a token set for the URL, **in the spec's option order** and deduplicated.
 *
 * Order matters twice and neither is cosmetic: a URL that changes with click order cannot be
 * compared for equality, so a shared link differs from the same view reached by clicking; and the
 * joined string is a `useEffect` dependency key, so a reorder re-fires the fetch.
 */
export function encodeSet(values: readonly string[], order: readonly string[]): string {
  const want = new Set(values);
  return order.filter((v) => want.has(v)).join(',');
}

/** Toggle one token in a set value, returning the new joined value. */
export function toggleSetValue(value: string, token: string, order: readonly string[]): string {
  const cur = new Set(decodeSet(value));
  if (cur.has(token)) cur.delete(token);
  else cur.add(token);
  return encodeSet([...cur], order);
}
