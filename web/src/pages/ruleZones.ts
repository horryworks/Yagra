// SPDX-License-Identifier: AGPL-3.0-only
// The zones a routing rule's hours are read in (ADR-262 decision 11): the line under the rule's
// times says which zones its nodes keep time in, and how many nodes each, so "08:30" can be read
// as it will be applied. The server counts; this decides what the line shows.

import type { components } from '../api/schema';

export type ReachZone = components['schemas']['ReachZone'];

/** The zones the line names: the first `max` (the server sends the most nodes first), and how
 *  many more there are. */
export function zoneLine(
  zones: readonly ReachZone[],
  max = 3,
): { shown: ReachZone[]; more: number } {
  return { shown: zones.slice(0, max), more: Math.max(0, zones.length - max) };
}

/** What to ask the server for the rule's nodes: its folders and tags, or every node when the rule
 *  names none (an empty `reach` asks for every node). */
export function zoneQuery(v: {
  only: boolean;
  groupIds: readonly string[];
  tags: readonly string[];
  join: 'or' | 'and';
}): { tags: readonly string[]; groups: readonly string[]; join: 'or' | 'and'; everyNode: boolean } {
  const names = v.only && (v.groupIds.length > 0 || v.tags.length > 0);
  return names
    ? { tags: v.tags, groups: v.groupIds, join: v.join, everyNode: false }
    : { tags: [], groups: [], join: 'or', everyNode: true };
}
