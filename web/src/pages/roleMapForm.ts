// SPDX-License-Identifier: AGPL-3.0-only
/** The group→role mapping editor's pure half, shared by the OIDC provider dialog and the LDAP
 *  directory card.
 *
 *  ADR-041 decision 2 is "reuse the OIDC mapping mechanism, do not build a second one", and that
 *  applies to the form as much as to the storage: two editors would be two answers to "what does a
 *  blank group name mean" and "what happens to a duplicate". The JSX differs between the two
 *  surfaces; this — the part that can be wrong — does not.
 *
 *  Lives in a `.ts` because Vitest here runs `environment: 'node'` with `include:
 *  ['src/ **\/*.test.ts']`, so a test written in a `.tsx` is a file nothing runs. */

import type { Role } from '../types/api';

/** One editable row. Rows carry a stable `key` because a group name is editable and starts empty,
 *  so it cannot be the React key without rows swapping identity as somebody types. */
export interface RoleMapRow {
  key: number;
  group: string;
  role: Role;
}

/** Expand a stored mapping into editable rows, in a stable (sorted) order.
 *
 *  Sorted rather than insertion-ordered because the source is a JSON object, whose key order is an
 *  accident of how it was serialized — an unsorted list would appear to reshuffle itself between
 *  visits to the page. */
export function toRoleMapRows(map: Record<string, string> | null | undefined): RoleMapRow[] {
  const asRole = (value: string): Role =>
    value === 'admin' || value === 'operator' ? value : 'viewer';
  return Object.entries(map ?? {})
    .sort(([a], [b]) => a.localeCompare(b))
    .map(([group, role], i) => ({ key: i, group, role: asRole(role) }));
}

/** Collapse rows back into the stored mapping.
 *
 *  A blank group name is dropped rather than saved: an empty row is what the "add" button produces,
 *  so saving one would persist a mapping that can never match a group. Trailing space is trimmed
 *  for the same reason — a directory never reports `"NetOps "`, so storing it would be a rule that
 *  silently never fires. A later row wins a duplicate, matching what the operator sees last. */
export function fromRoleMapRows(rows: readonly RoleMapRow[]): Record<string, Role> {
  const out: Record<string, Role> = {};
  for (const row of rows) {
    const group = row.group.trim();
    if (group) out[group] = row.role;
  }
  return out;
}

/** Append an empty row, with a key that cannot collide with an existing one. */
export function addRoleMapRow(rows: readonly RoleMapRow[]): RoleMapRow[] {
  const next = rows.reduce((max, r) => Math.max(max, r.key), -1) + 1;
  return [...rows, { key: next, group: '', role: 'viewer' }];
}
