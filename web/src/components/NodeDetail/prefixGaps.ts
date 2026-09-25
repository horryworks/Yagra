// SPDX-License-Identifier: AGPL-3.0-only
// What the folder pane says about the subnets its devices carry and its IP prefixes do not cover
// (ADR-170). The judgement lives here, in a `.ts`, so Vitest reaches it; `PrefixGapsSection.tsx` only
// lays it out.

import type { PrefixGap, PrefixGapReport } from '../../types/api';

export type { PrefixGap, PrefixGapReport };

/** Why a subnet is reported, in the server's order. ⚠️ **Keep the array on one line**:
 *  `yagra-core`'s `prefix_gaps.rs::the_webuis_kind_list_is_this_enum_in_order` reads it. The label
 *  is built at runtime (`` t(`prefixGaps.kind.${kind}`) ``), so `i18nEnumKeys.test.ts` iterates it. */
export const PREFIX_GAP_KINDS = ['unregistered', 'partial', 'other_folder', 'parent_only'] as const;

export type PrefixGapKind = (typeof PREFIX_GAP_KINDS)[number];

/** What the section says above its list. */
export type GapSummary =
  /** No device here has reported its addresses: nothing was compared, so no claim is made. */
  | { kind: 'noData'; total: number }
  /** Everything compared is covered. Says how much was compared, because a device that reported
   *  nothing is not in it. */
  | { kind: 'clean'; read: number; total: number }
  | { kind: 'gaps'; count: number; read: number; total: number };

export function summarizeGaps(report: PrefixGapReport): GapSummary {
  const read = report.nodes_with_addresses;
  const total = report.nodes_total;
  if (read === 0) return { kind: 'noData', total };
  if (report.gaps.length === 0) return { kind: 'clean', read, total };
  return { kind: 'gaps', count: report.gaps.length, read, total };
}

/** The i18n key and values for one gap's reason line.
 *
 *  A range withheld by scope arrives as `range: null` on an `other_folder` or `parent_only` gap —
 *  the server knows a folder claims it and will not say which. That reads "another folder" /
 *  "a parent folder", never as `unregistered`: telling the operator to register a subnet another
 *  folder already holds would create the duplicate. */
export function gapReason(gap: PrefixGap): { key: string; values: Record<string, string> } {
  const name = gap.range_group_name ?? '';
  const range = gap.range ?? '';
  switch (gap.kind) {
    case 'unregistered':
      return { key: 'prefixGaps.kind.unregistered', values: {} };
    case 'partial':
      return range
        ? { key: 'prefixGaps.reason.partial', values: { range } }
        : { key: 'prefixGaps.kind.partial', values: {} };
    case 'other_folder':
      return name && range
        ? { key: 'prefixGaps.reason.otherFolder', values: { name, range } }
        : { key: 'prefixGaps.kind.other_folder', values: {} };
    case 'parent_only':
      return name && range
        ? { key: 'prefixGaps.reason.parentOnly', values: { name, range } }
        : { key: 'prefixGaps.kind.parent_only', values: {} };
    default: {
      const unreachable: never = gap.kind;
      return unreachable;
    }
  }
}

/** How many places beyond the listed ones carry the subnet. */
export function moreDevices(gap: PrefixGap): number {
  const listed = new Set(gap.seen_on.map((s) => s.node_id)).size;
  return Math.max(0, gap.node_count - listed);
}
