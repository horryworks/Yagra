// SPDX-License-Identifier: AGPL-3.0-only
// Flap Analysis report body.
//
// Entity: a node (the metric is always `icmp_rtt_ms`, so it isn't worth a column). What matters is
// **rate**, not count — 12 flaps in an hour is an outage, 12 over a week is noise — so the report
// ranks by flaps/hour and splits chronic from intermittent.
//
// Deliberately has NO per-row chart: `detail` is two scalars (`flaps`, `per_hour`), and a sparkline
// drawn from two numbers would be decoration pretending to be data. The ranked bars carry the
// comparison instead.

import { useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Card } from '../../../components/ui/Card';
import { RankedBars, type RankedRow } from '../../../dashboard/primitives/RankedBars';
import { Chips, EmptyList, FindingRow, NodeRef, ReportToolbar, RightRail } from '../kit';
import { flapBucket, sortByDetail, sortCommon } from '../format';
import { flapCount as flapsOf, perHour as rateOf } from '../findingFacts';
import { severityColor } from '../findingTone';
import type { ReportBodyProps } from '../types';
import type { AnalysisFinding } from '../../../types/api';

function FlapRow({ finding }: { finding: AnalysisFinding }) {
  const { t } = useTranslation('troubleshoot');
  const rate = rateOf(finding);
  return (
    <FindingRow
      finding={finding}
      identity={
        <>
          <NodeRef finding={finding} />
          <span className="ts-anom-kind">{t(`report.flap.bucket.${flapBucket(rate)}`)}</span>
        </>
      }
      right={
        <RightRail
          when={t('report.flap.flaps', { count: flapsOf(finding) })}
          detail={t('report.flap.perHour', { rate: rate.toFixed(1) })}
        />
      }
    />
  );
}

export function FlapBody({ findings }: ReportBodyProps) {
  const { t } = useTranslation('troubleshoot');
  const [filter, setFilter] = useState<'all' | 'chronic' | 'intermittent'>('all');
  const [sort, setSort] = useState<'rate' | 'flaps' | 'node'>('rate');

  const top = useMemo<RankedRow[]>(
    () =>
      sortByDetail(findings, 'per_hour')
        .slice(0, 10)
        .map((f, i) => ({
          label: `${i + 1}. ${f.node_name}`,
          value: rateOf(f),
          valueText: t('report.flap.perHour', { rate: rateOf(f).toFixed(1) }),
          color: severityColor(f, 'var(--series-1)'),
        })),
    [findings, t],
  );

  const list = useMemo(() => {
    const base = filter === 'all' ? findings : findings.filter((f) => flapBucket(rateOf(f)) === filter);
    if (sort === 'rate') return sortByDetail(base, 'per_hour');
    if (sort === 'flaps') return sortByDetail(base, 'flaps');
    return sortCommon(base, 'node');
  }, [findings, filter, sort]);

  return (
    <>
      {top.length > 0 && (
        <Card title={t('report.flap.worst')}>
          <RankedBars rows={top} />
        </Card>
      )}
      <ReportToolbar
        id="tsr-flap"
        filters={
          <Chips
            value={filter}
            onChange={setFilter}
            options={[
              { value: 'all', label: t('report.common.filters.all') },
              { value: 'chronic', label: t('report.flap.bucket.chronic'), color: 'var(--status-critical)' },
              { value: 'intermittent', label: t('report.flap.bucket.intermittent'), color: 'var(--series-1)' },
            ]}
          />
        }
        sort={sort}
        onSort={(v) => setSort(v as 'rate' | 'flaps' | 'node')}
        sortOptions={[
          { value: 'rate', label: t('report.flap.sort.byRate') },
          { value: 'flaps', label: t('report.flap.sort.byFlaps') },
          { value: 'node', label: t('report.common.sort.byNode') },
        ]}
      />
      <div className="ts-anoms">
        {list.length ? (
          list.map((f) => <FlapRow key={f.id} finding={f} />)
        ) : (
          <EmptyList total={findings.length} />
        )}
      </div>
    </>
  );
}
