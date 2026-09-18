// SPDX-License-Identifier: AGPL-3.0-only
// What one row of a wireless controller's AP list *means* — the judgement behind the AP tab
// (ADR-064 増分 B3).
//
// Here rather than in `ApTab.tsx` because Vitest only runs `src/**/*.test.ts`: a pure helper
// written in the `.tsx` is a helper no test can execute (testing.md, `tsxJudgement.test.ts`).
//
// Nothing here knows a controller's *name*. The API answers in node ids, and names come from the
// fleet-wide resolver (`useEntityNames`), which is a hook — so these functions return ids and the
// cell renders them. That split is also what keeps "which controller serves this AP" answerable in
// a test without a store.

import type { WirelessApRow, WirelessControllerSummary, WlanApState } from '../../types/api';

/** The per-controller AP cap, as the backend defines it (`yagra_common::wlan`).
 *
 *  🚨 `MAX_APS_HARD` is also the list page size: the tab asks for the cap itself, so one fetch is
 *  the whole inventory and there is no cursor to follow. A second copy of a backend constant is
 *  normally the thing to avoid — this one is load-bearing on the client (it decides the fetch) and
 *  cannot be read from the API, so it is written down once, here, with this note. */
export const MAX_APS_MIN = 1;
export const MAX_APS_HARD = 2048;
export const MAX_APS_DEFAULT = 1024;

/** How a row's state reads when the controller sent a token this build does not know.
 *
 *  ⚠️ Not a fourth vendor state — `state` is `null` in exactly two cases, an unrecognised token and
 *  an AP the inventory carried without one, and neither is something to render as blank. */
export const AP_STATE_UNKNOWN = 'unknown';

/** The i18n key suffix for a row's state: one of the three known states, or `unknown`. */
export function apStateKey(row: WirelessApRow): WlanApState | typeof AP_STATE_UNKNOWN {
  return row.state ?? AP_STATE_UNKNOWN;
}

/** True once this AP has been made a node of its own.
 *
 *  `node_id` is the whole answer and the only one: an AP the controller reports is a row in
 *  `wireless_aps` whether or not anyone imported it, so "is it monitored" cannot be read off the
 *  state, the run state or whether it is associated. */
export function isImported(row: WirelessApRow): boolean {
  return row.node_id != null;
}

/** What to call an AP. Vendors leave the name unset on an AP nobody has named, and the MAC is the
 *  one field that is always there — it is what the id is derived from (決定 8b). */
export function apLabel(row: WirelessApRow): string {
  return row.name?.trim() || row.mac;
}

/** Which controllers can see this AP, serving one first.
 *
 *  🚨 An HA pair reports the same AP twice, and that is the fact this column exists for: the
 *  standby answers the AP's inventory with the same names and a `0` for every live number
 *  (measured — ADR-064), so an operator looking at an AP's numbers needs to know which member they
 *  came from. The Overview deliberately shows only the serving one (決定 4); the full picture is
 *  here.
 *
 *  ⚠️ `serving` can be null while `others` is not. `controller_node_id` is blanked when the serving
 *  controller is outside the caller's scope, while `reported_by` is filtered to the ones they may
 *  see — so a scoped operator can legitimately see "reported by B" for an AP that A is serving. */
export interface ReportingControllers {
  /** The controller whose numbers this row carries, or null when none is serving it (or the caller
   *  cannot see the one that is). */
  serving: string | null;
  /** Every other controller that also reports it, in the API's order, de-duplicated. */
  others: string[];
}

export function reportingControllers(row: WirelessApRow): ReportingControllers {
  const seen = new Set<string>();
  const ids: string[] = [];
  for (const sighting of row.reported_by) {
    const id = sighting.controller_node_id;
    if (id == null || seen.has(id)) continue;
    seen.add(id);
    ids.push(id);
  }
  const serving = row.controller_node_id ?? null;
  return {
    serving,
    others: ids.filter((id) => id !== serving),
  };
}

/** Free text a row answers a search with — every field the row puts on screen.
 *
 *  The MAC is included in both spellings the operator might type: as shown (`00:11:22:33:44:55`)
 *  and stripped, because a label on the device itself is often printed without separators. */
export function apSearchText(row: WirelessApRow): string[] {
  return [
    row.name ?? '',
    row.mac,
    row.mac.replace(/:/g, ''),
    row.ip ?? '',
    row.model ?? '',
    row.serial ?? '',
    row.sw_version ?? '',
  ];
}

/** How many APs the controller saw and had to drop, or 0.
 *
 *  🚨 A truncated inventory is the one failure on this screen that looks like success: the list is
 *  complete-looking, every row in it is real, and the APs past the cap are simply absent. Which is
 *  why this is a number the card prints and a dot on the tab, not a log line. */
export function apsOverCap(summary: WirelessControllerSummary | null): number {
  return summary?.aps_over_cap ?? 0;
}

/** True when the controller has never reported an inventory — it has settings and nothing else.
 *
 *  Distinguishes "this AC serves no APs" from "we have not heard from it yet", which the empty
 *  table cannot: both are zero rows. */
export function awaitingFirstInventory(summary: WirelessControllerSummary | null): boolean {
  return summary != null && summary.last_inventory_at == null;
}
