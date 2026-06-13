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

import { useEffect, useState } from 'react';
import type { NodeGroup, NodeSummary } from '../../types/api';
import { buildNodeTree, isSelfOrDescendant, type TreeGroup } from '../../lib/nodeTree';
import { StatusDot } from '../ui/StatusDot';
import { Button } from '../ui/Button';
import { GroupIcon } from './GroupIcon';
import './NodeTree.css';

/** Pixels of indent per tree depth. */
const INDENT = 16;
/** Left padding of a depth-0 row. */
const BASE_PAD = 6;

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
  | null;

interface Props {
  groups: NodeGroup[];
  nodes: NodeSummary[];
  canEdit: boolean;
  /** First inventory load in flight — show a loading placeholder, not the empty message. */
  loading?: boolean;
  onOpenNode: (node: NodeSummary) => void;
  onAddGroup: (parentId: string | null) => void;
  onEditGroup: (group: NodeGroup) => void;
  onDeleteGroup: (group: NodeGroup) => void;
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
}

export function NodeTree({
  groups,
  nodes,
  canEdit,
  loading,
  onOpenNode,
  onAddGroup,
  onEditGroup,
  onDeleteGroup,
  onRequestMoveNode,
  onMoveNode,
  onMoveGroup,
  onReorderNode,
  onReorderGroup,
}: Props) {
  const tree = buildNodeTree(groups, nodes);
  const [expanded, setExpanded] = useState<Set<string>>(() => new Set(tree.roots.map((g) => g.id)));
  const [drag, setDrag] = useState<DragItem | null>(null);
  const [dropTarget, setDropTarget] = useState<DropTarget>(null);
  const [menu, setMenu] = useState<Menu>(null);

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

  const toggle = (id: string) =>
    setExpanded((cur) => {
      const next = new Set(cur);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });

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

  const renderGroup = (group: TreeGroup, depth: number): React.ReactNode => {
    const isOpen = expanded.has(group.id);
    const count = group.children.length + group.nodes.length;
    const target: Target = { kind: 'group', id: group.id, scope: group.parent_id };
    return (
      <div className="ntree-group" key={group.id}>
        <div
          className={`ntree-row ntree-grow${dropClass(group.id)}${drag?.id === group.id ? ' dragging' : ''}`}
          style={{ paddingLeft: depth * INDENT + BASE_PAD }}
          draggable={canEdit}
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
            onClick={() => toggle(group.id)}
            aria-label={isOpen ? 'Collapse' : 'Expand'}
            disabled={count === 0}
          >
            ▶
          </button>
          <span className="ntree-icon">
            <GroupIcon type={group.group_type} />
          </span>
          <button type="button" className="ntree-grp-name" onClick={() => toggle(group.id)}>
            {group.name}
          </button>
          <span className="ntree-count">{count}</span>
          {canEdit && (
            <span className="ntree-actions">
              <button
                type="button"
                className="ntree-act"
                title="Add subgroup"
                onClick={() => onAddGroup(group.id)}
              >
                ＋
              </button>
              <button
                type="button"
                className="ntree-act"
                title="Edit / move group"
                onClick={() => onEditGroup(group)}
              >
                ✎
              </button>
              <button
                type="button"
                className="ntree-act"
                title="Delete group"
                onClick={() => onDeleteGroup(group)}
              >
                🗑
              </button>
            </span>
          )}
        </div>
        {isOpen && (
          <div className="ntree-children">
            {group.children.map((c) => renderGroup(c, depth + 1))}
            {group.nodes.map((n) => renderNode(n, depth + 1))}
          </div>
        )}
      </div>
    );
  };

  const renderNode = (node: NodeSummary, depth: number): React.ReactNode => {
    const target: Target = { kind: 'node', id: node.id, scope: node.group_id };
    return (
      <div
        className={`ntree-row ntree-node${dropClass(node.id)}${drag?.id === node.id ? ' dragging' : ''}`}
        key={node.id}
        style={{ paddingLeft: depth * INDENT + BASE_PAD }}
        draggable={canEdit}
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
        <button type="button" className="ntree-node-name" onClick={() => onOpenNode(node)}>
          {node.name}
        </button>
        <span className="ntree-node-addr mono">{node.address}</span>
        {(node.vendor || node.model) && (
          <span className="ntree-node-meta">{[node.vendor, node.model].filter(Boolean).join(' · ')}</span>
        )}
        {canEdit && (
          <span className="ntree-actions">
            <button
              type="button"
              className="ntree-act"
              title="Move to group…"
              onClick={() => onRequestMoveNode(node)}
            >
              ↗
            </button>
          </span>
        )}
      </div>
    );
  };

  const rootDropActive = dropTarget?.id === 'root' && !!drag;

  return (
    <div className="ntree">
      {canEdit && (
        <div className="ntree-toolbar">
          <Button variant="outline" onClick={() => onAddGroup(null)}>
            ＋ Add group
          </Button>
          <span className="muted ntree-hint">
            Drag onto a group to nest/assign; drag between rows to reorder.
          </span>
        </div>
      )}

      <div className="ntree-body">
        {tree.roots.map((g) => renderGroup(g, 0))}

        {/* Ungrouped nodes + the root drop zone. */}
        <div
          className={`ntree-ungrouped${rootDropActive ? ' drop-inside' : ''}`}
          onDragOver={(e) => {
            if (!drag) return;
            e.preventDefault();
            setDropTarget({ id: 'root', position: 'inside', ok: true });
          }}
          onDrop={(e) => {
            e.preventDefault();
            dropOnRoot();
          }}
        >
          <div className="ntree-row ntree-ungrouped-head" style={{ paddingLeft: BASE_PAD }}>
            <span className="ntree-twisty ntree-twisty-spacer" aria-hidden="true" />
            <span className="ntree-icon ntree-ungrouped-icon">⌁</span>
            <span className="ntree-grp-name ntree-ungrouped-label">Ungrouped</span>
            <span className="ntree-count">{tree.ungrouped.length}</span>
          </div>
          {tree.ungrouped.map((n) => renderNode(n, 1))}
          {tree.roots.length === 0 &&
            tree.ungrouped.length === 0 &&
            (loading ? (
              <p className="muted ntree-empty">Loading nodes…</p>
            ) : (
              <p className="muted ntree-empty">No nodes in inventory. Add one to start monitoring.</p>
            ))}
        </div>
      </div>

      {menu && (
        <div className="ntree-menu" style={{ left: menu.x, top: menu.y }} onClick={(e) => e.stopPropagation()}>
          {menu.kind === 'group' ? (
            <>
              <button type="button" onClick={() => { onAddGroup(menu.group.id); setMenu(null); }}>
                Add subgroup
              </button>
              <button type="button" onClick={() => { onEditGroup(menu.group); setMenu(null); }}>
                Edit / move…
              </button>
              <div className="ntree-menu-sep" />
              <button type="button" className="danger" onClick={() => { onDeleteGroup(menu.group); setMenu(null); }}>
                Delete
              </button>
            </>
          ) : (
            <>
              <button type="button" onClick={() => { onOpenNode(menu.node); setMenu(null); }}>
                Open
              </button>
              <button type="button" onClick={() => { onRequestMoveNode(menu.node); setMenu(null); }}>
                Move to group…
              </button>
            </>
          )}
        </div>
      )}
    </div>
  );
}
