// Set (or clear) a node's dependency upstream (parent) — the alert-suppression edge. A focused-edit
// modal shared by the node-detail header and the Dependency page, so setting an upstream is
// identical wherever it's reached. Excludes self + descendants from the picker (client-side cycle
// guard); the server validates too (self/cycle → 400). Assignment is immediate; the caller
// refreshes on success.

import { useEffect, useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { api, ApiError } from '../../services/api';
import type { TopologyNode } from '../../types/api';
import { invalidParentIds } from '../../lib/dependencies';
import { Modal } from '../ui/Modal';
import { Button } from '../ui/Button';
import { NodePicker } from '../NodePicker/NodePicker';

const errMsg = (e: unknown, fallback: string) => (e instanceof ApiError ? e.message : fallback);

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
  const [parent, setParent] = useState<{ id: string; name: string } | null>(null);
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
        setParent(cur ? { id: cur.id, name: cur.name } : null);
      })
      .catch(() => !cancelled && setTopo([]));
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
            valueLabel={parent?.name}
            onChange={setParent}
            exclude={exclude}
            placeholder={t('setParent.noUpstream')}
          />
        </label>
        <p className="form-hint">{t('setParent.hint', { name: nodeName })}</p>
        {error && <p className="form-error">{error}</p>}
      </div>
    </Modal>
  );
}
