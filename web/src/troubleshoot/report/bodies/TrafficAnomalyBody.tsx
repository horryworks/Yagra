// SPDX-License-Identifier: AGPL-3.0-only
// Traffic Anomaly report body (flow monitoring, ADR-031).
//
// Entity: a node. Structurally the flow twin of `event_storm` — a peak measured against the node's
// own baseline — so it deliberately reuses `RatioMeter`, only swapping the value formatter to bytes.
// That is legitimate sharing: the two `detail` payloads are the same shape modulo units. Everything
// the operator ranks by (the multiple) and reads (byte volumes) is different.
//
// Peak time comes from the additive `detail.peak_at` so JA gets a localized relative label; rows from
// an older core fall back to the backend's English `when_label`.

import { useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Card } from '../../../components/ui/Card';
import { RankedBars, type RankedRow } from '../../../dashboard/primitives/RankedBars';
import { formatBytes } from '../../../lib/format';
import { relTime } from '../../format';
import { Chips, EmptyList, FindingRow, NodeRef, RatioMeter, ReportToolbar, RightRail } from '../kit';
import { detailNum, sevOf, sortByDetail, sortCommon } from '../format';
import type { ReportBodyProps } from '../types';
import type { AnalysisFinding } from '../../../types/api';

const peakOf = (f: AnalysisFinding) => detailNum(f, 'peak_bytes') ?? 0;
const baseOf = (f: AnalysisFinding) => detailNum(f, 'baseline_mean_bytes') ?? 0;
const ratioOf = (f: AnalysisFinding) => {
  const b = baseOf(f);
  return b > 0 ? peakOf(f) / b : Infinity;
};

function ratioBucket(r: number): 'x10' | 'x3' | 'low' {
  if (r >= 10) return 'x10';
  if (r >= 3) return 'x3';
  return 'low';
}

function sevColor(f: AnalysisFinding): string {
  const s = sevOf(f);
  if (s === 'crit') return 'var(--status-critical)';
  if (s === 'warn') return 'var(--status-warning)';
  return 'var(--series-6)';
}

function TrafficRow({ finding }: { finding: AnalysisFinding }) {
  const { t } = useTranslation('troubleshoot');
  const peakAt = detailNum(finding, 'peak_at');
  return (
    <FindingRow
      finding={finding}
      identity={
        <>
          <NodeRef finding={finding} />
          <span className="ts-anom-kind">
            {t('report.traffic_anomaly.peak', { bytes: formatBytes(peakOf(finding)) })}
          </span>
        </>
      }
      viz={
        <RatioMeter
          peak={peakOf(finding)}
          baseline={baseOf(finding)}
          severity={sevOf(finding)}
          format={(v) => formatBytes(v)}
        />
      }
      right={
        <RightRail
          when={peakAt !== undefined ? relTime(peakAt * 1000) : finding.when_label}
          detail={t('report.traffic_anomaly.baseline', { bytes: formatBytes(baseOf(finding)) })}
        />
      }
    />
  );
}

export function TrafficAnomalyBody({ findings }: ReportBodyProps) {
  const { t } = useTranslation('troubleshoot');
  const [filter, setFilter] = useState<'all' | 'x10' | 'x3' | 'low'>('all');
  const [sort, setSort] = useState<'ratio' | 'peak' | 'node'>('ratio');

  const top = useMemo<RankedRow[]>(
    () =>
      sortByDetail(findings, 'peak_bytes')
        .slice(0, 10)
        .map((f, i) => ({
          label: `${i + 1}. ${f.node_name}`,
          value: peakOf(f),
          valueText: formatBytes(peakOf(f)),
          color: sevColor(f),
        })),
    [findings],
  );

  const list = useMemo(() => {
    const base = filter === 'all' ? findings : findings.filter((f) => ratioBucket(ratioOf(f)) === filter);
    if (sort === 'ratio') return base.slice().sort((a, b) => ratioOf(b) - ratioOf(a));
    if (sort === 'peak') return sortByDetail(base, 'peak_bytes');
    return sortCommon(base, 'node');
  }, [findings, filter, sort]);

  return (
    <>
      {top.length > 0 && (
        <Card title={t('report.traffic_anomaly.biggestPeaks')}>
          <RankedBars rows={top} />
        </Card>
      )}
      <ReportToolbar
        id="tsr-traffic-anomaly"
        filters={
          <Chips
            value={filter}
            onChange={setFilter}
            options={[
              { value: 'all', label: t('report.common.filters.all') },
              { value: 'x10', label: t('report.traffic_anomaly.filters.x10'), color: 'var(--status-critical)' },
              { value: 'x3', label: t('report.traffic_anomaly.filters.x3'), color: 'var(--status-warning)' },
              { value: 'low', label: t('report.traffic_anomaly.filters.low'), color: 'var(--series-6)' },
            ]}
          />
        }
        sort={sort}
        onSort={(v) => setSort(v as 'ratio' | 'peak' | 'node')}
        sortOptions={[
          { value: 'ratio', label: t('report.traffic_anomaly.sort.byRatio') },
          { value: 'peak', label: t('report.traffic_anomaly.sort.byPeak') },
          { value: 'node', label: t('report.common.sort.byNode') },
        ]}
      />
      <div className="ts-anoms">
        {list.length ? (
          list.map((f) => <TrafficRow key={f.id} finding={f} />)
        ) : (
          <EmptyList total={findings.length} />
        )}
      </div>
    </>
  );
}
