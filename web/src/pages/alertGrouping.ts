// SPDX-License-Identifier: AGPL-3.0-only
// The judgement behind Alerts ▸ Notification delivery's grouping section (ADR-236): what each level
// would do to the outage open now, and the folder rows in the order they are drawn.

import {
  GROUPING_LEVELS,
  type AlertGroupingConfig,
  type GroupingLevel,
  type NodeGroup,
} from '../types/api';

/** How many of the nodes down now would page on their own at `level`. The server lists the counts
 *  weakest level first, in `GROUPING_LEVELS` order; a list that came back short answers "all of
 *  them", which is what grouping nothing means. */
export function notifiedAt(cfg: AlertGroupingConfig, level: GroupingLevel): number {
  return cfg.now.notified[GROUPING_LEVELS.indexOf(level)] ?? cfg.now.down;
}

/** One folder with a level of its own, named. */
export interface FolderLevelRow {
  groupId: string;
  name: string;
  level: GroupingLevel;
}

/** The folders that set their own level, by name. A folder deleted since keeps no row on the
 *  server (its level goes with it), so an unknown id is shown by its id rather than dropped. */
export function folderLevelRows(cfg: AlertGroupingConfig, groups: NodeGroup[]): FolderLevelRow[] {
  const names = new Map(groups.map((g) => [g.id, g.name]));
  return cfg.folders
    .map((f) => ({ groupId: f.group_id, name: names.get(f.group_id) ?? f.group_id, level: f.level }))
    .sort((a, b) => a.name.localeCompare(b.name));
}

/** Whether a string the switch handed back is a level. */
export function isGroupingLevel(v: string): v is GroupingLevel {
  return (GROUPING_LEVELS as readonly string[]).includes(v);
}
