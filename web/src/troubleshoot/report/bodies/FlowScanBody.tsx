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

import { useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Badge } from '../../../components/ui/Badge';
import { Card } from '../../../components/ui/Card';
import { DataTable, type Column } from '../../../components/ui/DataTable';
import { EntityName } from '../../../components/ui/EntityName';
import { Select } from '../../../components/ui/Field';
import {
  ResultCount,
  SearchInput,
  TableSpacer,
  TableToolbar,
} from '../../../components/ui/TableToolbar';
import { formatSi } from '../../../lib/format';
import { ScanPlot, type ScanPoint } from '../ScanPlot';
import { detailNum, detailStr, fmtCount, scanPattern, sevOf } from '../format';
import type { ReportBodyProps } from '../types';
import type { AnalysisFinding } from '../../../types/api';

const srcOf = (f: AnalysisFinding) => detailStr(f, 'src') ?? '—';
const dstOf = (f: AnalysisFinding) => detailNum(f, 'distinct_dst') ?? 0;
const portsOf = (f: AnalysisFinding) => detailNum(f, 'distinct_ports') ?? 0;
const flowsOf = (f: AnalysisFinding) => detailNum(f, 'flows') ?? 0;
const patternOf = (f: AnalysisFinding) => scanPattern(dstOf(f), portsOf(f));

export function FlowScanBody({ findings }: ReportBodyProps) {
  const { t } = useTranslation('troubleshoot');
  const [query, setQuery] = useState('');
  const [pattern, setPattern] = useState<'all' | 'horizontal' | 'vertical'>('all');
  const [sort, setSort] = useState<'dst' | 'ports' | 'flows' | 'score'>('dst');

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
    const q = query.trim().toLowerCase();
    let l = findings.filter(
      (f) =>
        (pattern === 'all' || patternOf(f) === pattern) &&
        (!q || srcOf(f).toLowerCase().includes(q)),
    );
    if (sort === 'dst') l = l.slice().sort((a, b) => dstOf(b) - dstOf(a));
    else if (sort === 'ports') l = l.slice().sort((a, b) => portsOf(b) - portsOf(a));
    else if (sort === 'flows') l = l.slice().sort((a, b) => flowsOf(b) - flowsOf(a));
    else l = l.slice().sort((a, b) => b.score - a.score);
    return l;
  }, [findings, query, pattern, sort]);

  const columns = useMemo<Column<AnalysisFinding>[]>(
    () => [
      {
        key: 'src',
        header: t('report.flow_scan.cols.source'),
        width: 'minmax(150px, 1.2fr)',
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
        render: (f) => <EntityName name={f.node_name} id={f.node_id ?? undefined} />,
      },
      {
        key: 'dst',
        header: t('report.flow_scan.cols.dst'),
        width: '110px',
        align: 'right',
        render: (f) => fmtCount(dstOf(f)),
      },
      {
        key: 'ports',
        header: t('report.flow_scan.cols.ports'),
        width: '110px',
        align: 'right',
        render: (f) => fmtCount(portsOf(f)),
      },
      {
        key: 'flows',
        header: t('report.flow_scan.cols.flows'),
        width: '90px',
        align: 'right',
        render: (f) => formatSi(flowsOf(f)),
      },
      {
        key: 'pattern',
        header: t('report.flow_scan.cols.pattern'),
        width: '120px',
        render: (f) => <Badge>{t(`report.flow_scan.pattern.${patternOf(f)}`)}</Badge>,
      },
      {
        key: 'score',
        header: t('report.common.score'),
        width: '70px',
        align: 'right',
        render: (f) => String(Math.round(f.score)),
      },
    ],
    [t],
  );

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
      <TableToolbar>
        <SearchInput
          value={query}
          onChange={setQuery}
          placeholder={t('report.flow_scan.searchPlaceholder')}
          ariaLabel={t('report.flow_scan.searchPlaceholder')}
        />
        <Select
          aria-label={t('report.flow_scan.cols.pattern')}
          value={pattern}
          onChange={(e) => setPattern(e.target.value as 'all' | 'horizontal' | 'vertical')}
        >
          <option value="all">{t('report.common.filters.all')}</option>
          <option value="horizontal">{t('report.flow_scan.pattern.horizontal')}</option>
          <option value="vertical">{t('report.flow_scan.pattern.vertical')}</option>
        </Select>
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
        rowKey={(f) => f.id}
        empty={
          findings.length
            ? t('report.common.list.emptyFiltered')
            : t('report.common.list.emptyAll')
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
    </>
  );
}
