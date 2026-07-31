// SPDX-License-Identifier: AGPL-3.0-only
// Keyboard focus containment for overlay surfaces (dialogs, the mobile nav drawer).
//
// The DOM half — querying focusables and calling .focus() — stays at the call site; only the
// *decision* lives here, because that is the part with rules and the part a `.tsx` file would hide
// from the test runner (testing.md: Vitest only executes `*.test.ts`).

/** Elements that take keyboard focus in an overlay. Excludes anything disabled or explicitly
 *  removed from the tab order (`tabindex="-1"`, which the dialog container itself carries). */
export const FOCUSABLE_SELECTOR =
  'a[href], button:not([disabled]), input:not([disabled]), select:not([disabled]), ' +
  'textarea:not([disabled]), [tabindex]:not([tabindex="-1"])';

/**
 * Where Tab should land, or `null` to let the browser move focus normally.
 *
 * Only the two edges need intervention — wrapping last→first and first→last — plus the case where
 * focus is not on a focusable inside the overlay at all. That last one is not hypothetical: the
 * dialog container holds focus on open (it is `tabindex="-1"`, so it is not in `focusables`), and
 * without this branch the very first Shift+Tab would walk out into the page behind the overlay.
 */
export function trapTarget<T>(focusables: readonly T[], active: T | null, shiftKey: boolean): T | null {
  if (focusables.length === 0) return null;
  const first = focusables[0];
  const last = focusables[focusables.length - 1];
  const index = active === null ? -1 : focusables.indexOf(active);
  if (index === -1) return shiftKey ? last : first;
  if (shiftKey && index === 0) return last;
  if (!shiftKey && index === focusables.length - 1) return first;
  return null;
}
