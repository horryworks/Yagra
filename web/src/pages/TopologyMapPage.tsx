// SPDX-License-Identifier: AGPL-3.0-only
// Topology ▸ Network map, one folder level at a time (ADR-043, ADR-191). The derived connectivity
// graph — what is actually wired or routed to what — drawn for one folder: its own linked nodes one
// by one, each subfolder as a box carrying its counts, links between the same two things bundled
// into one line, and dashed stubs for links that leave the folder.
//
// Not in the menu: it is opened from a folder's or a node's context menu in the Nodes tree, from a
// node's detail page, and from a Geo map pin. The URL carries the level (`?group=`) and the
// selection (`?sel=`, the Nodes page's spelling), so every one of those is a plain link.
//
// Data: GET /api/v1/topology/map?group= on a slow reconcile cadence, kept live via the node-state
// SSE stream (`useNodeStates`, S14) so state updates without re-fetching the level.
//
// The layout is hand-written SVG (`components/TopologyMap/graphLayout.ts`); every judgement this
// page makes lives in `components/TopologyMap/topologyLevel.ts`, where a test reaches it.

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useConfigChanges } from '../lib/configChanges';
import { useTranslation } from 'react-i18next';
import { Link, useNavigate, useSearchParams } from 'react-router-dom';
import { PageHeader } from '../components/ui/PageHeader';
import { Card } from '../components/ui/Card';
import { TopologyMap } from '../components/TopologyMap/TopologyMap';
import { TopologyMapSidePanel } from '../components/TopologyMap/TopologyMapSidePanel';
import {
  layoutGraph,
  type GraphLayout,
  type PlacedEdge,
  type PlacedNode,
} from '../components/TopologyMap/graphLayout';
import {
  edgeShowsChip,
  folderHref,
  isLevelFor,
  levelNodesHref,
  levelToGraph,
  levelTrail,
  liveByGraphId,
  mapEscapeTarget,
  selectedGraphId,
  selectionFromParam,
  splitGraphId,
  stubHref,
  type LevelCaptions,
  type MapSelection,
} from '../components/TopologyMap/topologyLevel';
import { stateLabel } from '../lib/format';
import { escapeClearsSelection } from '../lib/escapeDismiss';
import { parseSelection, selectionToParam } from '../lib/treeSelection';
import { overlayLiveStates, type LiveOverlay } from '../lib/liveOverlay';
import { api } from '../services/api';
import { usePolled } from '../dashboard/usePolled';
import { useNodeStates, LIVE_RECONCILE_MS } from '../dashboard/useNodeStates';
import type { TreeSelection } from '../components/NodeTree/NodeTree';
import './TopologyMapPage.css';

const EMPTY_LAYOUT: GraphLayout = {
  nodes: [],
  edges: [],
  width: 0,
  height: 0,
  isolatedCount: 0,
  componentCount: 0,
};

export function TopologyMapPage() {
  const { t } = useTranslation('topology');
  const navigate = useNavigate();
  const [params, setParams] = useSearchParams();
  const group = params.get('group');
  const selParam = params.get('sel');
  const urlSel = useMemo(() => selectionFromParam(parseSelection(selParam)), [selParam]);
  // A line's id names two ends of one level, so the edge selection is kept with the level it was
  // made on and simply stops applying when the level changes.
  const [edgeSel, setEdgeSel] = useState<{ group: string | null; id: string } | null>(null);
  const edge = edgeSel && edgeSel.group === group ? edgeSel.id : null;

  // A node added, removed or moved between folders is on the map at once, not at the next tick.
  const configChanges = useConfigChanges();
  const { data, error } = usePolled(
    () => api.getTopologyMap(group),
    [group, configChanges],
    LIVE_RECONCILE_MS,
  );
  const level = isLevelFor(data, group) ? data : null;
  const live = useNodeStates();

  const captions: LevelCaptions = useMemo(
    () => ({
      folder: (f) => {
        const bad = f.counts.critical + f.counts.unreachable;
        return bad > 0
          ? t('map.folderSubBad', { count: f.node_count, bad })
          : t('map.folderSub', { count: f.node_count });
      },
      stub: () => t('map.externalSub'),
    }),
    [t],
  );

  // The layout depends on STRUCTURE ONLY — never on live state, which publishes a fresh Map on
  // every fleet-wide flush. The state each box is drawn with is overlaid below.
  const layout = useMemo(
    () => (level ? layoutGraph(levelToGraph(level, captions)) : EMPTY_LAYOUT),
    [level, captions],
  );
  const overlay = useRef<LiveOverlay<PlacedNode> | null>(null);
  const placedNodes = useMemo(() => {
    overlay.current = overlayLiveStates(layout.nodes, liveByGraphId(level, live), overlay.current);
    return overlay.current.out;
  }, [layout.nodes, level, live]);
  const viewLayout = useMemo(
    () => (placedNodes === layout.nodes ? layout : { ...layout, nodes: placedNodes }),
    [layout, placedNodes],
  );

  const setSelection = useCallback(
    (sel: TreeSelection) => {
      const next = new URLSearchParams(params);
      const value = selectionToParam(sel);
      if (value) next.set('sel', value);
      else next.delete('sel');
      setParams(next, { replace: true });
    },
    [params, setParams],
  );

  const onActivate = useCallback(
    (box: PlacedNode) => {
      const ref = splitGraphId(box.id);
      if (!ref || !level) return;
      setEdgeSel(null);
      if (ref.kind === 'folder') {
        navigate(folderHref(ref.id));
      } else if (ref.kind === 'external') {
        const stub = level.stubs.find((s) => s.id === ref.id);
        if (stub) navigate(stubHref(stub));
      } else {
        // A second press on the selected node lets it go (ADR-073).
        const same = urlSel?.kind === 'node' && urlSel.id === ref.id;
        setSelection(same ? null : { kind: 'node', id: ref.id });
      }
    },
    [level, navigate, setSelection, urlSel],
  );
  const onSelectEdge = useCallback(
    (id: string) => setEdgeSel((cur) => (cur?.id === id && cur.group === group ? null : { group, id })),
    [group],
  );
  const clearAll = useCallback(() => {
    setEdgeSel(null);
    setSelection(null);
  }, [setSelection]);

  // Escape unwinds the line first, then the URL's selection (`mapEscapeTarget`). One listener.
  useEffect(() => {
    if (!edge && !urlSel) return;
    const onKey = (e: KeyboardEvent) => {
      if (!escapeClearsSelection(e)) return;
      const target = mapEscapeTarget(!!edge, !!urlSel);
      if (target === 'edge') setEdgeSel(null);
      else if (target === 'selection') setSelection(null);
    };
    document.addEventListener('keydown', onKey);
    return () => document.removeEventListener('keydown', onKey);
  }, [edge, urlSel, setSelection]);

  const names = useMemo(() => new Map((level?.nodes ?? []).map((n) => [n.id, n.name])), [level]);
  const boxTitle = useCallback(
    (n: PlacedNode) => {
      if (n.kind === 'folder') {
        const ref = splitGraphId(n.id);
        const f = level?.folders.find((x) => x.id === ref?.id);
        return t('map.folderTitle', { name: n.name, count: f?.node_count ?? 0, state: stateLabel(n.state) });
      }
      if (n.kind === 'external') return t('map.externalTitle', { name: n.name });
      const cause = n.rootCause ? (names.get(n.rootCause) ?? null) : null;
      return cause
        ? t('map.nodeTitleSuppressed', { name: n.name, state: stateLabel(n.state), cause })
        : t('map.nodeTitle', { name: n.name, state: stateLabel(n.state) });
    },
    [level, names, t],
  );
  const edgeTitle = useCallback(
    (e: PlacedEdge) => t('map.edgeTitle', { count: e.count, source: t(`map.source.${e.source}`) }),
    [t],
  );
  const showChip = useCallback((e: PlacedEdge) => edgeShowsChip(e.id, e.count), []);

  const selection: MapSelection = edge ? { kind: 'edge', id: edge } : urlSel;
  const levelName = level?.group?.name ?? t('map.root');

  function body() {
    if (error && !level) return <Card><p className="muted">{error}</p></Card>;
    if (!level) return <Card><p className="muted">{t('map.loading')}</p></Card>;
    const nothing =
      level.folders.length === 0 && level.nodes.length === 0 && level.direct_node_count === 0;
    if (nothing && !level.group) {
      return <Card><p className="muted">{t('map.emptyInventory')}</p></Card>;
    }
    return (
      <>
        {level.overflow && (
          <Card>
            <p className="topomap-overflow-title">{t('map.overflow.title')}</p>
            <p className="muted">
              {t('map.overflow.body', { count: level.linked_node_count, max: level.node_limit })}
            </p>
            <Link to={levelNodesHref(level)}>{t('map.overflow.openNodes')}</Link>
          </Card>
        )}
        <div className="topomap-page-body">
          <div className="topomap-page-canvas">
            {layout.nodes.length === 0 && !level.overflow ? (
              <p className="muted topomap-page-empty">{t('map.empty.level')}</p>
            ) : (
              <TopologyMap
                layout={viewLayout}
                selectedId={selectedGraphId(urlSel)}
                selectedEdge={edge}
                fitKey={group ?? ''}
                boxTitle={boxTitle}
                edgeTitle={edgeTitle}
                showChip={showChip}
                onActivate={onActivate}
                onSelectEdge={onSelectEdge}
              />
            )}
          </div>
          <TopologyMapSidePanel
            level={level}
            selection={selection}
            levelName={levelName}
            onSelectEdge={onSelectEdge}
            onClear={clearAll}
          />
        </div>
      </>
    );
  }

  const trail = [
    { label: t('nav:sections.topology') },
    ...(level ? levelTrail(level, t('map.root')) : [{ label: t('map.root') }]),
  ];

  return (
    <div className="topomap-page">
      <PageHeader title={t('nav:topology.map')} trail={trail} note={t('map.note.level')} />
      {body()}
    </div>
  );
}
