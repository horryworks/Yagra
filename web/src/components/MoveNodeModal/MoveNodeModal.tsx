// Move a node into a group (or ungroup it). A focused-edit modal shared by the inventory tree
// (context-menu / "Move…" picker) and the node-detail header, so the assign-to-group flow is
// identical wherever it is reached. Assignment is immediate; the caller refreshes on success.

import { useState } from 'react';
import { useTranslation } from 'react-i18next';
import { api, ApiError } from '../../services/api';
import type { NodeGroup, NodeSummary } from '../../types/api';
import { groupOptions } from '../../lib/nodeTree';
import { Modal } from '../ui/Modal';
import { Button } from '../ui/Button';
import { Select } from '../ui/Field';

const errMsg = (e: unknown, fallback: string) => (e instanceof ApiError ? e.message : fallback);

export function MoveNodeModal({
  node,
  groups,
  onClose,
  onMoved,
}: {
  node: NodeSummary;
  groups: NodeGroup[];
  onClose: () => void;
  onMoved: () => void;
}) {
  const { t } = useTranslation('nodes');
  const [target, setTarget] = useState<string>(node.group_id ?? '');
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const save = () => {
    setBusy(true);
    setError(null);
    api
      .setNodeGroup(node.id, target || null)
      .then(onMoved)
      .catch((e: unknown) => {
        setError(errMsg(e, t('err.moveNode')));
        setBusy(false);
      });
  };

  return (
    <Modal
      title={t('moveNode.title', { name: node.name })}
      onClose={onClose}
      footer={
        <>
          <Button variant="outline" onClick={onClose} disabled={busy}>
            {t('common:actions.cancel')}
          </Button>
          <Button variant="primary" onClick={save} disabled={busy}>
            {t('moveNode.move')}
          </Button>
        </>
      }
    >
      <div className="form-stack">
        <label className="form-label">
          {t('field.group')}
          <Select value={target} onChange={(e) => setTarget(e.target.value)}>
            <option value="">{t('moveNode.ungroupedOption')}</option>
            {groupOptions(groups).map((o) => (
              <option key={o.id} value={o.id}>
                {o.label}
              </option>
            ))}
          </Select>
        </label>
        {error && <p className="form-error">{error}</p>}
      </div>
    </Modal>
  );
}
