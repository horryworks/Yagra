// SPDX-License-Identifier: AGPL-3.0-only
// Shared form controls (§4: forms/tables are common components). TextInput and Select share
// one stylesheet so every form field looks identical. Focus = accent border (not outline),
// per ui-conventions interactive states.

import type {
  InputHTMLAttributes,
  ReactNode,
  Ref,
  SelectHTMLAttributes,
  TextareaHTMLAttributes,
} from 'react';
import { useTranslation } from 'react-i18next';
import './Field.css';

/** `inputRef` reaches the element itself, the way `SearchField` takes it: React 18 does not pass
 *  `ref` through a function component's props. */
export function TextInput({
  className,
  inputRef,
  ...rest
}: InputHTMLAttributes<HTMLInputElement> & { inputRef?: Ref<HTMLInputElement> }) {
  return <input ref={inputRef} className={['field', className].filter(Boolean).join(' ')} {...rest} />;
}

/** Multi-line variant of {@link TextInput} for pasted blocks (PEM certificates, scripts). Shares
 *  the same `.field` styling so it sits in a form identically. */
export function TextArea({
  className,
  inputRef,
  ...rest
}: TextareaHTMLAttributes<HTMLTextAreaElement> & { inputRef?: Ref<HTMLTextAreaElement> }) {
  return (
    <textarea ref={inputRef} className={['field', 'field-area', className].filter(Boolean).join(' ')} {...rest} />
  );
}

/** Required-field marker: a red asterisk that also announces "required" to assistive tech.
 *  Use next to the label text of a mandatory field so it's obvious before submit. */
export function RequiredMark() {
  const { t } = useTranslation();
  return (
    <abbr className="form-req" title={t('form.required')} aria-label={t('form.required')}>
      *
    </abbr>
  );
}

/** Small, muted helper text under a form field (format hints, validation messages). Pass
 *  `error` to render it in the critical color. */
export function FieldHint({ children, error }: { children: ReactNode; error?: boolean }) {
  return (
    <span className={error ? 'form-hint form-hint-error' : 'form-hint'}>{children}</span>
  );
}

export function Select({
  className,
  children,
  ...rest
}: SelectHTMLAttributes<HTMLSelectElement>) {
  return (
    <select className={['field', className].filter(Boolean).join(' ')} {...rest}>
      {children}
    </select>
  );
}
