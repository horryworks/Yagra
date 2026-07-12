// Topology ▸ Network map. A visual dependency graph: every node with a parent → child link,
// colored by live status, with dependency-suppressed nodes (an upstream root cause) drawn muted
// and their edge dashed. Data: GET /api/v1/topology (parent links + state + root_cause), fetched on
// a slow reconcile cadence and kept live via the node-state SSE stream (`useNodeStates`, S14) so
// status updates without re-fetching the whole graph every 15s (S7). Isolated nodes (no dependency
// links) are summarised, not drawn, so a flat inventory doesn't paint thousands of disconnected
// boxes. The forest is a single-parent tree, so we lay it out ourselves — see components/TopologyMap.

import { useMemo } from 'react';
import { Trans, useTranslation } from 'react-i18next';
import { Link } from 'react-router-dom';
import { PageHeader } from '../components/ui/PageHeader';
import { Card } from '../components/ui/Card';
import { TopologyMap } from '../components/TopologyMap/TopologyMap';
import { layoutTopology } from '../components/TopologyMap/layout';
import { stateColorVar, stateLabel } from '../lib/format';
import type { NodeState } from '../types/api';
import { api } from '../services/api';
import { usePolled } from '../dashboard/usePolled';
import { useNodeStates, LIVE_RECONCILE_MS } from '../dashboard/useNodeStates';
import './TopologyMapPage.css';

/** Above this many linked nodes an SVG map stops being legible (and cheap) — surface the size
 *  and point at the tree/filter rather than paint an unreadable hairball. */
const MAP_CAP = 300;

const LEGEND_ORDER: NodeState[] = [
  'critical',
  'unreachable',
  'warning',
  'unknown',
  'maintenance',
  'ok',
];

export function TopologyMapPage() {
  const { t } = useTranslation('topology');
  const { data, loading, error } = usePolled(() => api.getTopology(), [], LIVE_RECONCILE_MS);
  const live = useNodeStates();
  // Overlay the live SSE state on top of the fetched graph (S14): a fresh event wins over the
  // slower structural refetch's snapshot.
  const nodes = useMemo(() => {
    const base = data?.nodes ?? [];
    return base.map((n) => {
      const s = live.get(n.id);
      return s && s !== n.state ? { ...n, state: s } : n;
    });
  }, [data, live]);
  const layout = useMemo(() => layoutTopology(nodes), [nodes]);

  const presentStates = useMemo(() => {
    const set = new Set(layout.nodes.map((n) => n.state));
    return LEGEND_ORDER.filter((s) => set.has(s));
  }, [layout.nodes]);
  const anySuppressed = layout.nodes.some((n) => n.suppressed);

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
          <p className="muted">
            <Trans
              t={t}
              i18nKey="map.noLinks"
              count={layout.isolatedCount}
              values={{ count: layout.isolatedCount }}
              components={{ depLink: <Link to="/topology/dependency" /> }}
            />
          </p>
        </Card>
      );
    if (layout.nodes.length > MAP_CAP)
      return (
        <Card>
          <p className="muted">{t('map.tooMany', { count: layout.nodes.length })}</p>
        </Card>
      );
    return (
      <div className="topomap-page-canvas">
        <TopologyMap layout={layout} />
      </div>
    );
  }

  return (
    <div className="topomap-page">
      <PageHeader
        title={t('nav:topology.map')}
        trail={[{ label: t('nav:sections.topology') }, { label: t('nav:topology.map') }]}
        note={note}
      />
      {(presentStates.length > 0 || anySuppressed) && (
        <div className="topomap-legend" aria-label={t('map.legend')}>
          {presentStates.map((s) => (
            <span className="topomap-legend-item" key={s}>
              <span className="topomap-legend-dot" style={{ background: stateColorVar(s) }} />
              {stateLabel(s)}
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
      {body()}
    </div>
  );
}
