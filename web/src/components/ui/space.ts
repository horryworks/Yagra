// SPDX-License-Identifier: AGPL-3.0-only
// The spacing scale as the layout parts take it (ADR-246 decision 1).
//
// A part takes a step by its size in px (`<Stack gap={12}>`) - the number a person reads off a
// design - and turns it into the token, so no part ever writes a raw value. The type admits only
// the eight steps, which is what keeps a screen from asking for `gap={10}`.
//
// Pinned to `styles/tokens.css` by `space.test.ts`; `scripts/ui-rules.mjs` carries the same list
// for the checker and is pinned to the same file by `uiBudget.test.ts`.

export const SPACE = {
  2: '--space-0-5',
  4: '--space-1',
  6: '--space-1-5',
  8: '--space-2',
  12: '--space-3',
  16: '--space-4',
  24: '--space-5',
  32: '--space-6',
} as const;

/** One step of the scale, by its size in px. */
export type Space = keyof typeof SPACE;

/** `var(--space-…)` for a step. */
export function spaceVar(step: Space): string {
  return `var(${SPACE[step]})`;
}

/** Joins class names, dropping the empty ones. */
export function cx(...names: (string | false | null | undefined)[]): string {
  return names.filter(Boolean).join(' ');
}

/** Whether an optional slot has anything to draw. `cond && <x/>` hands a part `false` when the
 *  condition fails, and a slot that drew its wrapper for that would leave an empty, spaced line.
 *  Zero is content ("0 nodes"); an empty string is not. */
export function isShown(node: unknown): boolean {
  return node !== undefined && node !== null && node !== false && node !== true && node !== '';
}
