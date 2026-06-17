// 01 · Fleet status widgets: status summary (reuses the existing presentational widget),
// health ring (donut of node states with % healthy), and a nodes-down KPI tile. All read the
// shared `useNodes()` poll, so adding several costs one inventory fetch.

import { stateColorVar } from '../../lib/format';
import { StatusSummary } from '../../widgets/StatusSummary';
import { Donut } from '../primitives/Donut';
import { KpiTile } from '../primitives/KpiTile';
import { useNodes } from '../useNodes';
import { downCount, percentHealthy, stateCounts } from './util';

export function StatusSummaryWidget() {
  const { nodes, loading } = useNodes();
  return <StatusSummary nodes={nodes} loading={loading} />;
}

export function HealthRingWidget() {
  const { nodes, loading } = useNodes();
  if (loading && nodes.length === 0) return <p className="muted">Loading nodes…</p>;
  if (nodes.length === 0) return <p className="muted">No nodes yet.</p>;
  const c = stateCounts(nodes);
  const segments = [
    { label: 'Healthy', value: c.ok, color: stateColorVar('ok') },
    { label: 'Warning', value: c.warning, color: stateColorVar('warning') },
    { label: 'Critical', value: c.critical + c.unreachable, color: stateColorVar('critical') },
    { label: 'Unknown', value: c.unknown, color: stateColorVar('unknown') },
    { label: 'Maintenance', value: c.maintenance, color: stateColorVar('maintenance') },
  ].filter((s) => s.value > 0);
  return (
    <Donut segments={segments} centerValue={String(percentHealthy(nodes))} centerSub="% healthy" />
  );
}

export function NodesDownWidget() {
  const { nodes, loading } = useNodes();
  if (loading && nodes.length === 0) return <p className="muted">Loading nodes…</p>;
  return <KpiTile value={String(downCount(nodes))} caption="nodes down" />;
}
