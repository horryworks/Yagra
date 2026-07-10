// 01 · Fleet status widgets: status summary (reuses the existing presentational widget),
// health ring (donut of node states with % healthy), and a nodes-down KPI tile. All read the
// shared `useNodes()` poll, so adding several costs one inventory fetch.

import { useTranslation } from 'react-i18next';
import { stateColorValue, stateColorVar, stateLabel } from '../../lib/format';
import { api } from '../../services/api';
import { MetricChart } from '../../components/MetricChart/MetricChart';
import { StatusSummary } from '../../widgets/StatusSummary';
import { Donut } from '../primitives/Donut';
import { KpiTile } from '../primitives/KpiTile';
import { useNodes } from '../useNodes';
import { usePolled } from '../usePolled';
import { downCount, percentHealthy, stateCounts } from './util';

export function StatusSummaryWidget() {
  const { nodes, loading } = useNodes();
  return <StatusSummary nodes={nodes} loading={loading} />;
}

export function HealthRingWidget() {
  const { t } = useTranslation('dashboard');
  const { nodes, loading } = useNodes();
  if (loading && nodes.length === 0) return <p className="muted">{t('widgets.loadingNodes')}</p>;
  if (nodes.length === 0) return <p className="muted">{t('widgets.noNodes')}</p>;
  const c = stateCounts(nodes);
  const segments = [
    { label: t('widgets.healthRing.healthy'), value: c.ok, color: stateColorVar('ok') },
    { label: stateLabel('warning'), value: c.warning, color: stateColorVar('warning') },
    { label: stateLabel('critical'), value: c.critical + c.unreachable, color: stateColorVar('critical') },
    { label: stateLabel('unknown'), value: c.unknown, color: stateColorVar('unknown') },
    { label: stateLabel('maintenance'), value: c.maintenance, color: stateColorVar('maintenance') },
  ].filter((s) => s.value > 0);
  return (
    <Donut
      segments={segments}
      centerValue={String(percentHealthy(nodes))}
      centerSub={t('widgets.healthRing.healthyPct')}
    />
  );
}

export function NodesDownWidget() {
  const { t } = useTranslation('dashboard');
  const { nodes, loading } = useNodes();
  if (loading && nodes.length === 0) return <p className="muted">{t('widgets.loadingNodes')}</p>;
  return <KpiTile value={String(downCount(nodes))} caption={t('widgets.nodesDown.caption')} />;
}

export function FleetHealthTimelineWidget() {
  const { t } = useTranslation('dashboard');
  const { data, loading, error } = usePolled(() => api.getStateHistory(), []);
  if (error) return <p className="muted">{error}</p>;
  if (loading && !data) return <p className="muted">{t('common:loading')}</p>;
  const ts = data?.timestamps ?? [];
  if (ts.length === 0) {
    return <p className="muted">{t('widgets.fleetTimeline.empty')}</p>;
  }
  const s = data?.series ?? {};
  const at = (k: string, i: number) => s[k]?.[i] ?? 0;
  // "Down" merges critical + unreachable (both hard-down) into one problem line.
  const down = ts.map((_, i) => at('critical', i) + at('unreachable', i));
  // Series colors go to the canvas (uPlot can't read CSS vars), so resolve the *active theme's*
  // status palette to concrete colors — keeping the timeline's Down/Warning/Unknown identical to
  // the table/donut/tree in both light and dark, instead of freezing the dark-theme hex.
  return (
    <MetricChart
      title=""
      timestamps={ts}
      fill
      yFormat={(v) => String(Math.round(v))}
      series={[
        { label: t('widgets.fleetTimeline.down'), values: down, color: stateColorValue('critical') },
        { label: stateLabel('warning'), values: s.warning ?? [], color: stateColorValue('warning') },
        { label: stateLabel('unknown'), values: s.unknown ?? [], color: stateColorValue('unknown') },
      ]}
    />
  );
}
