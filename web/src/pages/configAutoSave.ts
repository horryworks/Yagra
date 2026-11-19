// SPDX-License-Identifier: AGPL-3.0-only
// Automatic save per node (ADR-222 Inc.11): which nodes can take the switch, what switching many
// sends, and how the last automatic save reads. Shared by Nodes ▸ Config backups and the node's
// Config tab, so the two cannot disagree. Pure, so the rules are tested on their own.

import type { ConfigBackupRow, SaveRecord } from '../types/api';

/** What decides whether a node can take the switch: backed up now, with a model Yagra can save
 *  (decisions 15 and 16). Both the list row and the node's view carry these two. */
interface Switchable {
  enabled: boolean;
  save_supported: boolean;
}

/** Whether automatic save can be switched on or off for this node. A node that cannot shows "—". */
export function canAutoSave(row: Switchable): boolean {
  return row.enabled && row.save_supported;
}

/** The nodes switching `rows` to `on` would change: off ones that can take it, or on ones. */
export function autoSaveTargets(rows: readonly ConfigBackupRow[], on: boolean): string[] {
  return rows.filter((r) => canAutoSave(r) && r.auto_save !== on).map((r) => r.node_id);
}

/** The node's newest automatic save, from its saves listed newest first. */
export function lastAutoSave(saves: readonly SaveRecord[]): SaveRecord | null {
  return saves.find((s) => s.origin === 'auto') ?? null;
}

/** Whether the node is switched on and its newest automatic save failed: the list marks it as
 *  needing attention (decision 20). It is tried again after the next fetch (decision 21). */
export function autoSaveFailed(row: { auto_save: boolean; last_auto_save?: SaveRecord | null }): boolean {
  return row.auto_save && row.last_auto_save?.state === 'failed';
}

/** How the last automatic save reads beside the switch. `at` is when it ended, or was asked for. */
export type AutoSaveResult =
  | { kind: 'none' }
  | { kind: 'saved'; at: string }
  | { kind: 'failed'; at: string; failure: NonNullable<SaveRecord['failure']> | 'other' }
  | { kind: 'busy' };

export function autoSaveResult(save: SaveRecord | null | undefined): AutoSaveResult {
  if (!save) return { kind: 'none' };
  const at = save.finished_at ?? save.requested_at;
  switch (save.state) {
    case 'saved':
      return { kind: 'saved', at };
    case 'failed':
      return { kind: 'failed', at, failure: save.failure ?? 'other' };
    case 'pending':
    case 'running':
      return { kind: 'busy' };
  }
}
