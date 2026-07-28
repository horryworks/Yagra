// SPDX-License-Identifier: AGPL-3.0-only
// Assign a node or folder to a poll-pool by name (ADR-009/020). The "Custom…" escape hatch behind
// the inventory tree's pool chips, for a pool that doesn't exist yet — the chips themselves cover
// every pool already in use. A blank value clears the assignment back to inherited.
//
// Sibling of MoveNodeModal: same focused-edit shape, immediate write, caller refreshes on success.

import { useState } from 'react';
import { useTranslation } from 'react-i18next';
import { api, ApiError } from '../../services/api';
import type { SuppressionTarget } from '../../lib/suppression';
import { isValidPoolName } from '../../lib/pool';
import { Modal } from '../ui/Modal';
import { Button } from '../ui/Button';
import { TextInput } from '../ui/Field';

const errMsg = (e: unknown, fallback: string) => (e instanceof ApiError ? e.message : fallback);

export function SetPoolModal({
  target,
  currentPool,
  inheritedPool,
  onClose,
  onSaved,
}: {
  /** The node or folder being assigned (reuses the tree's context-menu target shape). */
  target: SuppressionTarget;
  /** Its own pool today; `null` ⇒ it currently inherits. */
  currentPool: string | null;
  /** What it would fall back to if cleared — shown as the placeholder. */
  inheritedPool?: string;
  onClose: () => void;
  onSaved: () => void;
}) {
  const { t } = useTranslation('nodes');
  const [pool, setPool] = useState(currentPool ?? '');
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const invalid = !isValidPoolName(pool);

  const save = () => {
    setBusy(true);
    setError(null);
    // Always sent: '' clears the assignment back to inherited.
    const value = pool.trim();
    const call =
      target.kind === 'node'
        ? api.setNodePool(target.id, value)
        : api.setNodeGroupPool(target.id, value);
    call.then(onSaved).catch((e: unknown) => {
      setError(errMsg(e, t('err.setPool')));
      setBusy(false);
    });
  };

  return (
    <Modal
      title={t('setPool.title', { name: target.name })}
      onClose={onClose}
      footer={
        <>
          <Button variant="outline" onClick={onClose} disabled={busy}>
            {t('common:actions.cancel')}
          </Button>
          <Button variant="primary" onClick={save} disabled={busy || invalid}>
            {t('common:actions.save')}
          </Button>
        </>
      }
    >
      <div className="form-stack">
        <label className="form-label">
          {t('field.pool')}
          <TextInput
            className="mono"
            value={pool}
            onChange={(e) => setPool(e.target.value)}
            placeholder={
              inheritedPool ? t('field.poolInheritPlaceholder', { pool: inheritedPool }) : ''
            }
            autoFocus
          />
          <span className={`form-hint${invalid ? ' form-hint-error' : ''}`}>
            {invalid
              ? t('field.poolInvalid')
              : target.kind === 'group'
                ? t('setPool.groupHint')
                : t('field.poolHint')}
          </span>
        </label>
        {error && <p className="form-error">{error}</p>}
      </div>
    </Modal>
  );
}
