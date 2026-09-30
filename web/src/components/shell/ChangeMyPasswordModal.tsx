// SPDX-License-Identifier: AGPL-3.0-only
// Change-your-own-password dialog, opened from the account badge (ADR-122).
//
// It sits beside `PreferencesModal` for the same reason that one does: the account badge is "the
// shelf that is only mine", and this is the only thing on it that writes to the server.
//
// 🚨 **A successful change signs the operator out.** The API revokes every session of the account,
// this one included (ADR-122 decision 3), so there is no "saved" state to return to — the dialog's
// success path is a navigation to /login, not a toast. The copy says so *before* the button is
// pressed, because being ejected from the screen you were reading is not something to discover
// afterwards.
//
// All of the judgement lives in `lib/password.ts` — Vitest never loads a `.tsx`, so a rule written
// here is a rule nothing runs (`.claude/rules/testing.md`).

import { useState } from 'react';
import { useTranslation } from 'react-i18next';
import { api } from '../../services/api';
import { done } from '../../lib/submitState';
import { useSubmit } from '../../lib/useSubmit';
import { TextInput } from '../ui/Field';
import { FormError, FormFooter } from '../ui/FormFooter';
import { Modal } from '../ui/Modal';
import { MIN_PW, validateOwnPasswordChange } from '../../lib/password';

interface Props {
  onClose: () => void;
  /** Called after the server has accepted the change and revoked every session. */
  onChanged: () => void;
}

export function ChangeMyPasswordModal({ onClose, onChanged }: Props) {
  const { t } = useTranslation('settings');
  const [current, setCurrent] = useState('');
  const [next, setNext] = useState('');
  const [confirm, setConfirm] = useState('');
  const form = useSubmit({ errorFallback: t('password.err.failed'), onDone: onChanged });

  const problem = validateOwnPasswordChange({ current, next, confirm });
  // Which problems are worth saying out loud while the operator is still typing. A mismatch against
  // an empty confirmation box is not a mistake yet, and "the same as the current one" is not one
  // either until the new password is long enough to be a candidate.
  const inlineProblem =
    problem === 'unchanged' || (problem === 'mismatch' && confirm.length > 0) ? problem : null;

  const submit = () => {
    if (problem !== null) return;
    form.submit(() => api.changeMyPassword(current, next).then(() => done()));
  };

  return (
    <Modal
      title={t('password.title')}
      onClose={onClose}
      footer={
        <FormFooter
          form={form}
          onClose={onClose}
          onSubmit={submit}
          submitLabel={t('password.submit')}
          canSubmit={problem === null}
        />
      }
    >
      <p className="pref-note muted">{t('password.note')}</p>
      <div className="modal-field">
        <label className="modal-field-label">{t('password.current')}</label>
        <TextInput
          type="password"
          value={current}
          onChange={(e) => setCurrent(e.target.value)}
          autoComplete="current-password"
          autoFocus
        />
      </div>
      <div className="modal-field">
        <label className="modal-field-label">{t('password.next', { min: MIN_PW })}</label>
        <TextInput
          type="password"
          value={next}
          onChange={(e) => setNext(e.target.value)}
          autoComplete="new-password"
        />
      </div>
      <div className="modal-field">
        <label className="modal-field-label">{t('password.confirm')}</label>
        <TextInput
          type="password"
          value={confirm}
          onChange={(e) => setConfirm(e.target.value)}
          autoComplete="new-password"
        />
      </div>
      {inlineProblem === 'mismatch' && <p className="form-error">{t('password.err.mismatch')}</p>}
      {inlineProblem === 'unchanged' && <p className="form-error">{t('password.err.unchanged')}</p>}
      <FormError form={form} />
    </Modal>
  );
}
