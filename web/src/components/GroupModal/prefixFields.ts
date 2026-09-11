// SPDX-License-Identifier: AGPL-3.0-only
// The IP-range rows of the group dialog, as pure functions (ADR-131).
//
// Split out for the reason `geoFields.ts` is: `web/vitest.config.ts` runs only `src/**/*.test.ts`
// in a `node` environment, so judgement left in the `.tsx` is judgement nothing runs.
//
// 🚨 **Nothing here validates the CIDR itself, and that is deliberate — not an omission.**
// There is no IPv6-capable range parser in the browser: `lib/cidr.ts`, the only expander in the
// repository, is IPv4-only, while IPv6 ranges are in scope. A shape check written here would
// therefore refuse every valid IPv6 range while looking helpful, which is worse than no check.
// The validator is PostgreSQL — `network($n::inet)::cidr` in
// `GroupRepo::set_manual_prefixes`/`canonical_prefix` — and its 400 (`invalid_prefix`, naming the
// offending value) is the message. What this file *does* check is everything the server would
// otherwise have to round-trip for: emptiness, duplicates, the caps, and the rows a sync owns.

import type { NodeGroup } from '../../types/api';

/** One editable row: a range and what it is called. Strings, so a half-typed CIDR is
 *  representable. */
export interface PrefixDraftRow {
  prefix: string;
  description: string;
}

/** How many hand-made ranges one folder may carry. Mirrors `MAX_GROUP_PREFIXES` in
 *  `api/groups.rs`; the server refuses beyond it with `too_many_prefixes`. */
export const MAX_PREFIX_ROWS = 64;

/** How long a description may be. Mirrors `MAX_PREFIX_DESCRIPTION` in `api/groups.rs`. */
export const MAX_PREFIX_DESCRIPTION = 200;

/** The folder's **hand-made** ranges as editable rows, in the order the server listed them.
 *
 * Sync-owned rows are excluded on purpose: they are not editable here, and putting them in the
 * draft would mean a Save silently trying to re-assert rows it cannot own. */
export function prefixDraftFrom(group: NodeGroup | undefined): PrefixDraftRow[] {
  return (group?.prefixes ?? [])
    .filter((p) => p.source === 'manual')
    .map((p) => ({ prefix: p.prefix, description: p.description }));
}

/** The folder's ranges a sync maintains — shown read-only, so the dialog never offers to remove
 *  a row it cannot remove. */
export function syncOwnedRows(group: NodeGroup | undefined): PrefixDraftRow[] {
  return (group?.prefixes ?? [])
    .filter((p) => p.source === 'sync')
    .map((p) => ({ prefix: p.prefix, description: p.description }));
}

/** Rows with nothing typed in them at all. An operator who presses "add range" and then saves
 *  should not get a 400 about an empty value. */
function meaningful(rows: readonly PrefixDraftRow[]): PrefixDraftRow[] {
  return rows.filter((r) => r.prefix.trim() !== '' || r.description.trim() !== '');
}

/** Whether the draft differs from what the folder already has, so an unchanged dialog does not
 *  issue a pointless second request (ranges are saved by their own endpoint, after the group
 *  body — exactly as the pin is).
 *
 * ⚠️ Compared on the **trimmed** values, because that is what would be sent. */
export function prefixesChanged(
  draft: readonly PrefixDraftRow[],
  group: NodeGroup | undefined,
): boolean {
  const before = prefixDraftFrom(group);
  const after = meaningful(draft).map((r) => ({
    prefix: r.prefix.trim(),
    description: r.description.trim(),
  }));
  if (before.length !== after.length) return true;
  return before.some(
    (b, i) => b.prefix !== after[i].prefix || b.description !== after[i].description,
  );
}

/** Every reason a range list is refused before it is sent. `as const` so the i18n coverage test can
 *  walk it: the dialog renders `` t(`err.${problem}`) `` with no fallback. */
export const PREFIX_PROBLEMS = [
  'prefixEmpty',
  'prefixDuplicate',
  'prefixOwnedBySync',
  'prefixTooMany',
  'prefixDescTooLong',
] as const;

/** Why a range list cannot be saved. */
export type PrefixProblem = (typeof PREFIX_PROBLEMS)[number];

/** The body for `PUT /node-groups/{id}/prefixes`, or the first reason it cannot be sent.
 *
 * An empty list is a valid body, not an error: it is how every hand-made range is cleared, which is
 * why there is no companion delete endpoint. */
export function prefixBodyFrom(
  draft: readonly PrefixDraftRow[],
  group: NodeGroup | undefined,
): { body: PrefixDraftRow[] } | { error: PrefixProblem; prefix?: string } {
  const rows = meaningful(draft).map((r) => ({
    prefix: r.prefix.trim(),
    description: r.description.trim(),
  }));
  if (rows.length > MAX_PREFIX_ROWS) return { error: 'prefixTooMany' };

  const owned = new Set(syncOwnedRows(group).map((r) => r.prefix));
  const seen = new Set<string>();
  for (const row of rows) {
    if (row.prefix === '') return { error: 'prefixEmpty' };
    if (row.description.length > MAX_PREFIX_DESCRIPTION) {
      return { error: 'prefixDescTooLong', prefix: row.prefix };
    }
    if (seen.has(row.prefix)) return { error: 'prefixDuplicate', prefix: row.prefix };
    seen.add(row.prefix);
    // ⚠️ Only catches a collision spelled exactly as the server listed it. A sync row written
    // `192.168.1.0/24` and typed here as `192.168.1.5/24` is the same range but not the same
    // string, and canonicalising it would need the parser this file deliberately does not have.
    // The server refuses that one with `prefix_owned_by_sync`; this is the early, inline half.
    if (owned.has(row.prefix)) return { error: 'prefixOwnedBySync', prefix: row.prefix };
  }
  return { body: rows };
}
