// SPDX-License-Identifier: AGPL-3.0-only
// The network map of the selected folder, drawn in its pane on the Nodes page under Health (ADR-191
// Inc.2). The same level the full map draws — inside a site, every node of the site with the
// subfolder it is filed in — at a height the operator drags (ADR-191 Inc.12), with three differences that come from living in a
// scrolling pane: the plain wheel scrolls the pane (Ctrl/⌘ + wheel zooms), one finger scrolls it
// on a touch screen, and pressing a box selects it in the tree instead of descending the map.
// Pressing a bundle of access points lists them under the map, as the full map's panel does
// (ADR-191 Inc.14): a bundle has no row in the tree, and selecting its parent instead was a press
// that took the operator somewhere they had not asked to go.
//
// Folding the section is remembered, and a folded map is not fetched at all — the level is the most
// expensive read the pane makes, so an operator who does not want it does not pay for it.
//
// An open map carries the full map's search in its heading row (ADR-191 Inc.13), held in `?mq=`
// because `/nodes` already spends `q` on the tree. Moving to another folder keeps it, and "open
// larger" hands it to the full map as its `q`. Where the box is not offered — the map folded, or a
// level that is too big, has no lines or failed to load — the same row shows the search still in
// force as a chip that clears it, because "open larger" still hands it on.

import { useCallback, useMemo, useRef, useState } from 'react';
import { createPortal } from 'react-dom';
import { useTranslation } from 'react-i18next';
import { Link, useSearchParams } from 'react-router-dom';
import { usePrefsStore } from '../../prefs';
import { topologyMapHref } from '../../lib/entityHref';
import { writeIdParam } from '../../lib/filterParams';
import { conditionIsActive, decodeCondition } from '../../lib/filterCondition';
import { TopologyMap } from '../TopologyMap/TopologyMap';
import { MapEdgeMembers } from '../TopologyMap/MapEdgeMembers';
import { MapBundleMembers } from '../TopologyMap/MapBundleMembers';
import { useMapTitles, useTopologyLevel } from '../TopologyMap/useTopologyLevel';
import { GROUP_MAP_SEARCH_KEY, groupMapTarget, withSearch } from '../TopologyMap/topologyLevel';
import { isBundleId, type PlacedNode } from '../TopologyMap/graphLayout';
import { MapSearchBox } from '../TopologyMap/MapSearchBox';
import { useMapSearch, type MapSearchState } from '../TopologyMap/useMapSearch';
import type { TopologyLevelView } from '../TopologyMap/useTopologyLevel';
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
  const [params, setParams] = useSearchParams();
  const mq = params.get(GROUP_MAP_SEARCH_KEY) ?? '';
  // Built from this render's query string, like every other key on `/nodes`: two writes landing in
  // one tick keep only the later one (the same limit `useUrlTerm` documents for the tree's `q`).
  const writeMq = useCallback(
    (value: string) => {
      const next = new URLSearchParams(params);
      writeIdParam(next, GROUP_MAP_SEARCH_KEY, value || null);
      setParams(next, { replace: true });
    },
    [params, setParams],
  );
  // The open map's search box is drawn into this slot in the heading row. The heading itself is the
  // same element folded or open, so the toggle the operator just pressed keeps keyboard focus.
  const [findSlot, setFindSlot] = useState<HTMLDivElement | null>(null);
  const bodyId = `nd-grpmap-${groupId}`;
  return (
    <section>
      <div className="nd-section-head nd-grpmap-head">
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
        <div className="nd-grpmap-find" ref={setFindSlot}>
          {collapsed && <ActiveSearchChip mq={mq} writeMq={writeMq} />}
        </div>
        <Link className="nd-grpmap-open" to={withSearch(topologyMapHref({ group: groupId }), mq)}>
          {t('groupDetail.map.open')}
        </Link>
      </div>
      {!collapsed && (
        <div id={bodyId}>
          <GroupMapOpen
            groupId={groupId}
            mq={mq}
            writeMq={writeMq}
            findSlot={findSlot}
            onOpenNode={onOpenNode}
            onOpenGroup={onOpenGroup}
          />
        </div>
      )}
    </section>
  );
}

/** The search in force while its box is not offered (the map folded, too big, without lines, or
 *  failed to load):
 *  it still fades boxes on the full map that "open larger" hands it to, so it stays visible and
 *  clearable here. */
function ActiveSearchChip({ mq, writeMq }: { mq: string; writeMq: (value: string) => void }) {
  const { t } = useTranslation('nodes');
  const cond = useMemo(() => decodeCondition(mq), [mq]);
  if (!conditionIsActive(cond)) return null;
  const key =
    cond.mode === 'regex'
      ? cond.not
        ? 'groupDetail.map.searchActiveRegexNot'
        : 'groupDetail.map.searchActiveRegex'
      : cond.not
        ? 'groupDetail.map.searchActiveNot'
        : 'groupDetail.map.searchActive';
  const label = t(key, { term: cond.term });
  return (
    <span className="nd-grpmap-chip" title={label}>
      <span className="nd-grpmap-chip-t">{label}</span>
      <button
        type="button"
        className="nd-grpmap-chip-x"
        aria-label={t('groupDetail.map.searchClear')}
        title={t('groupDetail.map.searchClear')}
        onClick={() => writeMq('')}
      >
        ✕
      </button>
    </span>
  );
}

function GroupMapOpen({
  groupId,
  mq,
  writeMq,
  findSlot,
  onOpenNode,
  onOpenGroup,
}: Props & { mq: string; writeMq: (value: string) => void; findSlot: HTMLDivElement | null }) {
  const view = useTopologyLevel(groupId);
  const search = useMapSearch(view.level, view.layout, mq, writeMq, groupId);
  // The box is not offered over a level that says "too big", "no lines yet" or failed to load: it
  // would count hits nobody can see. It stays while the next folder's level loads, so a term being
  // typed and the caret survive moving between folders. A term still inside the editor's debounce
  // when the operator clicks elsewhere is not lost: the editor commits it when the box loses focus.
  const undrawn = view.level
    ? view.level.overflow || view.layout.nodes.length === 0
    : !!view.error;
  return (
    <>
      {findSlot &&
        createPortal(
          undrawn ? (
            <ActiveSearchChip mq={mq} writeMq={writeMq} />
          ) : (
            <MapSearchBox state={search} inline />
          ),
          findSlot,
        )}
      <GroupMapBody
        groupId={groupId}
        view={view}
        search={search}
        onOpenNode={onOpenNode}
        onOpenGroup={onOpenGroup}
      />
    </>
  );
}

function GroupMapBody({
  groupId,
  view,
  search,
  onOpenNode,
  onOpenGroup,
}: Props & { view: TopologyLevelView; search: MapSearchState }) {
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
  const { level, error, layout, edge: edgeId, selectEdge, clearEdge } = view;
  const { boxTitle, edgeTitle, showChip } = useMapTitles(level);
  const edge = edgeId ? (level?.edges.find((e) => e.id === edgeId) ?? null) : null;
  // A bundle is selected in the pane only, like a line, and only on the folder it was pressed on.
  // A line and a bundle exclude each other: the list under the map shows one of them.
  const [bundleSel, setBundleSel] = useState<{ group: string; id: string } | null>(null);
  const bundleId = bundleSel && bundleSel.group === groupId ? bundleSel.id : null;
  const bundle = bundleId ? (layout.nodes.find((n) => n.id === bundleId) ?? null) : null;
  const onSelectEdge = useCallback(
    (id: string) => {
      setBundleSel(null);
      selectEdge(id);
    },
    [selectEdge],
  );

  const onActivate = useCallback(
    (box: PlacedNode) => {
      if (isBundleId(box.id)) {
        // A second press lets it go (ADR-073).
        clearEdge();
        setBundleSel((cur) => (cur?.id === box.id && cur.group === groupId ? null : { group: groupId, id: box.id }));
        return;
      }
      const target = groupMapTarget(box.id, level);
      if (target?.kind === 'node') onOpenNode?.(target.id);
      else if (target?.kind === 'group') onOpenGroup?.(target.id);
    },
    [level, groupId, clearEdge, onOpenNode, onOpenGroup],
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
          selectedId={bundle?.id ?? null}
          selectedEdge={edgeId}
          // A new height fits the level to the new pane once the drag ends; mid-drag the view stays.
          fitKey={`${groupId}@${committed}`}
          boxTitle={boxTitle}
          edgeTitle={edgeTitle}
          showChip={showChip}
          onActivate={onActivate}
          onSelectEdge={onSelectEdge}
          viewKey="topoGroup"
          wheelNeedsModifier
          wheelHint={t('groupDetail.map.wheelHint')}
          touchPans={false}
          search={search.search}
          focus={search.focus}
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
      {!edge && bundle && (
        <div className="nd-grpmap-edge">
          <h3 className="topomap-panel-sub">{t('topology:map.panel.bundle.title', { name: bundle.name })}</h3>
          <MapBundleMembers level={level} bundle={bundle} cond={search.search.matched ? search.cond : null} />
        </div>
      )}
    </>
  );
}
