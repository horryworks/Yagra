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
import { TextArea, TextInput } from './Field';
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
  rows,
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
  /** A multi-line secret (a service-account key file): the box is a text area of this many rows.
   *  It is not masked — a browser has no masked text area — which is why it starts closed whenever
   *  a value is stored, like every other secret here. */
  rows?: number;
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

  // Replace is the press that asked for the box, so the box takes the focus it was asked for.
  const focus = autoFocus || mode.kind === 'replace';
  const box =
    rows === undefined ? (
      <TextInput
        id={id}
        className="mono"
        type="password"
        autoComplete="new-password"
        value={value}
        disabled={disabled}
        placeholder={placeholder}
        autoFocus={focus}
        onChange={(e) => onChange(e.target.value)}
      />
    ) : (
      <TextArea
        id={id}
        className="mono"
        rows={rows}
        autoComplete="off"
        spellCheck={false}
        value={value}
        disabled={disabled}
        placeholder={placeholder}
        autoFocus={focus}
        onChange={(e) => onChange(e.target.value)}
      />
    );
  if (mode.kind === 'new' || !mode.canKeep) return box;
  return (
    <span className={rows === undefined ? 'secret-input' : 'secret-input secret-input-multi'}>
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
