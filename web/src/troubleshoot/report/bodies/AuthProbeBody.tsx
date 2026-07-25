// SPDX-License-Identifier: AGPL-3.0-only
// Auth Probe report body (passive monitoring, ADR-024).
//
// Entity: a **source IP** — the thing doing the authenticating. The node is the *target*, so unlike
// every metric report the node is the secondary column: the operator's questions are "who is hitting
// us" and "what are they hitting", and both must be first-class. Search is over the source address
// because triage is usually by subnet ("is this all one management range?").
//
// The backend puts the raw source IP in `duration`, which is a display-only field; this reads
// `detail.source_ip` instead so the value is structured and localizable around.

import { useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Card } from '../../../components/ui/Card';
import { RankedBars, type RankedRow } from '../../../dashboard/primitives/RankedBars';
import {
  ResultCount,
  SearchInput,
  TableSpacer,
  TableToolbar,
} from '../../../components/ui/TableToolbar';
import { Chips, EmptyList, FindingRow, MonoLine, NodeRef, ReportToolbar, RightRail } from '../kit';
import { detailNum, detailStr, fmtCount, sevOf, sortByDetail, sortCommon } from '../format';
import type { ReportBodyProps } from '../types';
import type { AnalysisFinding } from '../../../types/api';

const srcOf = (f: AnalysisFinding) => detailStr(f, 'source_ip');
const countOf = (f: AnalysisFinding) => detailNum(f, 'count') ?? 0;

function sevColor(f: AnalysisFinding): string {
  const s = sevOf(f);
  if (s === 'crit') return 'var(--status-critical)';
  if (s === 'warn') return 'var(--status-warning)';
  return 'var(--series-5)';
}

function AuthRow({ finding }: { finding: AnalysisFinding }) {
  const { t } = useTranslation('troubleshoot');
  const src = srcOf(finding);
  return (
    <FindingRow
      finding={finding}
      identity={
        <>
          {/* The source is the identity here — the node is context. */}
          <MonoLine title={src}>{src ?? t('report.auth_probe.unknownSource')}</MonoLine>
          <span className="ts-anom-metric">
            <span className="muted">{t('report.auth_probe.target')} </span>
            <NodeRef finding={finding} />
          </span>
        </>
      }
      right={
        <RightRail when={t('report.auth_probe.failures', { count: countOf(finding) })} />
      }
    />
  );
}

export function AuthProbeBody({ findings }: ReportBodyProps) {
  const { t } = useTranslation('troubleshoot');
  const [query, setQuery] = useState('');
  const [filter, setFilter] = useState<'all' | 'crit' | 'warn'>('all');
  const [sort, setSort] = useState<'count' | 'source' | 'node'>('count');

  const top = useMemo<RankedRow[]>(
    () =>
      sortByDetail(findings, 'count')
        .slice(0, 10)
        .map((f, i) => ({
          label: `${i + 1}. ${srcOf(f) ?? t('report.auth_probe.unknownSource')}`,
          value: countOf(f),
          valueText: fmtCount(countOf(f)),
          color: sevColor(f),
        })),
    [findings, t],
  );

  const list = useMemo(() => {
    const q = query.trim().toLowerCase();
    let l = findings.filter(
      (f) =>
        (filter === 'all' || sevOf(f) === filter) &&
        (!q || (srcOf(f) ?? '').toLowerCase().includes(q)),
    );
    if (sort === 'count') l = sortByDetail(l, 'count');
    else if (sort === 'source')
      l = l.slice().sort((a, b) => (srcOf(a) ?? '').localeCompare(srcOf(b) ?? ''));
    else l = sortCommon(l, 'node');
    return l;
  }, [findings, query, filter, sort]);

  return (
    <>
      {top.length > 0 && (
        <Card title={t('report.auth_probe.worstSources')}>
          <RankedBars rows={top} />
        </Card>
      )}
      <TableToolbar>
        <SearchInput
          value={query}
          onChange={setQuery}
          placeholder={t('report.auth_probe.searchPlaceholder')}
          ariaLabel={t('report.auth_probe.searchPlaceholder')}
        />
        <TableSpacer />
        <ResultCount
          shown={list.length}
          total={findings.length}
          noun={t('report.auth_probe.noun')}
        />
      </TableToolbar>
      <ReportToolbar
        id="tsr-auth-probe"
        filters={
          <Chips
            value={filter}
            onChange={setFilter}
            options={[
              { value: 'all', label: t('report.common.filters.all') },
              { value: 'crit', label: t('report.common.summary.critical'), color: 'var(--status-critical)' },
              { value: 'warn', label: t('report.common.summary.warning'), color: 'var(--status-warning)' },
            ]}
          />
        }
        sort={sort}
        onSort={(v) => setSort(v as 'count' | 'source' | 'node')}
        sortOptions={[
          { value: 'count', label: t('report.auth_probe.sort.byFailures') },
          { value: 'source', label: t('report.auth_probe.sort.bySource') },
          { value: 'node', label: t('report.common.sort.byNode') },
        ]}
      />
      <div className="ts-anoms">
        {list.length ? (
          list.map((f) => <AuthRow key={f.id} finding={f} />)
        ) : (
          <EmptyList total={findings.length} />
        )}
      </div>
    </>
  );
}
