// SPDX-License-Identifier: AGPL-3.0-only
// Event Correlation report body.
//
// Entity: a metric **pair** — not a node. `metric` holds "A ↔ B" and `node_name` is series A's label,
// so the row shows both sides of the relationship stacked, and the summary counts *pairs*.
//
// The differentiator is the correlation coefficient's **strength and sign**: the ranked bars are
// scaled against `max={1}` (not the best row) because a correlation bar has to read against a true
// 1.0 ceiling — normalizing by the peak would make r=0.86 look maximal.

import { useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Card } from '../../../components/ui/Card';
import { RankedBars, type RankedRow } from '../../../dashboard/primitives/RankedBars';
import { Chips, EmptyList, FindingRow, MonoLine, ReportToolbar, RightRail } from '../kit';
import { correlationDirection, detailNum, sortByDetail, sortCommon } from '../format';
import {
  correlationPair as pairOf,
  correlationR as rOf,
  correlationText as rText,
} from '../findingFacts';
import { correlationColor as colorFor } from '../findingTone';
import type { ReportBodyProps } from '../types';
import type { AnalysisFinding } from '../../../types/api';

function CorrelationRow({ finding }: { finding: AnalysisFinding }) {
  const { t } = useTranslation('troubleshoot');
  const r = rOf(finding);
  const samples = detailNum(finding, 'samples');
  const [a, b] = pairOf(finding);
  const strength = Math.min(100, Math.abs(r) * 100);
  return (
    <FindingRow
      finding={finding}
      identity={
        <>
          <MonoLine title={a}>{a}</MonoLine>
          <MonoLine title={b}>↔ {b}</MonoLine>
          <span className="ts-anom-kind">{t(`report.correlation.dir.${correlationDirection(r)}`)}</span>
        </>
      }
      viz={
        // Strength against a fixed 0–1 scale, so rows are comparable across the whole report.
        <div className="tsr-meter" title={rText(r)}>
          <div
            className="tsr-meter-fill"
            style={{ width: `${strength}%`, background: colorFor(r) }}
          />
        </div>
      }
      right={
        <RightRail
          when={rText(r)}
          detail={
            samples !== undefined ? t('report.correlation.samples', { count: samples }) : undefined
          }
        />
      }
    />
  );
}

export function CorrelationBody({ findings }: ReportBodyProps) {
  const { t } = useTranslation('troubleshoot');
  const [filter, setFilter] = useState<'all' | 'coRising' | 'inverse'>('all');
  const [sort, setSort] = useState<'r' | 'samples' | 'node'>('r');

  const top = useMemo<RankedRow[]>(
    () =>
      sortByDetail(findings, 'r')
        .slice()
        .sort((a, b) => Math.abs(rOf(b)) - Math.abs(rOf(a)))
        .slice(0, 10)
        .map((f, i) => {
          const r = rOf(f);
          const [a, b] = pairOf(f);
          return {
            // RankedBars keys on `label`, so the index keeps two identical pairs distinct.
            label: `${i + 1}. ${a} ↔ ${b}`,
            value: Math.abs(r),
            valueText: rText(r),
            color: colorFor(r),
          };
        }),
    [findings],
  );

  const list = useMemo(() => {
    const base =
      filter === 'all' ? findings : findings.filter((f) => correlationDirection(rOf(f)) === filter);
    if (sort === 'r') return base.slice().sort((a, b) => Math.abs(rOf(b)) - Math.abs(rOf(a)));
    if (sort === 'samples') return sortByDetail(base, 'samples');
    return sortCommon(base, 'node');
  }, [findings, filter, sort]);

  return (
    <>
      {top.length > 0 && (
        <Card title={t('report.correlation.strongest')}>
          {/* max={1}: a correlation bar must read against a true 1.0, not against the best row. */}
          <RankedBars rows={top} max={1} />
        </Card>
      )}
      <ReportToolbar
        id="tsr-correlation"
        filters={
          <Chips
            value={filter}
            onChange={setFilter}
            options={[
              { value: 'all', label: t('report.common.filters.all') },
              { value: 'coRising', label: t('report.correlation.dir.coRising'), color: 'var(--series-1)' },
              { value: 'inverse', label: t('report.correlation.dir.inverse'), color: 'var(--series-4)' },
            ]}
          />
        }
        sort={sort}
        onSort={(v) => setSort(v as 'r' | 'samples' | 'node')}
        sortOptions={[
          { value: 'r', label: t('report.correlation.sort.byR') },
          { value: 'samples', label: t('report.correlation.sort.bySamples') },
          { value: 'node', label: t('report.common.sort.byNode') },
        ]}
      />
      <div className="ts-anoms">
        {list.length ? (
          list.map((f) => <CorrelationRow key={f.id} finding={f} />)
        ) : (
          <EmptyList total={findings.length} />
        )}
      </div>
    </>
  );
}
