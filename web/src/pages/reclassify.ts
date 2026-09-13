// SPDX-License-Identifier: AGPL-3.0-only
// The judgement behind Nodes ▸ Reclassify (ADR-140), in a `.ts` so a test can reach it — Vitest never
// runs a `.tsx` (`testing.md`). `ReclassifyPage.tsx` is layout plus the calls.

import type { ReclassifyProposal } from '../types/api';

/** One change for `POST /api/v1/reclassify/apply`. */
export interface ReclassifyApplyItem {
  node_id: string;
  from_profile_id: string | null;
  to_profile_id: string;
}

/** The apply body for the selected rows.
 *
 *  Each item echoes the profile the screen SHOWED the node on, not just where it should go. That is
 *  what lets the server skip a node someone re-profiled by hand after this page loaded, instead of
 *  writing the rules' choice over that newer human decision. */
export function applyItems(
  proposals: readonly ReclassifyProposal[],
  selected: ReadonlySet<string>,
): ReclassifyApplyItem[] {
  return proposals
    .filter((p) => selected.has(p.node_id))
    .map((p) => ({
      node_id: p.node_id,
      from_profile_id: p.current_profile_id ?? null,
      to_profile_id: p.suggested_profile_id,
    }));
}

/** The selection after a reload: only nodes still listed stay selected.
 *
 *  A node that left the list — applied, locked, or re-profiled elsewhere — must not stay selected
 *  where nobody can see it, or the next apply would carry a row the operator can no longer inspect. */
export function pruneSelection(
  selected: ReadonlySet<string>,
  proposals: readonly ReclassifyProposal[],
): ReadonlySet<string> {
  const listed = new Set(proposals.map((p) => p.node_id));
  return new Set([...selected].filter((id) => listed.has(id)));
}

/** The rule that chose a proposal, written the way its two matchers combine — `prefix + regex`, or
 *  either alone. `null` when no rule matched and the device fell through to Generic SNMP, which the
 *  screen says in words rather than leaving the cell blank. */
export function ruleSignature(p: ReclassifyProposal): string | null {
  const r = p.rule;
  if (!r) return null;
  const parts = [r.sysobjectid_prefix, r.sysdescr_regex].filter(
    (s): s is string => typeof s === 'string' && s !== '',
  );
  return parts.length > 0 ? parts.join(' + ') : null;
}
