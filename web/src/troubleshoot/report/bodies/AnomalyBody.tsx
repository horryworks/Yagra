// SPDX-License-Identifier: AGPL-3.0-only
// Anomaly Detection report body — the findings list only; the shell owns the chrome.
//
// Entity: a node × metric series. Its differentiator is the **shape** of the deviation
// (spike/level/drift/flat/season) and the sparkline showing the value against its learned mean ± σ
// envelope — so it filters by shape and every row carries an `AnomalyChart`.
//
// Extracted verbatim (behaviour-wise) from the former AnomalyReportPage so the shared descriptor
// contract was proven against the one report already known to work.

import { useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { AnomalyChart } from '../../AnomalyChart';
import { KINDS, kindMeta, type Kind } from '../../data';
import { Chips, EmptyList, FindingRow, KindTag, MonoLine, NodeRef, ReportToolbar, RightRail } from '../kit';
import { sevOf, sortCommon, type CommonSort } from '../format';
import type { ReportBodyProps } from '../types';
import type { AnalysisFinding, AnomalyDetail } from '../../../types/api';

function AnomalyRow({ finding }: { finding: AnalysisFinding }) {
  const { t } = useTranslation('troubleshoot');
  const meta = kindMeta(finding.kind);
  const detail = finding.detail as AnomalyDetail;
  return (
    <FindingRow
      finding={finding}
      identity={
        <>
          <NodeRef finding={finding} />
          <MonoLine>{finding.metric}</MonoLine>
          <KindTag color={meta.color} label={t(meta.label)} />
        </>
      }
      viz={detail?.points ? <AnomalyChart detail={detail} severity={sevOf(finding)} /> : null}
      right={<RightRail when={finding.when_label} detail={finding.duration} />}
    />
  );
}

export function AnomalyBody({ findings }: ReportBodyProps) {
  const { t } = useTranslation('troubleshoot');
  const [filter, setFilter] = useState<'all' | Kind>('all');
  const [sort, setSort] = useState<CommonSort>('score');

  const chipOptions = useMemo(
    () => [
      { value: 'all' as const, label: t('report.common.filters.all') },
      ...(Object.keys(KINDS) as Kind[]).map((k) => ({
        value: k,
        label: t(KINDS[k].label),
        color: KINDS[k].color,
      })),
    ],
    [t],
  );

  const list = useMemo(() => {
    const base = filter === 'all' ? findings : findings.filter((f) => f.kind === filter);
    return sortCommon(base, sort);
  }, [findings, filter, sort]);

  return (
    <>
      <ReportToolbar
        id="tsr-anomaly"
        filters={<Chips options={chipOptions} value={filter} onChange={setFilter} />}
        sort={sort}
        onSort={(v) => setSort(v as CommonSort)}
        sortOptions={[
          { value: 'score', label: t('report.common.sort.byScore') },
          { value: 'node', label: t('report.common.sort.byNode') },
        ]}
      />
      <div className="ts-anoms">
        {list.length ? (
          list.map((f) => <AnomalyRow key={f.id} finding={f} />)
        ) : (
          <EmptyList total={findings.length} />
        )}
      </div>
    </>
  );
}
