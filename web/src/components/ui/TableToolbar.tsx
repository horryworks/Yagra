// SPDX-License-Identifier: AGPL-3.0-only
// Table toolbar — one consistent control row above every list (search → filters → spacer →
// result count → primary action). The pieces are small composables so each screen arranges the
// slots it needs; styles live with the table standard (styles/table.css).

import type { ReactNode } from 'react';
import { Trans, useTranslation } from 'react-i18next';
import { SearchIcon } from './icons';
import { Select } from './Field';

/** The toolbar row. Lay children left → right; drop a <TableSpacer/> before the count/action. */
export function TableToolbar({ children }: { children: ReactNode }) {
  return <div className="table-toolbar">{children}</div>;
}

/** Flexible gap that pushes the result count + primary action to the right. */
export function TableSpacer() {
  return <div className="table-spacer" />;
}

/** Search box with a leading magnifier. Controlled. */
export function SearchInput({
  value,
  onChange,
  placeholder,
  ariaLabel,
}: {
  value: string;
  onChange: (v: string) => void;
  placeholder?: string;
  ariaLabel?: string;
}) {
  const { t } = useTranslation('common');
  return (
    <div className="table-search">
      <SearchIcon />
      <input
        type="search"
        value={value}
        onChange={(e) => onChange(e.target.value)}
        placeholder={placeholder}
        aria-label={ariaLabel ?? t('actions.search')}
      />
    </div>
  );
}

/** One choice in a toolbar filter. `label` is already localized by the caller. */
export interface FilterOption<V extends string> {
  value: V;
  label: string;
}

/**
 * A toolbar filter dropdown — the `フィルタ` slot of the fixed toolbar order
 * (`検索 → フィルタ → spacer → 件数 → 主アクション`).
 *
 * Three things it fixes, each of which was already a defect somewhere in the tree:
 *
 * 1. **"No filter" had two spellings** — `''` on the Events and Saved-findings screens, `'all'` on
 *    Audit and Credentials. Only `''` works with the `unset()` mapping (`lib/filterQuery.ts`), so
 *    this component supplies that option itself rather than trusting each caller to pick it.
 * 2. **`aria-label` was silently optional and had already been forgotten** (both selects in
 *    `EventFilterBar`). A toolbar select has no visible label, so it is required here.
 * 3. It takes the value, not the event — which is what lets `options` come from an `as const` array
 *    (`extensibility.md` §4) instead of being hand-mapped per screen.
 *
 * There is deliberately no `FilterBar` wrapper around several of these: it would add only a `<div>`,
 * and the screens genuinely differ in which slots exist.
 */
export function FilterSelect<V extends string>({
  value,
  onChange,
  options,
  allLabel,
  ariaLabel,
}: {
  value: V | '';
  onChange: (v: V | '') => void;
  options: readonly FilterOption<V>[];
  /** The "no filter" choice, e.g. "All severities". Its value is always `''`. */
  allLabel: string;
  /** Required: there is no visible label to read. */
  ariaLabel: string;
}) {
  return (
    <Select
      className="table-filter"
      value={value}
      onChange={(e) => onChange(e.target.value as V | '')}
      aria-label={ariaLabel}
    >
      <option value="">{allLabel}</option>
      {options.map((o) => (
        <option key={o.value} value={o.value}>
          {o.label}
        </option>
      ))}
    </Select>
  );
}

/** Result count: "N of M <noun>" (omit `total` for "N <noun>"), with `shown` emphasized. The
 *  word order comes from the translation key so a language like Japanese can reorder it; callers
 *  pass `noun` already localized for their context. */
export function ResultCount({
  shown,
  total,
  noun,
}: {
  shown: number;
  total?: number;
  noun: string;
}) {
  const { t } = useTranslation('common');
  return (
    <span className="table-count">
      <Trans
        t={t}
        i18nKey={total != null ? 'resultCount' : 'resultCountNoTotal'}
        values={{ shown, total, noun }}
        components={{ b: <strong /> }}
      />
    </span>
  );
}
