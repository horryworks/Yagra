// SPDX-License-Identifier: AGPL-3.0-only
// Topology ▸ Network map. The derived connectivity graph (ADR-043): what is actually wired or
// routed to what, drawn from CDP/LLDP adjacency and shared-subnet membership rather than from
// hand-entered parent links. Nodes are coloured by live status; a node suppressed under an upstream
// root cause is drawn muted.
//
// Data: GET /api/v1/topology/links for the edges and GET /api/v1/topology for the node identities
// and their states, both on a slow reconcile cadence and kept live via the node-state SSE stream
// (`useNodeStates`, S14) so status updates without re-fetching the graph every 15s (S7). Nodes with
// no link are summarised, not drawn, so a flat inventory doesn't paint thousands of loose boxes.
//
// The layout is hand-written SVG (`components/TopologyMap/graphLayout.ts`) — the graph is undirected
// and keeps redundant links, so the tidy-tree that served the dependency map could not draw it.

import { useMemo, useRef } from 'react';
import { useTranslation } from 'react-i18next';
import { PageHeader } from '../components/ui/PageHeader';
import { Card } from '../components/ui/Card';
import { TopologyMap } from '../components/TopologyMap/TopologyMap';
import {
  layoutGraph,
  MAX_GRAPH_LINKS,
  MAX_GRAPH_NODES,
  type PlacedNode,
} from '../components/TopologyMap/graphLayout';
import { stateColorVar, stateLabel } from '../lib/format';
import { SEVERITY_ORDER } from '../lib/nodeState';
import { overlayLiveStates, type LiveOverlay } from '../lib/liveOverlay';
import { api } from '../services/api';
import { usePolled } from '../dashboard/usePolled';
import { useNodeStates, LIVE_RECONCILE_MS } from '../dashboard/useNodeStates';
import { LINK_SOURCES } from '../types/api';
import './TopologyMapPage.css';

export function TopologyMapPage() {
  const { t } = useTranslation('topology');
  const { data, loading, error } = usePolled(
    async () => {
      const [topology, links] = await Promise.all([
        api.getTopology(),
        api.getTopologyLinks(),
      ]);
      return { nodes: topology.nodes, ...links };
    },
    [],
    LIVE_RECONCILE_MS,
  );
  const live = useNodeStates();

  const nodes = useMemo(() => data?.nodes ?? [], [data]);
  const links = useMemo(() => data?.links ?? [], [data]);

  // The layout depends on STRUCTURE ONLY — never on live state. `layoutGraph` is a BFS plus four
  // barycentre sweeps over up to MAX_GRAPH_NODES/MAX_GRAPH_LINKS, and `live` publishes a fresh Map
  // whenever any node ANYWHERE in the fleet changes state (~10×/s during a post-restart
  // first-observe burst). Keying this on `data` is what stops the map re-laying-out under an
  // operator who is watching it during an alert storm. The state each box is drawn with is overlaid
  // below onto the already-placed nodes — `state` is the only field the overlay touches and
  // `layoutGraph` never reads it for placement, so the drawn result is identical either way.
  const layout = useMemo(() => layoutGraph({ nodes, links }), [nodes, links]);

  // Overlay the live SSE state on top of the placed graph (S14): a fresh event wins over the slower
  // structural refetch's snapshot. The ref carries the previous result so a flush that changes no
  // node ON THIS MAP hands back the same array — and therefore the same `layout` object and the
  // same element below, which is what keeps reconciliation proportional to what moved.
  const overlay = useRef<LiveOverlay<PlacedNode> | null>(null);
  const placedNodes = useMemo(() => {
    overlay.current = overlayLiveStates(layout.nodes, live, overlay.current);
    return overlay.current.out;
  }, [layout.nodes, live]);
  const viewLayout = useMemo(
    () => (placedNodes === layout.nodes ? layout : { ...layout, nodes: placedNodes }),
    [layout, placedNodes],
  );
  // A memoized element: when `viewLayout` is unchanged React bails out of the whole SVG subtree
  // instead of re-creating thousands of <g>/<line> elements on every fleet-wide flush.
  const map = useMemo(() => <TopologyMap layout={viewLayout} />, [viewLayout]);

  const presentStates = useMemo(() => {
    const set = new Set(placedNodes.map((n) => n.state));
    return SEVERITY_ORDER.filter((s) => set.has(s));
  }, [placedNodes]);
  // `suppressed` comes from the fetched root-cause edge, not from live state — read it off the
  // structural layout so it doesn't recompute per flush.
  const anySuppressed = layout.nodes.some((n) => n.suppressed);
  const presentSources = useMemo(() => {
    const set = new Set(layout.edges.map((e) => e.source));
    return LINK_SOURCES.filter((s) => set.has(s));
  }, [layout.edges]);

  const summary = data?.summary;
  // Every counter here is something the derivation declined to turn into a link. Showing the total
  // is what keeps an incomplete graph from reading as a complete one — and `unmatched_lldp_rows` is
  // the number that moves the day a switch that speaks LLDP is racked.
  const unresolved =
    (summary?.unmatched_lldp_rows ?? 0) +
    (summary?.unmatched_cdp_rows ?? 0) +
    (summary?.ambiguous_mgmt_addrs ?? 0) +
    (summary?.oversized_segments ?? 0);

  const note =
    layout.isolatedCount > 0
      ? t('map.note.linked', { linked: layout.nodes.length, unlinked: layout.isolatedCount })
      : t('map.note.default');

  function body() {
    if (error) return <Card><p className="muted">{error}</p></Card>;
    if (loading && !data) return <Card><p className="muted">{t('map.loading')}</p></Card>;
    if (nodes.length === 0)
      return <Card><p className="muted">{t('map.emptyInventory')}</p></Card>;
    if (layout.nodes.length === 0)
      return (
        <Card>
          <p className="muted">{t('map.noLinksYet', { count: layout.isolatedCount })}</p>
        </Card>
      );
    if (layout.nodes.length > MAX_GRAPH_NODES || layout.edges.length > MAX_GRAPH_LINKS)
      return (
        <Card>
          <p className="muted">
            {t('map.tooMany', { count: layout.nodes.length, links: layout.edges.length })}
          </p>
        </Card>
      );
    return <div className="topomap-page-canvas">{map}</div>;
  }

  return (
    <div className="topomap-page">
      <PageHeader
        title={t('nav:topology.map')}
        trail={[{ label: t('nav:sections.topology') }, { label: t('nav:topology.map') }]}
        note={note}
      />
      {(presentStates.length > 0 || anySuppressed || presentSources.length > 0) && (
        <div className="topomap-legend" aria-label={t('map.legend')}>
          {presentStates.map((s) => (
            <span className="topomap-legend-item" key={s}>
              <span className="topomap-legend-dot" style={{ background: stateColorVar(s) }} />
              {stateLabel(s)}
            </span>
          ))}
          {presentSources.map((s) => (
            <span className="topomap-legend-item" key={s}>
              <span className={`topomap-legend-edge ${s}`} />
              {t(`map.source.${s}`)}
            </span>
          ))}
          {anySuppressed && (
            <span className="topomap-legend-item">
              <span className="topomap-legend-line" />
              {t('map.suppressed')}
            </span>
          )}
        </div>
      )}
      {layout.componentCount > 1 && (
        <p className="topomap-hint muted">
          {t('map.components', { count: layout.componentCount })}
        </p>
      )}
      {unresolved > 0 && (
        <p className="topomap-hint muted">{t('map.unresolved', { count: unresolved })}</p>
      )}
      {body()}
    </div>
  );
}
