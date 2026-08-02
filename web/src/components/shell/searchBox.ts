// SPDX-License-Identifier: AGPL-3.0-only
// The judgement behind the top-bar global search, kept out of the .tsx so it is testable.
// Named searchBox rather than globalSearch because a name differing from GlobalSearch.tsx only in
// casing collides on a case-insensitive filesystem (TS1149) and breaks the Windows build.
// (`web/vitest.config.ts` runs only `src/**/*.test.ts`, in a `node` environment).
//
// The search box has been a permanently disabled control in the top bar since the shell was built,
// with a comment saying no search endpoint existed. `GET /api/v1/nodes/search` has existed since
// the NodePicker work, so the control is now wired to it.

import type { NodeSearchResult } from '../../types/api';

/** Server search cap for the popover. Deliberately smaller than the NodePicker's 50: this is a
 *  drop-down under the top bar, not a dedicated picker, and a long list here covers the page. */
export const MAX_RESULTS = 20;

/** Debounce so a fast typist fires one request rather than one per keystroke. Matches NodePicker. */
export const SEARCH_DEBOUNCE_MS = 200;

/** What a key press should do to the popover. Returning an action rather than mutating keeps the
 *  key handling describable in a test; the component maps each action onto its state. */
export type SearchKeyAction = 'up' | 'down' | 'open' | 'close' | null;

export function keyAction(key: string): SearchKeyAction {
  switch (key) {
    case 'ArrowDown':
      return 'down';
    case 'ArrowUp':
      return 'up';
    case 'Enter':
      return 'open';
    case 'Escape':
      return 'close';
    default:
      return null;
  }
}

/** Move the highlighted row, clamped to the list. An empty list has no active row. */
export function moveActive(current: number, delta: number, length: number): number {
  if (length <= 0) return 0;
  return Math.min(Math.max(current + delta, 0), length - 1);
}

/** Where selecting a result navigates. Nodes are the only searchable entity today. */
export function resultRoute(result: Pick<NodeSearchResult, 'id'>): string {
  return `/nodes/${result.id}`;
}

/** A full page came back, so there are probably more matches than are shown. */
export function isCapped(resultCount: number): boolean {
  return resultCount >= MAX_RESULTS;
}

/** Whether a bare `/` keypress should steal focus into the search box.
 *
 *  It must not when the operator is already typing somewhere — otherwise `/` cannot be typed into
 *  any field on any page, which is worse than having no shortcut. `Ctrl`/`Cmd`+K is unconditional
 *  because no field claims it.
 *
 *  `tagName` is the active element's tag (upper-case, as the DOM reports it), and `isEditable` is
 *  its `isContentEditable`. Both are passed in rather than read here so this stays DOM-free. */
export function shouldFocusOnSlash(tagName: string | undefined, isEditable: boolean): boolean {
  if (isEditable) return false;
  switch ((tagName ?? '').toUpperCase()) {
    case 'INPUT':
    case 'TEXTAREA':
    case 'SELECT':
      return false;
    default:
      return true;
  }
}
