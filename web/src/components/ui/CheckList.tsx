// SPDX-License-Identifier: AGPL-3.0-only
// A list of boxes, any number of which may be ticked, under one "select all" row in the same frame
// (ADR-256 Inc.3). Each row may carry a detail on the right (a count) and a mark (a warning badge).
// The list scrolls inside its frame, so a long one does not push the dialog's buttons away. It is
// named by the `FieldGroup` it sits in.

import type { ReactNode } from 'react';
import { useTranslation } from 'react-i18next';
import { SelectAllBox } from './SelectAllBox';
import { toggleOne } from '../../lib/selectAll';
import './CheckList.css';

export interface CheckItem {
  key: string;
  label: string;
  /** Quiet text at the row's right end. */
  detail?: ReactNode;
  /** Drawn after the label, for a state the row must say (a badge). */
  mark?: ReactNode;
}

interface Props {
  items: readonly CheckItem[];
  selected: ReadonlySet<string>;
  onChange: (next: Set<string>) => void;
}

export function CheckList({ items, selected, onChange }: Props) {
  const { t } = useTranslation('common');
  const keys = items.map((i) => i.key);
  return (
    <div className="checklist">
      <SelectAllBox visibleKeys={keys} selected={selected} onChange={onChange} className="checklist-row checklist-head">
        <span className="checklist-label">{t('select.all')}</span>
        <span className="checklist-detail mono">
          {t('select.count', { n: keys.filter((k) => selected.has(k)).length, total: keys.length })}
        </span>
      </SelectAllBox>
      <ul className="checklist-rows">
        {items.map((i) => (
          <li key={i.key}>
            <label className="checklist-row">
              <input
                type="checkbox"
                checked={selected.has(i.key)}
                onChange={(e) => onChange(toggleOne(selected, i.key, e.target.checked))}
              />
              <span className="checklist-label" title={i.label}>
                {i.label}
              </span>
              {i.mark}
              {i.detail !== undefined && <span className="checklist-detail mono">{i.detail}</span>}
            </label>
          </li>
        ))}
      </ul>
    </div>
  );
}
