// SPDX-License-Identifier: AGPL-3.0-only
// Picker for the stored credentials a sweep tries, in the order it tries them (ADR-205 Inc.3).
//
// Every offered credential is on screen at once, each with a checkbox: the ones in use first,
// numbered in the try order with ↑ / ↓ to move them, then the ones not in use, in the order offered.
// Ticking one adds it to the end of the order; unticking it moves it back down. There is no dropdown
// any more — "press the field to add one" was the step nobody could see. Each caller puts its own
// link to Credentials & secrets under the picker, where a new credential is added.

import { useTranslation } from 'react-i18next';
import type { CredentialSummary } from '../../types/api';
import { moveSelected, splitSelection, toggleSelection } from './credentialSelection';
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
  const toggle = (id: string) => onChange(toggleSelection(options, selected, id));
  const move = (id: string, delta: -1 | 1) => onChange(moveSelected(selected, id, delta));
  // Drawn from `selected` itself, in its order: what the operator reads top to bottom is the order
  // the sweep tries the credentials in (ADR-161, ADR-205 Inc.2).
  const { used, unused } = splitSelection(options, selected);

  return (
    <div className="cred-picker">
      {options.length === 0 ? (
        <p className="cred-picker-empty">{t('credPicker.empty')}</p>
      ) : (
        <ul className="cred-picker-list">
          {used.map((c, i) => (
            <li className="cred-picker-row" key={c.id}>
              <label className="cred-picker-pick">
                <input
                  type="checkbox"
                  checked
                  disabled={disabled}
                  aria-label={t('credPicker.use', { name: c.name })}
                  onChange={() => toggle(c.id)}
                />
                <span className="cred-picker-row-n mono">{i + 1}</span>
                <span className="cred-picker-row-name" title={c.name}>
                  {c.name}
                </span>
                <span className="cred-picker-row-kind mono">{c.kind}</span>
              </label>
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
                style={i === used.length - 1 ? { visibility: 'hidden' } : undefined}
                onClick={() => move(c.id, 1)}
              >
                ↓
              </button>
            </li>
          ))}
          {unused.map((c) => (
            <li className="cred-picker-row unused" key={c.id}>
              <label className="cred-picker-pick">
                <input
                  type="checkbox"
                  checked={false}
                  disabled={disabled}
                  aria-label={t('credPicker.use', { name: c.name })}
                  onChange={() => toggle(c.id)}
                />
                <span className="cred-picker-row-n mono" />
                <span className="cred-picker-row-name" title={c.name}>
                  {c.name}
                </span>
                <span className="cred-picker-row-kind mono">{c.kind}</span>
              </label>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
