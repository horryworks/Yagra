// SPDX-License-Identifier: AGPL-3.0-only
// Nodes ▸ Config backups as a list of every node that can be backed up (ADR-224): which of the
// three states a row is in, how many rows each holds, and what a switch would send. Pure, so the
// page stays a renderer and these rules are tested on their own.

import type { ConfigBackupRow } from '../types/api';

/** Where a node stands: fetched, switched off with its settings kept, or never switched on. */
export type RowState = 'on' | 'off' | 'never';
/** The list's filter: one state, or all three. */
export type ListFilter = RowState | 'all';
export const LIST_FILTERS: readonly ListFilter[] = ['on', 'off', 'never', 'all'];

export function stateOf(row: ConfigBackupRow): RowState {
  if (row.enabled) return 'on';
  return row.configured ? 'off' : 'never';
}

export function countsOf(rows: readonly ConfigBackupRow[]): Record<ListFilter, number> {
  const counts: Record<ListFilter, number> = { on: 0, off: 0, never: 0, all: rows.length };
  for (const r of rows) counts[stateOf(r)] += 1;
  return counts;
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
}

export function planSwitch(rows: readonly ConfigBackupRow[], enabled: boolean): SwitchPlan {
  if (!enabled) {
    return { ids: rows.filter((r) => r.enabled).map((r) => r.node_id), skipped: [], needsInput: false };
  }
  const off = rows.filter((r) => !r.enabled);
  const take = off.filter(canSwitchOn);
  return {
    ids: take.map((r) => r.node_id),
    skipped: off.filter((r) => !canSwitchOn(r)),
    needsInput: take.some((r) => !r.configured),
  };
}
