// SPDX-License-Identifier: AGPL-3.0-only
// 08 · Traffic-flow widgets (ADR-031). Fleet-wide (all-exporters) flow analytics on the dashboard:
// top talkers / AS / ports / protocol mix, a conversations Sankey, and a throughput trend. All read
// the fleet `/flow/*` endpoints (ClickHouse behind core); when flow monitoring isn't enabled those
// 503 and each widget shows a "not configured" gate (like the AuditWidget's 403 gate). Bytes are the
// ranking dimension everywhere (matching the per-node Flow tab).

import { useTranslation } from 'react-i18next';
import { Select } from '../../components/ui/Field';
import { FlowSankey } from '../../components/NodeDetail/FlowSankey';
import { MetricChart, PALETTE } from '../../components/MetricChart/MetricChart';
import { formatAsn, formatBytes, formatSi } from '../../lib/format';
import { portLabel, protoName } from '../../lib/flowLabels';
import { api } from '../../services/api';
import type { WidgetProps, WidgetSettings } from '../types';
import { Donut, type DonutSegment } from '../primitives/Donut';
import { RankedBars, type RankedRow } from '../primitives/RankedBars';
import { usePolled } from '../usePolled';
import { flowTrendSeries, trailingSecs } from './util';

/** Trailing window for the flow widgets (last hour). */
const SPAN_SECS = 3600;

/** Shared guard: the flow endpoints 503 when the store is off — show the gate, not a raw error
 *  (mirrors the AuditWidget 403 pattern). Returns the element to render, or `null` to fall through. */
function useFlowGuard<T>(polled: { data: T | null; loading: boolean; error: string | null }) {
  const { t } = useTranslation('dashboard');
  if (polled.error) return <p className="muted">{t('widgets.flow.unavailable')}</p>;
  if (polled.loading && !polled.data) return <p className="muted">{t('common:loading')}</p>;
  return null;
}

export function FlowTopTalkersWidget() {
  const { t } = useTranslation('dashboard');
  const polled = usePolled(() => api.getFlowTopTalkers({ ...trailingSecs(SPAN_SECS), limit: 8 }), []);
  const gate = useFlowGuard(polled);
  if (gate) return gate;
  const rows: RankedRow[] = (polled.data ?? []).map((e) => ({
    label: e.addr,
    value: e.bytes,
    valueText: formatBytes(e.bytes),
  }));
  return <RankedBars rows={rows} empty={t('widgets.flow.empty')} />;
}

/** Read the src/dst AS direction from instance settings (default destination). */
function dirOf(settings: WidgetSettings | undefined): 'src' | 'dst' {
  return settings?.dir === 'src' ? 'src' : 'dst';
}

/** Header action for the top-AS widget: source ↔ destination AS. */
export function FlowAsDirActions({ instance, setSettings }: WidgetProps) {
  const { t } = useTranslation('dashboard');
  return (
    <Select
      value={dirOf(instance.settings)}
      onChange={(e) => setSettings({ dir: e.target.value })}
      aria-label={t('widgets.flowTopAs.dirAria')}
      title={t('widgets.flowTopAs.dirAria')}
    >
      <option value="dst">{t('widgets.flowTopAs.dst')}</option>
      <option value="src">{t('widgets.flowTopAs.src')}</option>
    </Select>
  );
}

export function FlowTopAsWidget({ instance }: WidgetProps) {
  const { t } = useTranslation('dashboard');
  const dir = dirOf(instance.settings);
  const polled = usePolled(
    () => api.getFlowTopAs({ ...trailingSecs(SPAN_SECS), limit: 8, dir }),
    [dir],
  );
  const gate = useFlowGuard(polled);
  if (gate) return gate;
  const rows: RankedRow[] = (polled.data ?? []).map((e) => ({
    label: formatAsn(e.asn, e.name) ?? t('widgets.flow.asUnknown'),
    value: e.bytes,
    valueText: formatBytes(e.bytes),
    color: 'var(--series-2)',
  }));
  return <RankedBars rows={rows} empty={t('widgets.flow.empty')} />;
}

export function FlowTopPortsWidget() {
  const { t } = useTranslation('dashboard');
  const polled = usePolled(() => api.getFlowTopPorts({ ...trailingSecs(SPAN_SECS), limit: 8 }), []);
  const gate = useFlowGuard(polled);
  if (gate) return gate;
  const rows: RankedRow[] = (polled.data ?? []).map((e) => ({
    label: portLabel(e.port),
    value: e.bytes,
    valueText: formatBytes(e.bytes),
    color: 'var(--series-3)',
  }));
  return <RankedBars rows={rows} empty={t('widgets.flow.empty')} />;
}

export function FlowProtoMixWidget() {
  const { t } = useTranslation('dashboard');
  const polled = usePolled(() => api.getFlowProtocols({ ...trailingSecs(SPAN_SECS), limit: 8 }), []);
  const gate = useFlowGuard(polled);
  if (gate) return gate;
  const data = polled.data ?? [];
  if (data.length === 0) return <p className="muted">{t('widgets.flow.empty')}</p>;
  const segments: DonutSegment[] = data.map((e, i) => ({
    label: protoName(e.proto),
    value: e.bytes,
    color: PALETTE[i % PALETTE.length],
  }));
  const total = data.reduce((n, e) => n + e.bytes, 0);
  return <Donut segments={segments} centerValue={formatSi(total)} centerSub={t('widgets.flow.bytes')} />;
}

export function FlowConversationsWidget() {
  const { t } = useTranslation('dashboard');
  const polled = usePolled(
    () => api.getFlowConversations({ ...trailingSecs(SPAN_SECS), limit: 12 }),
    [],
  );
  const gate = useFlowGuard(polled);
  if (gate) return gate;
  const convos = polled.data ?? [];
  if (convos.length === 0) return <p className="muted">{t('widgets.flow.empty')}</p>;
  return <FlowSankey conversations={convos} />;
}

export function FlowTrendWidget() {
  const { t } = useTranslation('dashboard');
  const polled = usePolled(() => api.getFlowSeries(trailingSecs(SPAN_SECS)), []);
  const gate = useFlowGuard(polled);
  if (gate) return gate;
  const { timestamps, series } = flowTrendSeries(polled.data ?? [], protoName, PALETTE);
  if (timestamps.length === 0) return <p className="muted">{t('widgets.flow.empty')}</p>;
  return (
    <MetricChart
      title=""
      timestamps={timestamps}
      fill
      yFormat={(v) => formatSi(v)}
      legendFormat={(v) => formatBytes(v)}
      series={series}
    />
  );
}
