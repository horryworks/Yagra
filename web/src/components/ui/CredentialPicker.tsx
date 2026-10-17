// SPDX-License-Identifier: AGPL-3.0-only
// Picker for the stored credentials a sweep tries, in the order it tries them. The field-styled
// control opens a dropdown of stored SNMP credentials; selecting toggles membership. What is
// selected is listed under the control, numbered in the try order, each with ↑ / ↓ to move it and
// ✕ to drop it (ADR-205 Inc.2). The dropdown footer links to Credentials & secrets so a new
// credential can be added without leaving the flow.
// Popover open/close + click-outside follows the UserMenu pattern.

import { useEffect, useRef, useState } from 'react';
import { Link } from 'react-router-dom';
import { useTranslation } from 'react-i18next';
import type { CredentialSummary } from '../../types/api';
import { moveSelected, toggleSelection } from './credentialSelection';
import './CredentialPicker.css';

interface CredentialPickerProps {
  /** Credentials offered for selection (already filtered to scan-capable kinds). */
  options: CredentialSummary[];
  /** The selected credential ids, in the order they are tried. */
  selected: string[];
  onChange: (ids: string[]) => void;
  disabled?: boolean;
}

export function CredentialPicker({ options, selected, onChange, disabled }: CredentialPickerProps) {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);

  // Same dismissal contract as `UserMenu` / `OverflowMenu`: gated on `open`, and Escape closes.
  // Both were added by ADR-073 — the chips already had their own ✕, so what could not be dismissed
  // was the dropdown itself, and only by keyboard.
  useEffect(() => {
    if (!open) return;
    const onDown = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) setOpen(false);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') setOpen(false);
    };
    document.addEventListener('mousedown', onDown);
    document.addEventListener('keydown', onKey);
    return () => {
      document.removeEventListener('mousedown', onDown);
      document.removeEventListener('keydown', onKey);
    };
  }, [open]);

  const toggle = (id: string) => onChange(toggleSelection(options, selected, id));
  const remove = (id: string) => onChange(selected.filter((x) => x !== id));
  const move = (id: string, delta: -1 | 1) => onChange(moveSelected(selected, id, delta));

  // Drawn from `selected` itself, in its order: what the operator reads top to bottom is the order
  // the sweep tries the credentials in (ADR-161, ADR-205 Inc.2). An id no longer offered is not
  // drawn — it cannot be sent either (`toggleSelection` drops it).
  const byId = new Map(options.map((o) => [o.id, o]));
  const chosen = selected.flatMap((id) => {
    const c = byId.get(id);
    return c ? [c] : [];
  });

  return (
    <div className="cred-picker">
      <div className="cred-picker-anchor" ref={ref}>
        <button
          type="button"
          className="cred-picker-control field"
          disabled={disabled}
          aria-haspopup="listbox"
          aria-expanded={open}
          onClick={() => !disabled && setOpen((o) => !o)}
        >
          <span className={chosen.length === 0 ? 'cred-picker-placeholder' : 'cred-picker-summary'}>
            {chosen.length === 0
              ? t('credPicker.placeholder')
              : t('credPicker.selected', { count: chosen.length })}
          </span>
          <span className="cred-picker-caret" aria-hidden="true">
            ▾
          </span>
        </button>

        {open && (
          <div className="cred-picker-pop" role="listbox">
            {options.length === 0 ? (
              <div className="cred-picker-empty">{t('credPicker.empty')}</div>
            ) : (
              <div className="cred-picker-list">
                {options.map((o) => {
                  const on = selected.includes(o.id);
                  return (
                    <button
                      type="button"
                      key={o.id}
                      role="option"
                      aria-selected={on}
                      className={on ? 'cred-picker-option selected' : 'cred-picker-option'}
                      onClick={() => toggle(o.id)}
                    >
                      <span className="cred-picker-check" aria-hidden="true">
                        {on ? '✓' : ''}
                      </span>
                      <span className="cred-picker-opt-name">{o.name}</span>
                      <span className="cred-picker-opt-kind mono">{o.kind}</span>
                    </button>
                  );
                })}
              </div>
            )}
            <Link className="cred-picker-manage" to="/nodes/credentials">
              {t('credPicker.manage')} →
            </Link>
          </div>
        )}
      </div>

      {chosen.length > 0 && (
        <ol className="cred-picker-order">
          {chosen.map((c, i) => (
            <li className="cred-picker-row" key={c.id}>
              <span className="cred-picker-row-n mono">{i + 1}</span>
              <span className="cred-picker-row-name">{c.name}</span>
              <span className="cred-picker-row-kind mono">{c.kind}</span>
              {/* An arrow with nowhere to go is not drawn, but keeps its place so the column of
                  arrows stays aligned. */}
              <button
                type="button"
                className="cred-picker-row-btn"
                aria-label={t('credPicker.moveUp', { name: c.name })}
                disabled={disabled}
                style={i === 0 ? { visibility: 'hidden' } : undefined}
                onClick={() => move(c.id, -1)}
              >
                ↑
              </button>
              <button
                type="button"
                className="cred-picker-row-btn"
                aria-label={t('credPicker.moveDown', { name: c.name })}
                disabled={disabled}
                style={i === chosen.length - 1 ? { visibility: 'hidden' } : undefined}
                onClick={() => move(c.id, 1)}
              >
                ↓
              </button>
              <button
                type="button"
                className="cred-picker-row-btn cred-picker-row-x"
                aria-label={t('credPicker.remove', { name: c.name })}
                disabled={disabled}
                onClick={() => remove(c.id)}
              >
                ✕
              </button>
            </li>
          ))}
        </ol>
      )}
    </div>
  );
}
