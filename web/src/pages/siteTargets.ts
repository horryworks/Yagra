// SPDX-License-Identifier: AGPL-3.0-only
// Turning a folder's IP prefixes into a discovery target spec (ADR-100 decision 10).
//
// Every judgement the Site picker makes lives here rather than in `DiscoveryPage.tsx`, because
// Vitest's `include` never loads a `.tsx` — a helper left there is a helper nothing runs
// (`tsxJudgement.test.ts`).
//
// ⚠️ **This module does not expand anything.** `lib/cidr.ts` is the one expander in the repository
// and it owns the 1024-address ceiling, the /22 shape limit and the network/broadcast rules. What
// is decided here is only *which prefixes are offered as text* and *what to say about the ones
// that are not*.

import { expandTargets } from '../lib/cidr';
import { groupPath } from '../lib/nodeTree';
import type { NodeGroup } from '../types/api';

/** The result of turning a folder's prefixes into something the target field can hold. */
export interface SiteTargetSpec {
  /** The comma-separated spec, ready to be written into the targets field. */
  spec: string;
  /** How many prefixes were left out because the sweep cannot expand IPv6. */
  skippedV6: number;
}

/** An IPv6 prefix, by the only mark that separates the two families in text form. */
function isV6(prefix: string): boolean {
  return prefix.includes(':');
}

/**
 * A folder's prefixes as a discovery target spec.
 *
 * ⚠️ **IPv6 prefixes are dropped, and the count is returned rather than swallowed.** `expandTargets`
 * is IPv4-only, and one unparseable token makes it reject the *whole* spec — so leaving a v6 prefix
 * in would turn "sweep this site" into "nothing happens", with the reason nowhere on screen. It is
 * also not a limitation worth removing here: a /64 has more addresses than there are seconds in the
 * age of the universe, so host enumeration is not the technique for it.
 */
export function prefixesToSpec(prefixes: readonly { prefix: string }[]): SiteTargetSpec {
  const v4 = prefixes.filter((p) => !isV6(p.prefix));
  return {
    spec: v4.map((p) => p.prefix).join(', '),
    skippedV6: prefixes.length - v4.length,
  };
}

/**
 * How many addresses a target spec covers, or `null` when the spec is not usable as it stands.
 *
 * `null` folds together every reason `expandTargets` returns nothing — malformed, wider than /22,
 * or past 1024 addresses in total — because the field's own error message already names all three
 * and a second wording of it would be a second thing to keep in step. What the caller does with
 * `null` is *not* show a count, which is the honest rendering of "this will not run".
 */
export function hostCount(spec: string): number | null {
  if (!spec.trim()) return null;
  const n = expandTargets(spec).length;
  return n === 0 ? null : n;
}

/** One folder offered by the Site picker. */
export interface SiteTargetOption {
  id: string;
  /** The folder's place in the tree, e.g. `Japan / Ehime / JPMYJ01 Matsuyama Home`. */
  label: string;
  /** Its prefixes, in the order the API returned them (network order). */
  prefixes: NodeGroup['prefixes'];
}

/**
 * The folders worth offering as a sweep target: the ones that actually carry a prefix.
 *
 * ⚠️ **Not "the NetBox folders".** Nothing in `GroupSummary` says a folder came from NetBox, by
 * design (migration 0102's header), and nothing should: the question the picker is asking is "does
 * this folder know which subnets are at it", and a prefix is the answer whoever wrote it. That also
 * means a Region is offered exactly when it has one of its own — which, in every NetBox seen so
 * far, is never.
 *
 * ⚠️ A folder whose prefixes are all IPv6 is still offered. Refusing it here would leave an
 * operator with a site they can see prefixes on and no entry in the list, and no explanation
 * anywhere; offering it puts the "N prefixes skipped" line in front of them instead.
 */
export function siteTargetOptions(groups: readonly NodeGroup[]): SiteTargetOption[] {
  const all = groups as NodeGroup[];
  return all
    .filter((g) => g.prefixes.length > 0)
    .map((g) => ({
      id: g.id,
      label: groupPath(all, g.id).join(' / '),
      prefixes: g.prefixes,
    }))
    .sort((a, b) => a.label.localeCompare(b.label));
}
