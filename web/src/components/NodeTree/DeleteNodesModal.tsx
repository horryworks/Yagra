// SPDX-License-Identifier: AGPL-3.0-only
// Delete many nodes at once, from the inventory tree's working set (ADR-124 増分 6).
//
// Built on `ConfirmDeleteModal` like every other destructive consent. What is specific here is the
// sentence naming the targets and what happens when not all of them went — both decided in
// `deleteNodes.ts`, where a test reaches them.

import { useTranslation } from 'react-i18next';
import { api, ApiError } from '../../services/api';
import type { NodeSummary } from '../../types/api';
import { ConfirmDeleteModal } from '../ui/ConfirmDeleteModal';
import { deletedEverything, namesToConfirm } from './deleteNodes';

export function DeleteNodesModal({
  targets,
  onClose,
  onDeleted,
}: {
  targets: NodeSummary[];
  onClose: () => void;
  /** Called once the server has deleted what it will — also when that was not every target, so the
   *  tree and the working set follow what actually went. The dialog closes itself only when all
   *  of them did. */
  onDeleted: () => void;
}) {
  const { t } = useTranslation('nodes');
  const { shown, more } = namesToConfirm(targets);

  const confirm = () =>
    api
      .deleteNodes(targets.map((n) => n.id))
      .then((r) => {
        onDeleted();
        if (!deletedEverything(r)) {
          // Keeps the dialog open with both numbers. Raised as an `ApiError` because that is the
          // one type the shared dialog shows the message of — anything else is replaced by the
          // generic failure text, which would say the delete failed when most of it happened.
          throw new ApiError(
            'partial_delete',
            t('deleteNodes.partial', { deleted: r.deleted, requested: r.requested }),
            200,
          );
        }
      });

  return (
    <ConfirmDeleteModal
      title={t('deleteNodes.title', { count: targets.length })}
      errorFallback={t('err.deleteNodes')}
      onConfirm={confirm}
      onClose={onClose}
      onDone={onClose}
    >
      {t('deleteNodes.body')}
      <br />
      <strong>{shown.join(', ')}</strong>
      {more > 0 && <> {t('deleteNodes.more', { count: more })}</>}
    </ConfirmDeleteModal>
  );
}
