// SPDX-License-Identifier: AGPL-3.0-only
// Classifying each node by how the hand-authored dependency graph and the derived one differ.
//
// Pure and in a `.ts` on purpose: Vitest runs with `environment: 'node'` and
// `include: ['src/**/*.test.ts']`, so a test written in a `.tsx` is a file nothing runs. The
// judgement lives here; `DependencyPage.tsx` keeps only markup.
//
// The four verdicts are the operator's whole question when deciding whether to enable derived
// suppression, and they are deliberately not symmetric:
//
//   agree         both graphs give the node the same upstreams — nothing changes for it.
//   only_manual   an upstream exists by hand that the derivation did not find. Enabling `derived`
//                 *loses* this edge, so an alert that used to roll up will start standing alone.
//   only_derived  the derivation found an upstream nobody typed in. Enabling `derived` *gains* it,
//                 which is the direction that can suppress something real.
//   unmodelled    neither graph gives the node an upstream. Not a disagreement — but it is what an
//                 operator most needs to see, because it is the state the whole fleet was in.

import type { TopologyShadow } from '../types/api';

/** How one node's upstreams compare between the two graphs. */
export const DIFF_VERDICTS = ['agree', 'only_manual', 'only_derived', 'unmodelled'] as const;
export type DiffVerdict = (typeof DIFF_VERDICTS)[number];

/** One node's row in the comparison. */
export interface DiffRow {
  nodeId: string;
  verdict: DiffVerdict;
  /** Upstreams only the hand-authored graph has. */
  manualOnly: string[];
  /** Upstreams only the derived graph has. */
  derivedOnly: string[];
}

/**
 * Classify every node the shadow response mentions.
 *
 * `nodeIds` is the full inventory, so a node that appears in neither difference list is still
 * classified — the alternative would silently drop exactly the nodes that agree, which is most of
 * them, and leave the page looking like the graphs disagree everywhere.
 *
 * `parented` names the nodes that have *some* upstream in either graph. Without it a node both
 * graphs agree on cannot be told apart from a node neither graph has an opinion about, and those
 * are opposite states: the first is modelled, the second is the gap.
 */
export function classifyNodes(
  shadow: Pick<TopologyShadow, 'only_in_manual' | 'only_in_derived'>,
  nodeIds: string[],
  parented: ReadonlySet<string>,
): DiffRow[] {
  const manualOnly = new Map<string, string[]>();
  const derivedOnly = new Map<string, string[]>();
  for (const e of shadow.only_in_manual ?? []) {
    manualOnly.set(e.child, [...(manualOnly.get(e.child) ?? []), e.parent]);
  }
  for (const e of shadow.only_in_derived ?? []) {
    derivedOnly.set(e.child, [...(derivedOnly.get(e.child) ?? []), e.parent]);
  }
  return nodeIds.map((nodeId) => {
    const m = manualOnly.get(nodeId) ?? [];
    const d = derivedOnly.get(nodeId) ?? [];
    let verdict: DiffVerdict;
    if (m.length > 0 && d.length > 0) {
      // Both directions differ. Reported as `only_derived` because that is the direction that
      // matters for the decision being made: a gained edge can suppress a real outage, a lost one
      // can only make noise, and a row that says both says neither.
      verdict = 'only_derived';
    } else if (m.length > 0) {
      verdict = 'only_manual';
    } else if (d.length > 0) {
      verdict = 'only_derived';
    } else {
      verdict = parented.has(nodeId) ? 'agree' : 'unmodelled';
    }
    return { nodeId, verdict, manualOnly: m, derivedOnly: d };
  });
}

/**
 * Whether the deployment may move to `derived`.
 *
 * Mirrors the server's precondition rather than adding a second rule: the server refuses with 409
 * when a pool that has nodes has an unplaced poller, and the button being enabled while the call
 * would fail is a worse experience than the button being off with the reason next to it.
 */
export function canEnableDerived(shadow: Pick<TopologyShadow, 'unresolved_pools'>): boolean {
  return (shadow.unresolved_pools ?? []).length === 0;
}
