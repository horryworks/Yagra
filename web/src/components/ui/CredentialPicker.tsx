// SPDX-License-Identifier: AGPL-3.0-only
// Picker for the stored credentials a sweep or a backup tries, in the order it tries them
// (ADR-205 Inc.3, Inc.7, ADR-255).
//
// Every offered credential is one row with a checkbox, in one list. A row stays where it is when
// it is ticked or unticked, and every row is dragged by the ⠿ at its left, so the list can be put
// in order before anything is ticked. What is tried is the ticked rows read top to bottom,
// numbered in that order beside the grip. The order of the unticked rows is this screen's alone:
// it is not sent, so reopening the screen draws the selection on top again. Each caller puts its
// own link to Credentials & secrets under the picker, where a new credential is added.

import { useState, type ReactNode } from 'react';
import { useTranslation } from 'react-i18next';
import type { CredentialSummary } from '../../types/api';
import { arrangeRows, selectionOf, toggleRow } from './credentialSelection';
import { DragGrip, SortableItem, SortableList } from './Sortable';
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
  const nameOf = (id: string) => byId.get(id)?.name ?? id;

  const toggle = (id: string) => {
    setHeld(rows);
    onChange(toggleRow(rows, selected, id));
  };
  const reorder = (next: string[]) => {
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
        <SortableList ids={rows} onReorder={reorder} nameOf={nameOf}>
          <ul className="cred-picker-list">
            {rows.map((id) => {
              const c = byId.get(id);
              if (!c) return null;
              const on = ticked.has(id);
              const place = on ? ++n : null;
              return (
                <SortableItem
                  key={id}
                  id={id}
                  as="li"
                  disabled={disabled}
                  className={on ? 'cred-picker-row' : 'cred-picker-row unused'}
                >
                  <DragGrip name={c.name} />
                  <span className="cred-picker-row-n mono">{place}</span>
                  <label className="cred-picker-pick">
                    <input
                      type="checkbox"
                      checked={on}
                      disabled={disabled}
                      aria-label={t('credPicker.use', { name: c.name })}
                      onChange={() => toggle(id)}
                    />
                    <span className="cred-picker-row-name" title={c.name}>
                      {c.name}
                    </span>
                    <span className="cred-picker-row-kind mono">{c.kind}</span>
                  </label>
                  {rowStatus && on && <span className="cred-picker-row-status">{rowStatus(id)}</span>}
                </SortableItem>
              );
            })}
          </ul>
        </SortableList>
      )}
    </div>
  );
}
