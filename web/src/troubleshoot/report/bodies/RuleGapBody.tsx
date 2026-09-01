// SPDX-License-Identifier: AGPL-3.0-only
// Rule Gap report body (passive monitoring, ADR-024).
//
// Entity: an event **signature** (a trap OID or a syslog app-name) — not a node. This is the one
// report that is a work queue rather than a diagnosis: each row is "you received N of these and no
// rule matched", and the action is to go write a rule. That is why it uses a virtualized DataTable
// (signatures are long strings an operator scans, searches and copies) instead of a card grid, and
// why every row links to the event-rules screen.
//
// ⚠ `detail.kind` is the SOURCE kind (syslog/trap/webhook) and collides by name with
// `finding.kind`, which is always the literal `'rule_gap'`. Read the source kind from `detail`.
// `node_name` may be the literal "fleet" with a null `node_id` for cross-node signatures.
//
// ADR-053 Inc.7 moved the narrowing out of the toolbar and under the headers: the search box became
// the Signature column's filter, the source select became a multi-select over the passive-event
// vocabulary, and the two columns that had no control at all (Events, Scope) gained one. Sort stays
// in the action row — this ADR moves filtering, not ordering (決定 L).

import { useMemo, useState } from 'react';
import { Link } from 'react-router-dom';
import { useTranslation } from 'react-i18next';
import { Card } from '../../../components/ui/Card';
import { Badge } from '../../../components/ui/Badge';
import { ClearFilters } from '../../../components/ui/ClearFilters';
import { DataTable, type Column } from '../../../components/ui/DataTable';
import { EntityName } from '../../../components/ui/EntityName';
import { FilterButton, MobileFilterSheet } from '../../../components/ui/MobileFilterSheet';
import { ResultCount, TableSpacer, TableToolbar } from '../../../components/ui/TableToolbar';
import { Donut, type DonutSegment } from '../../../dashboard/primitives/Donut';
import { RankedBars, type RankedRow } from '../../../dashboard/primitives/RankedBars';
import { Select } from '../../../components/ui/Field';
import { defaultFilters, isAnyFiltered, type FilterState } from '../../../lib/columnFilter';
import { facetCounts } from '../../../lib/filterCounts';
import { applyFilters } from '../../../lib/filterPredicate';
import { fmtCount, sortByDetail } from '../format';
import { gapCount, gapSignature, gapSource, ruleGapColumns, ruleGapFilters } from '../reportFilters';
import { sourceColor as srcColor } from '../findingTone';
import type { ReportBodyProps } from '../types';
import type { AnalysisFinding } from '../../../types/api';

// The accessors live in `reportFilters.ts` beside the specs that read them: a local copy is one
// edit away from a filter that disagrees with the cell above it.
const sigOf = gapSignature;
const srcOf = gapSource;
const countOf = gapCount;

export function RuleGapBody({ findings }: ReportBodyProps) {
  const { t } = useTranslation('troubleshoot');
  const filterCols = useMemo(() => ruleGapColumns(t), [t]);
  // Component state rather than the URL: a report body is mounted under `?job=…` and several bodies
  // share that route, so a URL-backed key would be claimed by whichever one rendered (the same
  // reason `AnalysisRuns` keeps its filters local).
  const [filters, setFilters] = useState<FilterState>(() => defaultFilters(filterCols));
  const [sheet, setSheet] = useState(false);
  const [sort, setSort] = useState<'count' | 'signature'>('count');
  const narrowed = isAnyFiltered(filterCols, filters);

  /** Unmatched volume by source kind — a genuine mix, so a donut is the right shape. */
  const mix = useMemo<DonutSegment[]>(() => {
    const acc = new Map<string, number>();
    for (const f of findings) acc.set(srcOf(f), (acc.get(srcOf(f)) ?? 0) + countOf(f));
    return [...acc.entries()]
      .sort((a, b) => b[1] - a[1])
      .map(([label, value]) => ({ label, value, color: srcColor(label) }));
  }, [findings]);

  const top = useMemo<RankedRow[]>(
    () =>
      sortByDetail(findings, 'count')
        .slice(0, 10)
        .map((f, i) => ({
          label: `${i + 1}. ${sigOf(f)}`,
          value: countOf(f),
          valueText: fmtCount(countOf(f)),
          color: srcColor(srcOf(f)),
        })),
    [findings],
  );

  const rows = useMemo(() => {
    const l = applyFilters(findings, filterCols, filters, Date.now());
    return sort === 'count'
      ? sortByDetail(l, 'count')
      : l.slice().sort((a, b) => sigOf(a).localeCompare(sigOf(b)));
  }, [findings, filterCols, filters, sort]);

  const counts = useMemo(
    () => ({ src: facetCounts(findings, filterCols, filters, 'src', Date.now()) }),
    [findings, filterCols, filters],
  );

  const columns = useMemo<Column<AnalysisFinding>[]>(() => {
    // ⚠️ `specs[c.key]` would be an untyped index. Naming each spec keeps a renamed column key a
    // compile error rather than a column that silently ships with no filter cell (Inc.5 hazard).
    const specs = ruleGapFilters(t);
    return [
      {
        key: 'src',
        header: t('report.rule_gap.cols.source'),
        width: '110px',
        filter: specs.src,
        render: (f) => <Badge>{srcOf(f)}</Badge>,
      },
      {
        key: 'sig',
        header: t('report.rule_gap.cols.signature'),
        width: 'minmax(220px, 2fr)',
        filter: specs.sig,
        // Full value in the title: OIDs are long and get truncated, but must stay copyable.
        render: (f) => (
          <span className="mono" title={sigOf(f)}>
            {sigOf(f)}
          </span>
        ),
      },
      {
        key: 'count',
        header: t('report.rule_gap.cols.events'),
        width: '110px',
        align: 'right',
        filter: specs.count,
        render: (f) => fmtCount(countOf(f)),
      },
      {
        key: 'scope',
        header: t('report.rule_gap.cols.scope'),
        width: 'minmax(140px, 1fr)',
        filter: specs.scope,
        render: (f) =>
          f.node_id ? (
            <EntityName name={f.node_name} id={f.node_id} />
          ) : (
            <span className="muted">{t('report.rule_gap.fleet')}</span>
          ),
      },
      {
        key: 'action',
        header: t('report.rule_gap.cols.action'),
        width: '120px',
        render: () => (
          <Link className="ts-linkbtn" to="/alerts/event-rules">
            {t('report.rule_gap.writeRule')}
          </Link>
        ),
      },
    ];
  }, [t]);

  return (
    <>
      {findings.length > 0 && (
        <div className="tsr-split">
          <Card title={t('report.rule_gap.mix')}>
            <Donut segments={mix} />
          </Card>
          <Card title={t('report.rule_gap.topSignatures')}>
            <RankedBars rows={top} />
          </Card>
        </div>
      )}
      {/* The action row: what acts on the list, never what narrows it. Sort stays because ADR-053
          moves filtering, not ordering — and a fourth track in `.dt-filters` would slide the filter
          cells out from under their headers (決定 L). */}
      <TableToolbar>
        <FilterButton
          columns={filterCols}
          filters={filters}
          onOpen={() => setSheet(true)}
        />
        <ClearFilters
          columns={filterCols}
          filters={filters}
          onClear={() => setFilters(defaultFilters(filterCols))}
        />
        <Select
          aria-label={t('report.common.sort.label')}
          value={sort}
          onChange={(e) => setSort(e.target.value as 'count' | 'signature')}
        >
          <option value="count">{t('report.rule_gap.sort.byEvents')}</option>
          <option value="signature">{t('report.rule_gap.sort.bySignature')}</option>
        </Select>
        <TableSpacer />
        <ResultCount shown={rows.length} total={findings.length} noun={t('report.rule_gap.noun')} />
      </TableToolbar>
      <DataTable
        rows={rows}
        columns={columns}
        filters={filters}
        onFiltersChange={setFilters}
        filterCounts={counts}
        rowKey={(f) => f.id}
        empty={
          narrowed ? t('report.common.list.emptyFiltered') : t('report.common.list.emptyAll')
        }
        renderCard={(f) => (
          <div className="tsr-card">
            <div className="tsr-card-head">
              <Badge>{srcOf(f)}</Badge>
              <span className="mono">{fmtCount(countOf(f))}</span>
            </div>
            <div className="mono tsr-card-sig">{sigOf(f)}</div>
            <Link className="ts-linkbtn" to="/alerts/event-rules">
              {t('report.rule_gap.writeRule')}
            </Link>
          </div>
        )}
        cardEstimatePx={104}
      />
      {sheet && (
        <MobileFilterSheet
          columns={filterCols}
          filters={filters}
          onChange={setFilters}
          counts={counts}
          labels={{
            src: t('report.rule_gap.cols.source'),
            sig: t('report.rule_gap.cols.signature'),
            count: t('report.rule_gap.cols.events'),
            scope: t('report.rule_gap.cols.scope'),
          }}
          onClose={() => setSheet(false)}
        />
      )}
    </>
  );
}
