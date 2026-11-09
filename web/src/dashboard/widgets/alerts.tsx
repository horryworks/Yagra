// SPDX-License-Identifier: AGPL-3.0-only
// 02 · Alerts widgets. Active alerts + flapping + severity mix read the live alert store (kept live by
// the one useAlertStream AppShell mounts); alert volume buckets the history endpoint client-side.

import { useTranslation } from 'react-i18next';
import { WEEKDAY_KEYS } from '../../lib/cadence';
import {
  alertWhatOf,
  relativeTime,
  severityColorVar,
  severityLabel,
  stateColorVar,
  stateLabel,
} from '../../lib/format';
import { api } from '../../services/api';
import { sortedAlerts, useAlertStore } from '../../store';
import { useEntityNames } from '../../components/ui/entityNames';
import { AlertRows } from '../../widgets/AlertRows';
import { AlertSubjectName } from '../../widgets/AlertSubjectName';
import { AlertWhatText } from '../../widgets/AlertWhatText';
import { Donut } from '../primitives/Donut';
import { Heatmap } from '../primitives/Heatmap';
import { RankedBars } from '../primitives/RankedBars';
import { DrillLink } from '../../components/ui/DrillLink';
import { usePolled } from '../usePolled';
import { alertHistoryHourDrill, alertsDrill, nodeDrill } from './drill';
import { bucketAlertsByHour, calendarMatrix } from './util';

const HOUR_LABELS = Array.from({ length: 24 }, (_, h) => (h % 6 === 0 ? String(h) : ''));

export function ActiveAlertsWidget() {
  const { t } = useTranslation('dashboard');
  // Show a generous count; the card-body scrolls when the cell is short and fills when it's tall.
  return <AlertRows limit={20} empty={t('widgets.activeAlerts.empty')} />;
}

export function SeverityMixWidget() {
  const { t } = useTranslation('dashboard');
  const alerts = useAlertStore((s) => s.alerts);
  const list = Object.values(alerts);
  if (list.length === 0) return <p className="muted">{t('widgets.severityMix.empty')}</p>;
  const by = { critical: 0, warning: 0, info: 0 };
  for (const a of list) by[a.severity] += 1;
  const segments = [
    {
      label: severityLabel('critical'),
      value: by.critical,
      color: severityColorVar('critical'),
      href: alertsDrill({ severity: 'critical' }),
    },
    {
      label: severityLabel('warning'),
      value: by.warning,
      color: severityColorVar('warning'),
      href: alertsDrill({ severity: 'warning' }),
    },
    {
      label: severityLabel('info'),
      value: by.info,
      color: severityColorVar('info'),
      href: alertsDrill({ severity: 'info' }),
    },
  ].filter((s) => s.value > 0);
  return (
    <Donut segments={segments} centerValue={String(list.length)} centerSub={t('widgets.severityMix.active')} />
  );
}

export function FlappingWatchlistWidget() {
  const { t } = useTranslation('dashboard');
  const alerts = useAlertStore((s) => s.alerts);
  // Same resolution rules as AlertRows: the node shows its name (UUID on hover), and the check
  // id — a one-way hash with no name — shows what it measures, id recoverable on hover.
  const { nodeName } = useEntityNames();
  const flapping = sortedAlerts(alerts).filter((a) => a.flapping);
  if (flapping.length === 0) {
    return <p className="muted">{t('widgets.flapping.empty')}</p>;
  }
  return (
    <ul className="dwl">
      {flapping.map((a) => (
        <li className="dwl-row" key={`${a.node}|${a.check}|${a.severity}`}>
          <span className="dwl-dot" style={{ background: severityColorVar(a.severity) }} />
          <span className="dwl-name">
            {/* Was `nodeName(a.node)`, which reads the subject id without asking what kind it is.
                For a poller-pool alert that is `pool:<name>`, so the row rendered a broken-looking
                reference AND poisoned the name-resolution batch it was sent in. */}
            <AlertSubjectName alert={a} nodeName={nodeName} />
          </span>
          <span className="dwl-sub" title={a.check}>
            <AlertWhatText what={alertWhatOf(a)} />
          </span>
        </li>
      ))}
    </ul>
  );
}

export function AlertVolumeWidget() {
  const { t } = useTranslation('dashboard');
  const { data, loading, error } = usePolled(() => api.listAlertHistory({ limit: 1000 }), []);
  if (error) return <p className="muted">{error}</p>;
  if (loading && !data) return <p className="muted">{t('common:loading')}</p>;
  const buckets = bucketAlertsByHour(data ?? [], 24, Date.now());
  const total = buckets.reduce((n, b) => n + b.count, 0);
  if (total === 0) return <p className="muted">{t('widgets.alertVolume.empty')}</p>;
  const peak = Math.max(...buckets.map((b) => b.count), 1);
  return (
    <div className="vbars" role="group" aria-label={t('widgets.alertVolume.aria')}>
      {buckets.map((b) => (
        <DrillLink
          key={b.t}
          to={b.count > 0 ? alertHistoryHourDrill(Math.floor(b.t / 1000)) : undefined}
          className="vbar"
          title={t('widgets.alertVolume.opened', { count: b.count })}
        >
          <span className="vbar-fill" style={{ height: `${(b.count / peak) * 100}%` }} />
        </DrillLink>
      ))}
    </div>
  );
}

export function TopAlertingNodesWidget() {
  const { t } = useTranslation('dashboard');
  const { data, loading, error } = usePolled(() => api.getAlertTopNodes({ limit: 6 }), []);
  if (error) return <p className="muted">{error}</p>;
  if (loading && !data) return <p className="muted">{t('common:loading')}</p>;
  const rows = (data?.entries ?? []).map((e) => ({
    label: e.name,
    value: e.count,
    valueText: String(e.count),
    color: 'var(--series-2)',
    href: alertsDrill({ nodeId: e.node_id }),
  }));
  return (
    <RankedBars
      rows={rows}
      empty={t('widgets.topAlerting.empty')}
      partial={data?.partial}
    />
  );
}

export function AlertCalendarWidget() {
  const { t } = useTranslation('dashboard');
  const { data, loading, error } = usePolled(() => api.getAlertCalendar(7), []);
  if (error) return <p className="muted">{error}</p>;
  if (loading && !data) return <p className="muted">{t('common:loading')}</p>;
  const matrix = calendarMatrix(data ?? []);
  if ((data ?? []).length === 0) return <p className="muted">{t('widgets.alertCalendar.empty')}</p>;
  // Shared with the report/analysis schedule labels — one list, because the *order* is load-bearing
  // (`calendarMatrix` rows are `Date.getDay()`) and a private copy is how two screens end up
  // disagreeing about which row is Sunday.
  const rowLabels = WEEKDAY_KEYS.map((k) => t(`widgets.alertCalendar.dow.${k}`));
  return (
    <Heatmap
      rowLabels={rowLabels}
      colLabels={HOUR_LABELS}
      values={matrix}
      colorBase="var(--series-2)"
      title={(row, col, v) => t('widgets.alertCalendar.cell', { row, hour: col, count: v })}
    />
  );
}

export function RecentStateChangesWidget() {
  const { t } = useTranslation('dashboard');
  const { data, loading, error } = usePolled(() => api.getAlertTransitions(30), []);
  if (error) return <p className="muted">{error}</p>;
  if (loading && !data) return <p className="muted">{t('common:loading')}</p>;
  if ((data ?? []).length === 0) return <p className="muted">{t('widgets.recentChanges.empty')}</p>;
  return (
    <ul className="dwl">
      {(data ?? []).map((row, i) => (
        <li className="dwl-row" key={`${row.node_id}-${row.at_unix_ms}-${i}`}>
          <span
            className="dwl-dot"
            style={{
              background: row.resolved
                ? stateColorVar('ok')
                : severityColorVar(row.severity),
            }}
          />
          <DrillLink to={nodeDrill(row.node_id)} className="dwl-name">
            {row.name}
          </DrillLink>
          <span className="dwl-sub muted">
            {row.resolved
              ? `→ ${t('widgets.recentChanges.recovered')}`
              : `→ ${stateLabel(row.state)}`}{' '}
            ·{' '}
            {relativeTime(new Date(row.at_unix_ms).toISOString(), Date.now())}
          </span>
        </li>
      ))}
    </ul>
  );
}
