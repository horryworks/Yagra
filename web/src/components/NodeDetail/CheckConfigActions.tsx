// SPDX-License-Identifier: AGPL-3.0-only
// The "Remove monitoring" affordance on a URL or DNS monitor's health card.
//
// It used to offer Edit as well, opening a dialog that was the only way to change a monitor's URL
// or resolver. That form now lives in "Edit node" in the header, which is reachable on both the
// inline and full-page variants and does not depend on a health card being rendered — so this menu
// keeps only the action that belongs to the card itself. Two doors to one form is the duplication
// `extensibility.md` §3 is about, and this was the worse door.
//
// An OverflowMenu rather than an inline button: the card head already carries the range control,
// and a naked destructive button beside it invites the mis-click. It stays touch-reachable.
//
// "Remove monitoring" deletes the check row, not the node. That is genuinely what the endpoint
// does, so the confirmation says it plainly: the node stays in the inventory and simply stops being
// probed, which is a different outcome from deleting the node and the one an operator can undo by
// re-adding a check.

import { useState } from 'react';
import { useTranslation } from 'react-i18next';
import { api } from '../../services/api';
import { ConfirmDeleteModal } from '../ui/ConfirmDeleteModal';
import { OverflowMenu } from '../ui/OverflowMenu';
import { TrashIcon } from '../ui/icons';

export function CheckConfigActions({
  nodeId,
  kind,
  onChanged,
}: {
  nodeId: string;
  kind: 'url' | 'dns';
  onChanged?: () => void;
}) {
  const { t } = useTranslation('nodes');
  const [removing, setRemoving] = useState(false);

  return (
    <>
      <OverflowMenu
        actions={[
          {
            label: t('checkEdit.remove'),
            icon: <TrashIcon />,
            danger: true,
            onClick: () => setRemoving(true),
          },
        ]}
      />
      {removing && (
        <ConfirmDeleteModal
          title={t('checkEdit.remove')}
          confirmLabel={t('checkEdit.remove')}
          errorFallback={t('checkEdit.err.remove')}
          onConfirm={() => (kind === 'url' ? api.deleteUrlCheck(nodeId) : api.deleteDnsCheck(nodeId))}
          onClose={() => setRemoving(false)}
          onDone={() => {
            setRemoving(false);
            onChanged?.();
          }}
        >
          {t('checkEdit.removeConfirm')}
        </ConfirmDeleteModal>
      )}
    </>
  );
}
