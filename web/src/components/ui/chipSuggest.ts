// SPDX-License-Identifier: AGPL-3.0-only
// What a chip field's suggestion list shows and what Enter adds (ADR-263 Inc.3).
//
// Its own file, beside `ChipInput.tsx`, because Vitest never runs a `.tsx`: this is the judgement,
// the component is the layout. Not `chipInput.ts` — a `.ts` and a `.tsx` differing only in case
// collide on Windows (see the note at the top of `ChipInput.tsx`).

const fold = (s: string): string => s.trim().toLowerCase();

/**
 * The suggestions to list for what is typed: the ones not already chosen, narrowed to those that
 * contain the typed text, ignoring case. Nothing typed lists them all, in the order given.
 */
export function shownSuggestions(
  suggestions: readonly string[],
  chosen: readonly string[],
  draft: string,
): string[] {
  const q = fold(draft);
  return suggestions.filter((s) => !chosen.includes(s) && (q === '' || fold(s).includes(q)));
}

/** The highlighted row after an arrow press: -1 is "none", and the list does not wrap. */
export function moveActive(active: number, count: number, dir: 1 | -1): number {
  if (count === 0) return -1;
  if (dir === 1) return Math.min(active + 1, count - 1);
  return Math.max(active - 1, -1);
}

/**
 * What Enter adds. The highlighted suggestion when one is highlighted; otherwise the typed text —
 * spelled the way the suggestion is when it names one exactly but for case, so `matsuyama` does
 * not become a second tag beside `MATSUYAMA`. `null` when there is nothing to add.
 */
export function enterValue(shown: readonly string[], active: number, draft: string): string | null {
  if (active >= 0 && active < shown.length) return shown[active];
  if (draft.trim() === '') return null;
  return shown.find((s) => fold(s) === fold(draft)) ?? draft;
}

/**
 * `label` spelled the way a known value is when it names one exactly but for case, so `matsuyama`
 * does not become a second tag beside `MATSUYAMA` — whichever way it is committed (Enter, blur or
 * paste). A label naming no known value is returned as it is.
 */
export function canonicalLabel(label: string, known: readonly string[]): string {
  const f = fold(label);
  if (f === '') return label;
  return known.find((k) => fold(k) === f) ?? label;
}
