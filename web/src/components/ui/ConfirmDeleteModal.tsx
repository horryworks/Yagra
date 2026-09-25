// SPDX-License-Identifier: AGPL-3.0-only
// Destructive-consent dialog (ui-conventions §Modals case 2). Fifteen pages had each grown their
// own DeleteXModal — same busy/error state, same Cancel + Danger footer, same `.catch` wiring, ~45
// lines apiece — so a change to the confirm chrome meant fifteen edits and the copies had already
// drifted (some disabled Cancel while busy, some did not). This owns that scaffold; a caller
// supplies only what is actually specific: the title, the sentence naming the target, and the call.

import { useState, type ReactNode } from 'react';
import { Trans, useTranslation } from 'react-i18next';
import { isImeComposing } from '../../lib/ime';
import { errMsg } from '../../services/api';
import { Button } from './Button';
import { confirmPhraseMatches } from './confirmPhrase';
import { TextInput } from './Field';
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
  /** When set, the confirm button stays disabled until the operator types exactly this (ADR-174).
   *  For a deletion whose reach is larger than the row it was started from — a folder that takes
   *  every folder and node beneath it. Leave it unset for an ordinary one-row delete. */
  confirmPhrase?: string;
}

export function ConfirmDeleteModal({
  title,
  children,
  onConfirm,
  errorFallback,
  onClose,
  onDone,
  confirmLabel,
  confirmPhrase,
}: Props) {
  const { t } = useTranslation('common');
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [typed, setTyped] = useState('');
  const confirmed = confirmPhrase === undefined || confirmPhraseMatches(typed, confirmPhrase);

  const submit = () => {
    if (!confirmed) return;
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
          <Button variant="danger" onClick={submit} disabled={busy || !confirmed}>
            {confirmLabel ?? t('actions.delete')}
          </Button>
        </>
      }
    >
      <p className="modal-confirm-text">{children}</p>
      {confirmPhrase !== undefined && (
        <label className="form-label">
          <span>
            <Trans
              t={t}
              i18nKey="confirmDelete.typeToConfirm"
              values={{ phrase: confirmPhrase }}
              components={{ b: <strong /> }}
            />
          </span>
          <TextInput
            value={typed}
            onChange={(e) => setTyped(e.target.value)}
            onKeyDown={(e) => {
              // A folder name is often Japanese: Enter that commits an IME candidate must not delete.
              if (e.key === 'Enter' && !isImeComposing(e)) submit();
            }}
            disabled={busy}
            autoFocus
            autoComplete="off"
            spellCheck={false}
          />
        </label>
      )}
      {error && <p className="form-error">{error}</p>}
    </Modal>
  );
}
