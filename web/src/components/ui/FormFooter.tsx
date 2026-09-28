// SPDX-License-Identifier: AGPL-3.0-only
// A dialog's two buttons and its error line (ADR-184 increment 35).
//
// Thirty-odd dialogs wrote the same footer by hand — Cancel disabled while busy, the submit
// disabled while busy or invalid — and the error line under the fields. The copies had drifted:
// two batch dialogs kept "Cancel" after part of the batch had landed, and almost no error line
// carried `role="alert"`, so a screen reader never heard why a save failed.
//
// Deliberately NOT a whole-dialog wrapper: dialog bodies are laid out two ways (`.form-stack` and
// `.modal-field`), and a wrapper that added the error line would move it in one of them.
// `FormError` goes where the dialog's error paragraph already was.
//
// `lib/submitState.test.ts` fails the build for a footer written by hand anywhere else.

import type { ReactNode } from 'react';
import { useTranslation } from 'react-i18next';
import type { SubmitState } from '../../lib/submitState';
import { Button } from './Button';

interface FooterProps {
  form: Pick<SubmitState, 'busy' | 'settled'>;
  onClose: () => void;
  onSubmit: () => void;
  submitLabel: ReactNode;
  /** Shown on the submit button while the request is in flight, where a dialog had one. */
  busyLabel?: ReactNode;
  /** False while the fields are not ready to send. */
  canSubmit?: boolean;
  variant?: 'primary' | 'danger';
  /** Drawn before the two buttons (a "Test connection" beside Save). */
  extra?: ReactNode;
}

export function FormFooter({
  form,
  onClose,
  onSubmit,
  submitLabel,
  busyLabel,
  canSubmit = true,
  variant = 'primary',
  extra,
}: FooterProps) {
  const { t } = useTranslation('common');
  return (
    <>
      {extra}
      <Button variant="outline" onClick={onClose} disabled={form.busy}>
        {form.settled ? t('actions.close') : t('actions.cancel')}
      </Button>
      <Button variant={variant} onClick={onSubmit} disabled={form.busy || !canSubmit}>
        {form.busy && busyLabel !== undefined ? busyLabel : submitLabel}
      </Button>
    </>
  );
}

/** The dialog's error line — announced, so a screen reader hears why the save failed (F5). */
export function FormError({ form }: { form: Pick<SubmitState, 'error'> }) {
  if (!form.error) return null;
  return (
    <p className="form-error" role="alert">
      {form.error}
    </p>
  );
}
