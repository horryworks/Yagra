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
   *  single value is the common case. `null`/`undefined` matches nothing once a selection exists.
   *
   *  **Optional, and omitting it means "this column is answered by the server"** — the same rule
   *  `RangeFilterSpec.readTime` and `NumberFilterSpec.readNumber` follow, and `buildPredicate` then
   *  compiles the column to `null` rather than testing rows against a value it cannot read.
   *
   *  ⚠️ It was required until ADR-053 Inc.8, and the trade is worth stating: a **client-side** screen
   *  that now forgets it stops filtering silently instead of failing to compile. It was made optional
   *  because the alternative was worse — the Flow tab's drill-downs narrow six *aggregate queries*
   *  and have no row to read at all, so the only way to satisfy a required accessor was one that
   *  returns `null`, i.e. a predicate that rejects every row. A missing filter is a bug; a lying
   *  accessor is a screen that goes blank. If you are writing a client-side enum column, supply it. */
  readValue?: (row: T) => string | readonly string[] | null | undefined;
  /** Trigger label when nothing is selected ("All kinds"). */
  allLabel: string;
  /** Where the option counts come from. `'client'` counts the rows in the browser (exact and free);
   *  `'server'` asks an endpoint when the popover opens. Absent = no counts at all, which is the
   *  honest answer for a keyset-paged list with no aggregate endpoint. */
  counts?: 'client' | 'server';
}

// There used to be a `single?: boolean` here, and it is worth saying why it is gone rather than
// leaving the next person to wonder whether multi-select is safe on a server-side list.
//
// It existed because four endpoints — alert history, audit, thresholds, saved findings — took
// exactly one `severity` / `state` / `action`, so a multi-select over them would have let an
// operator tick three boxes and send one: rows missing from a filtered list with nothing on screen
// saying so, the worst shape a filter bug takes. `single` made the control tell the truth about the
// API instead of hiding it.
//
// ADR-053 Inc.4b widened all four endpoints to comma-joined sets, which is the *right* fix — the
// control was never the problem. The flag is deleted rather than kept "in case": an unused option
// on a shared type is an invitation to reach for it instead of widening the next endpoint, which is
// exactly the trade this increment decided against.

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
  containsSemantics?: 'substring' | 'prefix';
  placeholder?: string;
}

/** A column filtered by a relative time window. */
export interface RangeFilterSpec<T> {
  kind: 'range';
  presets: readonly RangePreset[];
  /** Whether the [`CUSTOM_RANGE`] preset reveals two absolute instants. A column without it offers
   *  presets only, which is all a client-side list has ever needed. */
  custom?: boolean;
  /** The preset that is "no filter" for URL purposes. ⚠️ Not necessarily a *widening* value: the
   *  Events page defaults to 24h precisely because an unbounded default made case-insensitive
   *  search unaffordable. Narrowing-by-default is why the empty state must name the window. */
  defaultPreset: string;
  /** Epoch ms for the row, when the list is filtered in the browser. **A server-side list omits
   *  it** — the range becomes `start`/`end` in the query and the client predicate must not
   *  re-apply it against a clock the server already used. */
  readTime?: (row: T) => number | null | undefined;
}

/** A column filtered by a numeric interval, **inclusive at both ends** (ADR-053 Inc.6 decision G).
 *
 *  Either end may be left open, which is the common case: "score 8 or worse" is one bound, not a
 *  window. The transport is one string (`encodeNumberRange`), because `FilterState` is flat — see
 *  the header — and a column that needed `score_min` + `score_max` would be the first to break it. */
export interface NumberFilterSpec<T> {
  kind: 'number';
  /** The row's value, when the list is filtered in the browser. **A server-side list omits it**,
   *  exactly as `RangeFilterSpec.readTime` is omitted: the bounds go into the query and a second
   *  browser-side pass would narrow a page the server already narrowed. */
  readNumber?: (row: T) => number | null | undefined;
  /** Bounds and granularity for the two inputs. Advisory — the browser enforces them on the
   *  spinner, and the codec does not, so a pasted URL outside them still reads back. */
  min?: number;
  max?: number;
  step?: number;
  /** Short unit shown after each input ("%", "ms"). Not part of the value. */
  unit?: string;
}

/** A column narrowed to a set of **exact values the operator types** (ADR-053 Inc.8, decision P).
 *
 * The fifth kind exists because the Flow tab's drill-downs fit none of the other four, and each
 * near-miss is a lie rather than an inconvenience:
 *
 *  - `enum` needs a closed option list. Ports, peer addresses and ASNs have no vocabulary — there
 *    are 65,536 ports and every IPv4 address is a candidate.
 *  - `text` is substring/regex, so `80` would offer to match `8080`. The backend matches the port
 *    **exactly**, so the control would describe a filter the store does not run.
 *  - `number` is an interval. "80 and 443" is not a range, and a range over ports means nothing an
 *    operator asks for.
 *
 * Transport is the comma-joined set the `enum` kind already uses, so the URL codec, `ClearFilters`
 * and the mobile sheet need no new spelling. What differs is that the tokens are **produced by
 * [`parse`], not chosen from a list** — which is also where validation lives, so an unparseable
 * token never reaches the query. That matters more than usual here: the flow store interpolates
 * these into SQL after re-parsing them server-side, and the API refuses what it cannot parse.
 */
export interface ValuesFilterSpec<T> {
  kind: 'values';
  /** Normalize one typed token, or return `null` to drop it. Runs on commit, never per keystroke. */
  parse: (token: string) => string | null;
  /** How a stored token reads in the trigger (`6` → `TCP`, `0` → `Unknown AS`). Identity if absent. */
  format?: (token: string) => string;
  /** The row's value(s), when the list is filtered in the browser. **A server-side column omits
   *  it** — the same rule `RangeFilterSpec.readTime` and `NumberFilterSpec.readNumber` follow. */
  readValues?: (row: T) => readonly string[] | string | null | undefined;
  placeholder?: string;
  /** Ceiling on how many values may be set. ⚠️ Mirror the API's own cap — a control that accepts
   *  more than the endpoint does turns a filter into a 400 the operator cannot see coming. */
  max?: number;
}

export type ColumnFilterSpec<T> =
  | EnumFilterSpec<T>
  | TextFilterSpec<T>
  | RangeFilterSpec<T>
  | NumberFilterSpec<T>
  | ValuesFilterSpec<T>;

/** The shape `filterPredicate` / `filterCounts` walk: a column key paired with its spec. Decoupled
 *  from `Column<T>` so the pure modules never import a component. */
export interface FilterableColumn<T> {
  key: string;
  filter: ColumnFilterSpec<T>;
}

/** Pull the filterable columns out of a `Column<T>[]`-shaped list, narrowing `filter` to non-null.
 *
 *  ⚠️ `T` is inferred from the `filter` property, not from a second type parameter. It was written
 *  the other way first (`<T, C extends {…}>`) and every call site had to name both or neither, so
 *  TypeScript picked `unknown` for `T` and rejected the columns it was handed. */
export function filterableColumns<T>(
  columns: readonly { key: string; filter?: ColumnFilterSpec<T> }[],
): FilterableColumn<T>[] {
  return columns.flatMap((c) => (c.filter ? [{ key: c.key, filter: c.filter }] : []));
}

/** The value a column has when nothing is set. */
/**
 * Turn a screen's `Record<columnKey, spec>` into filterable columns.
 *
 * A page never needs this — it attaches each spec to the real column it belongs under, which is
 * what keeps the filter cell aligned with its header. This exists so a **test** can run a screen's
 * specs through `filterPredicate.ts` without inventing a fake column list, which four spec tests
 * had each started doing with a different cast (ADR-053 Inc.5).
 */
export function specColumns<T>(
  specs: Record<string, ColumnFilterSpec<T>>,
): FilterableColumn<T>[] {
  return Object.entries(specs).map(([key, filter]) => ({ key, filter }));
}

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
 *  ⚠️ One column counts once however many dimensions its condition carries. The Events toolbar this
 *  replaced counted `regex` as a filter of its own, so a regex search read as two. A mode is not a
 *  filter. */
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
// The URL codec. One key per column, deleted at its default.

/** Read the filter state out of a query string, defaulting anything absent.
 *
 *  A value this build does not understand is left as-is rather than rejected — each kind's own
 *  decoder falls back (an unknown token drops out of a set, an unknown preset becomes the default),
 *  which is `readEnumParam`'s rule and the opposite of the API edge's. A stale bookmark must show
 *  the default view, never a 400 and never a control displaying a value it does not offer. */
export function readFilterParams<T>(
  columns: readonly FilterableColumn<T>[],
  params: URLSearchParams,
): FilterState {
  const out = defaultFilters(columns);
  for (const c of columns) {
    const v = params.get(c.key);
    if (v !== null) out[c.key] = v;
  }
  return out;
}

/** Write the filter state into a query string, **deleting** anything at its default.
 *
 *  So a bare URL is always the default view and a `?` always means something is narrowing the list.
 *  Mutates `params` in place, like `writeIdParam`; the caller still owes
 *  `setSearchParams(params, { replace: true })` or every keystroke becomes a history entry. */
export function writeFilterParams<T>(
  columns: readonly FilterableColumn<T>[],
  params: URLSearchParams,
  next: FilterState,
): void {
  for (const c of columns) {
    const value = next[c.key] ?? '';
    if (value === defaultValue(c.filter)) params.delete(c.key);
    else params.set(c.key, value);
  }
}

// ---------------------------------------------------------------------------
// Ranges — the `range` kind's transport.

/** The preset that means "an absolute window the operator typed", rather than a relative one. */
export const CUSTOM_RANGE = 'custom';

/** The part of a range spec the codec reads. Named separately, and *not* `RangeFilterSpec<T>`,
 *  because the codec has nothing to say about the row type — asking for the full spec would force
 *  every caller to name a `T` it does not have, or to cast one away. */
export interface RangeShape {
  presets: readonly RangePreset[];
  defaultPreset: string;
  custom?: boolean;
}

/** A range column's decoded value: which preset, and the two instants when it is the custom one. */
export interface RangeValue {
  preset: string;
  /** `<input type="datetime-local">` values (local wall clock), empty when that side is unbounded. */
  from: string;
  to: string;
}

/**
 * Encode a range selection into the column's single primitive value.
 *
 * The instants ride **inside** the one value rather than in sibling `from`/`to` query keys. Two
 * reasons, and the second is the load-bearing one: `FilterState` is flat by construction (see the
 * header), so a column that needed three keys would be the first to break that; and `from`/`to` are
 * already in `RESERVED_URL_KEYS`, which exists because pages own those names.
 */
export function encodeRange(v: RangeValue): string {
  if (v.preset !== CUSTOM_RANGE) return v.preset;
  return v.from || v.to ? `${CUSTOM_RANGE}:${v.from}|${v.to}` : CUSTOM_RANGE;
}

/** Decode a range column's value, falling back to the spec's default for anything unrecognised —
 *  a stale bookmark from a build with different presets must land on the default view, never on a
 *  control showing a value it does not offer (`filterParams.ts::readEnumParam`'s rule). */
export function decodeRange(raw: string, spec: RangeShape): RangeValue {
  const value = raw || spec.defaultPreset;
  if (spec.custom && (value === CUSTOM_RANGE || value.startsWith(`${CUSTOM_RANGE}:`))) {
    const [from = '', to = ''] = value.slice(CUSTOM_RANGE.length + 1).split('|');
    return { preset: CUSTOM_RANGE, from, to };
  }
  const known = spec.presets.some((p) => p.value === value);
  return { preset: known ? value : spec.defaultPreset, from: '', to: '' };
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

// ---------------------------------------------------------------------------
// Numeric intervals — the `number` kind's transport.

/** A decoded numeric interval. `null` on a side means that side is unbounded. */
export interface NumberRange {
  min: number | null;
  max: number | null;
}

/**
 * Encode a numeric interval as `min:max`, dropping the key entirely when both ends are open.
 *
 * ⚠️ **Deliberately not the `range` kind's `custom:from|to` spelling.** That one separates its two
 * instants with `|`; this one uses `:`. They look close enough that sharing a codec is tempting, and
 * they are not the same thing — a range column has presets and a default that narrows, a number
 * column has neither — so a shared codec would have to carry a preset concept for one caller. The
 * separator differing is the reminder.
 */
export function encodeNumberRange(v: NumberRange): string {
  if (v.min === null && v.max === null) return '';
  return `${v.min ?? ''}:${v.max ?? ''}`;
}

// ---------------------------------------------------------------------------
// Typed value sets — the `values` kind's transport. The wire form is the `enum` kind's comma-joined
// set; what is new is turning what the operator typed into that form.

/**
 * Normalize a typed list into the stored set: parse each token, drop what does not parse, dedupe,
 * and cap.
 *
 * ⚠️ **Dropping an unparseable token is right *here* and wrong at the API edge**, and the asymmetry
 * is deliberate (it is the same one `readFilterParams` documents). Mid-edit the operator has typed
 * half an address; refusing the whole field would fight them as they type. The API, which receives
 * a finished request, answers 400 — because silently ignoring `port=abc` there returns the
 * *unfiltered* top-N and calls it the answer.
 *
 * Order is preserved rather than sorted: unlike the `enum` kind there is no option list to sort
 * against, and the operator's own order is the only meaningful one. Dedupe keeps the first
 * occurrence so retyping a value does not move it.
 */
export function normalizeValues(
  input: string,
  parse: (token: string) => string | null,
  max?: number,
): string {
  const out: string[] = [];
  const seen = new Set<string>();
  for (const raw of input.split(',')) {
    const token = parse(raw.trim());
    if (token === null || token === '' || seen.has(token)) continue;
    seen.add(token);
    out.push(token);
    if (max !== undefined && out.length >= max) break;
  }
  return out.join(',');
}

/** Decode `min:max`. Anything unparseable on a side leaves that side unbounded rather than
 *  rejecting the whole value — the same "a stale bookmark lands on the default view" rule the set
 *  and range decoders follow. */
export function decodeNumberRange(raw: string): NumberRange {
  const i = raw.indexOf(':');
  if (i < 0) return { min: null, max: null };
  const num = (s: string): number | null => {
    const trimmed = s.trim();
    if (trimmed === '') return null;
    const n = Number(trimmed);
    return Number.isFinite(n) ? n : null;
  };
  return { min: num(raw.slice(0, i)), max: num(raw.slice(i + 1)) };
}
