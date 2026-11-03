// SPDX-License-Identifier: AGPL-3.0-only
// Alerts ▸ Alert rules ▸ Check a node, the event half (ADR-232 decision 8): the event rules that
// reach one node — on every node, on one of its folders, or on the node itself. The server answers
// which ones, from the same folder chain the matcher uses, so this list and the matcher cannot
// disagree. Every rule here applies at once; there is no "in force" to pick, unlike the thresholds.

import { useTranslation } from 'react-i18next';
import { api } from '../../services/api';
import type { EventRule } from '../../types/api';
import { useLoad } from '../../lib/useLoad';
import { SEVERITY_TONE, severityLabel } from '../../lib/format';
import { eventTargetId, eventTargetOf } from '../../pages/eventRuleView';
import { LoadGate } from '../ui/LoadGate';
import { DataTable, type Column } from '../ui/DataTable';
import { EmptyState } from '../ui/EmptyState';
import { Badge } from '../ui/Badge';
import { ClippedText } from '../ui/ClippedText';
import { useEntityNames } from '../ui/entityNames';
import './NodeRulesView.css';

export function NodeEventRulesView({ nodeId }: { nodeId: string }) {
  const { t } = useTranslation('alertsConfig');
  const { t: tn } = useTranslation('nodes');
  const { groupName } = useEntityNames();
  const load = useLoad(() => api.listEventRules(nodeId), [nodeId], {
    initial: [] as EventRule[],
    onConfigChange: true,
  });

  /** Where the rule was aimed: every node, a folder by name, or this node. */
  const decidedAt = (r: EventRule) => {
    const id = eventTargetId(r);
    switch (eventTargetOf(r)) {
      case 'global':
        return t('eventRules.tabs.global');
      case 'folder':
        return `${t('eventRules.tabs.folder')}: ${id ? groupName(id) : ''}`;
      case 'node':
        return tn('rules.thisNode');
    }
  };

  const columns: Column<EventRule>[] = [
    {
      key: 'name',
      header: t('eventRules.cols.name'),
      width: '1.2fr',
      render: (r) => <ClippedText text={r.name} />,
    },
    {
      key: 'target',
      header: t('eventRules.cols.target'),
      width: '1.2fr',
      render: (r) => <ClippedText text={decidedAt(r)} />,
    },
    {
      key: 'severity',
      header: t('eventRules.cols.severity'),
      width: '130px',
      render: (r) => <Badge tone={SEVERITY_TONE[r.severity]}>{severityLabel(r.severity)}</Badge>,
    },
    {
      key: 'status',
      header: t('eventRules.cols.status'),
      width: '110px',
      render: (r) => (
        <Badge tone={r.enabled ? 'up' : 'neutral'}>
          {r.enabled ? t('status.enabled') : t('status.disabled')}
        </Badge>
      ),
    },
  ];

  return (
    <section className="nd-rules-events">
      <h3 className="nd-rules-heading">{t('eventRules.check.title')}</h3>
      <LoadGate load={load} permission="manage_config">
        <div className="nd-rules-table nd-rules-table-short">
          <DataTable
            tableId="alerts.nodeEventRules"
            rows={load.data}
            columns={columns}
            rowKey={(r) => r.id}
            loading={load.loading}
            empty={<EmptyState text={t('eventRules.check.empty')} />}
          />
        </div>
      </LoadGate>
    </section>
  );
}
