// Topology ▸ Network map. A visual dependency graph: every node with a parent → child link,
// colored by live status, with dependency-suppressed nodes (an upstream root cause) drawn muted
// and their edge dashed. Data: GET /api/v1/topology (parent links + live state + root_cause),
// polled on the dashboard cadence. Isolated nodes (no dependency links) are summarised, not drawn,
// so a flat inventory doesn't paint thousands of disconnected boxes. The forest is a single-parent
// tree, so we lay it out ourselves (no graph-lib dependency) — see components/TopologyMap.

import { useMemo } from 'react';
import { Link } from 'react-router-dom';
import { PageHeader } from '../components/ui/PageHeader';
import { Card } from '../components/ui/Card';
import { TopologyMap } from '../components/TopologyMap/TopologyMap';
import { layoutTopology } from '../components/TopologyMap/layout';
import { stateColorVar, stateLabel } from '../lib/format';
import type { NodeState } from '../types/api';
import { api } from '../services/api';
import { usePolled } from '../dashboard/usePolled';
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
  const { data, loading, error } = usePolled(() => api.getTopology(), []);
  const nodes = useMemo(() => data?.nodes ?? [], [data]);
  const layout = useMemo(() => layoutTopology(nodes), [nodes]);

  const presentStates = useMemo(() => {
    const set = new Set(layout.nodes.map((n) => n.state));
    return LEGEND_ORDER.filter((s) => set.has(s));
  }, [layout.nodes]);
  const anySuppressed = layout.nodes.some((n) => n.suppressed);

  const note =
    layout.isolatedCount > 0
      ? `${layout.nodes.length} linked · ${layout.isolatedCount} unlinked (no dependencies, not shown)`
      : 'Dependency graph — parent → child links, colored by live status.';

  function body() {
    if (error) return <Card><p className="muted">{error}</p></Card>;
    if (loading && !data) return <Card><p className="muted">Loading topology…</p></Card>;
    if (nodes.length === 0)
      return <Card><p className="muted">No nodes in the inventory yet.</p></Card>;
    if (layout.nodes.length === 0)
      return (
        <Card>
          <p className="muted">
            No dependency links defined yet. Set a node&apos;s upstream on the{' '}
            <Link to="/topology/dependency">Dependency view</Link> (or a node&apos;s
            &ldquo;Dependency…&rdquo; action) to build the map. {layout.isolatedCount} node
            {layout.isolatedCount === 1 ? '' : 's'} in the inventory.
          </p>
        </Card>
      );
    if (layout.nodes.length > MAP_CAP)
      return (
        <Card>
          <p className="muted">
            This topology has {layout.nodes.length} linked nodes — too many to render as a legible
            map. Drill in from a specific node&apos;s detail page, or use the dependency widget on
            the dashboard.
          </p>
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
        title="Network map"
        trail={[{ label: 'Topology' }, { label: 'Network map' }]}
        note={note}
      />
      {(presentStates.length > 0 || anySuppressed) && (
        <div className="topomap-legend" aria-label="Legend">
          {presentStates.map((s) => (
            <span className="topomap-legend-item" key={s}>
              <span className="topomap-legend-dot" style={{ background: stateColorVar(s) }} />
              {stateLabel(s)}
            </span>
          ))}
          {anySuppressed && (
            <span className="topomap-legend-item">
              <span className="topomap-legend-line" />
              Suppressed (upstream cause)
            </span>
          )}
        </div>
      )}
      {body()}
    </div>
  );
}
