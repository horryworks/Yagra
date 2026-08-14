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
//
// **This is the one of the fifteen report bodies whose chips were a plain severity filter, so it is
// the one Inc.7 converted** (決定 J). The other twelve select a tool-specific lens — `soon/mid/far`,
// `chronic/intermittent`, `inverse` — which is not a row attribute and must not be folded into a
// generic filter. Being a card list with no header row, this gets a `FilterBar` rather than a filter
// row (決定 E/K), and `ReportToolbar` keeps only the sort control, so the row count is unchanged.

import { useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Card } from '../../../components/ui/Card';
import { RankedBars, type RankedRow } from '../../../dashboard/primitives/RankedBars';
import { ClearFilters } from '../../../components/ui/ClearFilters';
import { FilterBar } from '../../../components/ui/FilterBar';
import { MobileFilterButton, MobileFilterSheet } from '../../../components/ui/MobileFilterSheet';
import { ResultCount, TableSpacer, TableToolbar } from '../../../components/ui/TableToolbar';
import { defaultFilters, isAnyFiltered, type FilterState } from '../../../lib/columnFilter';
import { facetCounts } from '../../../lib/filterCounts';
import { applyFilters } from '../../../lib/filterPredicate';
import { EmptyList, FindingRow, MonoLine, NodeRef, ReportToolbar, RightRail } from '../kit';
import { fmtCount, sevOf, sortByDetail, sortCommon } from '../format';
import {
  authProbeColumns,
  authProbeFilterLabels,
  probeCount,
  probeSource,
} from '../reportFilters';
import type { ReportBodyProps } from '../types';
import type { AnalysisFinding } from '../../../types/api';

// Shared with the specs that filter on them (`reportFilters.ts`) — never a second copy.
const srcOf = probeSource;
const countOf = probeCount;

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
  const filterCols = useMemo(() => authProbeColumns(t), [t]);
  const labels = useMemo(() => authProbeFilterLabels(t), [t]);
  // Component state, not the URL — several report bodies share the `?job=…` route.
  const [filters, setFilters] = useState<FilterState>(() => defaultFilters(filterCols));
  const [sheet, setSheet] = useState(false);
  const [sort, setSort] = useState<'count' | 'source' | 'node'>('count');
  const narrowed = isAnyFiltered(filterCols, filters);

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
    const shown = applyFilters(findings, filterCols, filters, Date.now());
    if (sort === 'count') return sortByDetail(shown, 'count');
    if (sort === 'source')
      return shown.slice().sort((a, b) => (srcOf(a) ?? '').localeCompare(srcOf(b) ?? ''));
    return sortCommon(shown, 'node');
  }, [findings, filterCols, filters, sort]);

  const counts = useMemo(
    () => ({ severity: facetCounts(findings, filterCols, filters, 'severity', Date.now()) }),
    [findings, filterCols, filters],
  );

  return (
    <>
      {top.length > 0 && (
        <Card title={t('report.auth_probe.worstSources')}>
          <RankedBars rows={top} />
        </Card>
      )}
      {/* ⚠️ The action row is gated on `findings`, never on `list`: filtering to zero would
          otherwise take the controls that undo the filter away with the rows. */}
      <TableToolbar>
        <MobileFilterButton
          columns={filterCols}
          filters={filters}
          onOpen={() => setSheet(true)}
        />
        <ClearFilters
          columns={filterCols}
          filters={filters}
          onClear={() => setFilters(defaultFilters(filterCols))}
        />
        <TableSpacer />
        <ResultCount
          shown={list.length}
          total={findings.length}
          noun={t('report.auth_probe.noun')}
        />
      </TableToolbar>
      {/* A run of `FindingRow`s has no header row to hang a filter row under, so the controls carry
          their own names (決定 E). */}
      <FilterBar
        columns={filterCols}
        labels={labels}
        filters={filters}
        onChange={setFilters}
        counts={counts}
      />
      <ReportToolbar
        id="tsr-auth-probe"
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
          // `narrowed`, not `findings.length`: with nothing filtered and no findings the honest
          // message is "the analysis found nothing", not "your filter hid everything".
          <EmptyList total={narrowed ? findings.length : 0} />
        )}
      </div>
      {sheet && (
        <MobileFilterSheet
          columns={filterCols}
          labels={labels}
          filters={filters}
          onChange={setFilters}
          counts={counts}
          onClose={() => setSheet(false)}
        />
      )}
    </>
  );
}
