// SPDX-License-Identifier: AGPL-3.0-only
// Event Storm report body (passive monitoring, ADR-024).
//
// Entity: a node. The signal is **how far the peak bucket overshot the node's own learned baseline** —
// 400 events/5min is normal for a busy aggregation switch and alarming for an access port — so rows
// rank and filter by the *multiple*, not the raw count, and each carries a baseline-vs-peak meter.
//
// `when_label` is `rel_label(peak_bucket)` in Rust and cannot be localized, so the peak time is read
// from the additive `detail.peak_at` (unix seconds) and formatted with the shared `relTime`. Rows
// written by an older core lack the key and fall back to the backend string.

import { useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Card } from '../../../components/ui/Card';
import { RankedBars, type RankedRow } from '../../../dashboard/primitives/RankedBars';
import { relTime } from '../../format';
import {
  Chips,
  EmptyList,
  FindingRow,
  NodeRef,
  RatioMeter,
  ReportToolbar,
  RightRail,
} from '../kit';
import { detailNum, fmtCount, ratioBucket, sevOf, sortByDetail, sortCommon } from '../format';
import {
  stormBaseline as baseOf,
  stormPeak as peakOf,
  stormRatio as ratioOf,
} from '../findingFacts';
import type { ReportBodyProps } from '../types';
import type { AnalysisFinding } from '../../../types/api';

/** Ratio bucket — the filter axis that makes a storm comparable across differently-busy nodes. */
function StormRow({ finding }: { finding: AnalysisFinding }) {
  const { t } = useTranslation('troubleshoot');
  const bucketSecs = detailNum(finding, 'bucket_secs') ?? 300;
  const peakAt = detailNum(finding, 'peak_at');
  return (
    <FindingRow
      finding={finding}
      identity={
        <>
          <NodeRef finding={finding} />
          <span className="ts-anom-kind">
            {t('report.event_storm.peakInBucket', {
              count: Math.round(peakOf(finding)),
              minutes: Math.round(bucketSecs / 60),
            })}
          </span>
        </>
      }
      viz={
        <RatioMeter
          peak={peakOf(finding)}
          baseline={baseOf(finding)}
          severity={sevOf(finding)}
          format={(v) => fmtCount(Math.round(v))}
        />
      }
      right={
        <RightRail
          // Derived from `peak_at` so JA gets a localized relative time; the English label is the
          // fallback for rows written before that field existed.
          when={peakAt !== undefined ? relTime(peakAt * 1000) : finding.when_label}
          detail={t('report.event_storm.baseline', { count: Math.round(baseOf(finding)) })}
        />
      }
    />
  );
}

export function EventStormBody({ findings }: ReportBodyProps) {
  const { t } = useTranslation('troubleshoot');
  const [filter, setFilter] = useState<'all' | 'x10' | 'x3' | 'low'>('all');
  const [sort, setSort] = useState<'peak' | 'ratio' | 'node'>('ratio');

  const top = useMemo<RankedRow[]>(
    () =>
      sortByDetail(findings, 'peak')
        .slice(0, 10)
        .map((f, i) => ({
          label: `${i + 1}. ${f.node_name}`,
          value: peakOf(f),
          valueText: fmtCount(Math.round(peakOf(f))),
          color:
            sevOf(f) === 'crit'
              ? 'var(--status-critical)'
              : sevOf(f) === 'warn'
                ? 'var(--status-warning)'
                : 'var(--series-5)',
        })),
    [findings],
  );

  const list = useMemo(() => {
    const base = filter === 'all' ? findings : findings.filter((f) => ratioBucket(ratioOf(f)) === filter);
    if (sort === 'peak') return sortByDetail(base, 'peak');
    if (sort === 'ratio') return base.slice().sort((a, b) => ratioOf(b) - ratioOf(a));
    return sortCommon(base, 'node');
  }, [findings, filter, sort]);

  return (
    <>
      {top.length > 0 && (
        <Card title={t('report.event_storm.noisiest')}>
          <RankedBars rows={top} />
        </Card>
      )}
      <ReportToolbar
        id="tsr-event-storm"
        filters={
          <Chips
            value={filter}
            onChange={setFilter}
            options={[
              { value: 'all', label: t('report.common.filters.all') },
              { value: 'x10', label: t('report.event_storm.filters.x10'), color: 'var(--status-critical)' },
              { value: 'x3', label: t('report.event_storm.filters.x3'), color: 'var(--status-warning)' },
              { value: 'low', label: t('report.event_storm.filters.low'), color: 'var(--series-5)' },
            ]}
          />
        }
        sort={sort}
        onSort={(v) => setSort(v as 'peak' | 'ratio' | 'node')}
        sortOptions={[
          { value: 'ratio', label: t('report.event_storm.sort.byRatio') },
          { value: 'peak', label: t('report.event_storm.sort.byPeak') },
          { value: 'node', label: t('report.common.sort.byNode') },
        ]}
      />
      <div className="ts-anoms">
        {list.length ? (
          list.map((f) => <StormRow key={f.id} finding={f} />)
        ) : (
          <EmptyList total={findings.length} />
        )}
      </div>
    </>
  );
}
