// SPDX-License-Identifier: AGPL-3.0-only
// Reading an IP-range proposal (ADR-124 決定 6): what it means, and how it becomes moves.
//
// The server decides *which* folder claims each address — it has to, because a scoped caller is
// served breadcrumb folders with their prefixes cleared, so the same test done here would miss
// ranges (`api/groups.rs::visible_groups`). What is left is presentation, and it lives in a `.ts`
// because Vitest does not run `.tsx`.
//
// Two endpoints answer the question (ADR-176): one for the nodes the operator selected, one for a
// folder's subtree or the whole inventory. `fromSelection` / `fromSubtree` turn both into one
// `Proposal`, so everything below reads a single shape.

import type { MovePreview, SubtreeMovePreview } from '../../types/api';

/** Why a proposal has nothing to move. */
export const EMPTY_REASONS = [
  /** No folder this caller can see carries an IP range at all — nothing to match against. */
  'noPrefixes',
  /** Ranges exist, but no node's address is inside one of them. */
  'noMatch',
  /** Every node that matched was claimed by two or more folders at the same prefix length. */
  'allAmbiguous',
  /** Every node that matched is already in its folder or beneath it (ADR-176 決定 2). */
  'allInPlace',
] as const;
export type EmptyReason = (typeof EMPTY_REASONS)[number];

/** One proposed move, as both endpoints spell it. */
export type Proposal = MovePreview['matched'][number];
export type Ambiguity = MovePreview['ambiguous'][number];

/** Both previews, read as one. The lists may be a slice; the totals never are. */
export interface ProposalView {
  matched: Proposal[];
  matchedTotal: number;
  ambiguous: Ambiguity[];
  ambiguousTotal: number;
  unmatched: string[];
  unmatchedTotal: number;
  inPlaceTotal: number;
  anyPrefixes: boolean;
}

/** The preview of a selection: every list is whole. */
export function fromSelection(p: MovePreview): ProposalView {
  return {
    matched: p.matched,
    matchedTotal: p.matched.length,
    ambiguous: p.ambiguous,
    ambiguousTotal: p.ambiguous.length,
    unmatched: p.unmatched,
    unmatchedTotal: p.unmatched.length,
    inPlaceTotal: p.in_place.length,
    anyPrefixes: p.any_prefixes,
  };
}

/** The preview of a subtree: the server sliced the lists and says how many there were. */
export function fromSubtree(p: SubtreeMovePreview): ProposalView {
  return {
    matched: p.matched,
    matchedTotal: p.matched_total,
    ambiguous: p.ambiguous,
    ambiguousTotal: p.ambiguous_total,
    unmatched: p.unmatched,
    unmatchedTotal: p.unmatched_total,
    inPlaceTotal: p.in_place_total,
    anyPrefixes: p.any_prefixes,
  };
}

/**
 * Why there is nothing to move, or null when there is something.
 *
 * 🚨 **Separate answers, not one message.** "Nothing matched" and "there was never anything to
 * match against" look identical to an operator and mean opposite things: the first is about their
 * addresses, the second means the feature has no data behind it in this deployment. Folding them
 * would ship something inert that looks like it is working — which is the failure ADR-100 paid for
 * twice inside one ADR. "Already in place" is the good version of zero, and says so.
 */
export function emptyReason(view: ProposalView): EmptyReason | null {
  if (view.matchedTotal > 0) return null;
  if (!view.anyPrefixes) return 'noPrefixes';
  if (view.unmatchedTotal === 0 && view.ambiguousTotal === 0 && view.inPlaceTotal > 0) {
    return 'allInPlace';
  }
  // Ambiguity is only the whole story when nothing *also* failed to match; a mixture is more
  // honestly described as "nothing landed on one folder".
  if (view.ambiguousTotal > 0 && view.unmatchedTotal === 0) return 'allAmbiguous';
  return 'noMatch';
}

/**
 * How many proposals this round left for the next one — the ones past what one request may carry
 * (ADR-176 決定 4). The moved nodes are in place afterwards, so asking again returns exactly these.
 */
export function remainingAfterApply(view: ProposalView): number {
  return Math.max(0, view.matchedTotal - view.matched.length);
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
export function byDestination(view: Pick<ProposalView, 'matched'>): Destination[] {
  const out: Destination[] = [];
  const index = new Map<string, Destination>();
  for (const p of view.matched) {
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
