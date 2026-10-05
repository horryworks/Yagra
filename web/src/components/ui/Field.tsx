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
import { InfoTip } from './InfoTip';
import './Field.css';

/** `inputRef` reaches the element itself, the way `SearchField` takes it: React 18 does not pass
 *  `ref` through a function component's props.
 *
 *  `suffix` is the unit written inside the box's right edge (`msg/s`, `readings`) — where a format
 *  or a unit goes instead of a sentence under the field (ADR-200). Without it the DOM is the bare
 *  `<input>`, exactly as before. */
export function TextInput({
  className,
  inputRef,
  suffix,
  ...rest
}: InputHTMLAttributes<HTMLInputElement> & { inputRef?: Ref<HTMLInputElement>; suffix?: string }) {
  const input = (
    <input ref={inputRef} className={['field', className].filter(Boolean).join(' ')} {...rest} />
  );
  if (suffix === undefined) return input;
  return (
    <span className="field-affix">
      {input}
      <span className="field-suffix">{suffix}</span>
    </span>
  );
}

/**
 * One labelled field in a dialog or form, for a field that carries an ⓘ or its own error line
 * (ADR-200). The label is a sibling of the control, tied by `htmlFor`, so the ⓘ button beside it is
 * not inside a `<label>` — a button there becomes the label's control and the field loses its name.
 *
 * Only fields that need one of the two move onto this; the plain `modal-field` markup elsewhere is
 * not a backlog. `error` is a validation message for this field, shown in the critical colour.
 */
export function Field({
  label,
  htmlFor,
  required,
  infoKey,
  error,
  children,
}: {
  label: string;
  htmlFor: string;
  required?: boolean;
  /** A namespaced `.info` key (`settings-tokens:field.owner.info`). */
  infoKey?: string;
  error?: string | null;
  children: ReactNode;
}) {
  return (
    <div className="modal-field">
      <div className="field-head">
        <label className="modal-field-label" htmlFor={htmlFor}>
          {label}
          {required && <RequiredMark />}
        </label>
        {infoKey && <InfoTip infoKey={infoKey} label={label} />}
      </div>
      {children}
      {error && (
        <p className="form-hint form-hint-error" role="alert">
          {error}
        </p>
      )}
    </div>
  );
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
