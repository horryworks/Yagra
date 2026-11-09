// SPDX-License-Identifier: AGPL-3.0-only
// Where a dashboard widget's parts lead (ADR-250): the one place a widget gets the URL of the screen
// narrowed to what was pressed.
//
// Every link is spelled in the destination's own vocabulary - the column key its filter row reads
// from the URL, the value encoded the way that column encodes it (`encodeSet`, `encodeRange`). A
// link that named a key the destination does not read would open the unfiltered screen and look as
// if it worked, so `lib/filterSpecRegistry.test.ts` checks every href `DRILL_SAMPLES` builds
// against the route ledger: each key must be one the route's page or one of its tables owns.

import { PORT_PARAM } from '../../components/NodeDetail/alertJump';
import { nodeTabFilterPrefix, type NodeDetailTab } from '../../components/NodeDetail/tabs';
import { CUSTOM_RANGE, encodeRange, encodeSet } from '../../lib/columnFilter';
import { nodeHref, nodesPageHref } from '../../lib/entityHref';
import { unixToLocalInput } from '../../lib/format';
import { DISPLAY_ORDER, HARD_DOWN_STATES } from '../../lib/nodeState';
import {
  EVENT_ACTIONS,
  EVENT_KINDS,
  SEVERITIES,
  type EventAction,
  type EventKind,
  type NodeState,
  type Severity,
} from '../../types/api';
import type { WidgetSettings } from '../types';
import { readSelection } from './metricChart';

/** `path?k=v&…`, dropping empty values so a link never carries a key at its default. */
function withQuery(path: string, params: Record<string, string | undefined>): string {
  const q = new URLSearchParams();
  for (const [k, v] of Object.entries(params)) if (v) q.set(k, v);
  const s = q.toString();
  return s ? `${path}?${s}` : path;
}

/** One hour, the width of the hourly bars the volume widgets draw. */
export const HOUR_SECS = 3600;

/** A time window as a range column's custom value (`custom:<from>|<to>`, local wall time). */
function customRange(fromUnix: number, toUnix: number): string {
  return encodeRange({
    preset: CUSTOM_RANGE,
    from: unixToLocalInput(fromUnix),
    to: unixToLocalInput(toUnix),
  });
}

/** The node detail page, optionally on one tab with that tab's filters set. `filters` keys are the
 *  tab table's bare column keys; the tab's prefix is added here, as the tab itself adds it. */
export function nodeDrill(
  id: string,
  tab?: NodeDetailTab,
  filters: Record<string, string> = {},
): string {
  if (!tab) return nodeHref(id);
  const prefix = nodeTabFilterPrefix(tab);
  const params: Record<string, string> = { tab };
  for (const [k, v] of Object.entries(filters)) params[prefix + k] = v;
  return withQuery(nodeHref(id), params);
}

/** The node's Interfaces tab with one port's dock open. */
export function interfaceDrill(nodeId: string, ifindex: number): string {
  return `${nodeDrill(nodeId, 'interfaces')}&${new URLSearchParams({ [PORT_PARAM]: String(ifindex) })}`;
}

/** All nodes, filtered to nodes in any of `states`. */
export function nodesInStates(states: readonly NodeState[]): string {
  return withQuery('/nodes', { state: encodeSet(states, DISPLAY_ORDER) });
}

/** The states the dashboards draw as one "down" figure (the health ring's critical slice, the
 *  nodes-down tile). */
export const DOWN_STATES = HARD_DOWN_STATES;

/** All nodes, with one folder selected. */
export function folderDrill(groupId: string): string {
  return nodesPageHref({ kind: 'group', id: groupId });
}

/** Active alerts, narrowed. */
export function alertsDrill(q: { severity?: Severity; nodeId?: string; flapping?: boolean } = {}): string {
  return withQuery('/alerts', {
    severity: q.severity ? encodeSet([q.severity], SEVERITIES) : undefined,
    node_id: q.nodeId,
    notes: q.flapping ? 'flapping' : undefined,
  });
}

/** The start of the hour an instant falls in (unix seconds). */
export function hourStartOf(unixSeconds: number): number {
  return Math.floor(unixSeconds / HOUR_SECS) * HOUR_SECS;
}

/** Alert history for the hour that starts at `hourFromUnix`, fired transitions only (the alert
 *  volume bars count openings). */
export function alertHistoryHourDrill(hourFromUnix: number): string {
  return withQuery('/alerts/history', {
    phase: 'fired',
    range: customRange(hourFromUnix, hourFromUnix + HOUR_SECS),
  });
}

/** The event log, narrowed. `hourFromUnix` opens the hour that starts there. */
export function eventsDrill(
  q: { kind?: EventKind; action?: EventAction; message?: string; hourFromUnix?: number } = {},
): string {
  return withQuery('/events', {
    kind: q.kind ? encodeSet([q.kind], EVENT_KINDS) : undefined,
    action: q.action ? encodeSet([q.action], EVENT_ACTIONS) : undefined,
    message: q.message,
    at: q.hourFromUnix == null ? undefined : customRange(q.hourFromUnix, q.hourFromUnix + HOUR_SECS),
  });
}

/** An event-stats key as an event kind, or undefined for a key the log does not filter on. */
export function asEventKind(key: string): EventKind | undefined {
  return (EVENT_KINDS as readonly string[]).includes(key) ? (key as EventKind) : undefined;
}

/** An event-stats key as a pipeline action, or undefined. */
export function asEventAction(key: string): EventAction | undefined {
  return (EVENT_ACTIONS as readonly string[]).includes(key) ? (key as EventAction) : undefined;
}

/** Maintenance windows, narrowed to names containing `name`. */
export function maintenanceDrill(name: string): string {
  return withQuery('/alerts/maintenance', { name });
}

/** The metric chart's node, on its Collection tab narrowed to the charted metric; nothing while
 *  the card has no node chosen. */
export function metricChartHref(settings: WidgetSettings | undefined): string | undefined {
  const { nodeId, metric } = readSelection(settings);
  if (!nodeId) return undefined;
  return metric ? nodeDrill(nodeId, 'collection', { metric }) : nodeDrill(nodeId);
}

/** One sample of every href this module builds, for the ledger check. Ids are placeholders. */
export const DRILL_SAMPLES: readonly string[] = [
  nodeDrill('n1'),
  nodeDrill('n1', 'events'),
  nodeDrill('n1', 'collection', { metric: 'cpu' }),
  interfaceDrill('n1', 3),
  nodesInStates(DOWN_STATES),
  folderDrill('g1'),
  alertsDrill({ severity: 'critical' }),
  alertsDrill({ nodeId: 'n1' }),
  alertsDrill({ flapping: true }),
  alertHistoryHourDrill(1_700_000_000),
  eventsDrill({ kind: 'trap', message: 'linkDown' }),
  eventsDrill({ action: 'fired' }),
  eventsDrill({ hourFromUnix: 1_700_000_000 }),
  maintenanceDrill('nightly'),
];
