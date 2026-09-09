// SPDX-License-Identifier: AGPL-3.0-only
// Reading an IP-range proposal (ADR-124 決定 6): what it means, and how it becomes moves.
//
// The server decides *which* folder claims each address — it has to, because a scoped caller is
// served breadcrumb folders with their prefixes cleared, so the same test done here would miss
// ranges (`api/groups.rs::visible_groups`). What is left is presentation, and it lives in a `.ts`
// because Vitest does not run `.tsx`.

import type { MovePreview } from '../../types/api';

/** Why a proposal has nothing to move. */
export type EmptyReason =
  /** No folder this caller can see carries an IP range at all — nothing to match against. */
  | 'noPrefixes'
  /** Ranges exist, but no node's address is inside one of them. */
  | 'noMatch'
  /** Every node that matched was claimed by two or more folders at the same prefix length. */
  | 'allAmbiguous';

/**
 * Why there is nothing to move, or null when there is something.
 *
 * 🚨 **Three answers, not one message.** "Nothing matched" and "there was never anything to match
 * against" look identical to an operator and mean opposite things: the first is about their
 * addresses, the second means the feature has no data behind it in this deployment. Folding them
 * would ship something inert that looks like it is working — which is the failure ADR-100 paid for
 * twice inside one ADR.
 */
export function emptyReason(preview: MovePreview): EmptyReason | null {
  if (preview.matched.length > 0) return null;
  if (!preview.any_prefixes) return 'noPrefixes';
  // Ambiguity is only the whole story when nothing *also* failed to match; a mixture is more
  // honestly described as "nothing landed on one folder".
  if (preview.ambiguous.length > 0 && preview.unmatched.length === 0) return 'allAmbiguous';
  return 'noMatch';
}

/** The proposals collected per destination folder — one bulk request each. */
export interface Destination {
  groupId: string;
  nodeIds: string[];
}

/**
 * Group the proposals by the folder they would go to, in the order the folders first appear.
 *
 * This is also the request plan: one `moveNodes` call per destination, because the endpoint moves
 * a set of nodes into **one** folder. ⚠️ That makes a multi-destination apply non-atomic — a
 * failure partway leaves the earlier folders' nodes moved — which is why the caller reports what
 * landed rather than claiming all or nothing.
 */
export function byDestination(preview: MovePreview): Destination[] {
  const out: Destination[] = [];
  const index = new Map<string, Destination>();
  for (const p of preview.matched) {
    let dest = index.get(p.group_id);
    if (!dest) {
      dest = { groupId: p.group_id, nodeIds: [] };
      index.set(p.group_id, dest);
      out.push(dest);
    }
    dest.nodeIds.push(p.node_id);
  }
  return out;
}

/** One destination's result, as the apply loop collects them. */
export interface DestinationResult {
  groupId: string;
  /** Rows the server said it moved, or 0 when the request itself failed. */
  moved: number;
  requested: number;
  failed: boolean;
}

/** What to tell the operator after applying. */
export interface ApplySummary {
  moved: number;
  requested: number;
  /** Destinations whose request failed outright — named, because "12 of 14" without saying which
   *  two leaves the operator to diff the tree by eye. */
  failedGroups: string[];
  /** Whether everything asked for actually landed. */
  complete: boolean;
}

/** Fold the per-destination results into the one line the dialog shows. */
export function summarize(results: readonly DestinationResult[]): ApplySummary {
  const moved = results.reduce((n, r) => n + r.moved, 0);
  const requested = results.reduce((n, r) => n + r.requested, 0);
  const failedGroups = results.filter((r) => r.failed).map((r) => r.groupId);
  return { moved, requested, failedGroups, complete: moved === requested && requested > 0 };
}
