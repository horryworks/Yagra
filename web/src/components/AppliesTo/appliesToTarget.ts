// SPDX-License-Identifier: AGPL-3.0-only
// The judgement behind `AppliesTo.tsx` (ADR-262 decision 1), kept in a `.ts` so Vitest reaches it.

import type { GroupOption } from '../../lib/nodeTree';
import type { Join, TagMatch } from '../../types/api';

/** How the folders and the tags combine when a setting names both (ADR-262 decision 10). */
export type { Join, TagMatch };
export const JOINS: readonly Join[] = ['or', 'and'];

/** What a setting applies to: every node, or the nodes inside one of `groupIds` (at any depth)
 *  and/or carrying one (or every one, by `tagMatch`, ADR-267) of `tags`, as `join` says. `only` is
 *  the switch the operator sees; the API spells "every node" as both lists empty. */
export interface AppliesTo {
  only: boolean;
  groupIds: string[];
  tags: string[];
  join: Join;
  tagMatch: TagMatch;
}

/** Read a stored setting back as the dialog's state. A setting saved before ADR-267 has no
 *  `tag_match` and asks for any of its tags. */
export function appliesToFrom(
  groupIds: readonly string[],
  tags: readonly string[],
  join: Join = 'or',
  tagMatch: TagMatch = 'any',
): AppliesTo {
  return {
    only: groupIds.length > 0 || tags.length > 0,
    groupIds: [...groupIds],
    tags: [...tags],
    join,
    tagMatch,
  };
}

/** What to send: both lists empty when every node is chosen. The ticks are kept while "every node"
 *  is on, so switching back restores them, but they are not saved. */
export function appliesToBody(v: AppliesTo): {
  group_ids: string[];
  tags: string[];
  join: Join;
  tag_match: TagMatch;
} {
  return v.only
    ? { group_ids: v.groupIds, tags: v.tags, join: v.join, tag_match: v.tagMatch }
    : { group_ids: [], tags: [], join: 'or', tag_match: 'any' };
}

/** Whether the or / and choice means anything: only when both lists name something. */
export function joinMatters(v: AppliesTo): boolean {
  return v.only && v.groupIds.length > 0 && v.tags.length > 0;
}

/** Whether "has any / has all" means anything: only when two or more tags are named (ADR-267
 *  decision 2). One tag is carried or not, either way. */
export function tagMatchMatters(v: { only: boolean; tags: readonly string[] }): boolean {
  return v.only && v.tags.length >= 2;
}

/** Whether the tags are bracketed: when folders stand beside them and two or more tags are joined,
 *  so "Tokyo or (core and 24x7)" cannot be read as "(Tokyo or core) and 24x7". */
export function bracketsTags(groupCount: number, tagCount: number): boolean {
  return groupCount > 0 && tagCount >= 2;
}

/** The space to put before `text`: none when it opens with a full-width character (a Japanese
 *  bracket), one otherwise. Japanese puts no half-width space beside a full-width bracket. */
export function spaceBefore(text: string): string {
  return /^[\u3000-\u30ff\uff00-\uffef]/.test(text) ? '' : ' ';
}

/** The tag side of the reach line: the first two tags joined by `word`, the rest counted by
 *  `more`, bracketed by `open` / `close` when asked. */
export function tagSideText(
  tags: readonly string[],
  o: { word: string; bracket: boolean; open: string; close: string; more: (n: number) => string },
): string {
  const { shown, more } = shownNames(tags);
  const core = [shown.join(` ${o.word} `), more > 0 ? o.more(more) : '']
    .filter((s) => s !== '')
    .join(' ');
  return o.bracket ? `${o.open}${core}${o.close}` : core;
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
