// SPDX-License-Identifier: AGPL-3.0-only
// Pure helpers for the maintenance/mute "suppression" features shared by the All Nodes tree and
// the management pages: the right-click target, quick-duration presets, and the suppression index
// (which nodes/groups are currently under maintenance or muted, propagated down a targeted folder
// group). Kept free of React so the index logic is unit-tested directly.

import type { Alert, MaintenanceWindow, Mute, NodeGroup, NodeSummary } from '../types/api';

/** A right-click target in the All Nodes tree: a single node or a folder group. */
export interface SuppressionTarget {
  kind: 'node' | 'group';
  id: string;
  name: string;
}

/** The mute an active alert maps onto: always its node, pre-filled with the metric it fired on. */
export interface AlertMuteSeed {
  target: SuppressionTarget;
  /** Metric to pre-fill, or `undefined` for a whole-node mute. */
  metric?: string;
}

/**
 * Which mute silences *this* alert — the seed for the per-alert Mute action on the triage screen.
 *
 * The metric is carried through **verbatim, including the `__liveness__` sentinel**, and that is
 * the whole point: a mute is stored by check *name*, and the backend re-derives its identity as
 * `check_id(node, name)` — the same v5 hash the alert's `check` already is (`alerts.rs::check_id`).
 * Passing the alert's metric therefore produces a mute matching exactly the check the operator
 * clicked. Substituting the display text ("Reachability") or dropping the sentinel would silently
 * widen or miss the mute; rendering it readably is the *form's* job, not this function's.
 *
 * An alert with no captured metric (raised before migration 0036) has no check name to name, so it
 * seeds a whole-node mute — broader than the one alert, but the only thing that can be expressed.
 */
export function muteTargetFromAlert(alert: Alert, nodeName: string): AlertMuteSeed {
  return {
    target: { kind: 'node', id: alert.node, name: nodeName },
    metric: alert.metric ? alert.metric : undefined,
  };
}

/** Metric-name presets offered as a `<datalist>` wherever the operator types a metric by hand
 *  (alert-rule metric, mute check). These are the series every node emits today — ICMP on all of
 *  them, sysUptime whenever SNMP is bound — so they cover the common case without pretending to be
 *  the full catalogue; free text stays allowed for anything else collected. Shared because the
 *  alert-rule form and the mute form each carried their own identical copy. */
export const METRIC_PRESETS = ['icmp_rtt_ms', 'icmp_loss_pct', 'snmp_sys_uptime_ticks'];

/** Quick-duration presets (milliseconds from "now") offered in the right-click submenu. */
export const DURATION_PRESETS: { label: string; ms: number }[] = [
  { label: '1h', ms: 3_600_000 },
  { label: '4h', ms: 4 * 3_600_000 },
  { label: '24h', ms: 24 * 3_600_000 },
];

/** Group/node ids currently affected, so both row kinds in the tree can show an icon. Maintenance
 *  also sets a node's rolled-up `state` to `maintenance` (the backend covers node/profile/tag/
 *  folder-group scopes), but `maintenanceNodes` lets the icon appear immediately for node- and
 *  folder-group-scoped windows without waiting on the next state refresh. Mutes never change state,
 *  so both nodes and groups are tracked for them. */
export interface SuppressionIndex {
  maintenanceNodes: Set<string>;
  maintenanceGroups: Set<string>;
  muteNodes: Set<string>;
  muteGroups: Set<string>;
}

export function emptySuppressionIndex(): SuppressionIndex {
  return {
    maintenanceNodes: new Set(),
    maintenanceGroups: new Set(),
    muteNodes: new Set(),
    muteGroups: new Set(),
  };
}

/** A folder group plus every group beneath it (BFS over `parent_id`), bounded by the group count so
 *  malformed/cyclic data can't loop forever. Mirrors the backend `group_subtree` (ADR-022). */
export function groupSubtree(groups: NodeGroup[], rootId: string): Set<string> {
  const childrenOf = new Map<string, string[]>();
  for (const g of groups) {
    if (g.parent_id) childrenOf.set(g.parent_id, [...(childrenOf.get(g.parent_id) ?? []), g.id]);
  }
  const out = new Set<string>([rootId]);
  const queue = [rootId];
  for (let guard = 0; queue.length && guard <= groups.length; guard += 1) {
    const cur = queue.shift() as string;
    for (const child of childrenOf.get(cur) ?? []) {
      if (!out.has(child)) {
        out.add(child);
        queue.push(child);
      }
    }
  }
  return out;
}

/**
 * Which nodes/groups are currently in maintenance or muted, for the All Nodes icons. A folder-group
 * target propagates down its subtree — the group, all descendant subgroups, and their member nodes
 * (decision: incl. subgroups). Only *active* maintenance windows count; the mutes from the API are
 * already unexpired. Node/profile/tag-scoped windows aren't mapped onto tree rows here — those nodes
 * surface through their `maintenance` state instead.
 */
export function buildSuppressionIndex(
  windows: MaintenanceWindow[],
  mutes: Mute[],
  groups: NodeGroup[],
  nodes: NodeSummary[],
): SuppressionIndex {
  const index = emptySuppressionIndex();
  const nodesByGroup = new Map<string, string[]>();
  for (const n of nodes) {
    if (n.group_id) nodesByGroup.set(n.group_id, [...(nodesByGroup.get(n.group_id) ?? []), n.id]);
  }
  const markSubtree = (rootId: string, groupSet: Set<string>, nodeSet: Set<string>) => {
    for (const gid of groupSubtree(groups, rootId)) {
      groupSet.add(gid);
      for (const nid of nodesByGroup.get(gid) ?? []) nodeSet.add(nid);
    }
  };

  for (const w of windows) {
    if (!w.active) continue;
    if (w.scope_level === 'node') {
      index.maintenanceNodes.add(w.scope_id);
    } else if (w.scope_level === 'group_id') {
      markSubtree(w.scope_id, index.maintenanceGroups, index.maintenanceNodes);
    }
  }

  for (const m of mutes) {
    if (m.scope_kind === 'group' && m.group_id) {
      markSubtree(m.group_id, index.muteGroups, index.muteNodes);
    } else if (m.node_id) {
      index.muteNodes.add(m.node_id);
    }
  }
  return index;
}
