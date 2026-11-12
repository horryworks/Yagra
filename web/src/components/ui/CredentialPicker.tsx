// SPDX-License-Identifier: AGPL-3.0-only
// Picker for the stored credentials a sweep or a backup tries, in the order it tries them
// (ADR-205 Inc.3, Inc.7).
//
// Every offered credential is one row with a checkbox, in one list. A row stays where it is when
// it is ticked or unticked, and every row has ↑ / ↓, so the list can be put in order before
// anything is ticked. What is tried is the ticked rows read top to bottom, numbered in that order.
// The order of the unticked rows is this screen's alone: it is not sent, so reopening the screen
// draws the selection on top again. There is no dropdown — "press the field to add one" was the
// step nobody could see. Each caller puts its own link to Credentials & secrets under the picker,
// where a new credential is added.

import { useState, type ReactNode } from 'react';
import { useTranslation } from 'react-i18next';
import type { CredentialSummary } from '../../types/api';
import { arrangeRows, moveRow, selectionOf, toggleRow } from './credentialSelection';
import './CredentialPicker.css';

interface CredentialPickerProps {
  /** Credentials offered for selection (already filtered to scan-capable kinds). */
  options: CredentialSummary[];
  /** The selected credential ids, in the order they are tried. */
  selected: string[];
  onChange: (ids: string[]) => void;
  disabled?: boolean;
  /** What happened when a selected credential was last tried, drawn at the right of its row
   *  (ADR-240 Inc.2 decision 10). Nothing is drawn where it answers null. */
  rowStatus?: (id: string) => ReactNode;
  /** The line shown when nothing is offered. Omitted: the SNMP one, for the sweeps. */
  emptyText?: string;
}

export function CredentialPicker({
  options,
  selected,
  onChange,
  disabled,
  rowStatus,
  emptyText,
}: CredentialPickerProps) {
  const { t } = useTranslation();
  // The rows as last drawn, so the next draw keeps them in place; null until the first press.
  const [held, setHeld] = useState<string[] | null>(null);
  const rows = arrangeRows(options, selected, held);
  const ticked = new Set(selected);
  const byId = new Map(options.map((c) => [c.id, c]));

  const toggle = (id: string) => {
    setHeld(rows);
    onChange(toggleRow(rows, selected, id));
  };
  const move = (id: string, delta: -1 | 1) => {
    const next = moveRow(rows, id, delta);
    setHeld(next);
    const order = selectionOf(next, ticked);
    // Moving past an unticked row changes where it is drawn, not what is tried.
    if (order.join() !== selected.join()) onChange(order);
  };

  let n = 0;
  return (
    <div className="cred-picker">
      {options.length === 0 ? (
        <p className="cred-picker-empty">{emptyText ?? t('credPicker.empty')}</p>
      ) : (
        <ul className="cred-picker-list">
          {rows.map((id, i) => {
            const c = byId.get(id);
            if (!c) return null;
            const on = ticked.has(id);
            const place = on ? ++n : null;
            return (
              <li className={on ? 'cred-picker-row' : 'cred-picker-row unused'} key={id}>
                <label className="cred-picker-pick">
                  <input
                    type="checkbox"
                    checked={on}
                    disabled={disabled}
                    aria-label={t('credPicker.use', { name: c.name })}
                    onChange={() => toggle(id)}
                  />
                  <span className="cred-picker-row-n mono">{place}</span>
                  <span className="cred-picker-row-name" title={c.name}>
                    {c.name}
                  </span>
                  <span className="cred-picker-row-kind mono">{c.kind}</span>
                </label>
                {rowStatus && on && <span className="cred-picker-row-status">{rowStatus(id)}</span>}
                {/* An arrow with nowhere to go is not drawn, but keeps its place so the column of
                    arrows stays aligned. */}
                <button
                  type="button"
                  className="cred-picker-row-btn"
                  aria-label={t('credPicker.moveUp', { name: c.name })}
                  disabled={disabled}
                  style={i === 0 ? { visibility: 'hidden' } : undefined}
                  onClick={() => move(id, -1)}
                >
                  ↑
                </button>
                <button
                  type="button"
                  className="cred-picker-row-btn"
                  aria-label={t('credPicker.moveDown', { name: c.name })}
                  disabled={disabled}
                  style={i === rows.length - 1 ? { visibility: 'hidden' } : undefined}
                  onClick={() => move(id, 1)}
                >
                  ↓
                </button>
              </li>
            );
          })}
        </ul>
      )}
    </div>
  );
}
