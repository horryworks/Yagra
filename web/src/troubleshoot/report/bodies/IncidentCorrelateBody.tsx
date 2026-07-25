// SPDX-License-Identifier: AGPL-3.0-only
// Incident Correlate report body (cross-store: TSDB + events + flow).
//
// Entity: a node **incident** — a set of signals of at least two different kinds that coincided. The
// payload is inherently a sequence, so this is the one report that opts OUT of the shared `.ts-anom`
// row grid entirely: each incident is a full-width card with a header, the timeline, and the signal
// list beneath it. Ordering is the product here ("events started, then traffic shifted"), and a row
// in a grid cannot show ordering.
//
// A donut of the signal-kind mix answers the triage question across incidents: are these
// metric-led or flow-led?
//
// Known accepted gap: signal `label` strings are composed in Rust and are technical identifiers
// (metric names, IPs, byte counts, device message text) rather than prose, so they render verbatim in
// mono and are NOT localized. Structuring them backend-side is a clean follow-up.

import { useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Card } from '../../../components/ui/Card';
import { Donut, type DonutSegment } from '../../../dashboard/primitives/Donut';
import { EntityName } from '../../../components/ui/EntityName';
import { relTime } from '../../format';
import { formatTimestamp } from '../../../lib/format';
import {
  IncidentTimeline,
  LANES,
  laneOf,
  signalTone,
  type Lane,
  type TimelineSignal,
} from '../IncidentTimeline';
import { Chips, EmptyList, ReportToolbar } from '../kit';
import { sevOf, sortCommon } from '../format';
import type { ReportBodyProps } from '../types';
import type { AnalysisFinding } from '../../../types/api';

/** The timeline array out of a finding's detail, defensively typed. */
function timelineOf(f: AnalysisFinding): TimelineSignal[] {
  const raw = (f.detail as { timeline?: unknown } | null | undefined)?.timeline;
  if (!Array.isArray(raw)) return [];
  return raw.filter(
    (s): s is TimelineSignal =>
      typeof s === 'object' && s !== null && typeof (s as TimelineSignal).at === 'number',
  );
}

const kindsOf = (f: AnalysisFinding) => new Set(timelineOf(f).map((s) => s.kind));
const earliestOf = (f: AnalysisFinding) => {
  const ts = timelineOf(f).map((s) => s.at);
  return ts.length ? Math.min(...ts) : 0;
};

const LANE_COLORS: Record<Lane, string> = {
  metric: 'var(--series-1)',
  event: 'var(--series-5)',
  flow: 'var(--series-6)',
  other: 'var(--text-tertiary)',
};

function IncidentCard({ finding }: { finding: AnalysisFinding }) {
  const { t } = useTranslation('troubleshoot');
  const signals = useMemo(
    () => timelineOf(finding).slice().sort((a, b) => a.at - b.at),
    [finding],
  );
  const laneLabels = useMemo(
    () =>
      Object.fromEntries(LANES.map((l) => [l, t(`report.incident_correlate.lane.${l}`)])) as Record<
        Lane,
        string
      >,
    [t],
  );
  if (!signals.length) return null;
  const from = signals[0].at;
  const to = signals[signals.length - 1].at;

  return (
    <div className={`tsr-incident sev-${sevOf(finding)}`}>
      <div className="tsr-incident-head">
        <span className="tsr-incident-score">{Math.round(finding.score)}</span>
        <span className="tsr-incident-node">
          <EntityName name={finding.node_name} id={finding.node_id ?? undefined} />
        </span>
        <span className="tsr-incident-meta">
          {t('report.incident_correlate.signals', { count: signals.length })} ·{' '}
          {t('report.incident_correlate.began', { time: relTime(from * 1000) })}
        </span>
      </div>

      <IncidentTimeline
        timeline={signals}
        laneLabels={laneLabels}
        fromLabel={formatTimestamp(from * 1000)}
        toLabel={formatTimestamp(to * 1000)}
      />

      <ol className="tsr-signal-list">
        {signals.map((s, i) => (
          <li key={`${s.at}-${i}`} className={`tsr-signal ${signalTone(s.severity)}`}>
            <span className="tsr-signal-kind" style={{ color: LANE_COLORS[laneOf(s.kind)] }}>
              {laneLabels[laneOf(s.kind)]}
            </span>
            <span className="tsr-signal-time mono">{formatTimestamp(s.at * 1000)}</span>
            {/* Technical identifiers from the engine — rendered verbatim in mono, not localized. */}
            <span className="tsr-signal-label mono">{s.label}</span>
          </li>
        ))}
      </ol>
    </div>
  );
}

export function IncidentCorrelateBody({ findings }: ReportBodyProps) {
  const { t } = useTranslation('troubleshoot');
  const [filter, setFilter] = useState<'all' | 'metric' | 'event' | 'flow'>('all');
  const [sort, setSort] = useState<'score' | 'earliest' | 'signals' | 'node'>('score');

  /** Signal-kind mix across every incident — metric-led or flow-led? */
  const mix = useMemo<DonutSegment[]>(() => {
    const acc = new Map<Lane, number>();
    for (const f of findings) {
      for (const s of timelineOf(f)) {
        const lane = laneOf(s.kind);
        acc.set(lane, (acc.get(lane) ?? 0) + 1);
      }
    }
    return LANES.filter((l) => acc.has(l)).map((l) => ({
      label: t(`report.incident_correlate.lane.${l}`),
      value: acc.get(l) ?? 0,
      color: LANE_COLORS[l],
    }));
  }, [findings, t]);

  const list = useMemo(() => {
    const base = filter === 'all' ? findings : findings.filter((f) => kindsOf(f).has(filter));
    if (sort === 'score') return base.slice().sort((a, b) => b.score - a.score);
    if (sort === 'earliest') return base.slice().sort((a, b) => earliestOf(a) - earliestOf(b));
    if (sort === 'signals')
      return base.slice().sort((a, b) => timelineOf(b).length - timelineOf(a).length);
    return sortCommon(base, 'node');
  }, [findings, filter, sort]);

  return (
    <>
      {mix.length > 0 && (
        <Card title={t('report.incident_correlate.mix')}>
          <Donut segments={mix} />
        </Card>
      )}
      <ReportToolbar
        id="tsr-incident"
        filters={
          <Chips
            value={filter}
            onChange={setFilter}
            options={[
              { value: 'all', label: t('report.common.filters.all') },
              { value: 'metric', label: t('report.incident_correlate.has.metric'), color: LANE_COLORS.metric },
              { value: 'event', label: t('report.incident_correlate.has.event'), color: LANE_COLORS.event },
              { value: 'flow', label: t('report.incident_correlate.has.flow'), color: LANE_COLORS.flow },
            ]}
          />
        }
        sort={sort}
        onSort={(v) => setSort(v as 'score' | 'earliest' | 'signals' | 'node')}
        sortOptions={[
          { value: 'score', label: t('report.common.sort.byScore') },
          { value: 'earliest', label: t('report.incident_correlate.sort.byEarliest') },
          { value: 'signals', label: t('report.incident_correlate.sort.bySignals') },
          { value: 'node', label: t('report.common.sort.byNode') },
        ]}
      />
      <div className="tsr-incidents">
        {list.length ? (
          list.map((f) => <IncidentCard key={f.id} finding={f} />)
        ) : (
          <EmptyList total={findings.length} />
        )}
      </div>
    </>
  );
}
