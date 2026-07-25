// SPDX-License-Identifier: AGPL-3.0-only
// 07 · Passive-events widgets (syslog / SNMP traps / webhooks). A live event feed (existing
// `/events`) plus summary widgets over `/events/stats` (server-side aggregation — accurate over the
// full firehose when VictoriaLogs is enabled, else within PostgreSQL retention). Categorical
// dimensions: kind, action (pipeline outcome), trap type, source; plus an event-volume histogram and
// a rule-coverage KPI. All fleet-wide over a trailing 24h window.

import { useNavigate } from 'react-router-dom';
import { useTranslation } from 'react-i18next';
import { Badge } from '../../components/ui/Badge';
import { Button } from '../../components/ui/Button';
import { Select } from '../../components/ui/Field';
import { useEntityNames } from '../../components/ui/EntityName';
import { relativeTime } from '../../lib/format';
import { api } from '../../services/api';
import type { EventKind } from '../../types/api';
import { Donut, type DonutSegment } from '../primitives/Donut';
import { KpiTile } from '../primitives/KpiTile';
import { RankedBars, type RankedRow } from '../primitives/RankedBars';
import type { WidgetProps, WidgetSettings } from '../types';
import { usePolled } from '../usePolled';
import { densifyTimeBuckets, trailingIso } from './util';

/** Trailing window for every event widget (last 24h). */
const SPAN_SECS = 86_400;

/** `action` (pipeline outcome) → Badge tone. Mirrors the Events page "Result" column. */
const ACTION_TONE: Record<string, 'critical' | 'warning' | 'up' | 'info' | 'neutral'> = {
  fired: 'critical',
  refreshed: 'warning',
  cleared: 'up',
  info: 'info',
  suppressed: 'neutral',
  none: 'neutral',
};

/** Event-kind segment colors (categorical — series channel, not status). */
const KIND_COLOR: Record<string, string> = {
  syslog: 'var(--series-1)',
  trap: 'var(--series-2)',
  webhook: 'var(--series-3)',
};

/** `action` segment colors: raised/cleared read on the status channel, the rest on series. */
const ACTION_COLOR: Record<string, string> = {
  fired: 'var(--status-critical)',
  refreshed: 'var(--status-warning)',
  cleared: 'var(--status-ok)',
  info: 'var(--series-3)',
  suppressed: 'var(--series-4)',
  none: 'var(--series-5)',
};

// ── Event feed (live) ────────────────────────────────────────────────────────────────

/** The kind filter from instance settings (undefined = all kinds). */
function kindOf(settings: WidgetSettings | undefined): EventKind | undefined {
  const k = settings?.kind;
  return k === 'syslog' || k === 'trap' || k === 'webhook' ? k : undefined;
}

export function EventFeedWidget({ instance }: WidgetProps) {
  const { t } = useTranslation('dashboard');
  const { nodeName } = useEntityNames();
  const kind = kindOf(instance.settings);
  const { data, loading, error } = usePolled(() => api.listEvents({ limit: 20, kind }), [kind]);
  if (error) return <p className="muted">{error}</p>;
  if (loading && !data) return <p className="muted">{t('common:loading')}</p>;
  const events = data ?? [];
  if (events.length === 0) return <p className="muted">{t('widgets.eventFeed.empty')}</p>;
  return (
    <ul className="dwl">
      {events.map((e) => {
        const source = e.node_id ? nodeName(e.node_id) : (e.source_ip ?? e.hostname ?? '—');
        return (
          <li className="dwl-row" key={e.id}>
            <Badge tone={ACTION_TONE[e.action] ?? 'neutral'} title={e.trap_oid ?? undefined}>
              {e.trap_name ?? e.kind}
            </Badge>
            <span className="dwl-name">{source}</span>
            <span className="dwl-sub muted">
              {relativeTime(new Date(e.at_unix_ms).toISOString(), Date.now())} · {e.message}
            </span>
          </li>
        );
      })}
    </ul>
  );
}

/** Header actions for the event feed: kind filter + a jump to the full Events screen. */
export function EventFeedActions({ instance, setSettings }: WidgetProps) {
  const { t } = useTranslation('dashboard');
  const navigate = useNavigate();
  return (
    <>
      <Select
        value={kindOf(instance.settings) ?? 'all'}
        onChange={(e) => setSettings({ kind: e.target.value === 'all' ? undefined : e.target.value })}
        aria-label={t('widgets.eventFeed.kindAria')}
        title={t('widgets.eventFeed.kindAria')}
      >
        <option value="all">{t('widgets.eventFeed.allKinds')}</option>
        <option value="syslog">{t('widgets.eventFeed.syslog')}</option>
        <option value="trap">{t('widgets.eventFeed.trap')}</option>
        <option value="webhook">{t('widgets.eventFeed.webhook')}</option>
      </Select>
      <Button variant="ghost" onClick={() => navigate('/alerts/events')}>
        {t('widgets.eventFeed.viewAll')}
      </Button>
    </>
  );
}

// ── Summary widgets (`/events/stats`) ──────────────────────────────────────────────────

export function EventVolumeWidget() {
  const { t } = useTranslation('dashboard');
  const { data, loading, error } = usePolled(
    () => api.getEventVolume({ ...trailingIso(SPAN_SECS), bucketSecs: 3600, splitKind: true }),
    [],
  );
  if (error) return <p className="muted">{error}</p>;
  if (loading && !data) return <p className="muted">{t('common:loading')}</p>;
  const bins = densifyTimeBuckets(data ?? [], 24, 3_600_000, Date.now());
  const total = bins.reduce((n, b) => n + b.count, 0);
  if (total === 0) return <p className="muted">{t('widgets.eventVolume.empty')}</p>;
  const peak = Math.max(...bins.map((b) => b.count), 1);
  return (
    <div className="vbars" role="img" aria-label={t('widgets.eventVolume.aria')}>
      {bins.map((b) => (
        <span className="vbar" key={b.t} title={t('widgets.eventVolume.count', { count: b.count })}>
          <span className="vbar-fill" style={{ height: `${(b.count / peak) * 100}%` }} />
        </span>
      ))}
    </div>
  );
}

export function EventKindMixWidget() {
  const { t } = useTranslation('dashboard');
  const { data, loading, error } = usePolled(() => api.getEventStats('kind', trailingIso(SPAN_SECS)), []);
  if (error) return <p className="muted">{error}</p>;
  if (loading && !data) return <p className="muted">{t('common:loading')}</p>;
  const rows = data ?? [];
  const total = rows.reduce((n, r) => n + r.count, 0);
  if (total === 0) return <p className="muted">{t('widgets.eventKind.empty')}</p>;
  const segments: DonutSegment[] = rows.map((r) => ({
    label: t(`widgets.eventFeed.${r.key}`, r.key),
    value: r.count,
    color: KIND_COLOR[r.key] ?? 'var(--series-4)',
  }));
  return <Donut segments={segments} centerValue={String(total)} centerSub={t('widgets.eventKind.events')} />;
}

export function EventTriageMixWidget() {
  const { t } = useTranslation('dashboard');
  const { data, loading, error } = usePolled(() => api.getEventStats('action', trailingIso(SPAN_SECS)), []);
  if (error) return <p className="muted">{error}</p>;
  if (loading && !data) return <p className="muted">{t('common:loading')}</p>;
  const rows = data ?? [];
  const total = rows.reduce((n, r) => n + r.count, 0);
  if (total === 0) return <p className="muted">{t('widgets.eventTriage.empty')}</p>;
  const segments: DonutSegment[] = rows.map((r) => ({
    label: t(`widgets.eventTriage.action.${r.key}`, r.key),
    value: r.count,
    color: ACTION_COLOR[r.key] ?? 'var(--series-5)',
  }));
  return <Donut segments={segments} centerValue={String(total)} centerSub={t('widgets.eventKind.events')} />;
}

export function TopTrapTypesWidget() {
  const { t } = useTranslation('dashboard');
  const { data, loading, error } = usePolled(
    () => api.getEventStats('trap', { ...trailingIso(SPAN_SECS), kind: 'trap', limit: 8 }),
    [],
  );
  if (error) return <p className="muted">{error}</p>;
  if (loading && !data) return <p className="muted">{t('common:loading')}</p>;
  const rows: RankedRow[] = (data ?? []).map((r) => ({
    label: r.label ?? r.key,
    value: r.count,
    valueText: String(r.count),
    color: 'var(--series-2)',
  }));
  return <RankedBars rows={rows} empty={t('widgets.topTraps.empty')} />;
}

export function NoisyEventSourcesWidget() {
  const { t } = useTranslation('dashboard');
  const { nodeName } = useEntityNames();
  const { data, loading, error } = usePolled(
    () => api.getEventStats('source', { ...trailingIso(SPAN_SECS), limit: 8 }),
    [],
  );
  if (error) return <p className="muted">{error}</p>;
  if (loading && !data) return <p className="muted">{t('common:loading')}</p>;
  const rows: RankedRow[] = (data ?? []).map((r) => ({
    // A known source resolves to its node name; an unknown source shows its IP (the `label`/`key`).
    label: r.node_id ? nodeName(r.node_id) : (r.label ?? r.key),
    value: r.count,
    valueText: String(r.count),
    color: 'var(--series-4)',
  }));
  return <RankedBars rows={rows} empty={t('widgets.noisySources.empty')} />;
}

export function EventRuleCoverageWidget() {
  const { t } = useTranslation('dashboard');
  const { data, loading, error } = usePolled(async () => {
    const win = trailingIso(SPAN_SECS);
    const [all, matched] = await Promise.all([
      api.getEventStats('kind', win),
      api.getEventStats('kind', { ...win, matched: true }),
    ]);
    const sum = (xs: { count: number }[]) => xs.reduce((n, x) => n + x.count, 0);
    return { total: sum(all), matched: sum(matched) };
  }, []);
  if (error) return <p className="muted">{error}</p>;
  if (loading && !data) return <p className="muted">{t('common:loading')}</p>;
  const total = data?.total ?? 0;
  const matched = data?.matched ?? 0;
  const pct = total > 0 ? Math.round((matched / total) * 100) : 0;
  return (
    <KpiTile
      value={total > 0 ? `${pct}%` : '—'}
      caption={t('widgets.eventCoverage.caption', { matched, total })}
    />
  );
}
