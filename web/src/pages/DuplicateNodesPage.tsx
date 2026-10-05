// SPDX-License-Identifier: AGPL-3.0-only
// Nodes ▸ Duplicates (ADR-148). Device nodes that look like one device added more than once — at one
// address or at two — grouped with what they share, and a delete for the ones an operator picks.
//
// Nothing is deleted by this screen on its own: it selects nothing for the operator, marks one member
// of each group as the suggested keeper, and refuses a delete that would take every member of a group.
// The delete is the inventory tree's own bulk delete (`DeleteNodesModal`). Which rows a delete may
// send, and what an empty table means, are in `duplicateNodes.ts` where a test can reach them.

import { useEffect, useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { addressText } from '../lib/nodeAddress';
import { Link } from 'react-router-dom';
import { api } from '../services/api';
import { useCan } from '../store';
import type { DuplicateNodesView } from '../types/api';
import { PageHeader } from '../components/ui/PageHeader';
import { Button } from '../components/ui/Button';
import { Badge } from '../components/ui/Badge';
import { InfoPress } from '../components/ui/InfoTip';
import { TableToolbar, TableSpacer } from '../components/ui/TableToolbar';
import { DataTable, type Column } from '../components/ui/DataTable';
import { TimeCell } from '../components/ui/tableCells';
import { EntityName } from '../components/ui/EntityName';
import { useEntityNames } from '../components/ui/entityNames';
import { useLoad } from '../lib/useLoad';
import { LoadGate } from '../components/ui/LoadGate';
import { DeleteNodesModal } from '../components/NodeTree/DeleteNodesModal';
import {
  CONFIDENCE_INFO,
  confidenceCounts,
  deleteBlock,
  deleteTargets,
  emptyState,
  evidenceSummary,
  flattenRows,
  ignoredShown,
  pruneSelection,
  selectAllButKeepers,
  type DuplicateRow,
} from './duplicateNodes';
import { nodeHref } from '../lib/entityHref';
import './DuplicateNodesPage.css';

export function DuplicateNodesPage() {
  const { t } = useTranslation('monitoring');
  const canConfig = useCan('manage_config');
  const { groupName } = useEntityNames();
  const [selected, setSelected] = useState<ReadonlySet<string>>(new Set());
  const [deleting, setDeleting] = useState(false);

  const duplicates = useLoad(() => api.getDuplicateNodes(), [], {
    initial: null as DuplicateNodesView | null,
  });
  const { data: view, loading, reload: load } = duplicates;
  // A node that left the list leaves the selection with it, whenever an answer lands.
  useEffect(() => {
    if (view) setSelected((prev) => pruneSelection(prev, view));
  }, [view]);

  const rows = useMemo(() => flattenRows(view), [view]);
  const refusal = deleteBlock(view, selected);
  const counts = confidenceCounts(view);
  const ignored = ignoredShown(view);
  const empty = emptyState(view);

  const toggle = (id: string, on: boolean) =>
    setSelected((prev) => {
      const next = new Set(prev);
      if (on) next.add(id);
      else next.delete(id);
      return next;
    });

  const columns = useMemo<Column<DuplicateRow>[]>(() => {
    const cols: Column<DuplicateRow>[] = [];
    if (canConfig) {
      cols.push({
        key: 'select',
        header: t('duplicates.cols.select'),
        width: '70px',
        render: (r) => (
          <input
            type="checkbox"
            aria-label={t('duplicates.selectNode', { name: r.member.node_name })}
            checked={selected.has(r.member.node_id)}
            onChange={(e) => toggle(r.member.node_id, e.target.checked)}
          />
        ),
      });
    }
    cols.push(
      {
        key: 'group',
        header: t('duplicates.cols.group'),
        width: '170px',
        render: (r) => {
          const label = t(`duplicates.confidence.${r.group.confidence}`);
          return (
            <span className="dup-group" title={`#${r.groupNumber} ${label}`}>
              <span className="dup-group-no">#{r.groupNumber}</span>
              {r.first && (
                <InfoPress infoKey={CONFIDENCE_INFO[r.group.confidence]} className="badge badge-neutral">
                  {label}
                </InfoPress>
              )}
            </span>
          );
        },
      },
      {
        key: 'evidence',
        header: t('duplicates.cols.evidence'),
        width: '2fr',
        render: (r) => {
          if (!r.first) return null;
          const text = evidenceSummary(r.group, (k) => t(`duplicates.kind.${k}`));
          const against = r.group.contradictions
            .map((c) => t(`duplicates.contradiction.${c}`))
            .join(' · ');
          return (
            <span className="dup-evidence" title={against ? `${text} · ${against}` : text}>
              {text}
              {against && <span className="dup-against"> · {against}</span>}
            </span>
          );
        },
      },
      {
        key: 'node',
        header: t('duplicates.cols.node'),
        width: '1.3fr',
        render: (r) => (
          <span className="dup-node">
            <Link to={nodeHref(r.member.node_id)} title={r.member.node_name}>
              {r.member.node_name}
            </Link>
            {r.member.suggested_keep && <Badge>{t('duplicates.keep')}</Badge>}
          </span>
        ),
      },
      {
        key: 'address',
        header: t('duplicates.cols.address'),
        width: '150px',
        render: (r) => (
          <span className="mono" title={addressText(r.member.address, t)}>
            {addressText(r.member.address, t)}
          </span>
        ),
      },
      {
        key: 'folder',
        header: t('duplicates.cols.folder'),
        width: '1fr',
        render: (r) =>
          r.member.group_id ? (
            <EntityName name={groupName(r.member.group_id)} id={r.member.group_id} />
          ) : (
            <span className="muted" title={t('duplicates.root')}>
              {t('duplicates.root')}
            </span>
          ),
      },
      {
        key: 'serial',
        header: t('duplicates.cols.serial'),
        width: '1fr',
        render: (r) =>
          r.member.serial_number ? (
            <span className="mono" title={r.member.serial_number}>
              {r.member.serial_number}
            </span>
          ) : (
            <span className="muted">—</span>
          ),
      },
      {
        key: 'added',
        header: t('duplicates.cols.added'),
        width: '170px',
        render: (r) => <TimeCell iso={r.member.created_at} />,
      },
      {
        key: 'dependents',
        header: t('duplicates.cols.dependents'),
        width: '90px',
        align: 'right',
        render: (r) => r.member.dependents,
      },
    );
    return cols;
  }, [t, canConfig, selected, groupName]);

  return (
    <div>
      <PageHeader
        title={t('nav:nodes.duplicates')}
        trail={[{ label: t('nav:sections.nodes') }, { label: t('nav:nodes.duplicates') }]}
      />

      <LoadGate load={duplicates} permission="manage_config">
        <TableToolbar>
          {canConfig && rows.length > 0 && (
            <Button variant="outline" onClick={() => setSelected(selectAllButKeepers(view))}>
              {t('duplicates.selectKeepers')}
            </Button>
          )}
          {canConfig && selected.size > 0 && (
            <Button variant="outline" onClick={() => setSelected(new Set())}>
              {t('duplicates.clearSelection')}
            </Button>
          )}
          <TableSpacer />
          {view && (
            <span className="dup-counts">
              {t('duplicates.counts', {
                total: view.total,
                confident: counts.confident,
                possible: counts.possible,
                scanned: view.scanned,
              })}
            </span>
          )}
          {canConfig && (
            <Button
              variant="danger"
              onClick={() => setDeleting(true)}
              disabled={selected.size === 0 || refusal !== null}
            >
              {t('duplicates.delete', { count: selected.size })}
            </Button>
          )}
        </TableToolbar>

        {refusal && (
          <p className="form-error">
            {refusal.key === 'duplicates.block.keepOne'
              ? t(refusal.key, { groups: refusal.groups.map((n) => `#${n}`).join(', ') })
              : t(refusal.key, { max: refusal.max })}
          </p>
        )}
        {view && view.total > view.groups.length && (
          <p className="muted">
            {t('duplicates.truncated', { shown: view.groups.length, total: view.total })}
          </p>
        )}
        {ignored.shown.length > 0 && (
          <p className="muted dup-ignored">
            {t('duplicates.ignored', {
              count: view?.ignored_total ?? 0,
              values:
                ignored.shown
                  .map((i) =>
                    t('duplicates.ignoredValue', {
                      kind: t(`duplicates.kind.${i.kind}`),
                      value: i.value,
                      nodes: i.nodes,
                    }),
                  )
                  .join(', ') + (ignored.more > 0 ? ', …' : ''),
            })}
          </p>
        )}

        <DataTable
          tableId="nodes.duplicates"
          rows={rows}
          columns={columns}
          rowKey={(r) => r.member.node_id}
          rowClass={(r) => `dup-shade-${r.shade}`}
          loading={loading}
          empty={t(empty.key, { count: empty.count })}
        />
      </LoadGate>

      {deleting && (
        <DeleteNodesModal
          targets={deleteTargets(view, selected)}
          onClose={() => setDeleting(false)}
          onDeleted={() => {
            setSelected(new Set());
            load();
          }}
        />
      )}
    </div>
  );
}
