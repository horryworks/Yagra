// SPDX-License-Identifier: AGPL-3.0-only
// Serialize the Nodes split's right-pane selection to/from a URL query param so a browser reload
// restores the same pane (see design-guidelines.md "画面状態の永続化"). The wire form is
// `node:<id>` / `group:<id>`; anything else parses back to null (no selection).

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
