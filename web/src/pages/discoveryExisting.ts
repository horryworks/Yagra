// SPDX-License-Identifier: AGPL-3.0-only
// Which discovered devices are already in the node tree, and what that means for the import
// (ADR-139).
//
// The server decides the match — it has to, because a device node may sit in a folder this caller
// cannot see, and the browser holds no fleet-wide list of addresses to test against anyway. What is
// left is reading its answer, and that lives in a `.ts` because Vitest does not run `.tsx`.
//
// 🚨 **This replaced a page-memory `imported` map, and the difference is the feature.** That map
// remembered the rows imported during one visit, so reloading the page — or picking another sweep
// and coming back — offered the same devices for import again, and the server created a second
// node at the same address every time.

import type { InventoryMatch, SameDeviceMatch } from '../types/api';

/** The server's answer, keyed by the candidate's own address string.
 *
 *  `undefined` is what a core that predates the list sends. Nothing is marked then, which is the
 *  honest reading — and the import still refuses a taken address server-side, so the fallback is a
 *  less helpful screen, not a duplicate. */
export function existingByAddress(
  existing: readonly InventoryMatch[] | undefined,
): Map<string, InventoryMatch> {
  const out = new Map<string, InventoryMatch>();
  for (const m of existing ?? []) out.set(m.address, m);
  return out;
}

/** Whether a candidate may be offered for import: no device node stands at its address, whether or
 *  not the caller can see that node. */
export function isImportable(
  address: string,
  existing: ReadonlyMap<string, InventoryMatch>,
): boolean {
  return !existing.has(address);
}

/** The candidates still worth asking the server about — for the folder preview, which has nothing
 *  to say about a device that is not going to be imported. */
export function importableCandidates<C extends { address: string }>(
  candidates: readonly C[],
  existing: ReadonlyMap<string, InventoryMatch>,
): C[] {
  return candidates.filter((c) => isImportable(c.address, existing));
}

/** The candidates the Import button sends, in candidate order: ticked, and not already in the tree.
 *
 *  ⚠️ **A row ticked before the server marked it is not sent.** A sweep keeps polling while it runs
 *  and the operator can tick a row between two polls; the checkbox then disappears under them, and
 *  counting the stale tick would make "Import 3 selected" send two. */
export function selectedForImport<C extends { address: string }>(
  candidates: readonly C[],
  rows: Readonly<Record<string, { selected: boolean } | undefined>>,
  existing: ReadonlyMap<string, InventoryMatch>,
): C[] {
  return candidates.filter((c) => rows[c.address]?.selected && isImportable(c.address, existing));
}

/** Candidates that look like a device node monitored at **another** address (ADR-139 Inc.3), keyed
 *  by the candidate's own address string.
 *
 *  A mark only: such a row stays importable, because a site that reuses one private address plan
 *  can make the evidence wrong. `undefined` is what a core that predates the list sends. */
export function sameDeviceByAddress(
  sameDevice: readonly SameDeviceMatch[] | undefined,
): Map<string, SameDeviceMatch> {
  const out = new Map<string, SameDeviceMatch>();
  for (const m of sameDevice ?? []) out.set(m.address, m);
  return out;
}

type EvidenceKind = SameDeviceMatch['nodes'][number]['evidence'][number];

/** The sentence key for one piece of evidence, or `null` for a kind a candidate is never judged by
 *  (the server only sends the two below — the rest belong to Nodes ▸ Duplicates). */
export function sameDeviceReasonKey(kind: EvidenceKind): string | null {
  switch (kind) {
    case 'own_ip_one_way':
      return 'discovery.sameDevice.reason.ownIp';
    case 'name':
      return 'discovery.sameDevice.reason.name';
    case 'address':
    case 'serial':
    case 'own_ip':
    case 'arp_mac':
    case 'lldp_chassis':
    case 'cdp_device_id':
      return null;
    default: {
      // A kind a newer core added reaches an open tab on this bundle (N-1): no sentence, rather
      // than the raw token handed to `t()` as a key.
      const unhandled: never = kind;
      void unhandled;
      return null;
    }
  }
}

/** The badge a marked row carries: the most convincing of its nodes decides it. */
export function sameDeviceBadgeKey(m: SameDeviceMatch): string {
  return m.nodes.some((n) => n.confidence === 'confident')
    ? 'discovery.sameDevice.likely'
    : 'discovery.sameDevice.maybe';
}
