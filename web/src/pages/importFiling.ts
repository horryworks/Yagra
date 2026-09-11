// SPDX-License-Identifier: AGPL-3.0-only
// Reading an import-by-IP-range proposal (ADR-131): where each candidate would land, and what to
// say about it afterwards.
//
// The server decides *which* folder claims each address — it has to, because a scoped caller is
// served breadcrumb folders with their prefixes cleared, so the same test done here would miss
// ranges (`api/groups.rs::visible_groups`). What is left is presentation, and it lives in a `.ts`
// because Vitest does not run `.tsx`.

import type { ImportPreview, ImportResult } from '../types/api';

/** What the server said about one address. */
export type RowDestination =
  /** Exactly one folder's range contains it. */
  | { kind: 'matched'; groupId: string; prefix: string }
  /** Two or more folders claim it at the same prefix length. Never resolved automatically. */
  | { kind: 'ambiguous'; groupIds: string[] }
  /** No folder's range contains it. */
  | { kind: 'unmatched' };

/** The three answers, as an iterable set.
 *
 * ⚠️ `as const` because each member has a `t()` key built at runtime
 * (`` t(`discovery.dest.why.${kind}`) ``). EN/JA parity passes when a member is missing from
 * *both* locales, so `i18nEnumKeys.test.ts` iterates this instead. */
export const DESTINATION_KINDS = ['matched', 'ambiguous', 'unmatched'] as const;

/** One of the three answers. */
export type DestinationKind = (typeof DESTINATION_KINDS)[number];

/**
 * The addresses still without an answer — what to ask the server for next.
 *
 * 🚨 **The candidate list grows while a sweep runs** (`DiscoveryPage` re-polls every 2s), so
 * re-previewing the whole set each tick would fire a `ManageConfig` request every two seconds for
 * the length of the scan. Asking only for what is new is the same shape `seedRows` already uses
 * for the editable row state, and for the same reason.
 */
export function pendingAddresses(
  known: ReadonlyMap<string, RowDestination>,
  candidates: readonly { address: string }[],
): string[] {
  const out: string[] = [];
  const seen = new Set<string>();
  for (const c of candidates) {
    if (known.has(c.address) || seen.has(c.address)) continue;
    seen.add(c.address);
    out.push(c.address);
  }
  return out;
}

/**
 * Fold a preview response into the running map, keyed by address.
 *
 * Returns a new map; the previous answers are kept, because a second request only ever covers
 * addresses the first did not.
 */
export function mergePreview(
  known: ReadonlyMap<string, RowDestination>,
  preview: ImportPreview,
): Map<string, RowDestination> {
  const next = new Map(known);
  for (const m of preview.matched) {
    next.set(m.address, { kind: 'matched', groupId: m.group_id, prefix: m.prefix });
  }
  for (const a of preview.ambiguous) {
    next.set(a.address, { kind: 'ambiguous', groupIds: a.group_ids });
  }
  for (const u of preview.unmatched) {
    next.set(u, { kind: 'unmatched' });
  }
  return next;
}

/** What one row's Folder cell says. */
export interface DestinationLabel {
  /** The folder the device would land in — always a real destination, never a dash. */
  primary: string;
  /** The i18n key for the muted second line, when there is something to explain. */
  whyKey?: string;
  /** Interpolation for `whyKey`. */
  whyArgs?: Record<string, unknown>;
}

/**
 * Where one candidate would land, given the fallback folder's path (`null` ⇒ the tree root).
 *
 * 🚨 **Three answers, not two.** "No folder's range covers this" and "two folders claim it equally
 * well" both end in the fallback, but they are different facts about the deployment and only the
 * second is actionable — it means two folders have overlapping ranges configured. Folding them
 * would tell the operator that an address is outside every range when it is inside two.
 *
 * `filing` off ⇒ every row reads the same fallback, which is deliberate: it answers "where does
 * this land" *before* the button rather than in the message afterwards.
 */
export function destinationLabel(
  dest: RowDestination | undefined,
  opts: {
    filing: boolean;
    fallbackPath: string | null;
    rootLabel: string;
    pendingLabel: string;
    pathOf: (groupId: string) => string;
  },
): DestinationLabel {
  const fallback = opts.fallbackPath ?? opts.rootLabel;
  if (!opts.filing) return { primary: fallback };
  if (!dest) return { primary: opts.pendingLabel };
  switch (dest.kind) {
    case 'matched':
      return {
        primary: opts.pathOf(dest.groupId),
        whyKey: 'discovery.dest.why.matched',
        whyArgs: { prefix: dest.prefix },
      };
    case 'ambiguous':
      return {
        primary: fallback,
        whyKey: 'discovery.dest.why.ambiguous',
        whyArgs: { count: dest.groupIds.length },
      };
    case 'unmatched':
      return { primary: fallback, whyKey: 'discovery.dest.why.unmatched' };
    default: {
      // Exhaustive: a new kind is a compile error rather than a silently blank cell.
      const unhandled: never = dest;
      return unhandled;
    }
  }
}

/** One sentence of the post-import message. */
export interface ImportMessagePart {
  key: string;
  args: Record<string, unknown>;
}

/**
 * What to tell the operator after an import.
 *
 * 🚨 **The ambiguity sentence is separate and only appears when there is one.** Reporting
 * "3 fell back" without saying that one of them was contested reads as three addresses outside
 * every range — a different, and untrue, statement about their network.
 */
export function importMessage(
  result: ImportResult,
  sitePath: string | null,
): ImportMessagePart[] {
  const created = result.created;
  const filed = result.filed;
  if (!filed) {
    return [
      sitePath
        ? { key: 'discovery.msg.importedInto', args: { count: created, site: sitePath } }
        : { key: 'discovery.msg.imported', args: { count: created } },
    ];
  }
  const fellBack = filed.ambiguous + filed.unmatched;
  const parts: ImportMessagePart[] = [
    { key: 'discovery.msg.importedFiled', args: { count: created, filed: filed.matched } },
  ];
  if (fellBack > 0) {
    parts.push(
      sitePath
        ? { key: 'discovery.msg.fellBackInto', args: { count: fellBack, site: sitePath } }
        : { key: 'discovery.msg.fellBackToRoot', args: { count: fellBack } },
    );
  }
  if (filed.ambiguous > 0) {
    parts.push({ key: 'discovery.msg.contested', args: { count: filed.ambiguous } });
  }
  return parts;
}
