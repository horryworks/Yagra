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
// refuses to make. A node already in its folder, or beneath it, is counted and left (ADR-176).
//
// Two sources (ADR-176): the nodes the operator selected, or a folder's subtree / the whole
// inventory, which the server collects because the tree has only loaded the folders that are open.
// A subtree proposes at most what one request may carry; "continue" asks again for the rest.

import { useEffect, useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { addressText } from '../../lib/nodeAddress';
import { api, errMsg } from '../../services/api';
import type { NodeGroup, NodeSummary } from '../../types/api';
import { groupOptions } from '../../lib/nodeTree';
import { nodeBadges } from '../../lib/nodeKind';
import { NodeBadgeTag } from '../ui/NodeBadgeTag';
import { Modal } from '../ui/Modal';
import { Button } from '../ui/Button';
import {
  byDestination,
  emptyReason,
  fromSelection,
  fromSubtree,
  remainingAfterApply,
  summarize,
  type ApplySummary,
  type ProposalView,
} from './moveByPrefix';
import './MoveByPrefixModal.css';

/** What the dialog examines: a selection, or every node under a folder (`null` ⇒ everywhere). */
export type MoveByPrefixSource =
  | { kind: 'nodes'; targets: readonly NodeSummary[] }
  | { kind: 'subtree'; groupId: string | null };

/** A node as the dialog names it. A selection brings full summaries (with badges); a subtree
 *  proposal brings a name and an address. */
type Label = { name: string; address: string; summary?: NodeSummary };

export function MoveByPrefixModal({
  source,
  groups,
  onClose,
  onMoved,
}: {
  source: MoveByPrefixSource;
  groups: NodeGroup[];
  onClose: () => void;
  /** Refresh the inventory. Does not close — the result is worth reading. */
  onMoved: () => void;
}) {
  const { t } = useTranslation('nodes');
  const [view, setView] = useState<ProposalView | null>(null);
  const [labels, setLabels] = useState<Map<string, Label>>(new Map());
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [done, setDone] = useState<ApplySummary | null>(null);
  /** Bumped by "continue", which asks the server again once a round has been applied. */
  const [round, setRound] = useState(0);

  const targets = source.kind === 'nodes' ? source.targets : null;
  const subtreeRoot = source.kind === 'subtree' ? source.groupId : undefined;
  const paths = useMemo(() => new Map(groupOptions(groups).map((o) => [o.id, o.path])), [groups]);

  useEffect(() => {
    let live = true;
    const ask: Promise<[ProposalView, Map<string, Label>]> = targets
      ? api
          .previewMoveByPrefix(targets.map((n) => n.id))
          .then((p) => [
            fromSelection(p),
            new Map(targets.map((n) => [n.id, { name: n.name, address: n.address, summary: n }])),
          ])
      : api
          .previewMoveBySubtree(subtreeRoot ?? null)
          .then((p) => [
            fromSubtree(p),
            new Map(p.nodes.map((n) => [n.node_id, { name: n.name, address: n.address }])),
          ]);
    ask
      .then(([v, l]) => {
        if (!live) return;
        setView(v);
        setLabels(l);
      })
      .catch((e: unknown) => {
        if (live) setError(errMsg(e, t('err.movePreview')));
      });
    return () => {
      live = false;
    };
  }, [targets, subtreeRoot, round, t]);

  const destinations = view ? byDestination(view) : [];
  const reason = view ? emptyReason(view) : null;
  const remaining = view ? remainingAfterApply(view) : 0;

  const apply = async () => {
    setBusy(true);
    setError(null);
    // One request for every destination, written in one transaction (ADR-172 決定 2). It used to
    // be one request per destination, so a tab closed mid-way left some folders moved and the
    // rest not, with no summary shown. Now a failure means nothing moved.
    try {
      const r = await api.moveNodesByPrefix(destinations);
      onMoved();
      setDone(summarize(r.results));
    } catch (e: unknown) {
      setError(errMsg(e, t('err.moveByPrefix')));
    } finally {
      setBusy(false);
    }
  };

  const next = () => {
    setView(null);
    setDone(null);
    setError(null);
    setRound((r) => r + 1);
  };

  const nodeLine = (id: string) => {
    const n = labels.get(id);
    if (!n) return <li key={id}>{id}</li>;
    const s = n.summary;
    return (
      <li key={id}>
        <span className="mbp-name">{n.name}</span>
        <span className="mbp-addr mono">
          {addressText(n.address, t, { meshRepeater: s?.meraki_repeater })}
        </span>
        {s &&
          nodeBadges({
            kind: s.kind,
            merakiProductType: s.meraki_product_type,
            merakiRepeater: s.meraki_repeater,
          }).map((badge) => (
            <NodeBadgeTag
              key={badge.text}
              badge={badge}
              className="mbp-badge"
              label={t(badge.labelKey)}
            />
          ))}
      </li>
    );
  };

  /** "…and N more" under a list the server sliced. */
  const more = (shown: number, total: number) =>
    total > shown ? <p className="mbp-hint">{t('moveByPrefix.more', { count: total - shown })}</p> : null;

  const title =
    source.kind === 'nodes'
      ? t('moveByPrefix.title', { count: source.targets.length })
      : source.groupId === null
        ? t('moveByPrefix.titleAll')
        : t('moveByPrefix.titleFolder', { name: paths.get(source.groupId) ?? '' });

  return (
    <Modal
      title={title}
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
              {t('moveByPrefix.apply', { count: view?.matched.length ?? 0 })}
            </Button>
          )}
          {done && remaining > 0 && (
            <Button variant="primary" onClick={next}>
              {t('moveByPrefix.continue', { count: remaining })}
            </Button>
          )}
        </>
      }
    >
      <div className="mbp">
        {!view && !error && <p className="muted">{t('moveByPrefix.loading')}</p>}

        {view && reason && (
          // One sentence per reason, never one generic one — see `emptyReason`.
          <p className="mbp-reason">{t(`moveByPrefix.empty.${reason}`)}</p>
        )}

        {view && destinations.length > 0 && (
          <section className="mbp-section">
            <h4 className="mbp-head">{t('moveByPrefix.willMove', { count: view.matchedTotal })}</h4>
            {remaining > 0 && (
              <p className="mbp-hint">
                {t('moveByPrefix.firstBatch', { shown: view.matched.length, rest: remaining })}
              </p>
            )}
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

        {view && view.ambiguousTotal > 0 && (
          <section className="mbp-section">
            <h4 className="mbp-head">
              {t('moveByPrefix.ambiguous', { count: view.ambiguousTotal })}
            </h4>
            <p className="mbp-hint">{t('moveByPrefix.ambiguousHint')}</p>
            <ul className="mbp-list">
              {view.ambiguous.map((a) => (
                <li key={a.node_id}>
                  <span className="mbp-name">{labels.get(a.node_id)?.name ?? a.node_id}</span>
                  <span className="mbp-cands">
                    {a.group_ids.map((g) => paths.get(g) ?? g).join(' · ')}
                  </span>
                </li>
              ))}
            </ul>
            {more(view.ambiguous.length, view.ambiguousTotal)}
          </section>
        )}

        {view && view.unmatchedTotal > 0 && (
          <section className="mbp-section">
            <h4 className="mbp-head">
              {t('moveByPrefix.unmatched', { count: view.unmatchedTotal })}
            </h4>
            <ul className="mbp-list">{view.unmatched.map(nodeLine)}</ul>
            {more(view.unmatched.length, view.unmatchedTotal)}
          </section>
        )}

        {view && view.inPlaceTotal > 0 && (
          <p className="mbp-hint">{t('moveByPrefix.inPlace', { count: view.inPlaceTotal })}</p>
        )}

        {done && (
          <p className={done.complete ? 'mbp-done' : 'form-error'}>
            {t('moveByPrefix.result', { moved: done.moved, requested: done.requested })}
          </p>
        )}
        {error && <p className="form-error">{error}</p>}
      </div>
    </Modal>
  );
}
