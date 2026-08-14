// SPDX-License-Identifier: AGPL-3.0-only
// Scan Detection report body (flow monitoring, ADR-031).
//
// Entity: a **source address**. This is the clearest case for per-tool reports in the whole set —
// observed live, a single job returned 12 findings that were ALL on one node, because the varying
// entity is `detail.src`, not the node. A generic node-oriented report would show "1 node" and hide
// the twelve scanners.
//
// Two things carry the report:
//  - a DataTable, because triage is scan-and-search over addresses and counts;
//  - a ScanPlot scatter, because "4496 hosts × 20 ports" (sweep) and "20 hosts × 85 ports" (probe)
//    are different attacks that no single ranked axis can separate.
//
// The horizontal/vertical label is recomputed client-side by `scanPattern` — identical to the Rust
// comparison including the tie — because the backend ships it inside the English `duration` string.
//
// ADR-053 Inc.7 moved the narrowing under the headers. Every one of the seven columns has a filter
// now, which is the shape this table wanted: five of them are numbers, and "sources that touched
// more than 500 destinations" was unsayable from a toolbar. Sort stays in the action row (決定 L).

import { useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Badge } from '../../../components/ui/Badge';
import { Card } from '../../../components/ui/Card';
import { ClearFilters } from '../../../components/ui/ClearFilters';
import { DataTable, type Column } from '../../../components/ui/DataTable';
import { EntityName } from '../../../components/ui/EntityName';
import { Select } from '../../../components/ui/Field';
import { FilterButton, MobileFilterSheet } from '../../../components/ui/MobileFilterSheet';
import { ResultCount, TableSpacer, TableToolbar } from '../../../components/ui/TableToolbar';
import { defaultFilters, isAnyFiltered, type FilterState } from '../../../lib/columnFilter';
import { facetCounts } from '../../../lib/filterCounts';
import { applyFilters } from '../../../lib/filterPredicate';
import { formatSi } from '../../../lib/format';
import { ScanPlot, type ScanPoint } from '../ScanPlot';
import { fmtCount, sevOf } from '../format';
import {
  flowScanColumns,
  flowScanFilters,
  scanDst,
  scanFlows,
  scanPorts,
  scanShape,
  scanSource,
} from '../reportFilters';
import type { ReportBodyProps } from '../types';
import type { AnalysisFinding } from '../../../types/api';

// One definition per value, shared with the specs that filter on it (`reportFilters.ts`).
const srcOf = scanSource;
const dstOf = scanDst;
const portsOf = scanPorts;
const flowsOf = scanFlows;
const patternOf = scanShape;

export function FlowScanBody({ findings }: ReportBodyProps) {
  const { t } = useTranslation('troubleshoot');
  const filterCols = useMemo(() => flowScanColumns(t), [t]);
  // Component state, not the URL — several report bodies share the `?job=…` route.
  const [filters, setFilters] = useState<FilterState>(() => defaultFilters(filterCols));
  const [sheet, setSheet] = useState(false);
  const [sort, setSort] = useState<'dst' | 'ports' | 'flows' | 'score'>('dst');
  const narrowed = isAnyFiltered(filterCols, filters);

  const plotPoints = useMemo<ScanPoint[]>(
    () =>
      findings.map((f) => ({
        src: srcOf(f),
        distinctDst: dstOf(f),
        distinctPorts: portsOf(f),
        severity: sevOf(f),
      })),
    [findings],
  );

  const rows = useMemo(() => {
    const l = applyFilters(findings, filterCols, filters, Date.now()).slice();
    if (sort === 'dst') return l.sort((a, b) => dstOf(b) - dstOf(a));
    if (sort === 'ports') return l.sort((a, b) => portsOf(b) - portsOf(a));
    if (sort === 'flows') return l.sort((a, b) => flowsOf(b) - flowsOf(a));
    return l.sort((a, b) => b.score - a.score);
  }, [findings, filterCols, filters, sort]);

  const counts = useMemo(
    () => ({ pattern: facetCounts(findings, filterCols, filters, 'pattern', Date.now()) }),
    [findings, filterCols, filters],
  );

  const columns = useMemo<Column<AnalysisFinding>[]>(() => {
    // ⚠️ Named, not indexed: a renamed column key must be a compile error, not a column that ships
    // with no filter cell under it.
    const specs = flowScanFilters(t);
    return [
      {
        key: 'src',
        header: t('report.flow_scan.cols.source'),
        width: 'minmax(150px, 1.2fr)',
        filter: specs.src,
        render: (f) => (
          <span className="mono" title={srcOf(f)}>
            {srcOf(f)}
          </span>
        ),
      },
      {
        key: 'node',
        header: t('report.flow_scan.cols.node'),
        width: 'minmax(130px, 1fr)',
        filter: specs.node,
        render: (f) => <EntityName name={f.node_name} id={f.node_id ?? undefined} />,
      },
      {
        key: 'dst',
        header: t('report.flow_scan.cols.dst'),
        width: '110px',
        align: 'right',
        filter: specs.dst,
        render: (f) => fmtCount(dstOf(f)),
      },
      {
        key: 'ports',
        header: t('report.flow_scan.cols.ports'),
        width: '110px',
        align: 'right',
        filter: specs.ports,
        render: (f) => fmtCount(portsOf(f)),
      },
      {
        key: 'flows',
        header: t('report.flow_scan.cols.flows'),
        width: '90px',
        align: 'right',
        filter: specs.flows,
        render: (f) => formatSi(flowsOf(f)),
      },
      {
        key: 'pattern',
        header: t('report.flow_scan.cols.pattern'),
        width: '120px',
        filter: specs.pattern,
        render: (f) => <Badge>{t(`report.flow_scan.pattern.${patternOf(f)}`)}</Badge>,
      },
      {
        key: 'score',
        header: t('report.common.score'),
        width: '70px',
        align: 'right',
        filter: specs.score,
        render: (f) => String(Math.round(f.score)),
      },
    ];
  }, [t]);

  return (
    <>
      {findings.length > 0 && (
        <Card title={t('report.flow_scan.shape')}>
          <ScanPlot
            points={plotPoints}
            labels={{
              x: t('report.flow_scan.axis.dst'),
              y: t('report.flow_scan.axis.ports'),
              horizontal: t('report.flow_scan.pattern.horizontal'),
              vertical: t('report.flow_scan.pattern.vertical'),
            }}
          />
        </Card>
      )}
      {/* The action row. Everything that narrows the table now lives under its own header. */}
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
          onChange={(e) => setSort(e.target.value as 'dst' | 'ports' | 'flows' | 'score')}
        >
          <option value="dst">{t('report.flow_scan.sort.byDst')}</option>
          <option value="ports">{t('report.flow_scan.sort.byPorts')}</option>
          <option value="flows">{t('report.flow_scan.sort.byFlows')}</option>
          <option value="score">{t('report.common.sort.byScore')}</option>
        </Select>
        <TableSpacer />
        <ResultCount shown={rows.length} total={findings.length} noun={t('report.flow_scan.noun')} />
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
              <span className="mono">{srcOf(f)}</span>
              <Badge>{t(`report.flow_scan.pattern.${patternOf(f)}`)}</Badge>
            </div>
            <div className="tsr-card-sig">
              {t('report.flow_scan.cardCounts', { dst: dstOf(f), ports: portsOf(f) })}
            </div>
            <EntityName name={f.node_name} id={f.node_id ?? undefined} />
          </div>
        )}
        cardEstimatePx={108}
      />
      {sheet && (
        <MobileFilterSheet
          columns={filterCols}
          filters={filters}
          onChange={setFilters}
          counts={counts}
          labels={{
            src: t('report.flow_scan.cols.source'),
            node: t('report.flow_scan.cols.node'),
            dst: t('report.flow_scan.cols.dst'),
            ports: t('report.flow_scan.cols.ports'),
            flows: t('report.flow_scan.cols.flows'),
            pattern: t('report.flow_scan.cols.pattern'),
            score: t('report.common.score'),
          }}
          onClose={() => setSheet(false)}
        />
      )}
    </>
  );
}
