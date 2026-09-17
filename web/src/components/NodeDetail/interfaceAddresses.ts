// SPDX-License-Identifier: AGPL-3.0-only
// What the Interfaces list's IP addresses column says about a port (ADR-157).
//
// The judgement lives here rather than in `InterfacesTab.tsx` for the reason `neighbors.ts` gives:
// a `.tsx` is a file no test runs (testing.md). The cell draws whatever this returns.
//
// Every address the device reports is kept — a real SVI carries up to eleven (measured on the PoC
// recordings, 2026-09-17) — and the list shows the first with `+N` for the rest, the way the
// Neighbors column does (user decision, 2026-09-17). Which one is "first" is the server's order:
// IPv4 before IPv6, numeric within each. SNMP does not say which address is primary, so nothing
// here pretends to know.

import type { InterfaceAddress } from '../../types/api';

/** The fields this module reads — structural, so a caller needs no full row. */
export interface AddressedInterface {
  addresses?: InterfaceAddress[] | null;
}

/**
 * A port's addresses, or none. `undefined` is what a core older than ADR-157 answers (the field
 * is not there), and it reads the same as a port with no address — a dash, never a crash — so a
 * WebUI upgraded ahead of its core degrades to the previous screen.
 */
export function addressesOf(row: AddressedInterface): InterfaceAddress[] {
  return row.addresses ?? [];
}

/**
 * `<ip>/<prefix length>` — `192.168.0.1/24` (user decision, 2026-09-17). A `null` prefix is the
 * server's spelling of "the device gave a mask that could not be decoded", and the address is
 * still real, so it is shown bare rather than as `/0`, which would claim a network it is not in.
 */
export function formatAddress(a: InterfaceAddress): string {
  return a.prefix_len == null ? a.ip : `${a.ip}/${a.prefix_len}`;
}

export interface AddressCellText {
  /** The first address, formatted — what the cell shows. */
  first: string;
  /** How many more there are; `0` means the cell needs no `+N` and no popover. */
  more: number;
  /** Every address, formatted, in the server's order — the popover's list and the cell's title. */
  all: string[];
}

/** What the cell shows for a port, or `null` when the port has no address to show. */
export function addressCellText(list: readonly InterfaceAddress[]): AddressCellText | null {
  if (list.length === 0) return null;
  const all = list.map(formatAddress);
  return { first: all[0], more: all.length - 1, all };
}
