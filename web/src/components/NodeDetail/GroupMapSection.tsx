// SPDX-License-Identifier: AGPL-3.0-only
// The network map of the selected folder, drawn in its pane on the Nodes page under Health (ADR-191
// Inc.2). The same level the full map draws — inside a site, every node of the site with the
// subfolder it is filed in — at a fixed height, with three differences that come from living in a
// scrolling pane: the plain wheel scrolls the pane (Ctrl/⌘ + wheel zooms), one finger scrolls it
// on a touch screen, and pressing a box selects it in the tree instead of descending the map.
//
// Folding the section is remembered, and a folded map is not fetched at all — the level is the most
// expensive read the pane makes, so an operator who does not want it does not pay for it.

import { useCallback, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Link } from 'react-router-dom';
import { usePrefsStore } from '../../prefs';
import { topologyMapHref } from '../../lib/entityHref';
import { TopologyMap } from '../TopologyMap/TopologyMap';
import { MapEdgeMembers } from '../TopologyMap/MapEdgeMembers';
import { useMapTitles, useTopologyLevel } from '../TopologyMap/useTopologyLevel';
import { groupMapTarget } from '../TopologyMap/topologyLevel';
import type { PlacedNode } from '../TopologyMap/graphLayout';

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
  const { level, error, layout } = useTopologyLevel(groupId);
  const { boxTitle, edgeTitle, showChip } = useMapTitles(level);
  // A line's id names two ends of one level, so the selection is kept with the folder it was made
  // on and stops applying when another folder is opened.
  const [edgeSel, setEdgeSel] = useState<{ group: string; id: string } | null>(null);
  const edgeId = edgeSel?.group === groupId ? edgeSel.id : null;
  const edge = edgeId ? (level?.edges.find((e) => e.id === edgeId) ?? null) : null;

  const onActivate = useCallback(
    (box: PlacedNode) => {
      const target = groupMapTarget(box.id, level);
      if (target?.kind === 'node') onOpenNode?.(target.id);
      else if (target?.kind === 'group') onOpenGroup?.(target.id);
    },
    [level, onOpenNode, onOpenGroup],
  );
  const onSelectEdge = useCallback(
    (id: string) =>
      setEdgeSel((cur) => (cur?.id === id && cur.group === groupId ? null : { group: groupId, id })),
    [groupId],
  );

  if (error && !level) return <p className="nd-muted">{error}</p>;
  if (!level) return <p className="nd-muted">{t('groupDetail.map.loading')}</p>;
  if (level.overflow) return <p className="nd-muted">{t('groupDetail.map.overflow')}</p>;
  if (layout.nodes.length === 0) return <p className="nd-muted">{t('groupDetail.map.empty')}</p>;
  return (
    <>
      <div className="nd-grpmap">
        <TopologyMap
          layout={layout}
          selectedId={null}
          selectedEdge={edgeId}
          fitKey={groupId}
          boxTitle={boxTitle}
          edgeTitle={edgeTitle}
          showChip={showChip}
          onActivate={onActivate}
          onSelectEdge={onSelectEdge}
          viewKey="topoGroup"
          wheelNeedsModifier
          wheelHint={t('groupDetail.map.wheelHint')}
          touchPans={false}
        />
      </div>
      <p className="nd-grpmap-summary">
        {t('groupDetail.map.summary', {
          nodes: level.linked_node_count,
          edges: level.edge_count,
          stubs: level.stubs.length,
        })}
      </p>
      {edge && (
        <div className="nd-grpmap-edge">
          <MapEdgeMembers edge={edge} />
        </div>
      )}
    </>
  );
}
