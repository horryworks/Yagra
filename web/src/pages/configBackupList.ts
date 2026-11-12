// SPDX-License-Identifier: AGPL-3.0-only
// Nodes ▸ Config backups as a list of every node that can be backed up (ADR-224): which of the
// three states a row is in, how many rows each holds, and what a switch would send. Pure, so the
// page stays a renderer and these rules are tested on their own.

import type { ConfigBackupListState, ConfigBackupRow } from '../types/api';

/** The list's filter: one state, or all three — the server's `state` values, in display order. */
export const LIST_FILTERS = ['on', 'off', 'never', 'all'] as const satisfies readonly ConfigBackupListState[];
export type ListFilter = (typeof LIST_FILTERS)[number];
/** Where a node stands: fetched, switched off with its settings kept, or never switched on. */
export type RowState = Exclude<ListFilter, 'all'>;

export function stateOf(row: ConfigBackupRow): RowState {
  if (row.enabled) return 'on';
  return row.configured ? 'off' : 'never';
}

export function countsOf(rows: readonly ConfigBackupRow[]): Record<ListFilter, number> {
  const counts: Record<ListFilter, number> = { on: 0, off: 0, never: 0, all: rows.length };
  for (const r of rows) counts[stateOf(r)] += 1;
  return counts;
}

/** The view the list opens on until a person picks one: what is backed up, or everything while
 *  nothing is yet — an empty first view would hide the nodes there are to switch on. */
export function defaultFilter(counts: Record<ListFilter, number>): ListFilter {
  return counts.on > 0 || counts.all === 0 ? 'on' : 'all';
}

export function rowsIn(rows: readonly ConfigBackupRow[], filter: ListFilter): ConfigBackupRow[] {
  return filter === 'all' ? [...rows] : rows.filter((r) => stateOf(r) === filter);
}

/** Whether a row can be switched on from the list: it needs a model to be fetched with. */
export function canSwitchOn(row: ConfigBackupRow): boolean {
  return !row.enabled && row.model != null;
}

/** What switching `rows` on or off sends. `skipped` are the rows that cannot be switched on (no
 *  model); `needsInput` says the dialog must ask how the never-configured ones are reached. */
export interface SwitchPlan {
  ids: string[];
  skipped: ConfigBackupRow[];
  needsInput: boolean;
  /** How many of `ids` have settings of their own, and so keep their own logins (ADR-240 Inc.2). */
  keeping: number;
}

export function planSwitch(rows: readonly ConfigBackupRow[], enabled: boolean): SwitchPlan {
  if (!enabled) {
    return { ids: rows.filter((r) => r.enabled).map((r) => r.node_id), skipped: [], needsInput: false, keeping: 0 };
  }
  const off = rows.filter((r) => !r.enabled);
  const take = off.filter(canSwitchOn);
  return {
    ids: take.map((r) => r.node_id),
    skipped: off.filter((r) => !canSwitchOn(r)),
    needsInput: take.some((r) => !r.configured),
    keeping: take.filter((r) => r.configured).length,
  };
}
