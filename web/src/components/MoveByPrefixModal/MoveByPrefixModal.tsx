// SPDX-License-Identifier: AGPL-3.0-only
// "Move these nodes to the folder whose IP range contains their address" (ADR-124 決定 6).
//
// **It proposes; a person applies.** ADR-100 決定 2 refused to let a sync write `nodes.group_id`
// because an automatic rule and an operator's own moves fight each other last-writer-wins. The
// writer here is the operator, so the decision does not forbid this — but the hazard has the same
// shape, so nothing moves until the button is pressed and the operator has seen what would move.
//
// Three sections, each with its count, and the two that are *not* moves matter as much as the one
// that is: a node whose address matches nothing, and a node two sites claim equally well, are both
// shown rather than quietly dropped. Choosing between two sites is exactly the decision this
// refuses to make.

import { useEffect, useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { api, errMsg } from '../../services/api';
import type { MovePreview, NodeGroup, NodeSummary } from '../../types/api';
import { groupOptions } from '../../lib/nodeTree';
import { NODE_KIND_SPEC } from '../../lib/nodeKind';
import { Modal } from '../ui/Modal';
import { Button } from '../ui/Button';
import {
  byDestination,
  emptyReason,
  summarize,
  type ApplySummary,
  type DestinationResult,
} from './moveByPrefix';
import './MoveByPrefixModal.css';

export function MoveByPrefixModal({
  targets,
  groups,
  onClose,
  onMoved,
}: {
  targets: readonly NodeSummary[];
  groups: NodeGroup[];
  onClose: () => void;
  /** Refresh the inventory. Does not close — the result is worth reading. */
  onMoved: () => void;
}) {
  const { t } = useTranslation('nodes');
  const [preview, setPreview] = useState<MovePreview | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [done, setDone] = useState<ApplySummary | null>(null);

  const ids = useMemo(() => targets.map((n) => n.id), [targets]);
  const byId = useMemo(() => new Map(targets.map((n) => [n.id, n])), [targets]);
  const paths = useMemo(() => new Map(groupOptions(groups).map((o) => [o.id, o.path])), [groups]);

  useEffect(() => {
    let live = true;
    api
      .previewMoveByPrefix(ids)
      .then((p) => {
        if (live) setPreview(p);
      })
      .catch((e: unknown) => {
        if (live) setError(errMsg(e, t('err.movePreview')));
      });
    return () => {
      live = false;
    };
  }, [ids, t]);

  const destinations = preview ? byDestination(preview) : [];
  const reason = preview ? emptyReason(preview) : null;

  const apply = async () => {
    setBusy(true);
    setError(null);
    const results: DestinationResult[] = [];
    for (const d of destinations) {
      try {
        const r = await api.moveNodes(d.nodeIds, d.groupId);
        results.push({
          groupId: d.groupId,
          moved: r.moved,
          requested: r.requested,
          failed: false,
        });
      } catch {
        // Keep going: the folders already moved stay moved, and stopping here would leave the
        // operator with a partial result they were never told about.
        results.push({
          groupId: d.groupId,
          moved: 0,
          requested: d.nodeIds.length,
          failed: true,
        });
      }
    }
    onMoved();
    setDone(summarize(results));
    setBusy(false);
  };

  const nodeLine = (id: string) => {
    const n = byId.get(id);
    if (!n) return <li key={id}>{id}</li>;
    const spec = NODE_KIND_SPEC[n.kind];
    return (
      <li key={id}>
        <span className="mbp-name">{n.name}</span>
        <span className="mbp-addr mono">{n.address}</span>
        {spec.badge && (
          <span className="mbp-badge" title={t(spec.labelKey)}>
            {spec.badge}
          </span>
        )}
      </li>
    );
  };

  return (
    <Modal
      title={t('moveByPrefix.title', { count: targets.length })}
      onClose={onClose}
      size="wide"
      footer={
        <>
          <Button variant="outline" onClick={onClose} disabled={busy}>
            {done ? t('common:actions.close') : t('common:actions.cancel')}
          </Button>
          {!done && (
            <Button
              variant="primary"
              onClick={() => void apply()}
              disabled={busy || destinations.length === 0}
            >
              {t('moveByPrefix.apply', {
                count: preview?.matched.length ?? 0,
              })}
            </Button>
          )}
        </>
      }
    >
      <div className="mbp">
        {!preview && !error && <p className="muted">{t('moveByPrefix.loading')}</p>}

        {preview && reason && (
          // One of three sentences, never one generic one — see `emptyReason`.
          <p className="mbp-reason">{t(`moveByPrefix.empty.${reason}`)}</p>
        )}

        {preview && destinations.length > 0 && (
          <section className="mbp-section">
            <h4 className="mbp-head">
              {t('moveByPrefix.willMove', { count: preview.matched.length })}
            </h4>
            {destinations.map((d) => (
              <div className="mbp-dest" key={d.groupId}>
                <div className="mbp-dest-head">
                  <span className="mbp-dest-name">{paths.get(d.groupId) ?? d.groupId}</span>
                  <span className="mbp-dest-count">{d.nodeIds.length}</span>
                </div>
                <ul className="mbp-list">{d.nodeIds.map(nodeLine)}</ul>
              </div>
            ))}
          </section>
        )}

        {preview && preview.ambiguous.length > 0 && (
          <section className="mbp-section">
            <h4 className="mbp-head">
              {t('moveByPrefix.ambiguous', { count: preview.ambiguous.length })}
            </h4>
            <p className="mbp-hint">{t('moveByPrefix.ambiguousHint')}</p>
            <ul className="mbp-list">
              {preview.ambiguous.map((a) => (
                <li key={a.node_id}>
                  <span className="mbp-name">{byId.get(a.node_id)?.name ?? a.node_id}</span>
                  <span className="mbp-cands">
                    {a.group_ids.map((g) => paths.get(g) ?? g).join(' · ')}
                  </span>
                </li>
              ))}
            </ul>
          </section>
        )}

        {preview && preview.unmatched.length > 0 && (
          <section className="mbp-section">
            <h4 className="mbp-head">
              {t('moveByPrefix.unmatched', { count: preview.unmatched.length })}
            </h4>
            <ul className="mbp-list">{preview.unmatched.map(nodeLine)}</ul>
          </section>
        )}

        {done && (
          <p className={done.complete ? 'mbp-done' : 'form-error'}>
            {t('moveByPrefix.result', { moved: done.moved, requested: done.requested })}
            {done.failedGroups.length > 0 &&
              ` ${t('moveByPrefix.failedGroups', {
                groups: done.failedGroups.map((g) => paths.get(g) ?? g).join(', '),
              })}`}
          </p>
        )}
        {error && <p className="form-error">{error}</p>}
      </div>
    </Modal>
  );
}
