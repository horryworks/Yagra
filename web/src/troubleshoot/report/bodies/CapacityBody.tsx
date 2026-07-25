// SPDX-License-Identifier: AGPL-3.0-only
// Capacity Forecast report body.
//
// Entity: a node × resource (a utilization-percent metric). The question is "how long have I got?",
// so the report is organised entirely around **time to exhaustion**: it sorts soonest-first, buckets
// by the 30/90-day planning horizons, and every row carries a `CapacityRunway` showing the projected
// trend arriving at the 100% wall.
//
// All labels are derived from `detail` (`current`, `slope_per_day`, `tte_days`) rather than the
// backend's pre-rendered `"0% now"` / `"~5mo to 100%"`, which are English-only.

import { useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Card } from '../../../components/ui/Card';
import { RankedBars, type RankedRow } from '../../../dashboard/primitives/RankedBars';
import { formatUtil } from '../../../lib/format';
import { CapacityRunway, type RunwayDetail } from '../CapacityRunway';
import { Chips, EmptyList, FindingRow, MonoLine, NodeRef, ReportToolbar, RightRail } from '../kit';
import { capacityBucket, detailNum, humanDays, sevOf, sortCommon } from '../format';
import type { ReportBodyProps } from '../types';
import type { AnalysisFinding } from '../../../types/api';

const tteOf = (f: AnalysisFinding) => detailNum(f, 'tte_days') ?? Infinity;
const currentOf = (f: AnalysisFinding) => detailNum(f, 'current') ?? 0;
const slopeOf = (f: AnalysisFinding) => detailNum(f, 'slope_per_day') ?? 0;

/** Bucket → colour: the same 30/90-day thresholds the backend scores against. */
function bucketColor(tte: number): string {
  const b = capacityBucket(tte);
  if (b === 'soon') return 'var(--status-critical)';
  if (b === 'mid') return 'var(--status-warning)';
  return 'var(--series-1)';
}

function CapacityRow({ finding }: { finding: AnalysisFinding }) {
  const { t } = useTranslation('troubleshoot');
  const tte = tteOf(finding);
  const { count, unit } = humanDays(tte);
  const detail: RunwayDetail = {
    current: currentOf(finding),
    slope_per_day: slopeOf(finding),
    tte_days: tte,
  };
  return (
    <FindingRow
      finding={finding}
      identity={
        <>
          <NodeRef finding={finding} />
          <MonoLine>{finding.metric}</MonoLine>
          <span className="ts-anom-kind">
            {t('report.capacity.growth', { rate: slopeOf(finding).toFixed(2) })}
          </span>
        </>
      }
      viz={<CapacityRunway detail={detail} severity={sevOf(finding)} />}
      right={
        <RightRail
          when={t(`report.capacity.tte.${unit}`, { count })}
          detail={t('report.capacity.nowAt', { pct: formatUtil(currentOf(finding)) })}
        />
      }
    />
  );
}

export function CapacityBody({ findings }: ReportBodyProps) {
  const { t } = useTranslation('troubleshoot');
  const [filter, setFilter] = useState<'all' | 'soon' | 'mid' | 'far'>('all');
  const [sort, setSort] = useState<'tte' | 'growth' | 'node'>('tte');

  const top = useMemo<RankedRow[]>(
    () =>
      findings
        .slice()
        .sort((a, b) => tteOf(a) - tteOf(b))
        .slice(0, 10)
        .map((f, i) => ({
          label: `${i + 1}. ${f.node_name} · ${f.metric}`,
          value: currentOf(f),
          valueText: formatUtil(currentOf(f)),
          color: bucketColor(tteOf(f)),
        })),
    [findings],
  );

  const list = useMemo(() => {
    const base = filter === 'all' ? findings : findings.filter((f) => capacityBucket(tteOf(f)) === filter);
    if (sort === 'tte') return base.slice().sort((a, b) => tteOf(a) - tteOf(b));
    if (sort === 'growth') return base.slice().sort((a, b) => slopeOf(b) - slopeOf(a));
    return sortCommon(base, 'node');
  }, [findings, filter, sort]);

  return (
    <>
      {top.length > 0 && (
        <Card title={t('report.capacity.soonest')}>
          {/* max={100}: utilization is a percentage — the track is the resource, not the top row. */}
          <RankedBars rows={top} max={100} />
        </Card>
      )}
      <ReportToolbar
        id="tsr-capacity"
        filters={
          <Chips
            value={filter}
            onChange={setFilter}
            options={[
              { value: 'all', label: t('report.common.filters.all') },
              { value: 'soon', label: t('report.capacity.filters.soon'), color: 'var(--status-critical)' },
              { value: 'mid', label: t('report.capacity.filters.mid'), color: 'var(--status-warning)' },
              { value: 'far', label: t('report.capacity.filters.far'), color: 'var(--series-1)' },
            ]}
          />
        }
        sort={sort}
        onSort={(v) => setSort(v as 'tte' | 'growth' | 'node')}
        sortOptions={[
          { value: 'tte', label: t('report.capacity.sort.soonest') },
          { value: 'growth', label: t('report.capacity.sort.growth') },
          { value: 'node', label: t('report.common.sort.byNode') },
        ]}
      />
      <div className="ts-anoms">
        {list.length ? (
          list.map((f) => <CapacityRow key={f.id} finding={f} />)
        ) : (
          <EmptyList total={findings.length} />
        )}
      </div>
    </>
  );
}
