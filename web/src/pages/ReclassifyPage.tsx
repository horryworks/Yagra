// SPDX-License-Identifier: AGPL-3.0-only
// Nodes ▸ Reclassify (ADR-140). The device nodes whose profile differs from the one the current
// classification rules choose for what the device last said it is, and the two things an operator can
// do about each: apply the rules' choice, or keep the profile it has.
//
// Nothing here changes a node by itself. A profile carries a node's metric sets, its profile-scoped
// thresholds and its maintenance windows all at once, so the rules only ever propose. What an apply
// sends, and which selections survive a reload, are in `reclassify.ts` where a test can reach them.

import { useCallback, useEffect, useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Link } from 'react-router-dom';
import { api, errMsg } from '../services/api';
import { useCan } from '../store';
import type { ReclassifyProposal, ReclassifyView } from '../types/api';
import { PageHeader } from '../components/ui/PageHeader';
import { Button } from '../components/ui/Button';
import { Modal } from '../components/ui/Modal';
import { TableToolbar, TableSpacer } from '../components/ui/TableToolbar';
import { DataTable, type Column } from '../components/ui/DataTable';
import { classifyLoadError, type LoadBlock } from '../lib/loadState';
import { LoadBlockNotice } from '../components/ui/LoadBlockNotice';
import { applyItems, pruneSelection, ruleSignature } from './reclassify';
import './ReclassifyPage.css';

export function ReclassifyPage() {
  const { t } = useTranslation('monitoring');
  const canConfig = useCan('manage_config');
  const [view, setView] = useState<ReclassifyView | null>(null);
  const [block, setBlock] = useState<LoadBlock | null>(null);
  const [loading, setLoading] = useState(true);
  const [selected, setSelected] = useState<ReadonlySet<string>>(new Set());
  const [confirming, setConfirming] = useState(false);
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(() => {
    api
      .getReclassify()
      .then((v) => {
        setView(v);
        setBlock(null);
        setSelected((prev) => pruneSelection(prev, v.proposals));
      })
      .catch((e: unknown) => setBlock(classifyLoadError(e)))
      .finally(() => setLoading(false));
  }, []);

  useEffect(() => {
    load();
  }, [load]);

  const rows = useMemo(() => view?.proposals ?? [], [view]);
  const allSelected = rows.length > 0 && rows.every((r) => selected.has(r.node_id));

  const toggle = (id: string, on: boolean) =>
    setSelected((prev) => {
      const next = new Set(prev);
      if (on) next.add(id);
      else next.delete(id);
      return next;
    });

  const columns = useMemo<Column<ReclassifyProposal>[]>(() => {
    const cols: Column<ReclassifyProposal>[] = [];
    if (canConfig) {
      cols.push({
        key: 'select',
        header: t('reclassify.cols.select'),
        width: '70px',
        render: (r) => (
          <input
            type="checkbox"
            aria-label={t('reclassify.selectNode', { name: r.node_name })}
            checked={selected.has(r.node_id)}
            onChange={(e) => toggle(r.node_id, e.target.checked)}
          />
        ),
      });
    }
    cols.push(
      {
        key: 'node',
        header: t('reclassify.cols.node'),
        width: '1fr',
        render: (r) => (
          <Link to={`/nodes/${r.node_id}`} title={r.node_name}>
            {r.node_name}
          </Link>
        ),
      },
      {
        key: 'current',
        header: t('reclassify.cols.current'),
        width: '1fr',
        render: (r) => (
          <span title={r.current_profile_name ?? ''}>{r.current_profile_name ?? '—'}</span>
        ),
      },
      {
        key: 'suggested',
        header: t('reclassify.cols.suggested'),
        width: '1fr',
        render: (r) => (
          <strong title={r.suggested_profile_name ?? ''}>{r.suggested_profile_name ?? '—'}</strong>
        ),
      },
      {
        key: 'rule',
        header: t('reclassify.cols.rule'),
        width: '1.3fr',
        render: (r) => {
          const sig = ruleSignature(r);
          return sig ? (
            <span className="mono" title={sig}>
              {sig}
            </span>
          ) : (
            <span className="muted" title={t('reclassify.noRule')}>
              {t('reclassify.noRule')}
            </span>
          );
        },
      },
      {
        key: 'sysObjectId',
        header: t('reclassify.cols.sysObjectId'),
        width: '1fr',
        render: (r) => (
          <span className="mono" title={r.sys_object_id}>
            {r.sys_object_id}
          </span>
        ),
      },
      {
        key: 'sysDescr',
        header: t('reclassify.cols.sysDescr'),
        width: '1.5fr',
        render: (r) => <span title={r.sys_descr ?? ''}>{r.sys_descr ?? '—'}</span>,
      },
    );
    return cols;
  }, [t, canConfig, selected]);

  const lock = () => {
    const ids = rows.filter((r) => selected.has(r.node_id)).map((r) => r.node_id);
    setBusy(true);
    setError(null);
    setMessage(null);
    api
      .lockReclassify(ids, true)
      .then((out) => {
        setMessage(t('reclassify.result.locked', { count: out.updated }));
        setSelected(new Set());
        load();
      })
      .catch((e: unknown) => setError(errMsg(e, t('reclassify.err.lock'))))
      .finally(() => setBusy(false));
  };

  const apply = () => {
    setBusy(true);
    setError(null);
    setMessage(null);
    api
      .applyReclassify(applyItems(rows, selected))
      .then((out) => {
        setMessage(
          t('reclassify.result.applied', {
            applied: out.applied,
            changed: out.skipped_changed,
            locked: out.skipped_locked,
            hidden: out.skipped_hidden,
          }),
        );
        setConfirming(false);
        setSelected(new Set());
        load();
      })
      .catch((e: unknown) => setError(errMsg(e, t('reclassify.err.apply'))))
      .finally(() => setBusy(false));
  };

  return (
    <div>
      <PageHeader
        title={t('nav:nodes.reclassify')}
        trail={[{ label: t('nav:sections.nodes') }, { label: t('nav:nodes.reclassify') }]}
        note={t('reclassify.note')}
      />

      {block ? (
        <LoadBlockNotice
          permission="manage_config"
          block={block}
          unavailable={t('reclassify.unavailable')}
        />
      ) : (
        <>
          <TableToolbar>
            {canConfig && rows.length > 0 && (
              <label className="reclassify-select-all">
                <input
                  type="checkbox"
                  checked={allSelected}
                  onChange={(e) =>
                    setSelected(e.target.checked ? new Set(rows.map((r) => r.node_id)) : new Set())
                  }
                />
                <span>{t('reclassify.selectAll')}</span>
              </label>
            )}
            <TableSpacer />
            {view && (
              <span className="reclassify-counts">
                {t('reclassify.counts', {
                  differ: view.total,
                  locked: view.locked,
                  unidentified: view.unidentified,
                })}
              </span>
            )}
            {canConfig && (
              <>
                <Button variant="outline" onClick={lock} disabled={selected.size === 0 || busy}>
                  {t('reclassify.lock')}
                </Button>
                <Button
                  variant="primary"
                  onClick={() => setConfirming(true)}
                  disabled={selected.size === 0 || busy}
                >
                  {t('reclassify.apply')}
                </Button>
              </>
            )}
          </TableToolbar>

          {view && view.total > rows.length && (
            <p className="muted">
              {t('reclassify.truncated', { shown: rows.length, total: view.total })}
            </p>
          )}
          {message && <p className="reclassify-message">{message}</p>}
          {error && !confirming && <p className="form-error">{error}</p>}

          <DataTable
            tableId="nodes.reclassify"
            rows={rows}
            columns={columns}
            rowKey={(r) => r.node_id}
            loading={loading}
            empty={t('reclassify.empty')}
          />
          <p className="muted reclassify-hint">
            {t('reclassify.lockedHint')} {t('reclassify.unidentifiedHint')}
          </p>
        </>
      )}

      {confirming && (
        <Modal
          title={t('reclassify.confirm.title', { count: selected.size })}
          onClose={() => setConfirming(false)}
          footer={
            <>
              <Button variant="outline" onClick={() => setConfirming(false)} disabled={busy}>
                {t('common:actions.cancel')}
              </Button>
              <Button variant="primary" onClick={apply} disabled={busy}>
                {t('reclassify.confirm.submit')}
              </Button>
            </>
          }
        >
          <p>{t('reclassify.confirm.body')}</p>
          <p>{t('reclassify.confirm.stops')}</p>
          {error && <p className="form-error">{error}</p>}
        </Modal>
      )}
    </div>
  );
}
