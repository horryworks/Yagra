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

import { useMemo, useState } from 'react';
import { Link } from 'react-router-dom';
import { useTranslation } from 'react-i18next';
import { Card } from '../../../components/ui/Card';
import { Badge } from '../../../components/ui/Badge';
import { DataTable, type Column } from '../../../components/ui/DataTable';
import { EntityName } from '../../../components/ui/EntityName';
import {
  ResultCount,
  SearchInput,
  TableSpacer,
  TableToolbar,
} from '../../../components/ui/TableToolbar';
import { Donut, type DonutSegment } from '../../../dashboard/primitives/Donut';
import { RankedBars, type RankedRow } from '../../../dashboard/primitives/RankedBars';
import { Select } from '../../../components/ui/Field';
import { detailNum, detailStr, fmtCount, sortByDetail } from '../format';
import type { ReportBodyProps } from '../types';
import type { AnalysisFinding } from '../../../types/api';

const sigOf = (f: AnalysisFinding) => detailStr(f, 'signature') ?? f.metric;
/** The event SOURCE kind, from detail — never `finding.kind` (always 'rule_gap'). */
const srcOf = (f: AnalysisFinding) => detailStr(f, 'kind') ?? 'unknown';
const countOf = (f: AnalysisFinding) => detailNum(f, 'count') ?? 0;

/** Categorical colours for the source kinds (series palette — these are not statuses). */
const SRC_COLORS: Record<string, string> = {
  trap: 'var(--series-5)',
  syslog: 'var(--series-1)',
  webhook: 'var(--series-3)',
};
const srcColor = (k: string) => SRC_COLORS[k] ?? 'var(--text-tertiary)';

export function RuleGapBody({ findings }: ReportBodyProps) {
  const { t } = useTranslation('troubleshoot');
  const [query, setQuery] = useState('');
  const [src, setSrc] = useState<'all' | string>('all');
  const [sort, setSort] = useState<'count' | 'signature'>('count');

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

  const kinds = useMemo(() => [...new Set(findings.map(srcOf))].sort(), [findings]);

  const rows = useMemo(() => {
    const q = query.trim().toLowerCase();
    let l = findings.filter(
      (f) =>
        (src === 'all' || srcOf(f) === src) && (!q || sigOf(f).toLowerCase().includes(q)),
    );
    l = sort === 'count' ? sortByDetail(l, 'count') : l.slice().sort((a, b) => sigOf(a).localeCompare(sigOf(b)));
    return l;
  }, [findings, query, src, sort]);

  const columns = useMemo<Column<AnalysisFinding>[]>(
    () => [
      {
        key: 'src',
        header: t('report.rule_gap.cols.source'),
        width: '110px',
        render: (f) => <Badge>{srcOf(f)}</Badge>,
      },
      {
        key: 'sig',
        header: t('report.rule_gap.cols.signature'),
        width: 'minmax(220px, 2fr)',
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
        render: (f) => fmtCount(countOf(f)),
      },
      {
        key: 'scope',
        header: t('report.rule_gap.cols.scope'),
        width: 'minmax(140px, 1fr)',
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
    ],
    [t],
  );

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
      <TableToolbar>
        <SearchInput
          value={query}
          onChange={setQuery}
          placeholder={t('report.rule_gap.searchPlaceholder')}
          ariaLabel={t('report.rule_gap.searchPlaceholder')}
        />
        <Select
          aria-label={t('report.rule_gap.cols.source')}
          value={src}
          onChange={(e) => setSrc(e.target.value)}
        >
          <option value="all">{t('report.common.filters.all')}</option>
          {kinds.map((k) => (
            <option key={k} value={k}>
              {k}
            </option>
          ))}
        </Select>
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
        rowKey={(f) => f.id}
        empty={
          findings.length
            ? t('report.common.list.emptyFiltered')
            : t('report.common.list.emptyAll')
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
    </>
  );
}
