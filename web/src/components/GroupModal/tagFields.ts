// SPDX-License-Identifier: AGPL-3.0-only
// The label fields of the folder dialog, as pure functions (ADR-135 inc. 2).
//
// Split out for the reason `geoFields.ts` and `prefixFields.ts` are: `web/vitest.config.ts` runs
// only `src/**/*.test.ts` in a `node` environment, so a test written next to the `.tsx` is a file
// nothing runs. What is worth testing here is the changed-check — the folder's labels are saved by
// their own endpoint, after the group body, and only when they actually differ.
//
// The rules over a single label (length, control characters, the cap) are NOT here: they are
// `ui/labelRules.ts`, shared with the node editor and the bulk dialog, because one validator
// (`api/nodes.rs::validated_labels`) applies to all three server-side.

import type { NodeGroup } from '../../types/api';

/** The two lists the dialog edits: what this folder supplies, and what it refuses from above. */
export interface TagDraft {
  tags: string[];
  tagsExcluded: string[];
}

/** An existing folder's labels as an editable draft. Sorted, so the chips do not reshuffle between
 *  two openings of the same dialog. A folder being created has neither list. */
export function tagDraftFrom(group: NodeGroup | undefined): TagDraft {
  return {
    tags: [...(group?.tags ?? [])].sort((a, b) => a.localeCompare(b)),
    tagsExcluded: [...(group?.tags_excluded ?? [])].sort((a, b) => a.localeCompare(b)),
  };
}

/** Whether the draft differs from what the folder already has.
 *
 * ⚠️ **Order-insensitive.** `tagDraftFrom` sorts, but the chip input appends, so a draft that has
 * had a label removed and re-added holds the same set in a different order. Comparing the arrays
 * positionally would report that as a change and issue a write that alters nothing — harmless, but
 * it would put a spurious row in the audit log every time somebody opened the dialog and fiddled.
 */
export function tagsChanged(draft: TagDraft, group: NodeGroup | undefined): boolean {
  const before = tagDraftFrom(group);
  return (
    !sameSet(before.tags, draft.tags) || !sameSet(before.tagsExcluded, draft.tagsExcluded)
  );
}

function sameSet(a: readonly string[], b: readonly string[]): boolean {
  if (a.length !== b.length) return false;
  const sortedA = [...a].sort();
  const sortedB = [...b].sort();
  return sortedA.every((v, i) => v === sortedB[i]);
}
