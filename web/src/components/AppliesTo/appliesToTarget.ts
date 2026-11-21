// SPDX-License-Identifier: AGPL-3.0-only
// The judgement behind `AppliesTo.tsx` (ADR-262 decision 1), kept in a `.ts` so Vitest reaches it.

import type { components } from '../../api/schema';
import type { GroupOption } from '../../lib/nodeTree';

/** How the folders and the tags combine when a setting names both (ADR-262 decision 10). */
export type Join = components['schemas']['Join'];
export const JOINS: readonly Join[] = ['or', 'and'];

/** What a setting applies to: every node, or the nodes inside one of `groupIds` (at any depth)
 *  and/or carrying one of `tags`, as `join` says. `only` is the switch the operator sees; the API
 *  spells "every node" as both lists empty. */
export interface AppliesTo {
  only: boolean;
  groupIds: string[];
  tags: string[];
  join: Join;
}

/** Read a stored setting back as the dialog's state. */
export function appliesToFrom(
  groupIds: readonly string[],
  tags: readonly string[],
  join: Join = 'or',
): AppliesTo {
  return {
    only: groupIds.length > 0 || tags.length > 0,
    groupIds: [...groupIds],
    tags: [...tags],
    join,
  };
}

/** What to send: both lists empty when every node is chosen. The ticks are kept while "every node"
 *  is on, so switching back restores them, but they are not saved. */
export function appliesToBody(v: AppliesTo): { group_ids: string[]; tags: string[]; join: Join } {
  return v.only
    ? { group_ids: v.groupIds, tags: v.tags, join: v.join }
    : { group_ids: [], tags: [], join: 'or' };
}

/** Whether the or / and choice means anything: only when both lists name something. */
export function joinMatters(v: AppliesTo): boolean {
  return v.only && v.groupIds.length > 0 && v.tags.length > 0;
}

/** The names a list shows in the reach line: the first `max`, and how many more there are. */
export function shownNames(names: readonly string[], max = 2): { shown: string[]; more: number } {
  return { shown: names.slice(0, max), more: Math.max(0, names.length - max) };
}

/** How the two lists overlap, as the server counted them. */
export interface ReachParts {
  by_folder: number;
  by_tag: number;
  both: number;
}

/** Which sentence explains the reach number, and with what values: under `or` the two counts and
 *  their overlap (6 + 7, 5 in both = 8), under `and` the folder count it was narrowed from. */
export function reachDetail(
  parts: ReachParts,
  join: Join,
): { key: 'appliesTo.partsOr' | 'appliesTo.partsAnd'; values: Record<string, number> } {
  return join === 'or'
    ? {
        key: 'appliesTo.partsOr',
        values: { folders: parts.by_folder, tags: parts.by_tag, both: parts.both },
      }
    : { key: 'appliesTo.partsAnd', values: { folders: parts.by_folder } };
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
