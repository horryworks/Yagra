// SPDX-License-Identifier: AGPL-3.0-only
// Serialize the Nodes split's right-pane selection to/from a URL query param so a browser reload
// restores the same pane (see design-guidelines.md "画面状態の永続化"). The wire form is
// `node:<id>` / `group:<id>`; anything else parses back to null (no selection).
//
// Since ADR-124 the page holds a **second** persistent selection — the checked working set — and
// the order Escape unwinds the two lives here too, for the same reason the parsing does: it is a
// judgement, and a judgement in a `.tsx` is a judgement no test runs.

import type { TreeSelection } from '../components/NodeTree/NodeTree';

/** Selection → `sel` query value (`node:<id>` / `group:<id>`), or null to clear the param. */
export function selectionToParam(sel: TreeSelection): string | null {
  return sel ? `${sel.kind}:${sel.id}` : null;
}

/** `sel` query value → TreeSelection. Returns null when absent or malformed (unknown kind / no id).
 *  Splits on the FIRST colon so an id containing a colon survives the round-trip. */
export function parseSelection(raw: string | null): TreeSelection {
  if (!raw) return null;
  const sep = raw.indexOf(':');
  if (sep <= 0) return null;
  const kind = raw.slice(0, sep);
  const id = raw.slice(sep + 1);
  if ((kind === 'node' || kind === 'group') && id) return { kind, id };
  return null;
}

/** What one Escape press on the Nodes page clears, or null when there is nothing to clear. */
export type EscapeTarget = 'checked' | 'selection' | null;

/**
 * Which of the page's two persistent selections an Escape press unwinds (ADR-124 決定 2).
 *
 * **The working set goes first.** It is the newer, narrower, more transient of the two — the
 * operator is midway through assembling a batch — while the right-hand pane is where they were
 * reading. Clearing the pane first would make the second press throw away work the first press
 * left alone.
 *
 * ⚠️ **One handler asks this, never two.** Two `document` listeners each deciding for themselves
 * would both fire on the same press and clear both layers at once — which looks like the working
 * set "not being cleared" and the pane "closing by itself", two bug reports for one cause.
 *
 * ⚠️ This is the page's own layer and sits **below** the ones `escapeDismiss.ts` owns: a dialog, a
 * popover or an in-page surface takes the press first. Do not add the working set to those lists —
 * it is not something floating above the page, it is the page's state, exactly like `?sel=`.
 */
export function escapeTarget(hasChecked: boolean, hasSelection: boolean): EscapeTarget {
  if (hasChecked) return 'checked';
  if (hasSelection) return 'selection';
  return null;
}
