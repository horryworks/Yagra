// SPDX-License-Identifier: AGPL-3.0-only
// The Nodes tree's state / kind / pool filters.
//
// ⚠️ **These are server-side, and the reason is the same one that makes the tree lazy.** There is
// no client-side copy of the fleet to filter — the tree paints from the group skeleton and loads
// members per open group (A-3), and the live-state SSE map is an *overlay* of changes, not a
// snapshot, so the browser does not hold every node's state either. A filter applied to what
// happens to be loaded would answer "of the folders you have opened", which is not the question.
//
// What they are NOT is a `WHERE` clause, and that is deliberate rather than unfinished: none of the
// three has an answer in the `nodes` table. State lives in the alert engine, kind is derived from
// which side table carries a row (`NodeKind::resolve` owns that precedence), and a pool is
// inherited from the folder tree. The server applies them in-process through those same resolvers
// over a bounded candidate scan, and says so with `NodePage.truncated` when the scan was not
// exhaustive. See `api/nodes.rs`.
//
// **ADR-053 Inc.6 made all three sets** (decision E). The tree is a tree, not a table with headers,
// so the controls sit in a `FilterBar` rather than under columns — but the change an operator
// notices is that "everything that is not healthy" is now one question instead of three separate
// looks at the tree. That required widening `GET /api/v1/nodes`, and it had to: a multi-select over
// a single-valued endpoint would tick three boxes and send one, which is the exact failure the
// deleted `EnumFilterSpec.single` flag existed to prevent.
//
// In a `.ts` because Vitest never executes a `.tsx` (testing.md).

import {
  decodeSet,
  encodeSet,
  readFilterParams,
  specColumns,
  writeFilterParams,
  type ColumnFilterSpec,
  type FilterState,
  type FilterableColumn,
} from '../lib/columnFilter';
// `DISPLAY_ORDER` rather than a fresh list: it is the single enumeration of the state union
// (`lib/nodeState.ts` exists because this list once lived in five places under three names), and
// its order — healthy first, then problems, then the neutral two — is the one the control wants.
import { DISPLAY_ORDER, PROBLEM_STATES } from '../lib/nodeState';
import { NODE_KIND_SPEC } from '../lib/nodeKind';
import { NODE_KINDS, type NodeState } from '../types/api';
import type { TFunction } from 'i18next';

/** The URL key the tree's search box writes its term to (ADR-153).
 *
 *  A bare key beside `state` / `kind` / `pool`, and one of the keys the route ledger in
 *  `filterSpecRegistry.test.ts` owns for `/nodes` — so a column that one day wanted to be called
 *  `q` on this route fails there rather than silently sharing the term. */
export const TREE_SEARCH_KEY = 'q';

/** The states offered in the filter, in the order they are shown. */
export const NODE_STATE_FILTERS: readonly NodeState[] = DISPLAY_ORDER;

/**
 * The tree's filter columns.
 *
 * **No `readValue` on any of them.** These are server-side: the bounds go into the query and the
 * browser holds no copy of the fleet to re-check them against. `buildPredicate` is never called on
 * this screen, so the accessor would be dead weight that the next reader would take as an
 * invitation to filter locally — which would silently answer "of the folders you have opened".
 *
 * ⚠️ They each carried `readValue: () => null` until ADR-053 Inc.8, because the type demanded one.
 * That placeholder was not neutral: `null` means "this row has no value", so a predicate built over
 * it would have **rejected every row** rather than skipping the column. The accessor is optional now
 * and omitting it is what says "server-side" — the same spelling `readTime` and `readNumber` use.
 *
 * `pool` is the odd one: the option list is the deployment's live pools, passed in, because pools
 * are named by the operator and there is no enum to enumerate.
 */
export function inventoryFilterSpecs(
  t: TFunction,
  pools: readonly { name: string }[],
): Record<string, ColumnFilterSpec<never>> {
  return {
    state: {
      kind: 'enum',
      options: NODE_STATE_FILTERS.map((s) => ({ value: s, label: t(`format:state.${s}`) })),
      allLabel: t('inventory.filter.allStates'),
    },
    kind: {
      kind: 'enum',
      options: NODE_KINDS.map((k) => ({ value: k, label: t(NODE_KIND_SPEC[k].labelKey) })),
      allLabel: t('inventory.filter.allKinds'),
    },
    pool: {
      kind: 'enum',
      options: pools.map((p) => ({ value: p.name, label: p.name })),
      allLabel: t('inventory.filter.allPools'),
    },
  };
}

export function inventoryColumns(
  t: TFunction,
  pools: readonly { name: string }[],
): FilterableColumn<never>[] {
  return specColumns(inventoryFilterSpecs(t, pools));
}

/** Plain-text names for the bar and the mobile sheet. */
export function inventoryFilterLabels(t: TFunction): Record<string, string> {
  return {
    state: t('inventory.cols.state'),
    kind: t('inventory.cols.kind'),
    pool: t('inventory.cols.pool'),
  };
}

/** The query fields for `GET /api/v1/nodes`.
 *
 *  `''` becomes `undefined`, never an empty string: `?state=` reaches the API edge as a value it
 *  cannot parse, which is a 400 — the mistake `findingsQuery.ts` documents having made. The joined
 *  spelling is what the API takes since Inc.6, so a set passes straight through. */
export function inventoryQuery(f: FilterState): {
  state?: string;
  kind?: string;
  pool?: string;
} {
  return {
    state: f.state || undefined,
    kind: f.kind || undefined,
    pool: f.pool || undefined,
  };
}

/** A stable string identity for an effect's dependency list.
 *
 *  The filters are re-derived from the URL on every render, so the object is a new one each time
 *  and putting it in a dep array would re-issue the search on every keystroke elsewhere on the
 *  page. `useLazyGroupMembers` takes its `filterTerm` as a plain string for exactly this reason.
 *
 *  ⚠️ Each value is already order-normalised by `readInventoryFilters`, which is what keeps this a
 *  stable key: without that, ticking `ok` then `warning` and ticking them the other way round would
 *  produce two different strings and refetch. */
export function inventoryKey(f: FilterState): string {
  return `${f.state ?? ''} ${f.kind ?? ''} ${f.pool ?? ''}`;
}

/** Whether anything is narrowing the tree. */
export function isInventoryFiltered(f: FilterState): boolean {
  return (f.state ?? '') !== '' || (f.kind ?? '') !== '' || (f.pool ?? '') !== '';
}

// ---------------------------------------------------------------------------
// The "Needs attention" preset (ADR-163).
//
// ⚠️ **This is not a fourth filter.** It writes the `state` column that is already here, so there is
// no new URL key, no account preference, and nothing for `clearAllFilters` or `ClearFilters` to be
// taught about — the reason ADR-159's toggle needed `extraActive` is that it held state of its own.
// What it buys is the press: "show me everything that is not healthy" was five gestures.
//
// Why the *display state* and not the alert list: the tree paints from `NodeSummary.state`, which
// the server has already rolled up to the worst of a node's committed liveness and every active
// alert on it (`alerts/engine.rs::node_states_for`). Asking the alert store instead would mean a
// second subscription on this page — `useAlertStream()` is mounted by the Alerts screen and the
// three dashboards, not by the shell — to answer a question the server answers already.

/** The states the preset selects, in the order the state filter offers them.
 *
 *  **The set is the load-bearing half**: it is the same one the page header counts as
 *  "N need attention", so pressing the button leaves exactly those N rows on screen. A hand-written
 *  copy that drifted would put a control beside a number it disagrees with, and neither surface
 *  would look wrong on its own — which is why this is derived from `PROBLEM_STATES` rather than
 *  spelled out.
 *
 *  ⚠️ **The order here is not what makes the URL stable — `encodeSet` is.** That function emits the
 *  spec's option order whatever order it is handed, so reversing this array changes nothing an
 *  operator can see (measured: breaking it fails one test, and that test is the only thing watching
 *  it). It is kept in `DISPLAY_ORDER` so that this array and the URL it produces read the same way
 *  side by side, not because anything downstream depends on it. */
export const ATTENTION_STATES: readonly NodeState[] = NODE_STATE_FILTERS.filter((s) =>
  PROBLEM_STATES.has(s),
);

/** Whether the state filter currently holds exactly the attention states — what the toggle and the
 *  header count both read for `aria-pressed`.
 *
 *  ⚠️ Compared as a **set**, not as a string. A hand-typed `?state=critical,warning,unreachable`
 *  asks this same question, and a button left unlit above a tree it had narrowed is the control
 *  disagreeing with the screen. */
export function isAttentionOnly(f: FilterState): boolean {
  const chosen = decodeSet(f.state ?? '');
  return (
    chosen.length === ATTENTION_STATES.length && ATTENTION_STATES.every((s) => chosen.includes(s))
  );
}

/** Press the preset: select the attention states, or clear the state column when they are already
 *  the selection. Every other column is left exactly as it was.
 *
 *  Two properties worth stating because neither is the only defensible choice:
 *
 *  - It **replaces** the chosen states rather than adding to them. This is a preset, not a fourth
 *    filter; "everything that is not healthy" *plus* `ok` is a question nobody asked.
 *  - Turning it off **clears** the column rather than restoring what was selected before. There is
 *    nothing to restore from — the preset deliberately holds no state of its own (ADR-163 決定 5),
 *    and inventing a stash here would be the one place on this page where a filter remembers. */
export function toggleAttention(f: FilterState): FilterState {
  return {
    ...f,
    state: isAttentionOnly(f) ? '' : encodeSet(ATTENTION_STATES, NODE_STATE_FILTERS),
  };
}

/** Read the filters out of the URL.
 *
 *  An unrecognised token is dropped rather than erroring: a bookmark written by a newer build must
 *  not break the page. This is the opposite of the API edge, which rejects an unknown token —
 *  there, silently widening would answer a different question than the one asked; here, the
 *  operator can see the control did not take.
 *
 *  ⚠️ **`pool` is not filtered against the option list, and that is deliberate.** The pools are the
 *  deployment's own names, fetched asynchronously, so a link opened before that request lands would
 *  have its pool filter silently erased if this validated against an empty list. State and kind are
 *  compile-time vocabularies and have no such window. */
export function readInventoryFilters(
  columns: readonly FilterableColumn<never>[],
  params: URLSearchParams,
): FilterState {
  const raw = readFilterParams(columns, params);
  for (const c of columns) {
    if (c.key === 'pool' || c.filter.kind !== 'enum') continue;
    const order = c.filter.options.map((o) => o.value);
    raw[c.key] = encodeSet(
      decodeSet(raw[c.key] ?? '').filter((v) => order.includes(v)),
      order,
    );
  }
  return raw;
}

/** Which "matches are missing" notice the tree should show, if any.
 *
 *  The server says *that* the answer is incomplete; only the page can tell the operator what to do
 *  about it, and the two cases want different advice:
 *
 *  - `'page'` — the list came back full. There are more matches than fit; narrowing helps.
 *  - `'scan'` — the list is short and still incomplete, because a state / kind / pool filter is
 *    applied and the server stopped examining candidates before it ran out of them. Narrowing
 *    *also* helps here, but the old copy ("showing the first 500 matches") would be a plain lie:
 *    the page might be showing three.
 *
 *  Pure, so the distinction is testable — it is exactly the sort of thing that reads fine and is
 *  wrong. */
export type TruncationNotice = 'none' | 'page' | 'scan';

export function truncationNotice(
  truncated: boolean,
  shown: number,
  cap: number,
): TruncationNotice {
  if (!truncated) return 'none';
  return shown >= cap ? 'page' : 'scan';
}

/** Write the filters into `params`, deleting each key at its default — so "the URL has a query
 *  string" and "something is narrowing the list" stay the same statement. */
export function writeInventoryFilters(
  columns: readonly FilterableColumn<never>[],
  params: URLSearchParams,
  next: FilterState,
): void {
  writeFilterParams(columns, params, next);
}

// ---------------------------------------------------------------------------
// The chips under the inventory head (ADR-177).
//
// Since ADR-177 every control that narrows the tree lives behind ONE button, so what is in force is
// no longer visible on the controls themselves. The chip row is what says it — and a chip the row
// forgot is a narrowing nobody can see, which is why the list is decided here, where a test reaches
// it, rather than in the `.tsx` that draws it.

/** One thing currently narrowing (or reshaping) the tree, in the order the row shows them. */
export type InventoryChip =
  | { kind: 'pinned' }
  | { kind: 'attention' }
  | { kind: 'column'; key: string; values: string[] }
  | { kind: 'hideEmpty' };

/** What the chip row shows, and — its length — the number on the filter button.
 *
 *  - The two switches held on the account (Pinned only, Hide empty folders) come from `opts`; the
 *    columns come from the URL-backed `filters`.
 *  - ⚠️ **Needs attention replaces the State chip rather than sitting beside it.** The preset *is*
 *    a State selection (ADR-163 決定 5), so showing both would say one thing twice — and removing
 *    either would silently remove the other. Any other State selection gets its own chip.
 *  - Hide empty folders is shown although it hides no node: it hides folders, and a folder that is
 *    missing for a reason nobody can see is the ADR-159 complaint all over again. */
export function inventoryChips(
  columns: readonly FilterableColumn<never>[],
  filters: FilterState,
  opts: { pinnedOnly: boolean; hideEmpty: boolean },
): InventoryChip[] {
  const out: InventoryChip[] = [];
  if (opts.pinnedOnly) out.push({ kind: 'pinned' });
  const attention = isAttentionOnly(filters);
  if (attention) out.push({ kind: 'attention' });
  for (const c of columns) {
    if (c.key === 'state' && attention) continue;
    const values = decodeSet(filters[c.key] ?? '');
    if (values.length > 0) out.push({ kind: 'column', key: c.key, values });
  }
  if (opts.hideEmpty) out.push({ kind: 'hideEmpty' });
  return out;
}
