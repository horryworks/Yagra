// SPDX-License-Identifier: AGPL-3.0-only
// Pure helpers for the maintenance/mute "suppression" features shared by the All Nodes tree and
// the management pages: the right-click target, quick-duration presets, and the suppression index
// (which nodes/groups are currently under maintenance or muted, propagated down a targeted folder
// group). Kept free of React so the index logic is unit-tested directly.

import type {
  Alert,
  MaintenanceWindow,
  Mute,
  NodeGroup,
  NodeSummary,
  SuppressionExemption,
} from '../types/api';

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
 *
 * The node is passed in rather than read off `alert.node`, because that field is the alert's
 * *subject* and is only a node id when the subject is one (`lib/alertSubject`). A pool-coverage
 * alert has no node to mute; the caller decides that before offering the action.
 */
export function muteTargetFromAlert(
  alert: Alert,
  nodeId: string,
  nodeName: string,
): AlertMuteSeed {
  return {
    target: { kind: 'node', id: nodeId, name: nodeName },
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
 *  so both nodes and groups are tracked for them.
 *
 *  The `exempt*` sets are the negative half: a node an operator has released from a suppression it
 *  only inherited. Such a node is *removed* from `maintenanceNodes`/`muteNodes` — it is not
 *  suppressed — and listed here instead so the row can say so and offer to put it back. */
export interface SuppressionIndex {
  maintenanceNodes: Set<string>;
  maintenanceGroups: Set<string>;
  muteNodes: Set<string>;
  muteGroups: Set<string>;
  exemptMaintenanceNodes: Set<string>;
  exemptMuteNodes: Set<string>;
}

export function emptySuppressionIndex(): SuppressionIndex {
  return {
    maintenanceNodes: new Set(),
    maintenanceGroups: new Set(),
    muteNodes: new Set(),
    muteGroups: new Set(),
    exemptMaintenanceNodes: new Set(),
    exemptMuteNodes: new Set(),
  };
}

/** The chain of folder groups **containing** `groupId` — itself and every group above it.
 *
 *  The mirror image of {@link groupSubtree}: `root ∈ groupContainers(g)` exactly when
 *  `g ∈ groupSubtree(root)`. The tree asks it per row, where walking up is O(depth) instead of
 *  expanding every window's subtree. Bounded by the group count so malformed/cyclic data can't
 *  loop forever, exactly as `groupSubtree` is. */
export function groupContainers(groups: NodeGroup[], groupId: string | null): string[] {
  const out: string[] = [];
  if (!groupId) return out;
  const parentOf = new Map(groups.map((g) => [g.id, g.parent_id ?? null]));
  let cur: string | null = groupId;
  const seen = new Set<string>();
  for (let guard = 0; cur && guard <= groups.length; guard += 1) {
    if (seen.has(cur)) break;
    seen.add(cur);
    out.push(cur);
    cur = parentOf.get(cur) ?? null;
  }
  return out;
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
  exemptions: SuppressionExemption[] = [],
): SuppressionIndex {
  const index = emptySuppressionIndex();
  for (const e of exemptions) {
    if (e.kind === 'maintenance') index.exemptMaintenanceNodes.add(e.node_id);
    else index.exemptMuteNodes.add(e.node_id);
  }
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

  // Applied last, and only to the *inherited* marks — a window or mute that names the node is
  // re-added below. This mirrors the server (`resolve_maintenance` / `load_mutes`): an exemption
  // cancels coverage the node merely belongs to, never coverage aimed at it. Doing it in the other
  // order would drop a deliberate node-scoped window off the row while the engine still applied
  // it, which is the one disagreement an operator would read as the icon lying.
  for (const nid of index.exemptMaintenanceNodes) index.maintenanceNodes.delete(nid);
  for (const nid of index.exemptMuteNodes) index.muteNodes.delete(nid);
  for (const w of windows) {
    if (w.active && w.scope_level === 'node') index.maintenanceNodes.add(w.scope_id);
  }
  for (const m of mutes) {
    if (m.scope_kind !== 'group' && m.node_id) index.muteNodes.add(m.node_id);
  }
  return index;
}

/** Where a suppression on a row comes from, and therefore what can be done about it.
 *
 *  - `direct` — the row itself is the window's or mute's target, so the row *is* the thing to act
 *    on: end the window, or lift the mute.
 *  - `inherited` — an ancestor folder group carries it. Ending it would release every sibling too,
 *    so a node offers a release for itself instead and a group offers nothing (see
 *    {@link causesFor}).
 *  - `unattributed` — the node is in `maintenance` state but nothing the browser holds explains
 *    it. Profile-, tag- and fleet-wide windows are resolved server-side against facts this page
 *    never loads, so they arrive as a state with no row behind it. */
export const CAUSE_SOURCES = ['direct', 'inherited', 'unattributed'] as const;
export type CauseSource = (typeof CAUSE_SOURCES)[number];

/** One reason a tree row is suppressed, ready to render.
 *
 *  `labelKey`/`labelParams` are chosen here rather than in the component: Vitest runs
 *  `environment: 'node'` over `src/**\/*.test.ts` only, so a decision made inside a `.tsx` is a
 *  decision nothing tests (`testing.md`). */
export interface SuppressionCause {
  kind: 'maintenance' | 'mute';
  source: CauseSource;
  /** The window/mute id. Present only for `direct` — the only source with a row to act on. */
  id?: string;
  /** The window's name, or the mute's target, for the heading line. Empty for `unattributed`. */
  title: string;
  /** i18n key + params for the "where does this come from" line. */
  labelKey: string;
  labelParams?: Record<string, string>;
  /** RFC 3339. When this cause stops on its own. Empty when the browser cannot attribute one. */
  endsAt: string;
  /** The node has been released from this coverage: it is *not* being suppressed by it, and the
   *  only thing left to do is put it back. Never set on a `direct` cause — an exemption cancels
   *  coverage a node merely belongs to, never coverage aimed at it — and never on a group row,
   *  since exemptions are per node. Mirrors {@link buildSuppressionIndex} and the server. */
  released?: boolean;
}

/**
 * What the release panel asks the page to do. One union rather than four callbacks, so the page
 * answers with one exhaustive `switch` and adding a fifth action cannot be half-wired.
 *
 * Note the asymmetry between the two `direct` actions, and that it is deliberate: a **window** is
 * *ended* — the row survives as a record of the maintenance that actually happened, and reads as
 * `ended` on the Maintenance page — while a **mute** is deleted, because a mute records nothing
 * but its own existence.
 */
export type ReleaseAction =
  | { action: 'end-window'; windowId: string }
  | { action: 'lift-mute'; muteId: string }
  | { action: 'release-node'; nodeId: string; kind: 'maintenance' | 'mute' }
  | { action: 'undo-release'; nodeId: string; kind: 'maintenance' | 'mute' };

/** i18n keys {@link causesFor} can produce, so `i18nEnumKeys.test.ts` can demand both locales
 *  carry them. A key built at runtime is missing from *both* locales when it is forgotten, so
 *  parity passes and the operator sees the raw key (`extensibility.md` §4). */
export const CAUSE_LABEL_KEYS = [
  'tree.suppression.from.node',
  'tree.suppression.from.group',
  'tree.suppression.from.ancestor',
  'tree.suppression.from.elsewhere',
] as const;

/**
 * Why this row is suppressed right now, most actionable first.
 *
 * Deliberately best-effort, and the deliberate part is `unattributed`. A window scoped to a
 * profile, a tag or the whole fleet is resolved by the server against facts the inventory tree
 * does not load, so the browser can see the node is in `maintenance` and still not name the
 * window. Reporting that honestly ("something else is covering this") beats both alternatives:
 * inventing a cause, or showing no panel at all on a row whose icon is lit.
 *
 * The *write* has no such gap — releasing a node re-resolves coverage server-side — so a release
 * is offered on an `unattributed` node too.
 */
export function causesFor(
  target: SuppressionTarget,
  ctx: {
    windows: MaintenanceWindow[];
    mutes: Mute[];
    groups: NodeGroup[];
    /** The node behind a `node` target, when the tree has it loaded. Supplies the folder group the
     *  ancestor walk starts from and the rolled-up state that reveals an unattributed window. */
    node?: NodeSummary;
    /** Releases already granted to this node, so a cause it has been released from is reported as
     *  such instead of as a live suppression offering a release that has already happened. */
    exemptions?: SuppressionExemption[];
  },
): SuppressionCause[] {
  const { windows, mutes, groups, node } = ctx;
  const isGroup = target.kind === 'group';
  const releasedFrom = (kind: 'maintenance' | 'mute') =>
    !isGroup && (ctx.exemptions ?? []).some((e) => e.node_id === target.id && e.kind === kind);
  const maintReleased = releasedFrom('maintenance');
  const muteReleased = releasedFrom('mute');
  // The folder groups that contain this row. A group row contains itself, so an ancestor window is
  // found the same way for both kinds; the `direct` test below is what tells them apart.
  const containers = groupContainers(groups, isGroup ? target.id : (node?.group_id ?? null));
  const groupName = (id: string) => groups.find((g) => g.id === id)?.name ?? id;
  const out: SuppressionCause[] = [];

  for (const w of windows) {
    if (!w.active) continue;
    const direct =
      (!isGroup && w.scope_level === 'node' && w.scope_id === target.id) ||
      (isGroup && w.scope_level === 'group_id' && w.scope_id === target.id);
    if (direct) {
      out.push({
        kind: 'maintenance',
        source: 'direct',
        id: w.id,
        title: w.name,
        labelKey: isGroup ? 'tree.suppression.from.group' : 'tree.suppression.from.node',
        endsAt: w.ends_at,
      });
    } else if (w.scope_level === 'group_id' && containers.includes(w.scope_id)) {
      out.push({
        kind: 'maintenance',
        source: 'inherited',
        title: w.name,
        labelKey: 'tree.suppression.from.ancestor',
        labelParams: { name: groupName(w.scope_id) },
        endsAt: w.ends_at,
        released: maintReleased,
      });
    }
  }

  for (const m of mutes) {
    const direct =
      (!isGroup && m.scope_kind !== 'group' && m.node_id === target.id) ||
      (isGroup && m.scope_kind === 'group' && m.group_id === target.id);
    if (direct) {
      out.push({
        kind: 'mute',
        source: 'direct',
        id: m.id,
        title: m.metric_name ?? '',
        labelKey: isGroup ? 'tree.suppression.from.group' : 'tree.suppression.from.node',
        endsAt: m.until_at,
      });
    } else if (m.scope_kind === 'group' && m.group_id && containers.includes(m.group_id)) {
      out.push({
        kind: 'mute',
        source: 'inherited',
        title: '',
        labelKey: 'tree.suppression.from.ancestor',
        labelParams: { name: groupName(m.group_id) },
        endsAt: m.until_at,
        released: muteReleased,
      });
    }
  }

  // Only when nothing else explains it: the engine says this node is in maintenance and no window
  // the browser can see is responsible.
  if (!isGroup && node?.state === 'maintenance' && !out.some((c) => c.kind === 'maintenance')) {
    out.push({
      kind: 'maintenance',
      source: 'unattributed',
      title: '',
      labelKey: 'tree.suppression.from.elsewhere',
      endsAt: '',
      released: maintReleased,
    });
  }
  return out;
}

/**
 * One block of the release panel: what is (or was) silencing the row, and the single control it
 * offers.
 *
 * Built here rather than in `NodeTree.tsx` for the reason `testing.md` gives — Vitest runs
 * `src/**\/*.test.ts` in a node environment and never loads a `.tsx`. The panel's first bug was
 * exactly a decision made where no test could reach it: `causesFor` knew nothing about exemptions,
 * so a released node was shown a live "in maintenance" block offering a release it had already
 * been given, directly above the block saying it was released.
 */
export interface SuppressionPanelRow {
  /** React key. Stable within one render of one row; not an entity id. */
  key: string;
  kind: 'maintenance' | 'mute';
  /** i18n key for the block's heading — which of the four states this block reports. */
  headKey: string;
  /** The window's name (or the muted check), when there is one to show. */
  title: string;
  /** i18n key + params for the "where does this come from" line, when a cause explains it. */
  labelKey?: string;
  labelParams?: Record<string, string>;
  /** RFC 3339. When this stops on its own; empty when nothing can be attributed. */
  endsAt: string;
  /** The one control this block offers, with the i18n key naming what it does. `null` when the
   *  block can only explain — a group covered by an ancestor's window. */
  action: { action: ReleaseAction; labelKey: string } | null;
  /** Shown in place of a button when `action` is null. */
  noteKey?: string;
  noteParams?: Record<string, string>;
}

/** Every i18n key {@link suppressionPanelRows} can emit. A key assembled at runtime is missing
 *  from *both* locales when it is forgotten, so EN/JA parity passes and the operator sees the raw
 *  key (`extensibility.md` §4); `i18nEnumKeys.test.ts` reads this list and demands both. */
export const PANEL_LABEL_KEYS = [
  'tree.suppression.head.maintenance',
  'tree.suppression.head.mute',
  'tree.suppression.head.releasedMaintenance',
  'tree.suppression.head.releasedMute',
  'tree.suppression.act.endNow',
  'tree.suppression.act.unmute',
  'tree.suppression.act.excludeNode',
  'tree.suppression.act.unmuteNode',
  'tree.suppression.act.undoMaintenance',
  'tree.suppression.act.undoMute',
  'tree.suppression.releaseOnAncestor',
] as const;

const HEAD_KEY = {
  maintenance: 'tree.suppression.head.maintenance',
  mute: 'tree.suppression.head.mute',
} as const;
const RELEASED_HEAD_KEY = {
  maintenance: 'tree.suppression.head.releasedMaintenance',
  mute: 'tree.suppression.head.releasedMute',
} as const;
const UNDO_KEY = {
  maintenance: 'tree.suppression.act.undoMaintenance',
  mute: 'tree.suppression.act.undoMute',
} as const;
const EXCLUDE_KEY = {
  maintenance: 'tree.suppression.act.excludeNode',
  mute: 'tree.suppression.act.unmuteNode',
} as const;

/** The control a single cause offers, given who is asking. */
function actionFor(
  cause: SuppressionCause,
  target: SuppressionTarget,
): Pick<SuppressionPanelRow, 'action' | 'noteKey' | 'noteParams'> {
  if (cause.released) {
    return {
      action: {
        action: { action: 'undo-release', nodeId: target.id, kind: cause.kind },
        labelKey: UNDO_KEY[cause.kind],
      },
    };
  }
  if (cause.source === 'direct' && cause.id) {
    // A window is ended, not deleted — the label says so, because the row it leaves behind on the
    // Maintenance page is the difference. A mute has nothing to preserve, so it goes.
    return {
      action:
        cause.kind === 'maintenance'
          ? {
              action: { action: 'end-window', windowId: cause.id },
              labelKey: 'tree.suppression.act.endNow',
            }
          : {
              action: { action: 'lift-mute', muteId: cause.id },
              labelKey: 'tree.suppression.act.unmute',
            },
    };
  }
  if (target.kind === 'node') {
    return {
      action: {
        action: { action: 'release-node', nodeId: target.id, kind: cause.kind },
        labelKey: EXCLUDE_KEY[cause.kind],
      },
    };
  }
  // A group covered by an ancestor's window. Ending it from here would release every sibling under
  // that ancestor too, so this says where the control is rather than quietly offering it.
  return {
    action: null,
    noteKey: 'tree.suppression.releaseOnAncestor',
    noteParams: cause.labelParams,
  };
}

/**
 * Everything the release panel shows for one row, in order: what is suppressing it, then what it
 * has been released from. An empty array means nothing is — the caller says so in its own words.
 *
 * A release the node already holds produces **one** block, not two: the coverage it was carved out
 * of is reported under the "released" heading, keeping the window's name and expiry on screen (the
 * operator still wants to know what they are outside of), with putting it back as the only action.
 * A standalone block is appended only for a release nothing else explains — the coverage ended
 * before the exemption row was swept, which is the one state where the undo has no context.
 */
export function suppressionPanelRows(
  target: SuppressionTarget,
  ctx: {
    windows: MaintenanceWindow[];
    mutes: Mute[];
    groups: NodeGroup[];
    node?: NodeSummary;
    exemptions?: SuppressionExemption[];
  },
): SuppressionPanelRow[] {
  const causes = causesFor(target, ctx);
  const explained = new Set<string>();
  const rows: SuppressionPanelRow[] = causes.map((c, i) => {
    if (c.released) explained.add(c.kind);
    return {
      key: `c${i}`,
      kind: c.kind,
      headKey: c.released ? RELEASED_HEAD_KEY[c.kind] : HEAD_KEY[c.kind],
      title: c.title,
      labelKey: c.labelKey,
      labelParams: c.labelParams,
      endsAt: c.endsAt,
      ...actionFor(c, target),
    };
  });

  for (const e of ctx.exemptions ?? []) {
    if (target.kind !== 'node' || e.node_id !== target.id || explained.has(e.kind)) continue;
    rows.push({
      key: `x-${e.kind}`,
      kind: e.kind,
      headKey: RELEASED_HEAD_KEY[e.kind],
      title: '',
      endsAt: e.until_at,
      action: {
        action: { action: 'undo-release', nodeId: target.id, kind: e.kind },
        labelKey: UNDO_KEY[e.kind],
      },
    });
  }
  return rows;
}
