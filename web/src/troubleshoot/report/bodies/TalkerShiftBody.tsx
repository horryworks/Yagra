// SPDX-License-Identifier: AGPL-3.0-only
// Talker Shift report body (flow monitoring, ADR-031).
//
// Entity: an **address** that became dominant on a node this window without being in the previous
// one. The whole signal is the **rank it entered at** — a brand-new #1 talker is a different event
// from a new #7 — so the rank gets the strongest visual weight (a chip whose tone mirrors the
// backend's own `novelty_score` bands) and is a filter axis in its own right.

import { useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Card } from '../../../components/ui/Card';
import { RankedBars, type RankedRow } from '../../../dashboard/primitives/RankedBars';
import { formatBytes } from '../../../lib/format';
import { Chips, EmptyList, FindingRow, MonoLine, NodeRef, ReportToolbar, RightRail } from '../kit';
import { detailNum, detailStr, sortByDetail, sortCommon } from '../format';
import type { ReportBodyProps } from '../types';
import type { AnalysisFinding } from '../../../types/api';

const addrOf = (f: AnalysisFinding) => detailStr(f, 'addr');
const bytesOf = (f: AnalysisFinding) => detailNum(f, 'bytes') ?? 0;
/** 1-based rank the new talker entered at (the backend stores it 1-based in `detail`). */
const rankOf = (f: AnalysisFinding) => detailNum(f, 'rank') ?? 99;

/** Rank tone mirrors the backend's novelty scoring: #1 is the strongest signal. */
function rankTone(rank: number): 'crit' | 'warn' | 'info' {
  if (rank <= 1) return 'crit';
  if (rank <= 3) return 'warn';
  return 'info';
}

function toneColor(tone: 'crit' | 'warn' | 'info'): string {
  if (tone === 'crit') return 'var(--status-critical)';
  if (tone === 'warn') return 'var(--status-warning)';
  return 'var(--series-6)';
}

function TalkerRow({ finding }: { finding: AnalysisFinding }) {
  const { t } = useTranslation('troubleshoot');
  const rank = rankOf(finding);
  const tone = rankTone(rank);
  return (
    <FindingRow
      finding={finding}
      identity={
        <>
          {/* The address is the subject; the node is where it showed up. */}
          <MonoLine title={addrOf(finding)}>{addrOf(finding) ?? '—'}</MonoLine>
          <span className="ts-anom-metric">
            <span className="muted">{t('report.talker_shift.seenOn')} </span>
            <NodeRef finding={finding} />
          </span>
        </>
      }
      viz={
        <div className="tsr-rank-wrap">
          <span className={`tsr-rank ${tone}`}>#{rank}</span>
          <span className="tsr-rank-cap">{t('report.talker_shift.newAtRank')}</span>
        </div>
      }
      right={<RightRail when={formatBytes(bytesOf(finding))} />}
    />
  );
}

export function TalkerShiftBody({ findings }: ReportBodyProps) {
  const { t } = useTranslation('troubleshoot');
  const [filter, setFilter] = useState<'all' | 'top1' | 'top3'>('all');
  const [sort, setSort] = useState<'bytes' | 'rank' | 'node'>('bytes');

  const top = useMemo<RankedRow[]>(
    () =>
      sortByDetail(findings, 'bytes')
        .slice(0, 10)
        .map((f, i) => ({
          label: `${i + 1}. ${addrOf(f) ?? f.node_name}`,
          value: bytesOf(f),
          valueText: formatBytes(bytesOf(f)),
          color: toneColor(rankTone(rankOf(f))),
        })),
    [findings],
  );

  const list = useMemo(() => {
    const base = findings.filter((f) => {
      if (filter === 'top1') return rankOf(f) <= 1;
      if (filter === 'top3') return rankOf(f) <= 3;
      return true;
    });
    if (sort === 'bytes') return sortByDetail(base, 'bytes');
    if (sort === 'rank') return base.slice().sort((a, b) => rankOf(a) - rankOf(b));
    return sortCommon(base, 'node');
  }, [findings, filter, sort]);

  return (
    <>
      {top.length > 0 && (
        <Card title={t('report.talker_shift.heaviest')}>
          <RankedBars rows={top} />
        </Card>
      )}
      <ReportToolbar
        id="tsr-talker-shift"
        filters={
          <Chips
            value={filter}
            onChange={setFilter}
            options={[
              { value: 'all', label: t('report.common.filters.all') },
              { value: 'top1', label: t('report.talker_shift.filters.top1'), color: 'var(--status-critical)' },
              { value: 'top3', label: t('report.talker_shift.filters.top3'), color: 'var(--status-warning)' },
            ]}
          />
        }
        sort={sort}
        onSort={(v) => setSort(v as 'bytes' | 'rank' | 'node')}
        sortOptions={[
          { value: 'bytes', label: t('report.talker_shift.sort.byBytes') },
          { value: 'rank', label: t('report.talker_shift.sort.byRank') },
          { value: 'node', label: t('report.common.sort.byNode') },
        ]}
      />
      <div className="ts-anoms">
        {list.length ? (
          list.map((f) => <TalkerRow key={f.id} finding={f} />)
        ) : (
          <EmptyList total={findings.length} />
        )}
      </div>
    </>
  );
}
