// SPDX-License-Identifier: AGPL-3.0-only
// Node detail ▸ Alert rules (ADR-229): the threshold rules on this node's node-wide metrics, the
// one in force for each, and where it was decided. Per-port rules stay on the Interfaces tab.
//
// The grouping and the "which rule does the button edit" answer live in `nodeRules.ts`, because
// Vitest never executes a `.tsx` (testing.md).

import { useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { api } from '../../services/api';
import { useCan } from '../../store';
import type { NodeDetail, StoredThreshold } from '../../types/api';
import { useLoad } from '../../lib/useLoad';
import { LoadGate } from '../ui/LoadGate';
import { DataTable, type Column } from '../ui/DataTable';
import { EmptyState } from '../ui/EmptyState';
import { ScreenLink } from '../ui/ScreenLink';
import { Badge } from '../ui/Badge';
import { Button } from '../ui/Button';
import { useEntityNames } from '../ui/entityNames';
import { RuleBounds } from '../ThresholdModal/RuleBounds';
import { ThresholdModal } from '../ThresholdModal/ThresholdModal';
import type { ThresholdForm } from '../../pages/thresholdRequest';
import { nodeOverrideForm, nodeRuleRows, ownRule, type NodeRuleRow } from './nodeRules';
import './RulesTab.css';

/** What the row's button opened: the node's own rule to edit, or a new one for this node alone. */
type Editing = { mode: 'edit'; rule: StoredThreshold } | { mode: 'add'; start: ThresholdForm };

export function RulesTab({ node }: { node: NodeDetail }) {
  const { t } = useTranslation('nodes');
  const { t: tc } = useTranslation('alertsConfig');
  const canConfig = useCan('manage_config');
  const { scopeName } = useEntityNames();
  const [openMetric, setOpenMetric] = useState<string | null>(null);
  const [editing, setEditing] = useState<Editing | null>(null);
  const load = useLoad(() => api.listNodeThresholds(node.id), [node.id], {
    initial: [],
    onConfigChange: true,
  });
  const rows = useMemo(() => nodeRuleRows(load.data), [load.data]);

  /** Where one rule was decided: its level and what it names, or "This node" for its own. */
  const decidedAt = (r: StoredThreshold) => {
    if (r.scope_level === 'node' && r.scope_ids.includes(node.id)) return t('rules.thisNode');
    const level = tc(`thresholds.scopeLevel.${r.scope_level}`);
    const names = r.scope_ids.map((id) => scopeName(r.scope_level, id)).join(', ');
    return names ? `${level}: ${names}` : level;
  };

  const columns: Column<NodeRuleRow>[] = [
    {
      key: 'metric',
      header: t('rules.colMetric'),
      width: '1.2fr',
      render: (row) => <span className="mono">{row.metric}</span>,
    },
    {
      key: 'bounds',
      header: t('rules.colBounds'),
      width: '1.4fr',
      render: (row) => (
        <span className="nd-rules-stack">
          {row.inForce.map((r) => (
            <RuleBounds key={r.id} rule={r} />
          ))}
        </span>
      ),
    },
    {
      key: 'decided',
      header: t('rules.colDecided'),
      width: '1.6fr',
      render: (row) => (
        <span className="nd-rules-stack">
          {row.inForce.map((r) => (
            <span key={r.id} title={decidedAt(r)}>
              {decidedAt(r)}
              {r.row_match ? <span className="mono"> · {r.row_match}</span> : null}
            </span>
          ))}
        </span>
      ),
    },
    {
      key: 'overridden',
      header: t('rules.colOverridden'),
      width: '110px',
      render: (row) =>
        row.overridden.length > 0 ? (
          <Badge tone="neutral">{t('rules.overridden', { count: row.overridden.length })}</Badge>
        ) : null,
    },
    {
      key: 'actions',
      header: '',
      width: '150px',
      align: 'right',
      render: (row) => {
        if (!canConfig) return null;
        const own = ownRule(row, node.id);
        return (
          <Button
            type="button"
            variant="outline"
            onClick={(e) => {
              e.stopPropagation();
              setEditing(
                own
                  ? { mode: 'edit', rule: own }
                  : { mode: 'add', start: nodeOverrideForm(row, node.id) },
              );
            }}
          >
            {own ? t('rules.edit') : t('rules.override')}
          </Button>
        );
      },
    },
  ];

  return (
    <LoadGate load={load} permission="manage_config">
      <div className="nd-rules-table">
        <DataTable
          tableId="node.alertRules"
          autoRowHeight
          rows={rows}
          columns={columns}
          rowKey={(row) => row.metric}
          loading={load.loading}
          empty={
            <EmptyState
              text={t('rules.empty')}
              action={<ScreenLink to="/alerts/rules" />}
            />
          }
          onRowClick={(row) =>
            row.overridden.length > 0 &&
            setOpenMetric((cur) => (cur === row.metric ? null : row.metric))
          }
          expandedKey={openMetric}
          expanded={(row) =>
            row.metric === openMetric ? (
              <ul className="nd-rules-overridden">
                {row.overridden.map((r) => (
                  <li key={r.id}>
                    <span className="muted">{t('rules.overriddenRule')}</span>
                    <RuleBounds rule={r} />
                    <span>{decidedAt(r)}</span>
                  </li>
                ))}
              </ul>
            ) : null
          }
        />
      </div>
      {editing && (
        <ThresholdModal
          mode={editing.mode}
          rule={editing.mode === 'edit' ? editing.rule : undefined}
          start={editing.mode === 'add' ? editing.start : undefined}
          onClose={() => setEditing(null)}
          onSaved={() => load.reload()}
        />
      )}
    </LoadGate>
  );
}
