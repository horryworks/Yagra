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

import { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Button } from './Button';
import { TextArea, TextInput } from './Field';
import { secretMode, staysReplacing } from './secretField';
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
  replacing: replacingProp,
  onReplacingChange,
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
  /** Controlled Replace state, for a form that draws more than the box while a secret is being
   *  replaced (a credential's type, an SNMPv3 sub-form). Omit both to let the field keep its own. */
  replacing?: boolean;
  onReplacingChange?: (replacing: boolean) => void;
}) {
  const { t } = useTranslation();
  const [ownReplacing, setOwnReplacing] = useState(false);
  const replacing = replacingProp ?? ownReplacing;
  const setReplacing = (next: boolean) => {
    if (replacingProp === undefined) setOwnReplacing(next);
    onReplacingChange?.(next);
  };
  const mode = secretMode(stored, replacing, mustReplace);
  // Whether the box was opened by a press of Replace. Only that press may move the focus: the box
  // also opens by itself when `mustReplace` turns on — on a NetBox edit, while the operator is
  // still typing the new address — and taking the focus then sent the rest of the address into
  // the token box (ADR-178 decision 3).
  const [pressedReplace, setPressedReplace] = useState(false);

  // Once forced open, the box stays open with "Keep stored" when `mustReplace` turns off again
  // (the address typed back to its old host). Snapping back to the stored mark would hide a typed
  // value that Save still sends.
  useEffect(() => {
    if (staysReplacing(stored, replacing, mustReplace) && !replacing) setReplacing(true);
    // `setReplacing` is a fresh closure every render; the decision depends only on these three.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [stored, replacing, mustReplace]);

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
            setPressedReplace(true);
            setReplacing(true);
          }}
        >
          {t('secret.replace')}
        </Button>
      </span>
    );
  }

  // Replace is the press that asked for the box, so the box takes the focus it was asked for.
  // `autoFocus` acts only when the box mounts, which is why the box below never remounts while it
  // stays open (the Keep button comes and goes beside it instead).
  const focus = autoFocus || pressedReplace;
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
  if (mode.kind === 'new') return box;
  // One wrapper whether or not "Keep stored" is offered: switching between a bare box and a wrapped
  // one would remount the input, dropping the focus and the caret of whoever is typing in it.
  return (
    <span className={rows === undefined ? 'secret-input' : 'secret-input secret-input-multi'}>
      {box}
      {mode.canKeep && (
        <Button
          type="button"
          disabled={disabled}
          onClick={() => {
            onChange('');
            setPressedReplace(false);
            setReplacing(false);
          }}
        >
          {t('secret.keep')}
        </Button>
      )}
    </span>
  );
}
