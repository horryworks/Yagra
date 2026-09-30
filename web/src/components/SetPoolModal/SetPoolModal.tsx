// SPDX-License-Identifier: AGPL-3.0-only
// Assign a node, a folder, or the whole working set to a poll-pool by name (ADR-009/020). The
// "Custom…" escape hatch behind the inventory tree's pool chips, for a pool that doesn't exist yet
// — the chips themselves cover every pool already in use. A blank value clears the assignment back
// to inherited.
//
// Sibling of MoveNodeModal: same focused-edit shape, immediate write, caller refreshes on success —
// including the part that matters most for a batch, which is that a **partial** result keeps the
// dialog open rather than closing on a write it did not fully make (ADR-124 Inc.10).

import { useState } from 'react';
import { useTranslation } from 'react-i18next';
import { api } from '../../services/api';
import type { ActionTarget } from '../../lib/actionTarget';
import { targetNodeIds, targetNodeNames } from '../../lib/actionTarget';
import { isValidPoolName } from '../../lib/pool';
import { done, partialOutcome } from '../../lib/submitState';
import { useSubmit } from '../../lib/useSubmit';
import { Modal } from '../ui/Modal';
import { FormError, FormFooter } from '../ui/FormFooter';
import { TextInput } from '../ui/Field';

export function SetPoolModal({
  target,
  currentPool,
  inheritedPool,
  onClose,
  onSaved,
}: {
  /** The node, folder or working set being assigned. */
  target: ActionTarget;
  /** Its own pool today; `null` ⇒ it currently inherits. For a set, `null` unless every node
   *  agrees — see `sharedOwnPool`. */
  currentPool: string | null;
  /** What it would fall back to if cleared — shown as the placeholder. */
  inheritedPool?: string;
  onClose: () => void;
  onSaved: () => void;
}) {
  const { t } = useTranslation('nodes');
  const [pool, setPool] = useState(currentPool ?? '');
  const form = useSubmit({ errorFallback: t('err.setPool'), onDone: onSaved });
  const invalid = !isValidPoolName(pool);
  const names = targetNodeNames(target);

  const save = () => {
    // Always sent: '' clears the assignment back to inherited.
    const value = pool.trim();
    form.submit(() => {
      if (target.kind === 'nodes') {
        // 🚨 The batch reports two numbers and the dialog stays open on a shortfall. Closing would
        // read as "all of them moved", which is the claim `applied` exists to stop anyone making.
        return api
          .setNodesPool(targetNodeIds(target), value)
          .then((r) =>
            partialOutcome(r.applied, r.requested, (n) => t('setPool.partial', n), false),
          );
      }
      const call =
        target.kind === 'node'
          ? api.setNodePool(target.id, value)
          : api.setNodeGroupPool(target.id, value);
      return call.then(() => done());
    });
  };

  return (
    <Modal
      title={
        target.kind === 'nodes'
          ? t('setPool.titleMany', { count: names.length })
          : t('setPool.title', { name: target.name })
      }
      onClose={onClose}
      footer={
        <FormFooter
          form={form}
          onClose={onClose}
          onSubmit={save}
          submitLabel={t('common:actions.save')}
          canSubmit={!invalid}
        />
      }
    >
      <div className="form-stack">
        {names.length > 0 && (
          <ul className="form-targets scroll-y">
            {names.map((name, i) => (
              <li key={`${name}-${i}`}>{name}</li>
            ))}
          </ul>
        )}
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
                : target.kind === 'nodes'
                  ? t('setPool.manyHint')
                  : t('field.poolHint')}
          </span>
        </label>
        <FormError form={form} />
      </div>
    </Modal>
  );
}
