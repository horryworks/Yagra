// SPDX-License-Identifier: AGPL-3.0-only
// What to show where a node's address goes (ADR-175).
//
// `nodes.address` is INET NOT NULL, so a node that has no address is stored at the unspecified
// one: a Meraki device the Dashboard reports no LAN IP for (a mesh repeater), a DNS monitor using
// the system resolver (`checks.rs`'s `NO_ADDRESS`). `0.0.0.0` on screen reads as a real address,
// so every surface asks here instead of printing the field.

import type { TFunction } from 'i18next';

const UNSPECIFIED = new Set(['0.0.0.0', '::', '0:0:0:0:0:0:0:0']);

/** Whether the stored address is a real one — not the unspecified address "no address" is kept as. */
export function hasAddress(address: string | null | undefined): address is string {
  const a = address?.trim();
  return !!a && !UNSPECIFIED.has(a);
}

/** The address as text for a label: the address itself, or "No IP" — "No IP (mesh repeater)" for
 *  a Meraki access point that has no wired uplink. */
export function addressText(
  address: string | null | undefined,
  t: TFunction,
  opts: { meshRepeater?: boolean } = {},
): string {
  if (hasAddress(address)) return address;
  return opts.meshRepeater ? t('nodes:address.noneRepeater') : t('nodes:address.none');
}
