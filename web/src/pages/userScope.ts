// SPDX-License-Identifier: AGPL-3.0-only
// The judgement behind the Users page's scope control, kept out of the `.tsx` so it can be tested
// (Vitest runs `environment: 'node'` with `include: ['src/**/*.test.ts']` — a test in a `.tsx` is a
// file nothing runs).
//
// A scope is `"All"` or `{ Groups: [uuid, …] }`, and the two are **not** points on one line: an
// empty `Groups` set is a valid scope that sees nothing, while `"All"` is unrestricted. Every
// conversion here keeps them apart, because collapsing them in the widening direction is a
// privilege escalation and collapsing them the other way is an account that silently sees an empty
// fleet. The API refuses an empty set (`400 empty_scope`) for exactly that reason; this module is
// what stops the UI sending one.

import type { Role, Scope } from '../types/api';

/** Whether a scope restricts anything. `"All"` does not; any `Groups` set does — including an
 *  empty one, which is why this is a variant check rather than a length check. */
export function isScoped(scope: Scope): boolean {
  return scope !== 'All';
}

/** The group ids a scope names, or `[]` for `"All"`. Note the deliberate ambiguity this creates in
 *  the return value alone — always pair it with `isScoped` rather than testing for emptiness. */
export function scopeGroupIds(scope: Scope): string[] {
  return scope === 'All' ? [] : [...scope.Groups];
}

/** Build the scope a selection represents. An empty selection is `"All"`, matching what the picker
 *  means by "no groups ticked": the whole fleet. Sending `{ Groups: [] }` there would be the same
 *  gesture with the opposite meaning. */
export function scopeFromSelection(ids: readonly string[]): Scope {
  return ids.length === 0 ? 'All' : { Groups: [...new Set(ids)].sort() };
}

/** Whether two scopes are the same assignment — used to leave Save disabled until something
 *  actually changed, so a no-op edit does not revoke the account's sessions. */
export function sameScope(a: Scope, b: Scope): boolean {
  if (a === 'All' || b === 'All') return a === b;
  const x = [...a.Groups].sort();
  const y = [...b.Groups].sort();
  return x.length === y.length && x.every((v, i) => v === y[i]);
}

/** Whether a role can hold a group scope at all.
 *
 *  An Admin cannot: `ManageConfig` writes are not scope-filtered, so a scoped admin would read a
 *  narrowed inventory while still being able to edit the nodes it hides — reads answering 404 where
 *  writes answer 200. The server refuses it (`409 admin_is_unscoped`) and clears the scope on
 *  promotion; this is what keeps the UI from offering the action that must fail. */
export function canHoldScope(role: Role | string): boolean {
  return role !== 'admin';
}

/** i18n key + count for rendering a scope in one line, so the label cannot drift between the users
 *  list, the modal and the account menu. `n` is meaningless for the `all` key. */
export function scopeLabelKey(scope: Scope): { key: 'all' | 'groups' | 'none'; n: number } {
  if (scope === 'All') return { key: 'all', n: 0 };
  const n = scope.Groups.length;
  // A stored empty set predates the `empty_scope` rejection or was written by another client. It
  // is a real state and reads as "sees nothing" — never as "sees everything".
  return n === 0 ? { key: 'none', n: 0 } : { key: 'groups', n };
}
