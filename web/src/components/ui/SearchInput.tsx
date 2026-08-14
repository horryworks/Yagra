// SPDX-License-Identifier: AGPL-3.0-only
// A search box with a leading magnifier. Controlled.
//
// **It used to live in `TableToolbar.tsx`, and moving it out is the point of ADR-053 Inc.7.** While
// it sat there it read as "the toolbar's search slot" — i.e. as the place a list's filtering
// belongs — and that reading is exactly what the column filter row replaced. Every list has since
// moved its narrowing under its column headers (or into a `FilterBar`, for the lists that have no
// headers), so the toolbar is an action row and has no search slot to offer.
//
// The three call sites left are **pickers**: `NodePicker`, `ScopePicker` and the dashboard widget
// catalog. In all three the box narrows a set of *choices* inside a popover or a modal — there is no
// list of rows underneath it and no filter row to compete with. That is the only shape this is for.
//
// ⚠️ **A new list screen must not reach for this.** A list is narrowed by `ColumnFilterCell`, which
// carries the mode toggle, NOT, the multi-select and the URL codec that a bare text box cannot. If
// a list has no column headers to hang cells under, `FilterBar` is the answer, not this.
//
// The `.table-search` class name stays as it is: the styling lives in `styles/table.css` beside the
// toolbar's own rules, and renaming it would be a CSS-only churn with nothing to gain.

import { useTranslation } from 'react-i18next';
import { SearchIcon } from './icons';

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
