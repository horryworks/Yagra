// SPDX-License-Identifier: AGPL-3.0-only
// Troubleshoot > Investigate (ADR-251): pick a node, read whether anything on it is running out.
// The verdicts are the server's (`api/investigate.rs`); `investigate.ts` lays them out.

import { useTranslation } from 'react-i18next';
import { useSearchParams } from 'react-router-dom';
import { api } from '../../services/api';
import type { InvestigateReport } from '../../types/api';
import { useLoad } from '../../lib/useLoad';
import { Page, Inline } from '../../components/ui/Page';
import { PageHeader } from '../../components/ui/PageHeader';
import { LoadGate } from '../../components/ui/LoadGate';
import { EmptyState } from '../../components/ui/EmptyState';
import { Badge } from '../../components/ui/Badge';
import { Callout, SectionHead } from '../../components/ui/Section';
import { DataTable, type Column } from '../../components/ui/DataTable';
import { DrillLink } from '../../components/ui/DrillLink';
import { ClippedText } from '../../components/ui/ClippedText';
import { NodePicker } from '../../components/NodePicker/NodePicker';
import { useEntityNames } from '../../components/ui/entityNames';
import {
  NODE_PARAM,
  VERDICT_TONE,
  formatDays,
  formatPct,
  investigateRows,
  seriesHref,
  seriesName,
  type InvestigateRow,
} from './investigate';

export function InvestigatePage() {
  const { t } = useTranslation(['troubleshoot', 'nav']);
  const [params, setParams] = useSearchParams();
  const nodeId = params.get(NODE_PARAM);
  const { nodeName } = useEntityNames();
  const load = useLoad<InvestigateReport | null>(
    () => (nodeId ? api.investigateRunningOut(nodeId) : Promise.resolve(null)),
    [nodeId],
    { initial: null, enabled: nodeId !== null },
  );
  const report = load.data;
  const rows = report ? investigateRows(report) : [];

  const columns: Column<InvestigateRow>[] = [
    {
      key: 'hypothesis',
      header: t('investigate.cols.hypothesis'),
      width: '140px',
      render: (r) => t(`investigate.hypothesis.${r.hypothesis}`),
    },
    {
      key: 'verdict',
      header: t('investigate.cols.verdict'),
      width: '130px',
      render: (r) => (
        <Badge tone={VERDICT_TONE[r.verdict]}>{t(`investigate.verdict.${r.verdict}`)}</Badge>
      ),
    },
    {
      key: 'series',
      header: t('investigate.cols.series'),
      width: '1.4fr',
      render: (r) =>
        r.evidence && nodeId ? (
          <DrillLink to={seriesHref(nodeId, r.evidence)}>
            <ClippedText text={seriesName(r.evidence)} />
          </DrillLink>
        ) : (
          t('investigate.noMetric')
        ),
    },
    {
      key: 'current',
      header: t('investigate.cols.current'),
      width: '90px',
      render: (r) => formatPct(r.evidence?.current),
    },
    {
      key: 'target',
      header: t('investigate.cols.target'),
      width: '90px',
      render: (r) => (r.evidence ? formatPct(r.evidence.target) : ''),
    },
    {
      key: 'days',
      header: t('investigate.cols.days'),
      width: '110px',
      render: (r) => formatDays(r.evidence?.days_to_target),
    },
    {
      key: 'reason',
      header: t('investigate.cols.reason'),
      width: '1fr',
      render: (r) => (r.evidence ? t(`investigate.reason.${r.evidence.reason}`) : ''),
    },
  ];

  return (
    <Page layout="list">
      <PageHeader
        title={t('nav:troubleshoot.investigate')}
        trail={[
          { label: t('nav:sections.troubleshoot'), to: '/troubleshoot' },
          { label: t('nav:troubleshoot.investigate') },
        ]}
      />
      <Inline gap={12}>
        <NodePicker
          value={nodeId}
          valueLabel={nodeId ? nodeName(nodeId) : undefined}
          placeholder={t('investigate.pickNode')}
          onChange={(n) => setParams(n ? { [NODE_PARAM]: n.id } : {})}
        />
      </Inline>
      {nodeId === null ? (
        <EmptyState text={t('investigate.empty')} />
      ) : (
        <LoadGate load={load}>
          <SectionHead
            title={t('investigate.question.running_out')}
            tools={
              report && (
                <Inline gap={6}>
                  {report.hypotheses.map((h) => (
                    <Badge key={h.hypothesis} tone={VERDICT_TONE[h.verdict]}>
                      {t(`investigate.hypothesis.${h.hypothesis}`)}:{' '}
                      {t(`investigate.verdict.${h.verdict}`)}
                    </Badge>
                  ))}
                </Inline>
              )
            }
          />
          {report && report.monitoring_gaps.length > 0 && (
            <Callout tone="warning">
              {t('investigate.gaps', { count: report.monitoring_gaps.length })}
            </Callout>
          )}
          <DataTable
            tableId="troubleshoot.investigate"
            rows={rows}
            columns={columns}
            rowKey={(r) => r.key}
            loading={load.loading}
            empty={<EmptyState text={t('investigate.empty')} />}
          />
        </LoadGate>
      )}
    </Page>
  );
}
