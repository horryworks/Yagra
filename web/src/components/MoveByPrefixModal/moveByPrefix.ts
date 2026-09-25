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
 * This is also the request body: `POST /api/v1/nodes/move-by-prefix` takes every destination at
 * once and writes them in one transaction (ADR-172 決定 2). It used to be one `moveNodes` call per
 * destination, which a closed tab could stop halfway.
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

/** One destination's result, as the server reports it. */
export interface DestinationResult {
  /** Rows the server moved. Lower than `requested` for a node deleted since the preview, or one
   *  outside the caller's scope. */
  moved: number;
  requested: number;
}

/** What to tell the operator after applying. The request is all or nothing, so a failure is an
 *  error and never reaches here — what can still fall short is a node that had gone. */
export interface ApplySummary {
  moved: number;
  requested: number;
  /** Whether everything asked for actually landed. */
  complete: boolean;
}

/** Fold the per-destination results into the one line the dialog shows. */
export function summarize(results: readonly DestinationResult[]): ApplySummary {
  const moved = results.reduce((n, r) => n + r.moved, 0);
  const requested = results.reduce((n, r) => n + r.requested, 0);
  return { moved, requested, complete: moved === requested && requested > 0 };
}
