// SPDX-License-Identifier: AGPL-3.0-only
// The network map of the selected folder, drawn in its pane on the Nodes page under Health (ADR-191
// Inc.2). The same level the full map draws — inside a site, every node of the site with the
// subfolder it is filed in — at a height the operator drags (ADR-191 Inc.12), with three differences that come from living in a
// scrolling pane: the plain wheel scrolls the pane (Ctrl/⌘ + wheel zooms), one finger scrolls it
// on a touch screen, and pressing a box selects it in the tree instead of descending the map.
//
// Folding the section is remembered, and a folded map is not fetched at all — the level is the most
// expensive read the pane makes, so an operator who does not want it does not pay for it.

import { useCallback, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Link } from 'react-router-dom';
import { usePrefsStore } from '../../prefs';
import { topologyMapHref } from '../../lib/entityHref';
import { TopologyMap } from '../TopologyMap/TopologyMap';
import { MapEdgeMembers } from '../TopologyMap/MapEdgeMembers';
import { useMapTitles, useTopologyLevel } from '../TopologyMap/useTopologyLevel';
import { groupMapTarget } from '../TopologyMap/topologyLevel';
import type { PlacedNode } from '../TopologyMap/graphLayout';
import {
  MIN_GROUP_MAP_PX,
  groupMapCeiling,
  groupMapHeight,
  groupMapHeightFromDrag,
  groupMapHeightFromKey,
} from './groupMapHeight';

interface Props {
  groupId: string;
  onOpenNode?: (nodeId: string) => void;
  onOpenGroup?: (groupId: string) => void;
}

export function GroupMapSection({ groupId, onOpenNode, onOpenGroup }: Props) {
  const { t } = useTranslation('nodes');
  const collapsed = usePrefsStore((s) => s.groupMapCollapsed);
  const toggle = usePrefsStore((s) => s.toggleGroupMap);
  const bodyId = `nd-grpmap-${groupId}`;
  return (
    <section>
      <div className="nd-section-head">
        <button
          type="button"
          className="nd-grpmap-toggle"
          aria-expanded={!collapsed}
          aria-controls={bodyId}
          onClick={toggle}
        >
          <span aria-hidden="true">{collapsed ? '▸' : '▾'}</span>
          <span className="nd-section-t">{t('groupDetail.map.title')}</span>
        </button>
        <Link className="nd-grpmap-open" to={topologyMapHref({ group: groupId })}>
          {t('groupDetail.map.open')}
        </Link>
      </div>
      {!collapsed && (
        <div id={bodyId}>
          <GroupMapBody groupId={groupId} onOpenNode={onOpenNode} onOpenGroup={onOpenGroup} />
        </div>
      )}
    </section>
  );
}

function GroupMapBody({ groupId, onOpenNode, onOpenGroup }: Props) {
  const { t } = useTranslation('nodes');
  // The map's height (ADR-191 Inc.12): the stored one, re-clamped for this window, while no drag is
  // in flight; the drag's own value while one is. The store is written once, when the drag ends.
  const stored = usePrefsStore((s) => s.groupMapHeight);
  const setStored = usePrefsStore((s) => s.setGroupMapHeight);
  const viewportH = typeof window === 'undefined' ? 0 : window.innerHeight;
  const committed = groupMapHeight(stored, viewportH);
  const [dragH, setDragH] = useState<number | null>(null);
  const height = dragH ?? committed;
  const resize = useRef<{ y: number; h: number } | null>(null);
  const onResizeDown = useCallback(
    (e: React.PointerEvent) => {
      (e.target as Element).setPointerCapture?.(e.pointerId);
      resize.current = { y: e.clientY, h: committed };
    },
    [committed],
  );
  const onResizeMove = useCallback((e: React.PointerEvent) => {
    const r = resize.current;
    if (r) setDragH(groupMapHeightFromDrag(r.h, r.y, e.clientY, window.innerHeight));
  }, []);
  const onResizeUp = useCallback(
    (e: React.PointerEvent) => {
      (e.target as Element).releasePointerCapture?.(e.pointerId);
      const r = resize.current;
      if (!r) return;
      resize.current = null;
      // The height is worked out from where the pointer was released, not from the last move's
      // state, which may not have rendered yet. A cancelled gesture carries no position worth
      // reading, so it keeps whatever the drag had reached.
      if (e.type === 'pointercancel') {
        if (dragH !== null) setStored(dragH);
      } else if (e.clientY !== r.y) {
        setStored(groupMapHeightFromDrag(r.h, r.y, e.clientY, window.innerHeight));
      }
      setDragH(null);
    },
    [dragH, setStored],
  );
  const onResizeKey = useCallback(
    (e: React.KeyboardEvent) => {
      const next = groupMapHeightFromKey(committed, e.key, window.innerHeight);
      if (next === null) return;
      e.preventDefault();
      setStored(next);
    },
    [committed, setStored],
  );
  const { level, error, layout, edge: edgeId, selectEdge } = useTopologyLevel(groupId);
  const { boxTitle, edgeTitle, showChip } = useMapTitles(level);
  const edge = edgeId ? (level?.edges.find((e) => e.id === edgeId) ?? null) : null;

  const onActivate = useCallback(
    (box: PlacedNode) => {
      const target = groupMapTarget(box.id, level);
      if (target?.kind === 'node') onOpenNode?.(target.id);
      else if (target?.kind === 'group') onOpenGroup?.(target.id);
    },
    [level, onOpenNode, onOpenGroup],
  );

  if (error && !level) return <p className="nd-muted">{error}</p>;
  if (!level) return <p className="nd-muted">{t('groupDetail.map.loading')}</p>;
  if (level.overflow) return <p className="nd-muted">{t('groupDetail.map.overflow')}</p>;
  if (layout.nodes.length === 0) return <p className="nd-muted">{t('groupDetail.map.empty')}</p>;
  return (
    <>
      <div className="nd-grpmap" style={{ ['--nd-grpmap-h' as string]: `${height}px` }}>
        <TopologyMap
          layout={layout}
          selectedId={null}
          selectedEdge={edgeId}
          // A new height fits the level to the new pane once the drag ends; mid-drag the view stays.
          fitKey={`${groupId}@${committed}`}
          boxTitle={boxTitle}
          edgeTitle={edgeTitle}
          showChip={showChip}
          onActivate={onActivate}
          onSelectEdge={selectEdge}
          viewKey="topoGroup"
          wheelNeedsModifier
          wheelHint={t('groupDetail.map.wheelHint')}
          touchPans={false}
        />
      </div>
      {/* Drag the bottom edge to make the map taller or shorter; the size is remembered. The same
          handle as the Geo map's (ADR-074): a slider, so it is announced and arrow-key operable. */}
      <div
        className="nd-grpmap-resize"
        role="slider"
        tabIndex={0}
        aria-label={t('groupDetail.map.resize')}
        aria-orientation="vertical"
        aria-valuemin={MIN_GROUP_MAP_PX}
        aria-valuemax={groupMapCeiling(viewportH)}
        aria-valuenow={height}
        onPointerDown={onResizeDown}
        onPointerMove={onResizeMove}
        onPointerUp={onResizeUp}
        onPointerCancel={onResizeUp}
        onKeyDown={onResizeKey}
        onDoubleClick={() => setStored(null)}
        title={t('groupDetail.map.resize')}
      >
        <span className="nd-grpmap-resize-grip" aria-hidden="true" />
      </div>
      <p className="nd-grpmap-summary">
        {[
          t('groupDetail.map.summaryNodes', { count: level.linked_node_count }),
          t('groupDetail.map.summaryEdges', { count: level.edge_count }),
          t('groupDetail.map.summaryStubs', { count: level.stubs.length }),
        ].join(' · ')}
      </p>
      {edge && (
        <div className="nd-grpmap-edge">
          <MapEdgeMembers edge={edge} />
        </div>
      )}
    </>
  );
}
