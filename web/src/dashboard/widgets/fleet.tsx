// SPDX-License-Identifier: AGPL-3.0-only
// 01 · Fleet status widgets: status summary (reuses the existing presentational widget),
// health ring (donut of node states with % healthy), and a nodes-down KPI tile. All read the
// shared server-computed `useFleetSummary()` poll (`/fleet/summary`) — correct over the whole
// fleet, not the first page of `listNodes()` (S12) — so adding several costs one fetch.

import { useTranslation } from 'react-i18next';
import { stateColorValue, stateColorVar, stateLabel } from '../../lib/format';
import { HARD_DOWN_STATES } from '../../lib/nodeState';
import { api } from '../../services/api';
import { MetricChart } from '../../components/MetricChart/MetricChart';
import { StatusSummary } from '../../widgets/StatusSummary';
import { Donut } from '../primitives/Donut';
import { KpiTile } from '../primitives/KpiTile';
import { useFleetSummary } from '../useFleetSummary';
import { usePolled } from '../usePolled';

export function StatusSummaryWidget() {
  // Server-computed fleet tally (`/fleet/summary`), correct beyond the first page of nodes (S12).
  const { summary, loading } = useFleetSummary();
  return (
    <StatusSummary counts={summary?.states ?? {}} total={summary?.total ?? 0} loading={loading} />
  );
}

export function HealthRingWidget() {
  const { t } = useTranslation('dashboard');
  const { summary, loading } = useFleetSummary();
  if (loading && !summary) return <p className="muted">{t('widgets.loadingNodes')}</p>;
  if (!summary || summary.total === 0) return <p className="muted">{t('widgets.noNodes')}</p>;
  const c = summary.states;
  const segments = [
    { label: t('widgets.healthRing.healthy'), value: c.ok, color: stateColorVar('ok') },
    { label: stateLabel('warning'), value: c.warning, color: stateColorVar('warning') },
    {
      label: stateLabel('critical'),
      value: HARD_DOWN_STATES.reduce((n, s) => n + c[s], 0),
      color: stateColorVar('critical'),
    },
    { label: stateLabel('unknown'), value: c.unknown, color: stateColorVar('unknown') },
    { label: stateLabel('maintenance'), value: c.maintenance, color: stateColorVar('maintenance') },
  ].filter((s) => s.value > 0);
  const pct = summary.total > 0 ? Math.round((c.ok / summary.total) * 100) : 0;
  return (
    <Donut
      segments={segments}
      centerValue={String(pct)}
      centerSub={t('widgets.healthRing.healthyPct')}
    />
  );
}

export function NodesDownWidget() {
  const { t } = useTranslation('dashboard');
  const { summary, loading } = useFleetSummary();
  if (loading && !summary) return <p className="muted">{t('widgets.loadingNodes')}</p>;
  // Hard-down over the whole fleet (the one HARD_DOWN_STATES definition).
  const down = summary ? HARD_DOWN_STATES.reduce((n, s) => n + summary.states[s], 0) : 0;
  return <KpiTile value={String(down)} caption={t('widgets.nodesDown.caption')} />;
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
  // "Down" merges the hard-down states into one problem line.
  const down = ts.map((_, i) => HARD_DOWN_STATES.reduce((n, s) => n + at(s, i), 0));
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
