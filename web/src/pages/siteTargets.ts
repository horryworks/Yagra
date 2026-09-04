// SPDX-License-Identifier: AGPL-3.0-only
// Turning a folder's IP prefixes into a discovery target spec (ADR-100 decision 10).
//
// Every judgement the Site picker makes lives here rather than in `DiscoveryPage.tsx`, because
// Vitest's `include` never loads a `.tsx` — a helper left there is a helper nothing runs
// (`tsxJudgement.test.ts`).
//
// ⚠️ **This module does not expand anything.** `lib/cidr.ts` is the one expander in the repository
// and it owns the 1024-address ceiling, the /22 shape limit and the network/broadcast rules. What
// is decided here is only *which prefixes can be offered*, *how many addresses each covers* and
// *what to say about the ones that cannot be swept*.

import { expandCidr, expandTargets } from '../lib/cidr';
import { groupPath } from '../lib/nodeTree';
import type { NodeGroup } from '../types/api';

/** The most addresses one sweep may carry (`MAX_SCAN_TARGETS` in `api/discovery.rs`). */
export const SWEEP_LIMIT = 1024;

/** Why a prefix cannot be swept. Both members have a `t()` key, so the array is what the i18n
 *  coverage test iterates (`extensibility.md` §4). */
export const UNSWEEPABLE_REASONS = ['v6', 'tooLarge'] as const;
export type UnsweepableReason = (typeof UNSWEEPABLE_REASONS)[number];

/** One prefix, as the picker draws it. */
export interface PrefixRow {
  prefix: string;
  description: string;
  /** Addresses a sweep of this prefix alone would try. Always a real number, even for a prefix
   *  too large to sweep — a row that says "8,388,606 addresses, too large" explains itself, and
   *  one that says nothing does not. */
  hosts: number;
  /** Why this row has no checkbox, or `undefined` when it has one. */
  unsweepable?: UnsweepableReason;
}

/** An IPv6 prefix, by the only mark that separates the two families in text form. */
function isV6(prefix: string): boolean {
  return prefix.includes(':');
}

/**
 * How many addresses a single IPv4 prefix covers.
 *
 * ⚠️ **Arithmetic, and it must agree with the expander — which is what the test pins.** This is a
 * *display* number and needs to exist for a prefix `expandCidr` refuses (a /8 covers eight million
 * addresses; that is the fact the row is there to state). Computing it by expanding would return
 * zero for exactly the rows that need a number most. The rules it mirrors are `lib/cidr.ts`'s:
 * network and broadcast are skipped, except on a /31 and /32 where every address counts.
 */
function hostsIn(prefix: string): number {
  const bits = Number(prefix.split('/')[1]);
  if (!Number.isInteger(bits) || bits < 0 || bits > 32) return 0;
  const hostBits = 32 - bits;
  const total = 2 ** hostBits;
  return hostBits <= 1 ? total : total - 2;
}

/**
 * A folder's prefixes as rows the picker can draw.
 *
 * The two things a row cannot be swept for are kept apart because they are different problems with
 * different answers: an IPv6 prefix will never be sweepable (a sweep tries every address, and a
 * /64 has more than there are stars), while one that is merely too large is a range the operator
 * could narrow in NetBox. Folding them into "unsupported" would tell someone to go and fix the
 * unfixable one.
 */
export function prefixRows(prefixes: readonly { prefix: string; description: string }[]): PrefixRow[] {
  return prefixes.map((p) => {
    const row: PrefixRow = {
      prefix: p.prefix,
      description: p.description,
      hosts: isV6(p.prefix) ? 0 : hostsIn(p.prefix),
    };
    if (isV6(p.prefix)) row.unsweepable = 'v6';
    // The expander is the authority on what can actually be sent, so it is asked rather than
    // re-derived: a shape rule added there must not need a second edit here to take effect.
    else if (expandCidr(p.prefix).length === 0) row.unsweepable = 'tooLarge';
    return row;
  });
}

/** Every prefix a sweep could take — what the picker ticks when a site is chosen. */
export function defaultChecked(rows: readonly PrefixRow[]): Set<string> {
  return new Set(rows.filter((r) => !r.unsweepable).map((r) => r.prefix));
}

/**
 * The target spec for a set of ticked prefixes.
 *
 * Built from `rows` rather than from the set itself so the order on the wire is the order on
 * screen, and so a stale tick — a prefix that left NetBox between the pick and the press — cannot
 * reach the sweep.
 */
export function specFor(rows: readonly PrefixRow[], checked: ReadonlySet<string>): string {
  return rows
    .filter((r) => !r.unsweepable && checked.has(r.prefix))
    .map((r) => r.prefix)
    .join(', ');
}

/** Addresses the ticked rows cover, before de-duplication. */
export function sumHosts(rows: readonly PrefixRow[], checked: ReadonlySet<string>): number {
  return rows
    .filter((r) => !r.unsweepable && checked.has(r.prefix))
    .reduce((n, r) => n + r.hosts, 0);
}

/**
 * How many addresses a target spec covers, or `null` when it is not usable as it stands.
 *
 * `null` folds together every reason `expandTargets` returns nothing — malformed, wider than /22,
 * or past the sweep limit in total — because the free-text field's own error message already names
 * all three and a second wording of it would be a second thing to keep in step.
 *
 * ⚠️ Exact where it matters: unlike [`sumHosts`] it de-duplicates, so two overlapping prefixes at
 * one site are counted once. That is why the picker prefers this number and falls back to the sum
 * only when the spec is refused outright.
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
 * anywhere; offering it puts the rows and their reasons in front of them instead.
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
