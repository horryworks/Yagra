// The inventory tree (All nodes). Hierarchical groups (folders) with their member nodes, modelled
// on HoTTY's HostTree: expand/collapse, per-row hover actions, a right-click context menu, and
// HTML5 drag-and-drop — drop a node onto a group to assign it, drop a group onto a group to
// re-parent it, or drop either onto "Ungrouped" to move it to the root. Group moves that would
// nest a group inside its own subtree are refused (cycle guard). This component is presentation +
// interaction only; the page owns the data and turns the callbacks into API calls + a reload.

import { useEffect, useState } from 'react';
import type { NodeGroup, NodeSummary } from '../../types/api';
import { buildNodeTree, isSelfOrDescendant, type TreeGroup } from '../../lib/nodeTree';
import { StatusDot } from '../ui/StatusDot';
import { Button } from '../ui/Button';
import { GroupIcon } from './GroupIcon';
import './NodeTree.css';

type DragItem = { kind: 'node' | 'group'; id: string };
type Menu =
  | { x: number; y: number; kind: 'group'; group: TreeGroup }
  | { x: number; y: number; kind: 'node'; node: NodeSummary }
  | null;

interface Props {
  groups: NodeGroup[];
  nodes: NodeSummary[];
  canEdit: boolean;
  onOpenNode: (node: NodeSummary) => void;
  onAddGroup: (parentId: string | null) => void;
  onEditGroup: (group: NodeGroup) => void;
  onDeleteGroup: (group: NodeGroup) => void;
  /** Open the "move node" picker (context-menu / button path, keyboard-accessible). */
  onRequestMoveNode: (node: NodeSummary) => void;
  /** Direct move (drag-drop): assign a node to a group (or null = ungroup). */
  onMoveNode: (nodeId: string, groupId: string | null) => void;
  /** Direct move (drag-drop): re-parent a group (or null = top level). */
  onMoveGroup: (groupId: string, parentId: string | null) => void;
}

export function NodeTree({
  groups,
  nodes,
  canEdit,
  onOpenNode,
  onAddGroup,
  onEditGroup,
  onDeleteGroup,
  onRequestMoveNode,
  onMoveNode,
  onMoveGroup,
}: Props) {
  const tree = buildNodeTree(groups, nodes);
  const [expanded, setExpanded] = useState<Set<string>>(() => new Set(tree.roots.map((g) => g.id)));
  const [drag, setDrag] = useState<DragItem | null>(null);
  const [dropTarget, setDropTarget] = useState<string | 'root' | null>(null);
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

  /** Whether `drag` may be dropped on group `targetId` (a group can't go into its own subtree). */
  const canDropOnGroup = (targetId: string) => {
    if (!drag) return false;
    if (drag.kind === 'node') return true;
    return !isSelfOrDescendant(groups, drag.id, targetId);
  };

  const dropOnGroup = (targetId: string) => {
    if (!drag) return;
    if (drag.kind === 'node') onMoveNode(drag.id, targetId);
    else if (canDropOnGroup(targetId)) onMoveGroup(drag.id, targetId);
    setDrag(null);
    setDropTarget(null);
  };

  const dropOnRoot = () => {
    if (!drag) return;
    if (drag.kind === 'node') onMoveNode(drag.id, null);
    else onMoveGroup(drag.id, null);
    setDrag(null);
    setDropTarget(null);
  };

  const renderGroup = (group: TreeGroup, depth: number): React.ReactNode => {
    const isOpen = expanded.has(group.id);
    const count = group.children.length + group.nodes.length;
    const isDropOk = dropTarget === group.id && canDropOnGroup(group.id);
    const isDropBad = dropTarget === group.id && !canDropOnGroup(group.id);
    return (
      <div className="ntree-group" key={group.id}>
        <div
          className={`ntree-row ntree-grow${isDropOk ? ' drop-ok' : ''}${isDropBad ? ' drop-bad' : ''}`}
          style={{ paddingLeft: depth * 16 + 6 }}
          draggable={canEdit}
          onDragStart={(e) => {
            e.stopPropagation();
            setDrag({ kind: 'group', id: group.id });
          }}
          onDragEnd={() => {
            setDrag(null);
            setDropTarget(null);
          }}
          onDragOver={(e) => {
            if (!drag) return;
            e.preventDefault();
            e.stopPropagation();
            setDropTarget(group.id);
          }}
          onDrop={(e) => {
            e.preventDefault();
            e.stopPropagation();
            dropOnGroup(group.id);
          }}
          onContextMenu={(e) => {
            if (!canEdit) return;
            e.preventDefault();
            setMenu({ x: e.clientX, y: e.clientY, kind: 'group', group });
          }}
        >
          <button
            type="button"
            className={`ntree-chevron${isOpen ? ' open' : ''}`}
            onClick={() => toggle(group.id)}
            aria-label={isOpen ? 'Collapse' : 'Expand'}
            disabled={count === 0}
          >
            ▶
          </button>
          <span className="ntree-grp-icon">
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

  const renderNode = (node: NodeSummary, depth: number): React.ReactNode => (
    <div
      className="ntree-row ntree-node"
      key={node.id}
      style={{ paddingLeft: depth * 16 + 6 }}
      draggable={canEdit}
      onDragStart={(e) => {
        e.stopPropagation();
        setDrag({ kind: 'node', id: node.id });
      }}
      onDragEnd={() => {
        setDrag(null);
        setDropTarget(null);
      }}
      onContextMenu={(e) => {
        if (!canEdit) return;
        e.preventDefault();
        setMenu({ x: e.clientX, y: e.clientY, kind: 'node', node });
      }}
    >
      <span className="ntree-node-dot">
        <StatusDot state={node.state} />
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

  const rootDropActive = dropTarget === 'root' && !!drag;

  return (
    <div className="ntree">
      {canEdit && (
        <div className="ntree-toolbar">
          <Button variant="outline" onClick={() => onAddGroup(null)}>
            ＋ Add group
          </Button>
          <span className="muted ntree-hint">
            Drag a node onto a group to move it; drag a group onto another to nest it.
          </span>
        </div>
      )}

      <div className="ntree-body">
        {tree.roots.map((g) => renderGroup(g, 0))}

        {/* Ungrouped nodes + the root drop zone. */}
        <div
          className={`ntree-ungrouped${rootDropActive ? ' drop-ok' : ''}`}
          onDragOver={(e) => {
            if (!drag) return;
            e.preventDefault();
            setDropTarget('root');
          }}
          onDrop={(e) => {
            e.preventDefault();
            dropOnRoot();
          }}
        >
          <div className="ntree-row ntree-ungrouped-head" style={{ paddingLeft: 6 }}>
            <span className="ntree-grp-icon ntree-ungrouped-icon">⌁</span>
            <span className="ntree-grp-name ntree-ungrouped-label">Ungrouped</span>
            <span className="ntree-count">{tree.ungrouped.length}</span>
          </div>
          {tree.ungrouped.map((n) => renderNode(n, 1))}
          {tree.roots.length === 0 && tree.ungrouped.length === 0 && (
            <p className="muted ntree-empty">No nodes in inventory. Add one to start monitoring.</p>
          )}
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
