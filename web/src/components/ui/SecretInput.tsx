// SPDX-License-Identifier: AGPL-3.0-only
// A field for a secret the server keeps and never returns — an API key, a token, a password
// (ADR-200 §5.2).
//
// Every screen with one used to explain it underneath: "stored encrypted, never shown again",
// "leave empty to keep the stored token". The field says it instead. A stored value is a masked
// mark, the word "Stored" and a Replace button; Replace opens an empty box with "Keep stored" to go
// back. An empty box always means "keep what is stored" (`secretToSend`).
//
// What to draw is decided in `secretField.ts`, where a test reaches it.

import { useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Button } from './Button';
import { TextInput } from './Field';
import { secretMode } from './secretField';
import './SecretInput.css';

export function SecretInput({
  id,
  stored,
  value,
  onChange,
  mustReplace = false,
  disabled,
  placeholder,
  autoFocus,
}: {
  id?: string;
  /** Whether the server holds a value for this field already. */
  stored: boolean;
  value: string;
  onChange: (value: string) => void;
  /** The stored value may not be used any more: open the box, with no way back to it. */
  mustReplace?: boolean;
  disabled?: boolean;
  placeholder?: string;
  autoFocus?: boolean;
}) {
  const { t } = useTranslation();
  const [replacing, setReplacing] = useState(false);
  const mode = secretMode(stored, replacing, mustReplace);

  if (mode.kind === 'stored') {
    return (
      <span className="secret-input secret-input-stored">
        <span className="secret-input-mask mono" aria-hidden="true">
          ●●●●●●●●
        </span>
        <span className="secret-input-state">{t('secret.stored')}</span>
        {/* No `id` here: a `<label for>` pointing at this button would rename it to the field's
            label, and "API token" is not what pressing it does. */}
        <Button
          type="button"
          disabled={disabled}
          onClick={() => {
            onChange('');
            setReplacing(true);
          }}
        >
          {t('secret.replace')}
        </Button>
      </span>
    );
  }

  const box = (
    <TextInput
      id={id}
      className="mono"
      type="password"
      autoComplete="new-password"
      value={value}
      disabled={disabled}
      placeholder={placeholder}
      // Replace is the press that asked for the box, so the box takes the focus it was asked for.
      autoFocus={autoFocus || mode.kind === 'replace'}
      onChange={(e) => onChange(e.target.value)}
    />
  );
  if (mode.kind === 'new' || !mode.canKeep) return box;
  return (
    <span className="secret-input">
      {box}
      <Button
        type="button"
        disabled={disabled}
        onClick={() => {
          onChange('');
          setReplacing(false);
        }}
      >
        {t('secret.keep')}
      </Button>
    </span>
  );
}
