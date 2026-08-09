// SPDX-License-Identifier: AGPL-3.0-only
/** The Credentials table's client-side view: which rows are shown, and in what order.
 *
 *  Client-side filtering is deliberate here — the credential list is bounded by what an operator
 *  typed in, not by fleet size (ui-conventions, "scale-aware lists"), so the whole list is in the
 *  browser and this is the only thing deciding what an operator sees. Which makes it worth testing:
 *  a search that quietly stops matching the id column, or a sort that ignores its direction, is
 *  invisible until someone is hunting for one credential among fifty.
 *
 *  Lives in a `.ts` for the usual reason — Vitest here runs `environment: 'node'` with
 *  `include: ['src/ **\/*.test.ts']`, so inside `CredentialsPage.tsx` this was untestable. */

import type { CredentialSummary } from '../types/api';

/** The two sortable columns. */
export type CredentialSortKey = 'name' | 'used_by';

/** A column plus a direction (`1` ascending, `-1` descending). */
export interface CredentialSort {
  key: CredentialSortKey;
  dir: 1 | -1;
}

/** The fields this module reads. Generic over the row so the page passes real `CredentialSummary`
 *  values and gets them back unchanged, rather than a re-declared copy of the API shape. */
type CredentialRow = Pick<CredentialSummary, 'id' | 'name' | 'kind' | 'used_by'>;

/** What clicking a column header does: re-clicking the active column flips its direction, while
 *  moving to another column starts ascending — never inheriting the previous column's direction,
 *  which would silently sort the new column backwards. */
export function nextCredentialSort(
  current: CredentialSort,
  key: CredentialSortKey,
): CredentialSort {
  return current.key === key ? { key, dir: (current.dir * -1) as 1 | -1 } : { key, dir: 1 };
}

/** Apply the search box, the type filter and the sort, in that order.
 *
 *  The search matches the name **or** the credential id, case-insensitively: the id is the handle
 *  that appears in a node's config and in an error message, so pasting one has to find its row. */
export function visibleCredentials<T extends CredentialRow>(
  rows: readonly T[],
  query: string,
  kindFilter: string,
  sort: CredentialSort,
): T[] {
  const q = query.trim().toLowerCase();
  const filtered = rows.filter(
    (c) =>
      (q === '' || c.name.toLowerCase().includes(q) || c.id.toLowerCase().includes(q)) &&
      (kindFilter === 'all' || c.kind === kindFilter),
  );
  return filtered.sort((a, b) => {
    const av = sort.key === 'name' ? a.name : a.used_by;
    const bv = sort.key === 'name' ? b.name : b.used_by;
    return (av < bv ? -1 : av > bv ? 1 : 0) * sort.dir;
  });
}
