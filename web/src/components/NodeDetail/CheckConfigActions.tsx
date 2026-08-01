// SPDX-License-Identifier: AGPL-3.0-only
// The Edit / Remove affordance on a URL or DNS monitor's health card.
//
// An OverflowMenu rather than inline buttons: the card head already carries the range control, and
// ui-conventions forbid a hover-only affordance — the ⋮ menu is reachable by touch on mobile.
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
import { EditIcon, TrashIcon } from '../ui/icons';
import type { DnsCheckConfig, UrlCheckConfig } from '../../types/api';
import { DnsCheckModal, UrlCheckModal } from './CheckConfigModal';

type Target =
  | { kind: 'url'; config: UrlCheckConfig }
  | { kind: 'dns'; config: DnsCheckConfig };

export function CheckConfigActions({
  nodeId,
  onChanged,
  ...target
}: { nodeId: string; onChanged?: () => void } & Target) {
  const { t } = useTranslation('nodes');
  const [editing, setEditing] = useState(false);
  const [removing, setRemoving] = useState(false);

  const done = () => {
    setEditing(false);
    setRemoving(false);
    onChanged?.();
  };

  return (
    <>
      <OverflowMenu
        actions={[
          {
            label: t('common:actions.edit'),
            icon: <EditIcon />,
            onClick: () => setEditing(true),
          },
          {
            label: t('checkEdit.remove'),
            icon: <TrashIcon />,
            danger: true,
            onClick: () => setRemoving(true),
          },
        ]}
      />
      {editing &&
        (target.kind === 'url' ? (
          <UrlCheckModal
            nodeId={nodeId}
            config={target.config}
            onClose={() => setEditing(false)}
            onSaved={done}
          />
        ) : (
          <DnsCheckModal
            nodeId={nodeId}
            config={target.config}
            onClose={() => setEditing(false)}
            onSaved={done}
          />
        ))}
      {removing && (
        <ConfirmDeleteModal
          title={t('checkEdit.remove')}
          confirmLabel={t('checkEdit.remove')}
          errorFallback={t('checkEdit.err.remove')}
          onConfirm={() =>
            target.kind === 'url' ? api.deleteUrlCheck(nodeId) : api.deleteDnsCheck(nodeId)
          }
          onClose={() => setRemoving(false)}
          onDone={done}
        >
          {t('checkEdit.removeConfirm')}
        </ConfirmDeleteModal>
      )}
    </>
  );
}
