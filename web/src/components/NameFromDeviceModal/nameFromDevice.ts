// SPDX-License-Identifier: AGPL-3.0-only
// What the "name from device" dialog shows and sends (ADR-208 Inc.2).
//
// Pure, so the node-environment Vitest reaches it: `NameFromDeviceModal.tsx` is the wiring. The
// preview comes from the server — the tree's rows do not carry a node's `sysName` — and the apply
// echoes each row back exactly as it was shown, so the server can leave a node that moved since.
import type { NameFromDeviceBody, NameFromDevicePreview } from '../../types/api';

/** How many rows the dialog lists before folding the rest into a count. */
export const NAME_FROM_DEVICE_SHOWN = 8;

/** The rows to list, and how many more there are beyond them. */
export function shownRows(p: NameFromDevicePreview): {
  rows: NameFromDevicePreview['changes'];
  more: number;
} {
  return {
    rows: p.changes.slice(0, NAME_FROM_DEVICE_SHOWN),
    more: Math.max(0, p.changes.length - NAME_FROM_DEVICE_SHOWN),
  };
}

/** The selected nodes that will keep their name, split by why. `other` is what the server left
 *  out of both counts: a node deleted since, outside the caller's folders, or a URL/DNS monitor. */
export function unchangedCounts(p: NameFromDevicePreview): {
  total: number;
  noDeviceName: number;
  alreadySame: number;
  other: number;
} {
  const other = Math.max(
    0,
    p.requested - p.changes.length - p.no_device_name - p.already_same,
  );
  return {
    total: p.no_device_name + p.already_same + other,
    noDeviceName: p.no_device_name,
    alreadySame: p.already_same,
    other,
  };
}

/** The apply body: every row as it was shown. `null` when there is nothing to rename. */
export function applyBody(p: NameFromDevicePreview): NameFromDeviceBody | null {
  if (p.changes.length === 0) return null;
  return {
    changes: p.changes.map((c) => ({ node_id: c.node_id, from: c.name, to: c.sys_name })),
  };
}
