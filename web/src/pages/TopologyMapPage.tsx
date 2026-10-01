// SPDX-License-Identifier: AGPL-3.0-only
// Topology ▸ Network map, one folder level at a time (ADR-043, ADR-191). The derived connectivity
// graph — what is actually wired or routed to what — drawn for one folder: its own linked nodes one
// by one, each subfolder as a box carrying its counts, links between the same two things bundled
// into one line, and dashed stubs for links that leave the folder.
// Inside a site the server draws the level flat instead: every node of the site, each tagged with
// the subfolder it is filed in (ADR-191 Inc.2). The same level is drawn in a folder's pane on the
// Nodes page; both read it through `useTopologyLevel`.
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

import { useCallback, useEffect, useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Link, useNavigate, useSearchParams } from 'react-router-dom';
import { PageHeader } from '../components/ui/PageHeader';
import { Card } from '../components/ui/Card';
import { TopologyMap } from '../components/TopologyMap/TopologyMap';
import { TopologyMapSidePanel } from '../components/TopologyMap/TopologyMapSidePanel';
import { isBundleId, type PlacedNode } from '../components/TopologyMap/graphLayout';
import { useMapTitles, useTopologyLevel } from '../components/TopologyMap/useTopologyLevel';
import {
  folderHref,
  levelNodesHref,
  levelTrail,
  mapEscapeTarget,
  selectedGraphId,
  selectionFromParam,
  splitGraphId,
  stubHref,
  withSearch,
  type MapSelection,
} from '../components/TopologyMap/topologyLevel';
import { escapeClearsSelection } from '../lib/escapeDismiss';
import { decodeCondition, encodeCondition, type TextCondition } from '../lib/filterCondition';
import { isImeComposing } from '../lib/ime';
import { TextConditionEditor } from '../components/ui/TextConditionEditor';
import { searchMap, stepThrough } from '../components/TopologyMap/mapSearch';
import { parseSelection, selectionToParam } from '../lib/treeSelection';
import type { TreeSelection } from '../components/NodeTree/NodeTree';
import './TopologyMapPage.css';

export function TopologyMapPage() {
  const { t } = useTranslation('topology');
  const navigate = useNavigate();
  const [params, setParams] = useSearchParams();
  const group = params.get('group');
  const selParam = params.get('sel');
  const urlSel = useMemo(() => selectionFromParam(parseSelection(selParam)), [selParam]);
  // The search (ADR-191 Inc.11): the column filter's condition, held in `?q=` like a list's filter.
  const q = params.get('q') ?? '';
  const cond = useMemo(() => decodeCondition(q), [q]);
  const {
    level,
    error,
    layout: viewLayout,
    edge,
    selectEdge,
    clearEdge: clearLine,
  } = useTopologyLevel(group);

  // A bundle of access points (ADR-191 Inc.9) is selected in the page only, like a line: it names
  // something that exists on this level and nowhere else.
  const [bundleSel, setBundleSel] = useState<{ group: string | null; id: string } | null>(null);
  const bundle = bundleSel && bundleSel.group === group ? bundleSel.id : null;
  const clearEdge = useCallback(() => {
    clearLine();
    setBundleSel(null);
  }, [clearLine]);
  const onSelectEdge = useCallback(
    (id: string) => {
      setBundleSel(null);
      selectEdge(id);
    },
    [selectEdge],
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
      if (isBundleId(box.id)) {
        // A second press lets it go, as on a node (ADR-073).
        clearLine();
        setBundleSel((cur) => (cur?.id === box.id && cur.group === group ? null : { group, id: box.id }));
        return;
      }
      const ref = splitGraphId(box.id);
      if (!ref || !level) return;
      clearEdge();
      if (ref.kind === 'folder') {
        navigate(withSearch(folderHref(ref.id), q));
      } else if (ref.kind === 'external') {
        const stub = level.stubs.find((s) => s.id === ref.id);
        if (stub) navigate(withSearch(stubHref(stub), q));
      } else {
        // A second press on the selected node lets it go (ADR-073).
        const same = urlSel?.kind === 'node' && urlSel.id === ref.id;
        setSelection(same ? null : { kind: 'node', id: ref.id });
      }
    },
    [level, navigate, setSelection, urlSel, clearEdge, clearLine, group, q],
  );
  const clearAll = useCallback(() => {
    clearEdge();
    setSelection(null);
  }, [setSelection, clearEdge]);

  // Escape unwinds the line first, then the URL's selection (`mapEscapeTarget`). One listener.
  useEffect(() => {
    if (!edge && !bundle && !urlSel) return;
    const onKey = (e: KeyboardEvent) => {
      if (!escapeClearsSelection(e)) return;
      const target = mapEscapeTarget(!!edge || !!bundle, !!urlSel);
      if (target === 'edge') clearEdge();
      else if (target === 'selection') setSelection(null);
    };
    document.addEventListener('keydown', onKey);
    return () => document.removeEventListener('keydown', onKey);
  }, [edge, bundle, urlSel, setSelection, clearEdge]);

  const { boxTitle, edgeTitle, showChip } = useMapTitles(level);

  const setCond = useCallback(
    (next: TextCondition) => {
      const p = new URLSearchParams(params);
      const value = encodeCondition(next);
      if (value) p.set('q', value);
      else p.delete('q');
      setParams(p, { replace: true });
    },
    [params, setParams],
  );
  const search = useMemo(() => searchMap(level, viewLayout, cond), [level, viewLayout, cond]);
  // Enter steps through the hits, bringing each to the middle; Shift+Enter steps back.
  const [focus, setFocus] = useState<{ id: string; seq: number } | null>(null);
  // The hits are worked out from the box's text at the moment Enter is pressed, not from the last
  // render: the Enter that commits a freshly typed term arrives before the URL has the term, and
  // stepping through the previous search's hits would centre the wrong box.
  const step = useCallback(
    (dir: 1 | -1, term: string) => {
      const order = searchMap(level, viewLayout, { ...cond, term }).order;
      setFocus((cur) => {
        const id = stepThrough(order, cur?.id ?? null, dir);
        return id ? { id, seq: (cur?.seq ?? 0) + 1 } : cur;
      });
    },
    [level, viewLayout, cond],
  );

  const selection: MapSelection = edge
    ? { kind: 'edge', id: edge }
    : bundle
      ? { kind: 'bundle', id: bundle }
      : urlSel;
  const bundleNode = bundle ? (viewLayout.nodes.find((n) => n.id === bundle) ?? null) : null;
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
            {viewLayout.nodes.length === 0 && !level.overflow ? (
              <p className="muted topomap-page-empty">{t('map.empty.level')}</p>
            ) : (
              <TopologyMap
                layout={viewLayout}
                selectedId={bundle ?? selectedGraphId(urlSel)}
                selectedEdge={edge}
                fitKey={group ?? ''}
                boxTitle={boxTitle}
                edgeTitle={edgeTitle}
                showChip={showChip}
                onActivate={onActivate}
                onSelectEdge={onSelectEdge}
                search={search}
                focus={focus}
              />
            )}
          </div>
          <TopologyMapSidePanel
            level={level}
            selection={selection}
            bundle={bundleNode}
            cond={search.matched ? cond : null}
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
    ...(level
      ? levelTrail(level, t('map.root')).map((c) => (c.to ? { ...c, to: withSearch(c.to, q) } : c))
      : [{ label: t('map.root') }]),
  ];

  const searchBox = (
    <div
      className="topomap-search"
      title={t('map.search.stepHint')}
      onKeyDown={(e) => {
        // The editor commits on Enter itself; here Enter also moves to the next hit.
        if (e.key !== 'Enter' || isImeComposing(e)) return;
        const box = e.target as HTMLInputElement;
        if (box.tagName !== 'INPUT') return;
        step(e.shiftKey ? -1 : 1, box.value);
      }}
    >
      <TextConditionEditor
        value={cond}
        onChange={setCond}
        modes={['contains', 'regex']}
        allowNot
        placeholder={t('map.search.placeholder')}
      />
      {search.matched && (
        <p className="topomap-search-count" aria-live="polite">
          {t('map.search.count', { count: search.total })}
          {search.undrawn > 0 && ' ' + t('map.search.undrawn', { count: search.undrawn })}
        </p>
      )}
    </div>
  );

  return (
    <div className="topomap-page">
      <PageHeader
        title={t('nav:topology.map')}
        trail={trail}
        note={t('map.note.level')}
        actions={level ? searchBox : undefined}
      />
      {body()}
    </div>
  );
}
