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

import { useEffect, useMemo, useRef, useState } from 'react';
import { useVirtualizer } from '@tanstack/react-virtual';
import { useTranslation } from 'react-i18next';
import type { NodeGroup, NodeSummary } from '../../types/api';
import {
  buildNodeTree,
  flattenTree,
  flatRowKey,
  isSelfOrDescendant,
  type FlatRow,
  type StateCounts,
  type TreeGroup,
} from '../../lib/nodeTree';
import { usePrefsStore } from '../../prefs';
import {
  DURATION_PRESETS,
  type SuppressionIndex,
  type SuppressionTarget,
} from '../../lib/suppression';
import { StatusDot } from '../ui/StatusDot';
import { Button } from '../ui/Button';
import { WrenchIcon, BellOffIcon } from '../ui/icons';
import { HealthBar } from '../HealthBar/HealthBar';
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

type DragItem = { kind: 'node' | 'group'; id: string };
type DropPos = 'before' | 'after' | 'inside';
type DropTarget = { id: string | 'root'; position: DropPos; ok: boolean } | null;
/** What a drop landed on: a group/node row (with its sibling scope) or the root zone. */
type Target =
  | { kind: 'group'; id: string; scope: string | null }
  | { kind: 'node'; id: string; scope: string | null };
type Menu =
  | { x: number; y: number; kind: 'group'; group: TreeGroup }
  | { x: number; y: number; kind: 'node'; node: NodeSummary }
  | { x: number; y: number; kind: 'root' }
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
  /** First inventory load in flight — show a loading placeholder, not the empty message. */
  loading?: boolean;
  /** Currently-selected row (highlighted with the inset accent bar); drives the split detail pane. */
  selected?: TreeSelection;
  /** Select a node/group row (single-click). Falls back to `onOpenNode` when not provided. */
  onSelectNode?: (node: NodeSummary) => void;
  onSelectGroup?: (group: NodeGroup) => void;
  /** Case-insensitive name filter; non-empty force-expands and hides non-matching rows. */
  filter?: string;
  /** Render the internal Add-group / drag-hint toolbar (the split hosts Add-group in its pane head). */
  showToolbar?: boolean;
  onOpenNode: (node: NodeSummary) => void;
  onAddGroup: (parentId: string | null) => void;
  onEditGroup: (group: NodeGroup) => void;
  onDeleteGroup: (group: NodeGroup) => void;
  /** Right-click → add a monitoring node, placed into `groupId` (`null` = top level / Ungrouped).
   *  The manual, Discovery-free way to add a target. Omit to hide the menu item. */
  onAddNode?: (groupId: string | null) => void;
  /** Right-click → delete a node (opens a destructive-consent modal). Omit to hide the item. */
  onDeleteNode?: (node: NodeSummary) => void;
  /** Open the "move node" picker (context-menu / button path, keyboard-accessible). */
  onRequestMoveNode: (node: NodeSummary) => void;
  /** Move a node into a group (or null = ungroup), appending it — drop onto a group / picker. */
  onMoveNode: (nodeId: string, groupId: string | null) => void;
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
  /** Right-click → put a node/group into maintenance. `durationMs` = preset length from now;
   *  `null` = open the full create form prefilled with the scope ("Custom…"). */
  onSetMaintenance?: (target: SuppressionTarget, durationMs: number | null) => void;
  /** Right-click → mute a node/group. `durationMs`/`null` as for `onSetMaintenance`. */
  onSetMute?: (target: SuppressionTarget, durationMs: number | null) => void;
}

export function NodeTree({
  groups,
  nodes,
  canEdit,
  groupCounts,
  loadedGroups,
  loading,
  selected,
  onSelectNode,
  onSelectGroup,
  filter,
  showToolbar = true,
  onOpenNode,
  onAddGroup,
  onEditGroup,
  onDeleteGroup,
  onAddNode,
  onDeleteNode,
  onRequestMoveNode,
  onMoveNode,
  onMoveGroup,
  onReorderNode,
  onReorderGroup,
  suppression,
  onSetMaintenance,
  onSetMute,
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

  // Active name filter (case-insensitive). While filtering, every group is force-expanded and
  // non-matching rows are hidden, so matches are always revealed.
  const q = (filter ?? '').trim().toLowerCase();
  const filtering = q.length > 0;
  // The flattened, display-ordered list of visible rows — the single source of truth the virtualized
  // body renders (collapse state + filter applied). Only the on-screen window is turned into DOM, so
  // a tens-of-thousands-node inventory stays responsive (S13).
  const flat = useMemo(
    () => flattenTree(tree, { collapsed, filter: filter ?? '', groupCounts, loadedGroups }),
    [tree, collapsed, filter, groupCounts, loadedGroups],
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
  const selectNode = (node: NodeSummary) => (onSelectNode ? onSelectNode(node) : onOpenNode(node));
  const selectGroup = (group: NodeGroup) => onSelectGroup?.(group);

  // The suppression markers (maintenance wrench + mute bell-off) shown on a row when active.
  const suppressionMarks = (maint: boolean, muted: boolean): React.ReactNode => {
    if (!maint && !muted) return null;
    return (
      <span className="ntree-supp">
        {maint && (
          <span className="ntree-supp-icon maint" title={t('tree.maintTitle')}>
            <WrenchIcon />
          </span>
        )}
        {muted && (
          <span className="ntree-supp-icon mute" title={t('tree.muteTitle')}>
            <BellOffIcon />
          </span>
        )}
      </span>
    );
  };

  // The Maintenance/Mute quick-duration section appended to a row's context menu. A preset fires
  // immediately (now + length); "Custom…" opens the full create form prefilled with the scope.
  const suppressionMenu = (target: SuppressionTarget): React.ReactNode => {
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
      </>
    );
  };

  // Close the context menu on any outside click / Escape.
  useEffect(() => {
    if (!menu) return;
    const close = () => setMenu(null);
    const onKey = (e: KeyboardEvent) => e.key === 'Escape' && setMenu(null);
    document.addEventListener('click', close);
    document.addEventListener('keydown', onKey);
    return () => {
      document.removeEventListener('click', close);
      document.removeEventListener('keydown', onKey);
    };
  }, [menu]);

  const reset = () => {
    setDrag(null);
    setDropTarget(null);
  };

  /** Read the drop position from the cursor's Y in the row. A node dragged over a group always
   *  means "inside" (a node can't be a sibling of a group); otherwise the top/bottom quarters of a
   *  group (or top/bottom half of a node) are before/after, the middle of a group is inside. */
  const positionFor = (e: React.DragEvent, targetIsGroup: boolean): DropPos => {
    if (drag?.kind === 'node' && targetIsGroup) return 'inside';
    const rect = (e.currentTarget as HTMLElement).getBoundingClientRect();
    const y = e.clientY - rect.top;
    const h = rect.height || 1;
    if (targetIsGroup) {
      if (y < h * 0.25) return 'before';
      if (y > h * 0.75) return 'after';
      return 'inside';
    }
    return y < h * 0.5 ? 'before' : 'after';
  };

  /** Whether the current drag may drop on `target` at `position` (cycle guard for group nesting). */
  const dropAllowed = (target: Target, position: DropPos): boolean => {
    if (!drag) return false;
    if (drag.kind === 'node') {
      // A node can reorder next to another node or be assigned into a group, but not onto itself.
      return !(target.kind === 'node' && target.id === drag.id);
    }
    // Dragging a group: it relates to groups only, never to a node, and never to itself.
    if (target.kind === 'node' || target.id === drag.id) return false;
    if (position === 'inside') return !isSelfOrDescendant(groups, drag.id, target.id);
    // before/after re-parents the group to the target's parent scope.
    return target.scope == null || !isSelfOrDescendant(groups, drag.id, target.scope);
  };

  const onRowDragOver = (e: React.DragEvent, target: Target, targetIsGroup: boolean) => {
    if (!drag) return;
    e.preventDefault();
    e.stopPropagation();
    e.dataTransfer.dropEffect = 'move';
    const position = positionFor(e, targetIsGroup);
    setDropTarget({ id: target.id, position, ok: dropAllowed(target, position) });
  };

  const onRowDrop = (e: React.DragEvent, target: Target, targetIsGroup: boolean) => {
    e.preventDefault();
    e.stopPropagation();
    if (!drag) return;
    const position = positionFor(e, targetIsGroup);
    if (!dropAllowed(target, position)) {
      reset();
      return;
    }
    if (drag.kind === 'node') {
      if (target.kind === 'group') {
        onMoveNode(drag.id, target.id); // assign into the group (append)
      } else {
        const rel = position === 'before' ? { before: target.id } : { after: target.id };
        onReorderNode(drag.id, { groupId: target.scope, ...rel });
      }
    } else if (position === 'inside') {
      onMoveGroup(drag.id, target.id); // nest under the group (append)
    } else {
      const rel = position === 'before' ? { before: target.id } : { after: target.id };
      onReorderGroup(drag.id, { parentId: target.scope, ...rel });
    }
    reset();
  };

  const dropOnRoot = () => {
    if (!drag) return;
    if (drag.kind === 'node') onMoveNode(drag.id, null);
    else onMoveGroup(drag.id, null);
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
    const target: Target = { kind: 'group', id: group.id, scope: group.parent_id };
    return (
      <div
        className={`ntree-row ntree-grow${isSel ? ' sel' : ''}${dropClass(group.id)}${drag?.id === group.id ? ' dragging' : ''}`}
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
          if (!canEdit) return;
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
          <GroupIcon type={group.group_type} />
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
        {suppressionMarks(
          !!suppression?.maintenanceGroups.has(group.id),
          !!suppression?.muteGroups.has(group.id),
        )}
        {canEdit && (
          <span className="ntree-actions">
            <button
              type="button"
              className="ntree-act"
              title={t('group.addSubgroup')}
              onClick={(e) => {
                e.stopPropagation();
                onAddGroup(group.id);
              }}
            >
              ＋
            </button>
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
      </div>
    );
  };

  const renderNode = (node: NodeSummary, depth: number): React.ReactNode => {
    const target: Target = { kind: 'node', id: node.id, scope: node.group_id };
    const isSel = selected?.kind === 'node' && selected.id === node.id;
    return (
      <div
        className={`ntree-row ntree-node${isSel ? ' sel' : ''}${dropClass(node.id)}${drag?.id === node.id ? ' dragging' : ''}`}
        key={node.id}
        style={{ paddingLeft: depth * INDENT + BASE_PAD }}
        draggable={canEdit}
        onClick={() => selectNode(node)}
        onDragStart={(e) => {
          e.stopPropagation();
          e.dataTransfer.effectAllowed = 'move';
          setDrag({ kind: 'node', id: node.id });
        }}
        onDragEnd={reset}
        onDragOver={(e) => onRowDragOver(e, target, false)}
        onDrop={(e) => onRowDrop(e, target, false)}
        onContextMenu={(e) => {
          if (!canEdit) return;
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
            selectNode(node);
          }}
        >
          {node.name}
        </button>
        {node.source === 'meraki' && (
          <span className="ntree-badge ntree-badge-meraki" title={t('tree.merakiBadgeTitle')}>
            Meraki
          </span>
        )}
        {suppressionMarks(
          node.state === 'maintenance' || !!suppression?.maintenanceNodes.has(node.id),
          !!suppression?.muteNodes.has(node.id),
        )}
        {canEdit && (
          <span className="ntree-actions">
            <button
              type="button"
              className="ntree-act"
              title={t('tree.moveToGroup')}
              onClick={(e) => {
                e.stopPropagation();
                onRequestMoveNode(node);
              }}
            >
              ↗
            </button>
          </span>
        )}
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
          if (!canEdit) return;
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

      <div className="ntree-body" ref={scrollRef}>
        {flat.length === 0 ? (
          // Empty flat list: a blank body while filtering with no matches, else loading / empty-state.
          filtering ? null : loading ? (
            <p className="muted ntree-empty">{t('tree.loadingNodes')}</p>
          ) : (
            <p
              className="muted ntree-empty"
              onContextMenu={(e) => {
                if (!canEdit) return;
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

      {menu && (
        <div className="ntree-menu" style={{ left: menu.x, top: menu.y }} onClick={(e) => e.stopPropagation()}>
          {menu.kind === 'group' ? (
            <>
              <button type="button" onClick={() => { onAddGroup(menu.group.id); setMenu(null); }}>
                {t('group.addSubgroup')}
              </button>
              {onAddNode && (
                <button type="button" onClick={() => { onAddNode(menu.group.id); setMenu(null); }}>
                  {t('tree.addNodeHere')}
                </button>
              )}
              <button type="button" onClick={() => { onEditGroup(menu.group); setMenu(null); }}>
                {t('tree.editMove')}
              </button>
              {suppressionMenu({ kind: 'group', id: menu.group.id, name: menu.group.name })}
              <div className="ntree-menu-sep" />
              <button type="button" className="danger" onClick={() => { onDeleteGroup(menu.group); setMenu(null); }}>
                {t('common:actions.delete')}
              </button>
            </>
          ) : menu.kind === 'node' ? (
            <>
              <button type="button" onClick={() => { onOpenNode(menu.node); setMenu(null); }}>
                {t('tree.open')}
              </button>
              <button type="button" onClick={() => { onRequestMoveNode(menu.node); setMenu(null); }}>
                {t('tree.moveToGroup')}
              </button>
              {onAddNode && (
                <button type="button" onClick={() => { onAddNode(menu.node.group_id); setMenu(null); }}>
                  {t('tree.addNodeEllipsis')}
                </button>
              )}
              {suppressionMenu({ kind: 'node', id: menu.node.id, name: menu.node.name })}
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
        </div>
      )}
    </div>
  );
}
