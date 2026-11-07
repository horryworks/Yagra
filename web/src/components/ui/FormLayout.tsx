// SPDX-License-Identifier: AGPL-3.0-only
// How the fields of a form are laid out (ADR-246 decision 2). `Field` (Field.tsx) is one labelled
// control; these arrange them:
//   Fields        fields one above the other, 12px apart - a dialog body, a card of settings
//   FieldRow      fields side by side, one above the other on a phone
//   CheckField    a checkbox and what it means
//   ChoiceGroup   one of a few options, each a card that can say what choosing it does
//   SettingGroup  rows that belong to one subject (a walk: its switch and its interval)
//   SettingRow    the name on the left, the control on the right - a deployment setting
//   SaveNote      "Saved" or why it was not, beside the button that saved
//
// Before these, a dialog's fields were spaced by one of two shared systems (`.modal-field` with a
// 14px margin, `.form-stack` with a 14px gap - four dialogs nested one in the other and got 28px)
// or by one of nine page-local copies at 4, 5 or 6px.

import { useId, type ReactNode } from 'react';
import { InfoTip } from './InfoTip';
import { cx, isShown } from './space';
import './FormLayout.css';

/** Fields one above the other (`stack`, the default), or each label beside its control
 *  (`columns`) - for a card of a dozen short values (an LDAP directory's DNs and attributes),
 *  which stacked would scroll a page. On a phone `columns` stacks too. A child that is not a field
 *  spans both columns. */
export function Fields({
  layout = 'stack',
  className,
  children,
}: {
  layout?: 'stack' | 'columns';
  className?: string;
  children: ReactNode;
}) {
  return <div className={cx('ui-fields', `is-${layout}`, className)}>{children}</div>;
}

/** A label for several controls that act together - a set of checkboxes, a list of rows - where
 *  `Field`'s `<label>` would name only one of them, or none. The controls are announced as a group
 *  under this name. Looks exactly like a `Field`. */
export function FieldGroup({
  label,
  infoKey,
  children,
}: {
  label: string;
  /** A namespaced `.info` key, as on `Field`. */
  infoKey?: string;
  children: ReactNode;
}) {
  const id = useId();
  return (
    <div className="ui-field modal-field" role="group" aria-labelledby={id}>
      <div className="field-head">
        <span className="modal-field-label" id={id}>
          {label}
        </span>
        {infoKey && <InfoTip infoKey={infoKey} label={label} />}
      </div>
      {children}
    </div>
  );
}

export function FieldRow({ children }: { children: ReactNode }) {
  return <div className="ui-field-row">{children}</div>;
}

/** A field's value where a control cannot go (an integration key's fixed kind): a control's
 *  height and inset, none of its chrome, so a form holding both still reads as one form. */
export function FieldValue({ children }: { children: ReactNode }) {
  return <span className="ui-field-value">{children}</span>;
}

export function CheckField({
  checked,
  onChange,
  label,
  detail,
  disabled,
  name,
}: {
  checked: boolean;
  onChange: (next: boolean) => void;
  label: ReactNode;
  /** What choosing it does, when the label cannot say it alone. */
  detail?: ReactNode;
  disabled?: boolean;
  name?: string;
}) {
  return (
    <label className={cx('ui-check', isShown(detail) && 'has-detail', disabled && 'is-disabled')}>
      <input
        type="checkbox"
        name={name}
        checked={checked}
        disabled={disabled}
        onChange={(e) => onChange(e.target.checked)}
      />
      <span className="ui-check-text">
        <span>{label}</span>
        {isShown(detail) && <span className="ui-check-detail">{detail}</span>}
      </span>
    </label>
  );
}

export interface Choice<T extends string> {
  value: T;
  label: ReactNode;
  /** What choosing it does. */
  detail?: ReactNode;
  /** The detail warns (it removes something, or cannot be undone). */
  danger?: boolean;
  disabled?: boolean;
}

export function ChoiceGroup<T extends string>({
  name,
  value,
  onChange,
  options,
  label,
}: {
  name: string;
  value: T | null;
  onChange: (next: T) => void;
  options: Choice<T>[];
  /** The group's accessible name. */
  label: string;
}) {
  return (
    <div className="ui-choices" role="radiogroup" aria-label={label}>
      {options.map((o) => (
        <label
          key={o.value}
          className={cx('ui-choice', o.value === value && 'is-checked', o.disabled && 'is-disabled')}
        >
          <input
            type="radio"
            name={name}
            value={o.value}
            checked={o.value === value}
            disabled={o.disabled}
            onChange={() => onChange(o.value)}
          />
          <span className="ui-choice-text">
            <span className="ui-choice-label">{o.label}</span>
            {isShown(o.detail) && (
              <span className={cx('ui-choice-detail', o.danger && 'is-danger')}>{o.detail}</span>
            )}
          </span>
        </label>
      ))}
    </div>
  );
}

export function SettingGroup({ children }: { children: ReactNode }) {
  return <div className="ui-setting-group">{children}</div>;
}

export function SettingRow({
  label,
  info,
  detail,
  warn,
  children,
}: {
  label: ReactNode;
  /** An `InfoTip`, drawn beside the name. */
  info?: ReactNode;
  /** What the value in force means now ("every second") - never a standing explanation. */
  detail?: ReactNode;
  /** A danger, shown only while the chosen value carries it (ADR-200). */
  warn?: ReactNode;
  /** The control, with its unit and band. */
  children: ReactNode;
}) {
  return (
    <div className="ui-setting">
      <div className="ui-setting-label">
        <div className="ui-setting-name">
          {label}
          {info}
        </div>
        {isShown(detail) && <div className="ui-setting-detail">{detail}</div>}
        {isShown(warn) && <div className="ui-setting-warn">{warn}</div>}
      </div>
      <div className="ui-setting-control">{children}</div>
    </div>
  );
}

export function SaveNote({ tone, children }: { tone: 'ok' | 'error'; children: ReactNode }) {
  return (
    <p className={cx('ui-save-note', `is-${tone}`)} role={tone === 'error' ? 'alert' : 'status'}>
      {children}
    </p>
  );
}
