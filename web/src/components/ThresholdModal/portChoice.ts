// SPDX-License-Identifier: AGPL-3.0-only
// The port list in the threshold-rule dialog (ADR-209 decision 7): a port rule used to be creatable
// only from Node detail ▸ Interfaces. Pure, so the label and the order are tested — Vitest does not
// run a `.tsx`.

import type { InterfaceRow } from '../../types/api';

/** One port as the dialog offers it, carrying what the port-rule dialog it hands off to needs. */
export interface PortChoice {
  ifindex: number;
  /** The port as the Interfaces dock names it: `ifName`, else `if<ifIndex>`. */
  label: string;
  /** With the alias, when the device has one — what tells two `GE0/0/1`s on two boards apart. */
  option: string;
  speedBps: number | null;
}

/** The node's ports in ifIndex order. Stale rows (a port the device no longer reports) are left
 *  out: a rule on one would match nothing. */
export function portChoices(rows: readonly InterfaceRow[]): PortChoice[] {
  return rows
    .filter((r) => !r.stale)
    .slice()
    .sort((a, b) => a.ifindex - b.ifindex)
    .map((r) => {
      const label = r.if_name ?? `if${r.ifindex}`;
      const alias = r.if_alias?.trim();
      return {
        ifindex: r.ifindex,
        label,
        option: alias ? `${label} — ${alias}` : label,
        speedBps: r.if_speed_bps ?? null,
      };
    });
}
