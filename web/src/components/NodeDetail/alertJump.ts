// SPDX-License-Identifier: AGPL-3.0-only
// Where an alert in the node header's list takes the operator (ADR-238 decision 3).
//
// An alert about a port opens the Interfaces tab with that port selected, so its dock shows the
// series that crossed the bound. Anything else is read on Overview, which is where the device's
// own gauges and the Active alerts section are.

import type { NodeDetailTab } from './tabs';

export type AlertJump = { tab: 'interfaces'; ifindex: number } | { tab: 'overview' };

/** The place an alert is read. Only `ifindex` decides it: the port name is display-only. */
export function alertJumpOf(alert: { ifindex?: number | null }): AlertJump {
  return alert.ifindex != null ? { tab: 'interfaces', ifindex: alert.ifindex } : { tab: 'overview' };
}

/** Whether the row offers its jump from `activeTab`. A port jump is always offered — on the
 *  Interfaces tab it still selects the port. Overview is not offered while the operator is on it. */
export function offersJump(jump: AlertJump, activeTab: NodeDetailTab): boolean {
  return jump.tab === 'interfaces' || jump.tab !== activeTab;
}

/** A request for the Interfaces tab to select one port. `seq` makes a second press on the same
 *  port a new request, so the port is selected again after the operator closed its dock. */
export interface PortFocus {
  ifindex: number;
  seq: number;
}

/** The port the Interfaces tab should select now, or `null` when the request was already applied
 *  or names a port the loaded list does not have yet (the tab asks again when the rows arrive). */
export function portToSelect(
  focus: PortFocus | null | undefined,
  appliedSeq: number,
  ifindexes: readonly number[],
): number | null {
  if (!focus || focus.seq === appliedSeq) return null;
  return ifindexes.includes(focus.ifindex) ? focus.ifindex : null;
}

/** The column-free key, under the Interfaces tab's `interfaces.` prefix, that asks the tab to open
 *  one port's dock (ADR-250 Inc.2): `/nodes/<id>?tab=interfaces&interfaces.port=<ifindex>`.
 *
 *  A request, not state. The tab applies it once and drops it from the URL, because an ifindex
 *  names a different port on the next device the tree walks to (ADR-134's reason the selected row
 *  stays out of the URL). */
export const PORT_PARAM = 'interfaces.port';

/** The port a URL asks for, or `null` for none or a value that is not an ifindex. */
export function readPortParam(params: URLSearchParams): number | null {
  const raw = params.get(PORT_PARAM);
  if (raw === null || !/^\d+$/.test(raw)) return null;
  const n = Number(raw);
  return Number.isSafeInteger(n) ? n : null;
}
