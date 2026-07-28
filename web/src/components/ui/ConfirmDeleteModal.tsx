// SPDX-License-Identifier: AGPL-3.0-only
// Destructive-consent dialog (ui-conventions §Modals case 2). Fifteen pages had each grown their
// own DeleteXModal — same busy/error state, same Cancel + Danger footer, same `.catch` wiring, ~45
// lines apiece — so a change to the confirm chrome meant fifteen edits and the copies had already
// drifted (some disabled Cancel while busy, some did not). This owns that scaffold; a caller
// supplies only what is actually specific: the title, the sentence naming the target, and the call.

import { useState, type ReactNode } from 'react';
import { useTranslation } from 'react-i18next';
import { errMsg } from '../../services/api';
import { Button } from './Button';
import { Modal } from './Modal';

interface Props {
  title: ReactNode;
  /** The sentence naming what is about to be deleted — usually a `<Trans>` with the entity name. */
  children: ReactNode;
  /** Performs the deletion. A rejection keeps the dialog open and shows the message. */
  onConfirm: () => Promise<unknown>;
  /** Shown when the rejection is not an `ApiError` (network drop, unexpected throw). */
  errorFallback: string;
  onClose: () => void;
  /** Called after `onConfirm` resolves — close the dialog and reload the list. */
  onDone: () => void;
  /** Confirm-button label. Defaults to the shared "Delete"; pass e.g. "Revoke" where that reads
   *  better for the operator. */
  confirmLabel?: ReactNode;
}

export function ConfirmDeleteModal({
  title,
  children,
  onConfirm,
  errorFallback,
  onClose,
  onDone,
  confirmLabel,
}: Props) {
  const { t } = useTranslation('common');
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const submit = () => {
    setBusy(true);
    setError(null);
    onConfirm()
      .then(() => onDone())
      .catch((e: unknown) => {
        setError(errMsg(e, errorFallback));
        setBusy(false);
      });
  };

  return (
    <Modal
      title={title}
      onClose={onClose}
      footer={
        <>
          <Button variant="outline" onClick={onClose} disabled={busy}>
            {t('actions.cancel')}
          </Button>
          <Button variant="danger" onClick={submit} disabled={busy}>
            {confirmLabel ?? t('actions.delete')}
          </Button>
        </>
      }
    >
      <p className="modal-confirm-text">{children}</p>
      {error && <p className="form-error">{error}</p>}
    </Modal>
  );
}
