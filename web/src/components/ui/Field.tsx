// Shared form controls (§4: forms/tables are common components). TextInput and Select share
// one stylesheet so every form field looks identical. Focus = accent border (not outline),
// per ui-conventions interactive states.

import type { InputHTMLAttributes, ReactNode, SelectHTMLAttributes } from 'react';
import './Field.css';

export function TextInput({ className, ...rest }: InputHTMLAttributes<HTMLInputElement>) {
  return <input className={['field', className].filter(Boolean).join(' ')} {...rest} />;
}

/** Required-field marker: a red asterisk that also announces "required" to assistive tech.
 *  Use next to the label text of a mandatory field so it's obvious before submit. */
export function RequiredMark() {
  return (
    <abbr className="form-req" title="Required" aria-label="required">
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
