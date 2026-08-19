// SPDX-License-Identifier: AGPL-3.0-only
// Multi-select picker for stored credentials, rendered as removable chips. Clicking the
// field-styled control opens a dropdown of stored SNMP credentials; selecting toggles
// membership and the choice appears as a tag (with an ✕ to deselect). The dropdown footer
// links to Credentials & secrets so a new credential can be added without leaving the flow.
// Popover open/close + click-outside follows the UserMenu pattern.

import { useEffect, useRef, useState } from 'react';
import { Link } from 'react-router-dom';
import { useTranslation } from 'react-i18next';
import type { CredentialSummary } from '../../types/api';
import './CredentialPicker.css';

interface CredentialPickerProps {
  /** Credentials offered for selection (already filtered to scan-capable kinds). */
  options: CredentialSummary[];
  /** Currently selected credential ids. */
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

  const toggle = (id: string) =>
    onChange(selected.includes(id) ? selected.filter((x) => x !== id) : [...selected, id]);
  const remove = (id: string) => onChange(selected.filter((x) => x !== id));

  // Render chips in the picker's own option order so they stay stable as selection changes.
  const chosen = options.filter((o) => selected.includes(o.id));

  return (
    <div className="cred-picker" ref={ref}>
      <button
        type="button"
        className="cred-picker-control field"
        disabled={disabled}
        aria-haspopup="listbox"
        aria-expanded={open}
        onClick={() => !disabled && setOpen((o) => !o)}
      >
        {chosen.length === 0 ? (
          <span className="cred-picker-placeholder">{t('credPicker.placeholder')}</span>
        ) : (
          <span className="cred-picker-chips">
            {chosen.map((c) => (
              <span className="cred-chip" key={c.id}>
                <span className="cred-chip-name">{c.name}</span>
                <span className="cred-chip-kind mono">{c.kind}</span>
                <span
                  className="cred-chip-x"
                  role="button"
                  tabIndex={-1}
                  aria-label={t('credPicker.remove', { name: c.name })}
                  onClick={(e) => {
                    e.stopPropagation();
                    remove(c.id);
                  }}
                >
                  ✕
                </span>
              </span>
            ))}
          </span>
        )}
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
  );
}
