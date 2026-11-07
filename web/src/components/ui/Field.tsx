// SPDX-License-Identifier: AGPL-3.0-only
// Shared form controls (§4: forms/tables are common components). TextInput and Select share
// one stylesheet so every form field looks identical. Focus = accent border (not outline),
// per ui-conventions interactive states.

import {
  cloneElement,
  Fragment,
  isValidElement,
  useId,
  type InputHTMLAttributes,
  type ReactElement,
  type ReactNode,
  type Ref,
  type SelectHTMLAttributes,
  type TextareaHTMLAttributes,
} from 'react';
import { useTranslation } from 'react-i18next';
import { InfoTip } from './InfoTip';
import { isShown } from './space';
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
 * One labelled field in a dialog or form - the only way a form draws one since ADR-246, inside
 * `Fields` / `FieldRow` (FormLayout.tsx). The label is a sibling of the control, tied by `htmlFor`,
 * so the ⓘ button beside it is not inside a `<label>` — a button there becomes the label's control
 * and the field loses its name.
 *
 * `htmlFor` may be left out: the field then makes an id and gives it to its one child, so the label
 * names the control without the screen inventing ids (137 of 160 hand-written labels named nothing
 * when this was measured). A child that already has an `id` keeps it; a fragment or a list of
 * children gets none, so pass `htmlFor` and put that id on the control. `status` is a line the
 * field shows because of its state (the value in force, a count, "Loading"), under the control;
 * `error` is a validation message, shown in the critical colour.
 *
 * It still carries `.modal-field` until ADR-246 Inc.8, so a dialog not yet moved onto `Fields`
 * keeps its spacing.
 */
export function Field({
  label,
  htmlFor,
  required,
  infoKey,
  infoLabel,
  action,
  status,
  error,
  children,
}: {
  /** Plain words almost always; markup only where the label must emphasise a part of itself. */
  label: ReactNode;
  htmlFor?: string;
  required?: boolean;
  /** A namespaced `.info` key (`settings-tokens:field.owner.info`). */
  infoKey?: string;
  /** What the ⓘ is about, when `label` is not plain text. */
  infoLabel?: string;
  /** A small button or link at the right end of the label row ("Use built-in", "Default order"). */
  action?: ReactNode;
  status?: ReactNode;
  error?: string | null;
  children: ReactNode;
}) {
  const auto = useId();
  const child =
    isValidElement<{ id?: string }>(children) && children.type !== Fragment
      ? (children as ReactElement<{ id?: string }>)
      : null;
  const ownId = child?.props.id;
  const id = htmlFor ?? ownId ?? auto;
  const control = child && !htmlFor && !ownId ? cloneElement(child, { id }) : children;
  return (
    <div className="ui-field modal-field">
      <div className="field-head">
        <label className="modal-field-label" htmlFor={id}>
          {label}
          {required && <RequiredMark />}
        </label>
        {infoKey && (
          <InfoTip infoKey={infoKey} label={infoLabel ?? (typeof label === 'string' ? label : '')} />
        )}
        {isShown(action) && <span className="field-head-action">{action}</span>}
      </div>
      {control}
      {isShown(status) && <p className="form-status">{status}</p>}
      {error && (
        <p className="field-error" role="alert">
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

/** A validation message under a field, in the critical colour — for a field that is not on
 *  {@link Field}, whose `error` prop draws the same line.
 *
 *  There is deliberately no muted variant: a standing sentence under a field is what ADR-200
 *  removed (a format goes in the box as a suffix or a placeholder, a pointer becomes a
 *  `ScreenLink`, an explanation that changes a decision becomes an ⓘ). A line that appears because
 *  of the form's state or input — a count, the value in force — is `<p className="form-status">`,
 *  and `proseBudget.test.ts` (G10) refuses the old hint classes. */
export function FieldError({ children }: { children: ReactNode }) {
  return <span className="field-error">{children}</span>;
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
