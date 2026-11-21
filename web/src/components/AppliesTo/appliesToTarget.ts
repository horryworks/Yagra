// SPDX-License-Identifier: AGPL-3.0-only
// The judgement behind `AppliesTo.tsx` (ADR-262 decision 1), kept in a `.ts` so Vitest reaches it.

import type { GroupOption } from '../../lib/nodeTree';

/** What a setting applies to: every node, or the nodes inside one of `groupIds` (at any depth) or
 *  carrying one of `tags`. `only` is the switch the operator sees; the API spells "every node" as
 *  both lists empty. */
export interface AppliesTo {
  only: boolean;
  groupIds: string[];
  tags: string[];
}

/** Read a stored pair back as the dialog's state. */
export function appliesToFrom(groupIds: readonly string[], tags: readonly string[]): AppliesTo {
  return { only: groupIds.length > 0 || tags.length > 0, groupIds: [...groupIds], tags: [...tags] };
}

/** What to send: both lists empty when every node is chosen. The ticks are kept while "every node"
 *  is on, so switching back restores them, but they are not saved. */
export function appliesToBody(v: AppliesTo): { group_ids: string[]; tags: string[] } {
  return v.only ? { group_ids: v.groupIds, tags: v.tags } : { group_ids: [], tags: [] };
}

/** Whether a choice of "only these" names nothing, which would match no node. */
export function namesNothing(v: AppliesTo): boolean {
  return v.only && v.groupIds.length === 0 && v.tags.length === 0;
}

/** One row of the folder list. */
export interface FolderChoice {
  id: string;
  /** The folder's path, so two folders with one name stay apart. */
  label: string;
  /** The setting names it and it no longer exists: it reaches nothing. */
  gone: boolean;
}

/** The folder rows: a named folder that no longer exists first (so it is seen and can be unticked),
 *  then every folder in tree order, by path. */
export function folderChoices(
  options: readonly GroupOption[],
  named: readonly string[],
): FolderChoice[] {
  const known = new Set(options.map((o) => o.id));
  return [
    ...named.filter((id) => !known.has(id)).map((id) => ({ id, label: id, gone: true })),
    ...options.map((o) => ({ id: o.id, label: o.path, gone: false })),
  ];
}
