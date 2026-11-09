// SPDX-License-Identifier: AGPL-3.0-only
// Troubleshoot > Investigate > Running out (ADR-251): asked about one node, or about a folder and
// everything beneath it. The verdicts are the server's (`api/investigate.rs`); `investigate.ts`
// lays them out.

import { useTranslation } from 'react-i18next';
import { Link, useSearchParams } from 'react-router-dom';
import { api } from '../../services/api';
import type { FolderInvestigateReport, InvestigateReport } from '../../types/api';
import { useLoad } from '../../lib/useLoad';
import { nodeHref } from '../../lib/entityHref';
import { Page, Inline } from '../../components/ui/Page';
import { PageHeader } from '../../components/ui/PageHeader';
import { LoadGate } from '../../components/ui/LoadGate';
import { EmptyState } from '../../components/ui/EmptyState';
import { Badge } from '../../components/ui/Badge';
import { Facts } from '../../components/ui/Facts';
import { Callout, SectionHead } from '../../components/ui/Section';
import { DataTable, type Column } from '../../components/ui/DataTable';
import { DrillLink } from '../../components/ui/DrillLink';
import { ClippedText } from '../../components/ui/ClippedText';
import { EntityName } from '../../components/ui/EntityName';
import { useEntityNames } from '../../components/ui/entityNames';
import { TargetPicker } from './TargetPicker';
import {
  INVESTIGATE_PATH,
  QUESTION_CARDS,
  VERDICT_TONE,
  folderCounts,
  folderRows,
  formatDays,
  formatPct,
  investigateRows,
  questionHref,
  seriesHref,
  seriesName,
  targetFromParams,
  targetParam,
  type FolderRow,
  type InvestigateRow,
} from './investigate';

const QUESTION = 'running_out' as const;

/** The columns both answers share, after their own first ones. */
function useSeriesColumns<R extends { evidence: InvestigateRow['evidence'] }>(
  nodeOf: (r: R) => string,
): Column<R>[] {
  const { t } = useTranslation('troubleshoot');
  return [
    {
      key: 'series',
      header: t('investigate.cols.series'),
      width: '1.4fr',
      render: (r) =>
        r.evidence ? (
          <DrillLink to={seriesHref(nodeOf(r), r.evidence)}>
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
}

function NodeAnswer({ nodeId }: { nodeId: string }) {
  const { t } = useTranslation('troubleshoot');
  const load = useLoad<InvestigateReport | null>(
    () => api.investigateRunningOut(nodeId),
    [nodeId],
    { initial: null },
  );
  const report = load.data;
  const rows = report ? investigateRows(report) : [];
  const series = useSeriesColumns<InvestigateRow>(() => nodeId);
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
    ...series,
  ];
  return (
    <LoadGate load={load}>
      <SectionHead
        title={t(`investigate.question.${QUESTION}`)}
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
  );
}

function FolderAnswer({ groupId }: { groupId: string }) {
  const { t } = useTranslation('troubleshoot');
  const { nodeName } = useEntityNames();
  const load = useLoad<FolderInvestigateReport | null>(
    () => api.investigateFolderRunningOut(groupId),
    [groupId],
    { initial: null },
  );
  const report = load.data;
  const rows = report ? folderRows(report) : [];
  const counts = report ? folderCounts(report) : null;
  const series = useSeriesColumns<FolderRow>((r) => r.nodeId);
  const columns: Column<FolderRow>[] = [
    {
      key: 'node',
      header: t('investigate.cols.node'),
      width: '1.2fr',
      render: (r) => (
        <Link to={questionHref(QUESTION, { kind: 'node', id: r.nodeId })}>
          <EntityName name={nodeName(r.nodeId)} id={r.nodeId} />
        </Link>
      ),
    },
    {
      key: 'hypothesis',
      header: t('investigate.cols.hypothesis'),
      width: '140px',
      render: (r) => t(`investigate.hypothesis.${r.hypothesis}`),
    },
    ...series,
  ];
  return (
    <LoadGate load={load}>
      {report && counts && (
        <Facts
          layout="inline"
          items={[
            { key: 'total', label: t('investigate.folder.total'), value: report.nodes_total },
            { key: 'data', label: t('investigate.folder.withData'), value: report.nodes_with_data },
            { key: 'cand', label: t('investigate.folder.candidates'), value: report.candidates },
            { key: 'judged', label: t('investigate.folder.judged'), value: report.judged },
            { key: 'hit', label: t('investigate.folder.hits'), value: counts.hit },
            { key: 'unknown', label: t('investigate.verdict.unknown'), value: counts.unknown },
          ]}
        />
      )}
      {report && report.not_judged > 0 && (
        <Callout tone="warning" status>
          {t('investigate.folder.notJudged', { count: report.not_judged })}
        </Callout>
      )}
      {report && !report.links_screened && (
        <Callout tone="info" status>
          {t('investigate.folder.noLinks')}
        </Callout>
      )}
      <DataTable
        tableId="troubleshoot.investigateFolder"
        rows={rows}
        columns={columns}
        rowKey={(r) => r.key}
        loading={load.loading}
        empty={<EmptyState text={t('investigate.folder.empty')} />}
      />
    </LoadGate>
  );
}

export function RunningOutPage() {
  const { t } = useTranslation(['troubleshoot', 'nav']);
  const [params, setParams] = useSearchParams();
  const target = targetFromParams(params);
  const { nodeName, groupName } = useEntityNames();
  const title = t(`investigate.question.${QUESTION}`);

  return (
    <Page layout={target ? 'list' : 'sections'}>
      <PageHeader
        title={title}
        trail={[
          { label: t('nav:sections.troubleshoot'), to: '/troubleshoot' },
          { label: t('nav:troubleshoot.investigate'), to: INVESTIGATE_PATH },
          { label: title },
        ]}
      />
      {target === null ? (
        <TargetPicker
          kinds={QUESTION_CARDS[QUESTION].targets}
          onPick={(p) => setParams({ [targetParam(p.kind)]: p.id })}
        />
      ) : (
        <>
          <Inline gap={8}>
            <Badge tone="info">
              {t('investigate.about', {
                name: target.kind === 'node' ? nodeName(target.id) : groupName(target.id),
              })}
            </Badge>
            {target.kind === 'node' && (
              <Link to={nodeHref(target.id)}>{t('investigate.openNode')}</Link>
            )}
            <Link to={questionHref(QUESTION)}>{t('investigate.changeTarget')}</Link>
          </Inline>
          {target.kind === 'node' ? (
            <NodeAnswer nodeId={target.id} />
          ) : (
            <FolderAnswer groupId={target.id} />
          )}
        </>
      )}
    </Page>
  );
}
