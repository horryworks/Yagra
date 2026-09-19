// SPDX-License-Identifier: AGPL-3.0-only
// Set (or clear) a node's dependency upstream (parent) — the alert-suppression edge. A focused-edit
// modal shared by the node-detail header and the Dependency page, so setting an upstream is
// identical wherever it's reached. Excludes self + descendants from the picker (client-side cycle
// guard); the server validates too (self/cycle → 400). Assignment is immediate; the caller
// refreshes on success.

import { useEffect, useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { api, errMsg } from '../../services/api';
import type { TopologyNode } from '../../types/api';
import { invalidParentIds } from '../../lib/dependencies';
import { Modal } from '../ui/Modal';
import { Button } from '../ui/Button';
import { NodePicker } from '../NodePicker/NodePicker';

export function SetParentModal({
  nodeId,
  nodeName,
  currentParentId,
  onClose,
  onSaved,
}: {
  nodeId: string;
  nodeName: string;
  currentParentId: string | null;
  onClose: () => void;
  onSaved: () => void;
}) {
  const { t } = useTranslation('nodes');
  const [topo, setTopo] = useState<TopologyNode[] | null>(null);
  // 🚨 Seeded from the id the caller already holds, not left `null` until the graph answers. The
  // name comes from `/topology`; the FACT that there is an upstream does not. It used to start
  // at `null` and be filled in by that read — so when the read failed, the picker said "no
  // upstream" about a node that had one, and Save sent `null`: clearing a dependency the
  // operator had never been shown.
  const [parent, setParent] = useState<{ id: string; name: string } | null>(
    currentParentId ? { id: currentParentId, name: '' } : null,
  );
  const [loadFailed, setLoadFailed] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  // Load the dependency graph once: to resolve the current upstream's name for the trigger and to
  // exclude cycle-forming choices (self + descendants) from the picker.
  useEffect(() => {
    let cancelled = false;
    api
      .getTopology()
      .then((r) => {
        if (cancelled) return;
        setTopo(r.nodes);
        const cur = currentParentId ? r.nodes.find((n) => n.id === currentParentId) : undefined;
        // Only the name is news here. An upstream the graph does not list (out of this caller's
        // scope, say) stays selected rather than being read as "none".
        if (cur) setParent({ id: cur.id, name: cur.name });
      })
      .catch(() => {
        if (cancelled) return;
        setTopo([]);
        setLoadFailed(true);
      });
    return () => {
      cancelled = true;
    };
  }, [currentParentId]);

  const exclude = useMemo(
    () => (topo ? invalidParentIds(topo, nodeId) : new Set([nodeId])),
    [topo, nodeId],
  );

  const save = () => {
    setBusy(true);
    setError(null);
    api
      .setNodeParent(nodeId, parent?.id ?? null)
      .then(onSaved)
      .catch((e: unknown) => {
        setError(errMsg(e, t('err.setDependency')));
        setBusy(false);
      });
  };

  return (
    <Modal
      title={t('setParent.title', { name: nodeName })}
      onClose={onClose}
      footer={
        <>
          <Button variant="outline" onClick={onClose} disabled={busy}>
            {t('common:actions.cancel')}
          </Button>
          <Button variant="primary" onClick={save} disabled={busy}>
            {t('common:actions.save')}
          </Button>
        </>
      }
    >
      <div className="form-stack">
        <label className="form-label">
          {t('setParent.dependsOn')}
          <NodePicker
            value={parent?.id ?? null}
            valueLabel={parent ? parent.name || t('setParent.currentUnnamed') : undefined}
            onChange={setParent}
            exclude={exclude}
            placeholder={t('setParent.noUpstream')}
          />
        </label>
        <p className="form-hint">{t('setParent.hint', { name: nodeName })}</p>
        {loadFailed && <p className="form-error">{t('setParent.loadFailed')}</p>}
        {error && <p className="form-error">{error}</p>}
      </div>
    </Modal>
  );
}
