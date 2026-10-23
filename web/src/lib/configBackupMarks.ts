// SPDX-License-Identifier: AGPL-3.0-only
// Which nodes have their configuration backed up, and whether that is going well (ADR-210 stage 3):
// the mark the inventory tree draws on a node's row and the state column of Nodes ▸ Config backups.
// Both read `GET /api/v1/config-backups`, so the two cannot disagree.

import type { ConfigBackupRow } from '../types/api';

/** `ok`: a login works and the last fetch succeeded. `busy`: a fetch is out, or nothing has been
 *  tried yet. `trouble`: the last attempt failed, no login has been found, or no poller can carry
 *  the login (ADR-210 stage 5). */
export const BACKUP_MARKS = ['ok', 'busy', 'trouble'] as const;
export type BackupMark = (typeof BACKUP_MARKS)[number];

export function markOf(row: ConfigBackupRow): BackupMark {
  if (row.poller !== 'ready') return 'trouble';
  if (row.pending) return 'busy';
  if (!row.last) return 'busy';
  if (row.login_confirmed && row.last.outcome === 'success') return 'ok';
  return 'trouble';
}

/** The words the list's state column shows: the mark, or why no poller can carry the login — the
 *  one case where "needs attention" alone would send someone to the wrong screen. */
export function stateKeyOf(row: ConfigBackupRow): string {
  switch (row.poller) {
    case 'ready':
      return `config.list.mark.${markOf(row)}`;
    case 'no_poller':
    case 'poller_too_old':
      return `config.readiness.${row.poller}`;
  }
}

/** Every listed node's mark, by node id. */
export function marksOf(rows: readonly ConfigBackupRow[]): Map<string, BackupMark> {
  return new Map(rows.map((r) => [r.node_id, markOf(r)]));
}
