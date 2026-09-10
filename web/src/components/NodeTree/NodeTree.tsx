// SPDX-License-Identifier: AGPL-3.0-only
// The inventory tree (All nodes). Hierarchical groups (folders) with their member nodes, modelled
// on HoTTY's HostTree: expand/collapse, per-row hover actions, a right-click context menu, and
// HTML5 drag-and-drop.
//
// Every row shares the same leading layout — a fixed-width twisty slot (a real chevron for groups,
// an invisible spacer for nodes and childless groups) then a fixed-width icon slot (the group icon
// or, for a node, its status dot). Indentation is purely `depth × INDENT`, so a child's icon lines
// up one step in from its parent's and names sit in a clean column.
//
// Drag-and-drop supports both moving AND reordering (HoTTY-style): the drop position is read from
// the cursor's vertical position within the target row — the top/bottom edge means "before/after"
// (reorder among siblings), the middle of a group means "inside" (nest / assign). Dropping onto
// "Ungrouped" moves a node to the root / a group to the top level. Group moves that would nest a
// group inside its own subtree are refused (cycle guard). This component is presentation +
// interaction only; the page owns the data and turns the callbacks into API calls + a reload.

import { useCallback, useMemo, useRef, useState } from 'react';
import { useVirtualizer } from '@tanstack/react-virtual';
import { useTranslation } from 'react-i18next';
import type { NodeGroup, NodeSummary, PoolOption } from '../../types/api';
import { poolChoices } from '../../lib/pool';
import { NODE_KIND_SPEC } from '../../lib/nodeKind';
import {
  asGroupType,
  buildNodeTree,
  filterTerm,
  flattenTree,
  flatRowKey,
  type FlatRow,
  type StateCounts,
  type TreeGroup,
} from '../../lib/nodeTree';
import { usePrefsStore } from '../../prefs';
import {
  DURATION_PRESETS,
  type ReleaseAction,
  type SuppressionIndex,
  type SuppressionPanelRow,
  type SuppressionTarget,
} from '../../lib/suppression';
import { formatScheduleTime } from '../../lib/format';
import { StatusDot } from '../ui/StatusDot';
import { Button } from '../ui/Button';
import { ActionMenu } from '../ui/ActionMenu';
import { AnchoredPopover } from '../ui/AnchoredPopover';
import { WrenchIcon, BellIcon, BellOffIcon } from '../ui/icons';
import { HealthBar } from '../HealthBar/HealthBar';
import {
  dropAction,
  dropAllowed,
  dropPosition,
  nodeDragItem,
  rootDropAction,
  type DragItem,
  type DropAction,
  type DropPos,
  type Target,
} from './nodeTreeDnd';
import {
  canMoveByPrefix,
  canRunDiscovery,
  groupMenuHasItems,
  hasSuppression,
  nodeMoveItems,
  rootMenuHasItems,
  type MenuCapabilities,
} from './nodeTreeMenu';
import { clickOutcome, toggleChecked, type CheckedNodes } from './nodeTreeSelect';
import { pinFocusScroll, restoreScroll, type ScrollAt } from './nodeTreeScroll';
import { GroupIcon } from './GroupIcon';
import './NodeTree.css';

/** What the inventory currently has selected (drives the split's detail pane). */
export type TreeSelection = { kind: 'node' | 'group'; id: string } | null;

/** Pixels of indent per tree depth. */
const INDENT = 16;
/** Left padding of a depth-0 row. */
const BASE_PAD = 6;
/** Fixed row height (matches `--row-h` in tokens.css) — every tree row is one line, so the
 *  flattened list virtualizes with a uniform estimate (S13). */
const ROW_H = 30;

/** One shared empty working set, so a tree rendered without `checked` reads from a stable value
 *  rather than allocating a new `Map` on every render (which would defeat every memo below it). */
const EMPTY_CHECKED: CheckedNodes = new Map();

type DropTarget = { id: string | 'root'; position: DropPos; ok: boolean } | null;
type Menu =
  | { x: number; y: number; kind: 'group'; group: TreeGroup }
  | { x: number; y: number; kind: 'node'; node: NodeSummary }
  | { x: number; y: number; kind: 'root' }
  // Opened by clicking a suppression marker: what is silencing this row, and what can be done
  // about it. A `Menu` variant rather than its own popover so it shares the one context menu —
  // its point, its dismissal and, since ADR-124 Inc.2, its portal and its clamping to the viewport.
  | { x: number; y: number; kind: 'suppress'; target: SuppressionTarget; node?: NodeSummary }
  | null;

interface Props {
  groups: NodeGroup[];
  nodes: NodeSummary[];
  canEdit: boolean;
  /** Per-group DIRECT member state counts (server rollup, A-1). When given, group rows roll up from
   *  these — correct over the whole fleet even before a group's members are lazily loaded (A-3). */
  groupCounts?: Record<string, StateCounts>;
  /** Ids of groups whose members have been lazily fetched (A-3). An open group not in this set shows
   *  a loading placeholder instead of its members. Omit (with `groupCounts`) ⇒ every group loaded. */
  loadedGroups?: Set<string>;
  /** Filter mode only: ids of groups whose whole membership is being fetched because the term
   *  matched the group's own NAME (`revealedGroupKeys`). Only these can show a loading row while
   *  filtering — every other group is showing the search page's hits and nothing more. */
  revealedGroups?: Set<string>;
  /** First inventory load in flight — show a loading placeholder, not the empty message. */
  loading?: boolean;
  /** Currently-selected row (highlighted with the inset accent bar); drives the split detail pane. */
  selected?: TreeSelection;
  /** Select a node/group row (single-click). Falls back to `onOpenNode` when not provided. */
  onSelectNode?: (node: NodeSummary) => void;
  onSelectGroup?: (group: NodeGroup) => void;
  /** Clear the selection (ADR-073). Two gestures reach it: clicking the already-selected row, and
   *  clicking the empty space below the rows. Omit and both become no-ops — the tree keeps its
   *  pre-ADR-073 behaviour of only ever moving the selection, never removing it. */
  onSelectNone?: () => void;
  /** Case-insensitive name filter; non-empty force-expands and hides non-matching rows. */
  filter?: string;
  /** The nodes handed in were already narrowed by the pane's state / kind / pool controls, which
   *  run server-side. The tree cannot see those filters, so without this it does not know it is
   *  filtering at all: every folder stays on screen — including the ones with nothing matching
   *  under them — and a collapsed folder stays collapsed over its own matches. */
  narrowed?: boolean;
  /** Render the internal Add-group / drag-hint toolbar (the split hosts Add-group in its pane head). */
  showToolbar?: boolean;
  onOpenNode: (node: NodeSummary) => void;
  onAddGroup: (parentId: string | null) => void;
  onEditGroup: (group: NodeGroup) => void;
  onDeleteGroup: (group: NodeGroup) => void;
  /** Right-click → edit a node (its check, profile, credential, identity, pool) — the same dialog
   *  the detail pane's "Edit node" opens, reachable without selecting the row first. Omit to hide
   *  the menu item. */
  onEditNode?: (node: NodeSummary) => void;
  /** Right-click → add a monitoring node, placed into `groupId` (`null` = top level / Ungrouped).
   *  The manual, Discovery-free way to add a target. Omit to hide the menu item. */
  onAddNode?: (groupId: string | null) => void;
  /** Right-click → delete a node (opens a destructive-consent modal). Omit to hide the item. */
  onDeleteNode?: (node: NodeSummary) => void;
  /** Open the "move node" picker (context-menu / button path, keyboard-accessible). */
  onRequestMoveNode: (node: NodeSummary) => void;
  /** The working set — nodes checked with Ctrl / Shift for a bulk action (ADR-124 決定 2).
   *  Held by the page, never in the URL. */
  checked?: CheckedNodes;
  /** Row a Shift click measures its range from. An **id**: the flat row list is rebuilt on every
   *  SSE frame and filter change, so an index would point at whatever sits there now. */
  anchorId?: string | null;
  /** The working set changed. **Omit to disable Ctrl / Shift entirely** — which is what a caller
   *  without ManageConfig does, so a viewer cannot assemble a batch with nowhere to send it. */
  onCheckedChange?: (next: Map<string, NodeSummary>, anchorId: string | null) => void;
  /** Move every checked node (the menu's bulk item). Omit to hide it. */
  onMoveChecked?: () => void;
  /** Propose folders for every checked node by IP range. Omit to hide it. */
  onMoveCheckedByPrefix?: () => void;
  /** Propose a folder for this one node by IP range. Omit to hide it. */
  onMoveNodeByPrefix?: (node: NodeSummary) => void;
  /** Move nodes into a group (or null = ungroup), appending them — a drop, of one row or of the
   *  whole working set. **A list even for one** (ADR-124 Inc.4): the drag used to hand over a
   *  single id and the page used to answer it with the single-node endpoint, which is how a
   *  three-row selection moved one node. */
  onMoveNodes: (nodeIds: readonly string[], groupId: string | null) => void;
  /** Re-parent a group (or null = top level), appending it — drop into a group / onto Ungrouped. */
  onMoveGroup: (groupId: string, parentId: string | null) => void;
  /** Drag-reorder a node next to a sibling node (before/after) within a group. */
  onReorderNode: (
    nodeId: string,
    dest: { groupId: string | null; before?: string; after?: string },
  ) => void;
  /** Drag-reorder a group next to a sibling group (before/after) under a parent. */
  onReorderGroup: (
    groupId: string,
    dest: { parentId: string | null; before?: string; after?: string },
  ) => void;
  /** Which nodes/groups are currently in maintenance or muted (drives the per-row markers). */
  suppression?: SuppressionIndex;
  /** What the release panel shows for a row — resolved by the page, which holds the window, mute
   *  and exemption lists. Omit and the markers stay decorative, as they were before the panel. */
  suppressionRows?: (target: SuppressionTarget, node?: NodeSummary) => SuppressionPanelRow[];
  /** Act on a suppression from the panel or the context menu. One callback over a union so the
   *  page answers with a single exhaustive switch. Omit to hide every release control. */
  onRelease?: (action: ReleaseAction) => void;
  /** Right-click → put a node/group into maintenance. `durationMs` = preset length from now;
   *  `null` = open the full create form prefilled with the scope ("Custom…"). */
  onSetMaintenance?: (target: SuppressionTarget, durationMs: number | null) => void;
  /** Right-click → mute a node/group. `durationMs`/`null` as for `onSetMaintenance`. */
  onSetMute?: (target: SuppressionTarget, durationMs: number | null) => void;
  /** Pools offered by the right-click poll-pool chips (`GET /api/v1/pools`). */
  pools?: PoolOption[];
  /** Right-click → assign a node/group to a poll-pool. A pool name sets it, `''` clears it back
   *  to inherited, and `null` opens the Custom… dialog — the same convention as the suppression
   *  chips above. */
  onSetPool?: (target: SuppressionTarget, pool: string | null) => void;
  /** Right-click → aim a discovery sweep at this folder's IP prefixes (ADR-100 decision 10).
   *  Shown only for a folder that carries some — see `canRunDiscovery`. Omit to hide the item. */
  onRunDiscovery?: (group: NodeGroup) => void;
}

export function NodeTree({
  groups,
  nodes,
  canEdit,
  groupCounts,
  loadedGroups,
  revealedGroups,
  loading,
  selected,
  onSelectNode,
  onSelectGroup,
  onSelectNone,
  filter,
  narrowed,
  showToolbar = true,
  onOpenNode,
  onAddGroup,
  onEditGroup,
  onDeleteGroup,
  onEditNode,
  onAddNode,
  onDeleteNode,
  onRequestMoveNode,
  checked,
  anchorId,
  onCheckedChange,
  onMoveChecked,
  onMoveCheckedByPrefix,
  onMoveNodeByPrefix,
  onMoveNodes,
  onMoveGroup,
  onReorderNode,
  onReorderGroup,
  suppression,
  suppressionRows,
  onRelease,
  onSetMaintenance,
  onSetMute,
  pools,
  onSetPool,
  onRunDiscovery,
}: Props) {
  const { t } = useTranslation('nodes');
  const tree = useMemo(() => buildNodeTree(groups, nodes), [groups, nodes]);
  // Expansion defaults to fully-expanded and persists across reloads: the prefs store keeps the
  // set of groups the user explicitly collapsed (empty ⇒ everything open), so the last layout is
  // restored and any newly-added group shows expanded automatically.
  const collapsed = usePrefsStore((s) => s.nodeTreeCollapsed);
  const toggle = usePrefsStore((s) => s.toggleNodeTreeGroup);
  const [drag, setDrag] = useState<DragItem | null>(null);
  const [dropTarget, setDropTarget] = useState<DropTarget>(null);
  const [menu, setMenu] = useState<Menu>(null);
  /** Group row whose ＋ menu is open — keeps that row's hover-revealed actions on screen. */
  const [addMenuGroup, setAddMenuGroup] = useState<string | null>(null);

  // Active name filter (case-insensitive). While filtering, every group is force-expanded and
  // non-matching rows are hidden, so matches are always revealed — and a group matched by its own
  // name reveals its whole subtree, members included.
  const q = filterTerm(filter ?? '');
  const filtering = q.length > 0 || narrowed === true;
  // The flattened, display-ordered list of visible rows — the single source of truth the virtualized
  // body renders (collapse state + filter applied). Only the on-screen window is turned into DOM, so
  // a tens-of-thousands-node inventory stays responsive (S13).
  const flat = useMemo(
    () =>
      flattenTree(tree, {
        collapsed,
        filter: filter ?? '',
        narrowed,
        groupCounts,
        loadedGroups,
        revealedGroups,
      }),
    [tree, collapsed, filter, narrowed, groupCounts, loadedGroups, revealedGroups],
  );
  const scrollRef = useRef<HTMLDivElement>(null);
  const rowVirtualizer = useVirtualizer({
    count: flat.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => ROW_H,
    overscan: 16,
    getItemKey: (index) => flatRowKey(flat[index]),
  });
  // Row click selects (drives the split detail pane); without a select handler, fall back to the
  // legacy "open node" behaviour so the tree still works on its own.
  //
  // Clicking the row that is already selected clears it instead (ADR-073 decision 1). Both the row
  // and its name button funnel through here, so one place covers both. This is the gesture that
  // works when the others cannot: the blank-space click needs blank space, which a tree taller than
  // its pane does not have, and Escape needs a keyboard.
  const isSelected = (kind: 'node' | 'group', id: string) =>
    selected?.kind === kind && selected.id === id;
  const selectNode = (node: NodeSummary) => {
    if (onSelectNone && isSelected('node', node.id)) return onSelectNone();
    return onSelectNode ? onSelectNode(node) : onOpenNode(node);
  };
  const selectGroup = (group: NodeGroup) => {
    if (onSelectNone && isSelected('group', group.id)) return onSelectNone();
    onSelectGroup?.(group);
  };

  const checkedNodes: CheckedNodes = checked ?? EMPTY_CHECKED;

  /** Apply what a click on a node row decided (ADR-124 決定 2/4 + 増分 1).
   *
   *  🚨 **The decision itself is in `nodeTreeSelect.ts` and must stay there.** This function held
   *  it once, and that is exactly how the two-click Shift range shipped broken: Vitest never loads
   *  a `.tsx`, so the branch that failed to set the anchor was the one branch no test could run,
   *  while `rangeChecked` — handed an anchor by the test itself — passed every case.
   *
   *  Ctrl / Shift never touch `?sel=`, so the pane keeps showing whatever was open while a batch
   *  is assembled. */
  const clickNode = (e: React.MouseEvent, node: NodeSummary) => {
    if (!onCheckedChange) return selectNode(node);
    const outcome = clickOutcome(e, node, {
      flat,
      anchorId: anchorId ?? null,
      // Passed whole: whether a folder selection may start a batch is decided in the `.ts`.
      selection: selected ?? null,
      checked: checkedNodes,
    });
    if (outcome.checked) onCheckedChange(outcome.checked, outcome.anchorId);
    if (outcome.select) selectNode(node);
  };

  // Whether a right-click on each row kind would produce a menu with anything in it.
  //
  // 🚨 **These used to be one `if (!canEdit) return;`**, and that shipped an operator a tree with
  // no context menu at all — `canEdit` is `ManageConfig`, while the maintenance and mute entries
  // inside the menu are `ManageMaintenance` and `AckAlerts`, which an operator holds (ADR-057).
  // Closing a mixed menu on its strictest member takes the looser items with it, silently. It read
  // as deliberate because it was *consistent*: an admin saw the menu, so nothing looked broken.
  //
  // A node row always has one item — Open, which is navigation and needs no permission — so its
  // menu always opens. The other two are conditional, because an empty menu is worse than none.
  const caps: MenuCapabilities = {
    canEdit,
    canSuppress: !!onSetMaintenance || !!onSetMute,
    canAddNode: !!onAddNode,
  };

  // The suppression markers (maintenance wrench + mute bell-off) shown on a row when active, plus
  // the dashed-outline variant for a node released from a suppression it inherited. Each is a
  // button: clicking one opens the panel below, which is the only place in the UI that answers
  // "why is this row silent".
  const suppressionMarks = (m: {
    target: SuppressionTarget;
    node?: NodeSummary;
    maint: boolean;
    muted: boolean;
    releasedMaint?: boolean;
    releasedMute?: boolean;
  }): React.ReactNode => {
    if (!m.maint && !m.muted && !m.releasedMaint && !m.releasedMute) return null;
    const open = (e: React.MouseEvent) => {
      e.preventDefault();
      // Without this the document-level closer installed below runs in the same click and shuts
      // the panel on the frame it opened.
      e.stopPropagation();
      setMenu({ x: e.clientX, y: e.clientY, kind: 'suppress', target: m.target, node: m.node });
    };
    const mark = (cls: string, title: string, icon: React.ReactNode) => (
      <button type="button" className={`ntree-supp-icon ${cls}`} title={title} onClick={open}>
        {icon}
      </button>
    );
    return (
      <span className="ntree-supp">
        {m.maint && mark('maint', t('tree.suppression.markMaint'), <WrenchIcon />)}
        {m.muted && mark('mute', t('tree.suppression.markMute'), <BellOffIcon />)}
        {m.releasedMaint &&
          mark('maint released', t('tree.suppression.markReleasedMaint'), <WrenchIcon />)}
        {/* A plain bell, not a struck-through bell-off: the mute glyph is *already* a negation, so
            negating it again gave two icons that differ by one faint diagonal at 16px and were
            reported as indistinguishable. Un-slashing says "this node rings again" outright. */}
        {m.releasedMute &&
          mark('mute released', t('tree.suppression.markReleasedMute'), <BellIcon />)}
      </span>
    );
  };

  /** The panel a marker click opens: every suppression on this row, and what can be done to it.
   *  Which blocks appear and what each one offers is decided by `lib/suppression`; this renders
   *  what it returns. That split is not cosmetic — Vitest never loads a `.tsx`, so a judgement made
   *  here is a judgement nothing tests, and the first version of this panel got exactly that wrong
   *  (a released node was offered a release it already had). */
  const suppressionPanel = (m: Extract<Menu, { kind: 'suppress' }>): React.ReactNode => {
    const act = (a: ReleaseAction) => {
      onRelease?.(a);
      setMenu(null);
    };
    const rows = suppressionRows?.(m.target, m.node) ?? [];
    return (
      <div className="ntree-supp-panel">
        {rows.length === 0 ? (
          // Reachable as a race — the window ended between the render that lit the marker and the
          // click. Saying so beats an empty box.
          <div className="ntree-supp-cause">
            <div className="ntree-supp-note">{t('tree.suppression.none')}</div>
          </div>
        ) : (
          rows.map((r) => {
            // Bound before the closure: TypeScript drops a property narrowing inside a callback.
            const control = r.action;
            return (
              <div className="ntree-supp-cause" key={r.key}>
                <div className="ntree-supp-head">{t(r.headKey)}</div>
                {r.title && <div className="ntree-supp-title">{r.title}</div>}
                {(r.labelKey || r.endsAt) && (
                  <div className="ntree-supp-meta">
                    {r.labelKey && t(r.labelKey, r.labelParams)}
                    {r.labelKey && r.endsAt && ' · '}
                    {r.endsAt &&
                      t('tree.suppression.until', { time: formatScheduleTime(r.endsAt) })}
                  </div>
                )}
                {/* Not gated on `canEdit`: that is ManageConfig (editing the inventory), and
                    ending a window or lifting a mute is not. The page decides — it withholds
                    `onRelease` entirely when the caller may release neither kind, and strips the
                    action from the blocks of the kind they may not (`releasableRows`). */}
                {onRelease && control && (
                  <div className="ntree-supp-act">
                    <button type="button" onClick={() => act(control.action)}>
                      {t(control.labelKey)}
                    </button>
                  </div>
                )}
                {onRelease && !control && r.noteKey && (
                  <div className="ntree-supp-note">{t(r.noteKey, r.noteParams)}</div>
                )}
              </div>
            );
          })
        )}
      </div>
    );
  };

  // The Maintenance/Mute quick-duration section appended to a row's context menu. A preset fires
  // immediately (now + length); "Custom…" opens the full create form prefilled with the scope.
  //
  // Releasing is *not* a fourth chip beside them. It switches this menu to the panel at the same
  // coordinates, so the decision of what a release does to each cause lives in exactly one place —
  // and so a mis-aimed click lands on a panel that names what it would release rather than on an
  // action. The chips create suppression, which is safe to get wrong; releasing is what makes a
  // fleet page during planned work.
  const suppressionMenu = (
    target: SuppressionTarget,
    node: NodeSummary | undefined,
    at: { x: number; y: number },
  ): React.ReactNode => {
    if (!onSetMaintenance && !onSetMute) return null;
    const row = (label: string, handler: (t: SuppressionTarget, ms: number | null) => void) => (
      <div className="ntree-menu-section">
        <div className="ntree-menu-label">{label}</div>
        <div className="ntree-menu-durs">
          {DURATION_PRESETS.map((p) => (
            <button
              type="button"
              key={p.label}
              className="ntree-dur"
              onClick={() => {
                handler(target, p.ms);
                setMenu(null);
              }}
            >
              {p.label}
            </button>
          ))}
          <button
            type="button"
            className="ntree-dur"
            onClick={() => {
              handler(target, null);
              setMenu(null);
            }}
          >
            {t('tree.custom')}
          </button>
        </div>
      </div>
    );
    return (
      <>
        <div className="ntree-menu-sep" />
        {onSetMaintenance && row(t('tree.maintenance'), onSetMaintenance)}
        {onSetMute && row(t('tree.mute'), onSetMute)}
        {onRelease && hasSuppression(suppression, target, node) && (
          <button
            type="button"
            onClick={() =>
              setMenu({ x: at.x, y: at.y, kind: 'suppress', target, node })
            }
          >
            {t('tree.suppression.act.open')}
          </button>
        )}
      </>
    );
  };

  // The poll-pool chip section appended to a row's context menu (ADR-009/020). A pool chip assigns
  // immediately, "Inherit" clears the target's own pool, and "Custom…" opens the dialog for a pool
  // that doesn't exist yet. Same shape and same immediate-write behaviour as `suppressionMenu`.
  //
  // `currentPool` is the target's OWN pool (`null` ⇒ inherited), which is exactly what these chips
  // write — so it is what marks the active one.
  const poolMenu = (
    target: SuppressionTarget,
    currentPool: string | null | undefined,
  ): React.ReactNode => {
    if (!onSetPool) return null;
    const choices = poolChoices(pools ?? [], currentPool);
    const inherited = !currentPool?.trim();
    const chip = (
      key: string,
      label: string,
      value: string | null,
      opts: { current?: boolean; warn?: boolean } = {},
    ) => (
      <button
        type="button"
        key={key}
        className={`ntree-dur${opts.current ? ' is-current' : ''}${opts.warn ? ' warn' : ''}`}
        // The warning is spelled out for screen readers and on hover, not carried by colour alone.
        title={opts.warn ? t('tree.poolNoLivePoller') : undefined}
        onClick={() => {
          onSetPool(target, value);
          setMenu(null);
        }}
      >
        {label}
        {opts.warn && <span aria-hidden="true"> !</span>}
      </button>
    );
    return (
      <>
        <div className="ntree-menu-sep" />
        <div className="ntree-menu-section">
          <div className="ntree-menu-label">{t('tree.pool')}</div>
          <div className="ntree-menu-durs">
            {choices.map((c) =>
              chip(c.name, c.name, c.name, { current: c.current, warn: !c.live }),
            )}
            {chip('__inherit', t('tree.poolInherit'), '', { current: inherited })}
            {chip('__custom', t('tree.custom'), null)}
          </div>
        </div>
      </>
    );
  };

  /** Outside click and Escape both land here, from `AnchoredPopover` (ADR-124 Inc.2). Stable, so
   *  the popover's document listeners are not re-subscribed on every SSE-driven render. */
  const closeMenu = useCallback(() => setMenu(null), []);

  /** Whether the context menu was open when the pointer went down on the tree body.
   *
   *  🚨 The popover dismisses on **mousedown**, so by the time the body's `click` fires `menu` is
   *  already null — and a body that read it then would close the menu AND clear `?sel=` in one
   *  press, which is the layering ADR-073 決定 4 forbids (transient first). The old menu closed on
   *  `click`, after the body had already seen it open; this ref is that ordering, kept. */
  const menuAtDown = useRef(false);

  /** Where the tree was parked when the pointer last went down on **a control**, or null when
   *  there is nothing to put back (ADR-124 増分 5).
   *
   *  🚨 **Only a press on a control pins, and any scroll clears it.** A pin taken from every press
   *  goes stale — pressing the scrollbar fires no `click` on the body, so nothing resets it, and
   *  the next keyboard focus would drag the operator back to where they were before their own
   *  wheel. A keyboard `Tab` has no pin at all, so the browser still scrolls the target into
   *  view, which for a keyboard is the correct behaviour. */
  const pinned = useRef<ScrollAt | null>(null);

  const reset = () => {
    setDrag(null);
    setDropTarget(null);
  };

  /** Every row this drag will move. 🚨 **`.dragging` used to name the grabbed row alone**, so a
   *  three-row selection dimmed one row — which was the defect saying so on screen, a release
   *  before anyone read it (ADR-124 Inc.4). A `Set` rather than `ids.includes` because the batch
   *  runs to 1000 and this is asked once per visible row per render. */
  const draggingIds = useMemo(
    () => new Set(drag === null ? [] : drag.kind === 'node' ? drag.ids : [drag.id]),
    [drag],
  );

  /** The cursor's position inside the row, as the numbers `dropPosition` decides from. */
  const positionFor = (e: React.DragEvent, targetIsGroup: boolean): DropPos => {
    const rect = (e.currentTarget as HTMLElement).getBoundingClientRect();
    return dropPosition(e.clientY - rect.top, rect.height, targetIsGroup, drag);
  };

  const onRowDragOver = (e: React.DragEvent, target: Target, targetIsGroup: boolean) => {
    if (!drag) return;
    e.preventDefault();
    e.stopPropagation();
    e.dataTransfer.dropEffect = 'move';
    const position = positionFor(e, targetIsGroup);
    setDropTarget({ id: target.id, position, ok: dropAllowed(groups, drag, target, position) });
  };

  const onRowDrop = (e: React.DragEvent, target: Target, targetIsGroup: boolean) => {
    e.preventDefault();
    e.stopPropagation();
    if (!drag) return;
    const position = positionFor(e, targetIsGroup);
    if (!dropAllowed(groups, drag, target, position)) {
      reset();
      return;
    }
    perform(dropAction(drag, target, position));
    reset();
  };

  /** Turn a decided action into the callback it names. The exhaustive switch is what makes a new
   *  `DropAction` shape a compile error here rather than a drop that does nothing. */
  const perform = (a: DropAction) => {
    switch (a.kind) {
      case 'move-nodes':
        return onMoveNodes(a.nodeIds, a.groupId);
      case 'move-group':
        return onMoveGroup(a.groupId, a.parentId);
      case 'reorder-node':
        return onReorderNode(a.nodeId, {
          groupId: a.groupId,
          ...(a.before ? { before: a.before } : { after: a.after }),
        });
      case 'reorder-group':
        return onReorderGroup(a.groupId, {
          parentId: a.parentId,
          ...(a.before ? { before: a.before } : { after: a.after }),
        });
    }
  };

  const dropOnRoot = () => {
    if (!drag) return;
    perform(rootDropAction(drag));
    reset();
  };

  /** Drop-feedback class for a row that is the current target. */
  const dropClass = (id: string): string => {
    if (!dropTarget || dropTarget.id !== id) return '';
    return dropTarget.ok ? ` drop-${dropTarget.position}` : ' drop-bad';
  };

  const groupRow = (row: Extract<FlatRow, { kind: 'group' }>): React.ReactNode => {
    const { group, depth, isOpen, hasChildren, tally } = row;
    const isSel = selected?.kind === 'group' && selected.id === group.id;
    const target: Target = { kind: 'group', id: group.id, scope: group.parent_id ?? null };
    return (
      <div
        className={`ntree-row ntree-grow${isSel ? ' sel' : ''}${dropClass(group.id)}${draggingIds.has(group.id) ? ' dragging' : ''}`}
        style={{ paddingLeft: depth * INDENT + BASE_PAD }}
        draggable={canEdit}
        onClick={() => selectGroup(group)}
        onDragStart={(e) => {
          e.stopPropagation();
          e.dataTransfer.effectAllowed = 'move';
          setDrag({ kind: 'group', id: group.id });
        }}
        onDragEnd={reset}
        onDragOver={(e) => onRowDragOver(e, target, true)}
        onDrop={(e) => onRowDrop(e, target, true)}
        onContextMenu={(e) => {
          if (!groupMenuHasItems(caps)) return;
          e.preventDefault();
          setMenu({ x: e.clientX, y: e.clientY, kind: 'group', group });
        }}
      >
        <button
          type="button"
          className={`ntree-twisty${isOpen ? ' open' : ''}`}
          onClick={(e) => {
            e.stopPropagation();
            toggle(group.id);
          }}
          aria-label={isOpen ? t('nav:shell.collapse') : t('nav:shell.expand')}
          disabled={!hasChildren}
        >
          ▶
        </button>
        <span className="ntree-icon">
          <GroupIcon type={asGroupType(group.group_type)} />
        </span>
        <button
          type="button"
          className="ntree-grp-name"
          onClick={(e) => {
            e.stopPropagation();
            selectGroup(group);
          }}
        >
          {group.name}
        </button>
        <HealthBar tally={tally} className="ntree-health" />
        <span className="ntree-count">{tally.total}</span>
        {/* The hover-revealed actions come BEFORE the markers on purpose — see `.ntree-actions` in
            the stylesheet. Revealing them shifts everything to their left, and a marker that moves
            under the pointer is a mis-click onto Delete group. */}
        {canEdit && (
          // `menu-open` keeps the hover-revealed actions rendered while this row's ＋ menu is up:
          // the menu opens BELOW the row, so reaching it takes the pointer off the row, and the
          // hover rule would otherwise unmount the menu on the way there.
          <span
            className={`ntree-actions${addMenuGroup === group.id ? ' menu-open' : ''}`}
          >
            <ActionMenu
              label={t('addMenu.label')}
              align="end"
              onOpenChange={(o) => setAddMenuGroup(o ? group.id : null)}
              items={[
                ...(onAddNode
                  ? [
                      {
                        key: 'node',
                        label: t('tree.addNodeHere'),
                        onSelect: () => onAddNode(group.id),
                      },
                    ]
                  : []),
                {
                  key: 'group',
                  label: t('group.addSubgroup'),
                  onSelect: () => onAddGroup(group.id),
                },
              ]}
              trigger={(p) => (
                <button
                  {...p}
                  type="button"
                  className="ntree-act"
                  title={t('addMenu.trigger')}
                  aria-label={t('addMenu.trigger')}
                >
                  ＋
                </button>
              )}
            />
            <button
              type="button"
              className="ntree-act"
              title={t('tree.editMoveGroup')}
              onClick={(e) => {
                e.stopPropagation();
                onEditGroup(group);
              }}
            >
              ✎
            </button>
            <button
              type="button"
              className="ntree-act"
              title={t('group.delete')}
              onClick={(e) => {
                e.stopPropagation();
                onDeleteGroup(group);
              }}
            >
              🗑
            </button>
          </span>
        )}
        {suppressionMarks({
          target: { kind: 'group', id: group.id, name: group.name },
          maint: !!suppression?.maintenanceGroups.has(group.id),
          muted: !!suppression?.muteGroups.has(group.id),
        })}
      </div>
    );
  };

  const renderNode = (node: NodeSummary, depth: number): React.ReactNode => {
    const target: Target = { kind: 'node', id: node.id, scope: node.group_id ?? null };
    const isSel = selected?.kind === 'node' && selected.id === node.id;
    // 🚨 A class of its own, never `sel`. `tests/ui/treeDeselect.spec.ts` pins `.ntree-row.sel`
    // at exactly one row, which is the property that proves the pane's selection is single.
    const isChecked = checkedNodes.has(node.id);
    const move = nodeMoveItems(checkedNodes, node.id, canEdit);
    return (
      <div
        className={`ntree-row ntree-node${isSel ? ' sel' : ''}${isChecked ? ' checked' : ''}${dropClass(node.id)}${draggingIds.has(node.id) ? ' dragging' : ''}`}
        key={node.id}
        style={{ paddingLeft: depth * INDENT + BASE_PAD }}
        draggable={canEdit}
        onClick={(e) => clickNode(e, node)}
        onDragStart={(e) => {
          e.stopPropagation();
          e.dataTransfer.effectAllowed = 'move';
          // What this carries is `nodeTreeDnd.ts`'s call, not a shape spelled out here — the
          // same rule the ↗ two elements below reads through `nodeMoveItems`.
          setDrag(nodeDragItem(checkedNodes, node.id));
        }}
        onDragEnd={reset}
        onDragOver={(e) => onRowDragOver(e, target, false)}
        onDrop={(e) => onRowDrop(e, target, false)}
        onContextMenu={(e) => {
          e.preventDefault();
          setMenu({ x: e.clientX, y: e.clientY, kind: 'node', node });
        }}
      >
        {/* Spacer keeps the status dot in the same column as a group's icon at this depth. */}
        <span className="ntree-twisty ntree-twisty-spacer" aria-hidden="true" />
        <span className="ntree-icon">
          <StatusDot state={node.state} withLabel={false} />
        </span>
        <button
          type="button"
          className="ntree-node-name"
          onClick={(e) => {
            e.stopPropagation();
            clickNode(e, node);
          }}
        >
          {node.name}
        </button>
        {/* What kind of node this is, when it is not an ordinary ICMP/SNMP device — a URL monitor,
            a DNS monitor or a Meraki device. Unmarked is the default: the tree is overwhelmingly
            ordinary devices, so a badge on every one of 50k rows would say nothing. */}
        {NODE_KIND_SPEC[node.kind].badge && (
          <span className="ntree-badge" title={t(NODE_KIND_SPEC[node.kind].labelKey)}>
            {NODE_KIND_SPEC[node.kind].badge}
          </span>
        )}
        {/* Before the markers — see `.ntree-actions` in the stylesheet. The ↗ acts on whatever the
            menu's move items act on: this row, or the working set it belongs to (`nodeMoveItems`,
            ADR-124 Inc.2) — one rule, not a second copy of it. */}
        {move && (
          <span className="ntree-actions">
            <button
              type="button"
              className="ntree-act"
              title={
                move.scope === 'selection' && onMoveChecked
                  ? t('tree.moveSelected', { count: move.count })
                  : t('tree.moveToGroup')
              }
              onClick={(e) => {
                e.stopPropagation();
                if (move.scope === 'selection' && onMoveChecked) onMoveChecked();
                else onRequestMoveNode(node);
              }}
            >
              ↗
            </button>
          </span>
        )}
        {suppressionMarks({
          target: { kind: 'node', id: node.id, name: node.name },
          node,
          // The index already accounts for a release, including re-adding a window that names the
          // node. `state` is the engine's rolled-up opinion and lags a release by up to one refresh
          // (~30s), so it is only consulted while the row is *not* released — otherwise the wrench
          // would sit next to the struck-through one for half a minute after every release.
          maint:
            !!suppression?.maintenanceNodes.has(node.id) ||
            (node.state === 'maintenance' && !suppression?.exemptMaintenanceNodes.has(node.id)),
          muted: !!suppression?.muteNodes.has(node.id),
          releasedMaint: !!suppression?.exemptMaintenanceNodes.has(node.id),
          releasedMute: !!suppression?.exemptMuteNodes.has(node.id),
        })}
      </div>
    );
  };

  // The Ungrouped section header row — also the root drop zone (drop here → move to top level) and
  // the right-click "add at top level" target. In the flattened list it's a single row; the old
  // wrapper's dashed separator moves onto the row via `.ntree-ungrouped-head`.
  const ungroupedHeadRow = (count: number): React.ReactNode => {
    const rootDropActive = dropTarget?.id === 'root' && !!drag;
    return (
      <div
        className={`ntree-row ntree-ungrouped-head${rootDropActive ? ' drop-inside' : ''}`}
        style={{ paddingLeft: BASE_PAD }}
        onDragOver={(e) => {
          if (!drag) return;
          e.preventDefault();
          setDropTarget({ id: 'root', position: 'inside', ok: true });
        }}
        onDrop={(e) => {
          e.preventDefault();
          dropOnRoot();
        }}
        onContextMenu={(e) => {
          if (!rootMenuHasItems(caps)) return;
          e.preventDefault();
          setMenu({ x: e.clientX, y: e.clientY, kind: 'root' });
        }}
      >
        <span className="ntree-twisty ntree-twisty-spacer" aria-hidden="true" />
        <span className="ntree-icon ntree-ungrouped-icon">⌁</span>
        <span className="ntree-grp-name ntree-ungrouped-label">{t('ungrouped')}</span>
        <span className="ntree-count">{count}</span>
      </div>
    );
  };

  // Placeholder shown under an open group whose members are still being lazily fetched (A-3).
  const loadingRow = (depth: number): React.ReactNode => (
    <div className="ntree-row ntree-loading" style={{ paddingLeft: depth * INDENT + BASE_PAD }}>
      <span className="ntree-twisty ntree-twisty-spacer" aria-hidden="true" />
      <span className="ntree-loading-label muted">{t('tree.loadingNodes')}</span>
    </div>
  );

  const renderRow = (row: FlatRow): React.ReactNode => {
    switch (row.kind) {
      case 'group':
        return groupRow(row);
      case 'node':
      case 'ungrouped-node':
        return renderNode(row.node, row.depth);
      case 'group-loading':
        return loadingRow(row.depth);
      case 'ungrouped-head':
        return ungroupedHeadRow(row.count);
    }
  };

  const virtualRows = rowVirtualizer.getVirtualItems();

  /** What the open node menu's move items act on (ADR-124 Inc.2). Decided in `nodeTreeMenu.ts`;
   *  here it is only applied. */
  const moveItems =
    menu?.kind === 'node' ? nodeMoveItems(checkedNodes, menu.node.id, canEdit) : null;

  /** The two items that act on the working set. Rendered in the single item's place when the
   *  right-clicked row is in the set, and below a separator when it is not. */
  const selectionMoveItems = (count: number): React.ReactNode => (
    <>
      {onMoveChecked && (
        <button
          type="button"
          onClick={() => {
            onMoveChecked();
            setMenu(null);
          }}
        >
          {t('tree.moveSelected', { count })}
        </button>
      )}
      {onMoveCheckedByPrefix && canMoveByPrefix(groups, canEdit) && (
        <button
          type="button"
          onClick={() => {
            onMoveCheckedByPrefix();
            setMenu(null);
          }}
        >
          {t('tree.moveSelectedByPrefix', { count })}
        </button>
      )}
    </>
  );

  return (
    <div className="ntree">
      {showToolbar && canEdit && (
        <div className="ntree-toolbar">
          <Button variant="outline" onClick={() => onAddGroup(null)}>
            ＋ {t('group.add')}
          </Button>
          <span className="muted ntree-hint">{t('tree.dragHint')}</span>
        </div>
      )}

      {/* Clicking the empty space below the rows clears the selection (ADR-073 decision 5).
          `e.target === e.currentTarget` is what makes this "blank space" and not "the tree": rows
          live inside the virtualizer's spacer, so a click on one never reports the body as its
          target. Deliberately a React `onClick` and not a `document` listener — the same-frame
          bug `rowMenu.spec.ts` pins (the click that opens a menu also reaching the handler that
          closes it) is specific to document-level dismissal, and this shape cannot have it.
          While the context menu is open the click only dismisses that: transient first (decision 4)
          — decided from what the body saw at mousedown, see `menuAtDown`. */}
      <div
        className="ntree-body"
        ref={scrollRef}
        // ADR-124 増分 5 決定 B, both halves, from one place. Capture phase, so a descendant that
        // ever stops mousedown propagation cannot disarm either of them — and `menuAtDown` is the
        // ADR-073 決定 4 ordering, which must not become optional. Same element, same event.
        onMouseDownCapture={(e) => {
          menuAtDown.current = menu !== null;
          pinFocusScroll(scrollRef.current, e.target as Element, (at) => {
            pinned.current = at;
          });
        }}
        // A scroll the OPERATOR made is the one scroll that must survive, so it voids the pin.
        // Our own restore writes fire this asynchronously, after the pin is already null.
        onScroll={() => {
          pinned.current = null;
        }}
        // React's spelling of `focusin`, so a focus landing on any descendant reaches this
        // element — which is what covers every control in every row from a single handler.
        onFocus={(e) => {
          restoreScroll(e.currentTarget, pinned.current);
          pinned.current = null;
        }}
        onClick={(e) => {
          const wasOpen = menuAtDown.current;
          menuAtDown.current = false;
          pinned.current = null;
          if (wasOpen || !onSelectNone) return;
          if (e.target === e.currentTarget) onSelectNone();
        }}
      >
        {flat.length === 0 ? (
          // Empty flat list: a blank body while filtering with no matches, else loading / empty-state.
          filtering ? null : loading ? (
            <p className="muted ntree-empty">{t('tree.loadingNodes')}</p>
          ) : (
            <p
              className="muted ntree-empty"
              onContextMenu={(e) => {
                if (!rootMenuHasItems(caps)) return;
                e.preventDefault();
                setMenu({ x: e.clientX, y: e.clientY, kind: 'root' });
              }}
            >
              {t('tree.emptyInventory')}
            </p>
          )
        ) : (
          // Virtualized body: only the on-screen window of `flat` is turned into DOM (S13).
          <div style={{ height: rowVirtualizer.getTotalSize(), position: 'relative' }}>
            {virtualRows.map((vi) => (
              <div
                key={vi.key}
                data-index={vi.index}
                style={{
                  position: 'absolute',
                  top: 0,
                  left: 0,
                  width: '100%',
                  transform: `translateY(${vi.start}px)`,
                }}
              >
                {renderRow(flat[vi.index])}
              </div>
            ))}
          </div>
        )}
      </div>

      {/* The context menu — `AnchoredPopover` at the right-click's point (ADR-124 Inc.2): portalled,
          clamped and flipped to stay inside the viewport, scrolling when taller than it, closed by
          Escape or an outside mousedown. Mounted only while open, so the same element carries every
          variant and a switch between them at the same point re-places rather than re-opens. */}
      {menu && (
        <AnchoredPopover
          open
          at={{ x: menu.x, y: menu.y }}
          role={menu.kind === 'suppress' ? 'dialog' : 'menu'}
          label={
            menu.kind === 'root'
              ? t('tree.menuRoot')
              : t('tree.menuFor', {
                  name:
                    menu.kind === 'group'
                      ? menu.group.name
                      : menu.kind === 'node'
                        ? menu.node.name
                        : menu.target.name,
                })
          }
          className="ntree-menu"
          onDismiss={closeMenu}
        >
          {menu.kind === 'suppress' ? (
            suppressionPanel(menu)
          ) : menu.kind === 'group' ? (
            <>
              {/* Reshaping the folder tree is `ManageConfig`; suppressing it is not. Each item
                  asks for its own permission so an operator who may open a maintenance window on
                  a folder still gets that half of the menu (ADR-057). */}
              {canEdit && (
                <button type="button" onClick={() => { onAddGroup(menu.group.id); setMenu(null); }}>
                  {t('group.addSubgroup')}
                </button>
              )}
              {onAddNode && (
                <button type="button" onClick={() => { onAddNode(menu.group.id); setMenu(null); }}>
                  {t('tree.addNodeHere')}
                </button>
              )}
              {canEdit && (
                <button type="button" onClick={() => { onEditGroup(menu.group); setMenu(null); }}>
                  {t('tree.editMove')}
                </button>
              )}
              {/* Aim a sweep at this site's subnets (ADR-100 decision 10). It navigates rather
                  than acting: a sweep needs credentials and a poll-pool, and the Discovery screen
                  is where those are chosen — so the folder's prefixes arrive in the target field
                  and a person presses Start with the ranges in front of them. */}
              {onRunDiscovery && canRunDiscovery(menu.group, caps) && (
                <button
                  type="button"
                  onClick={() => {
                    onRunDiscovery(menu.group);
                    setMenu(null);
                  }}
                >
                  {t('tree.runDiscovery')}
                </button>
              )}
              {poolMenu(
                { kind: 'group', id: menu.group.id, name: menu.group.name },
                menu.group.pool,
              )}
              {suppressionMenu(
                { kind: 'group', id: menu.group.id, name: menu.group.name },
                undefined,
                menu,
              )}
              {canEdit && (
                <>
                  <div className="ntree-menu-sep" />
                  <button type="button" className="danger" onClick={() => { onDeleteGroup(menu.group); setMenu(null); }}>
                    {t('common:actions.delete')}
                  </button>
                </>
              )}
            </>
          ) : menu.kind === 'node' ? (
            <>
              <button type="button" onClick={() => { onOpenNode(menu.node); setMenu(null); }}>
                {t('tree.open')}
              </button>
              {onEditNode && (
                <button type="button" onClick={() => { onEditNode(menu.node); setMenu(null); }}>
                  {t('tree.editNodeEllipsis')}
                </button>
              )}
              {/* The move items, and what they act on (ADR-124 Inc.2). A right-click on a row that
                  is in the working set moves the working set and offers nothing that moves one —
                  the single item used to sit here above the batch item, and on a menu that ran off
                  the bottom of the screen it was the only one the operator could see. */}
              {moveItems?.scope === 'selection' && selectionMoveItems(moveItems.count)}
              {moveItems?.scope === 'row' && (
                <>
                  <button
                    type="button"
                    onClick={() => {
                      onRequestMoveNode(menu.node);
                      setMenu(null);
                    }}
                  >
                    {moveItems.nameTheRow
                      ? t('tree.moveNodeToGroup', { name: menu.node.name })
                      : t('tree.moveToGroup')}
                  </button>
                  {onMoveNodeByPrefix && canMoveByPrefix(groups, canEdit) && (
                    <button
                      type="button"
                      onClick={() => {
                        onMoveNodeByPrefix(menu.node);
                        setMenu(null);
                      }}
                    >
                      {moveItems.nameTheRow
                        ? t('tree.moveNodeByPrefix', { name: menu.node.name })
                        : t('tree.moveByPrefix')}
                    </button>
                  )}
                </>
              )}
              {/* The way in for someone who does not know the keyboard gesture (ADR-055 R6).
                  Ctrl / Shift is written nowhere on screen until a batch exists, so without this
                  item the feature is reachable only by people who were told about it. */}
              {onCheckedChange && (
                <button
                  type="button"
                  onClick={() => {
                    onCheckedChange(toggleChecked(checkedNodes, menu.node), menu.node.id);
                    setMenu(null);
                  }}
                >
                  {checkedNodes.has(menu.node.id)
                    ? t('tree.removeFromSelection')
                    : t('tree.addToSelection')}
                </button>
              )}
              {/* A batch exists but this row is not in it: the batch is still offered, below the
                  row's own (named) items, because the operator may well have meant it. */}
              {moveItems?.alsoSelection && (
                <>
                  <div className="ntree-menu-sep" />
                  {selectionMoveItems(moveItems.count)}
                </>
              )}
              {onAddNode && (
                <button type="button" onClick={() => { onAddNode(menu.node.group_id ?? null); setMenu(null); }}>
                  {t('tree.addNodeEllipsis')}
                </button>
              )}
              {poolMenu({ kind: 'node', id: menu.node.id, name: menu.node.name }, menu.node.pool)}
              {suppressionMenu(
                { kind: 'node', id: menu.node.id, name: menu.node.name },
                menu.node,
                menu,
              )}
              {onDeleteNode && (
                <>
                  <div className="ntree-menu-sep" />
                  <button type="button" className="danger" onClick={() => { onDeleteNode(menu.node); setMenu(null); }}>
                    {t('tree.deleteEllipsis')}
                  </button>
                </>
              )}
            </>
          ) : (
            // kind === 'root': right-click on the Ungrouped header / empty tree → add at top level.
            onAddNode && (
              <button type="button" onClick={() => { onAddNode(null); setMenu(null); }}>
                {t('tree.addNodeHere')}
              </button>
            )
          )}
        </AnchoredPopover>
      )}
    </div>
  );
}
