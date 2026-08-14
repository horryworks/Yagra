// SPDX-License-Identifier: AGPL-3.0-only
// Multi-select option list with counts — the control the repository did not have (ADR-053).
//
// The toolbar's `FilterSelect` was single-valued and `Segmented` is a radio group, so "syslog AND
// trap" could not be expressed anywhere in the UI before this. (`FilterSelect` itself was deleted
// in Inc.7, once every screen had moved here.) It is deliberately NOT a `<select multiple>`: that
// control requires ctrl-click to add a second value, which nobody discovers, and it cannot carry a
// count beside each option.
//
// Two ARIA details that are easy to get wrong and are the reason this is one component rather than
// per-screen markup:
//   - The checkmark is **drawn**, not a native `<input type=checkbox>`. An interactive input inside
//     `role="option"` is invalid markup and screen readers announce the two competing states.
//   - Each option is a real `<button role="option">`, so focus and Enter/Space activation come from
//     the platform instead of being hand-rolled. `role="option"` overrides the button role for
//     assistive tech while leaving the behaviour.
// The option-search box is a SIBLING of the listbox, never inside it — a listbox may contain only
// options and groups.

import { useLayoutEffect, useMemo, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import type { FilterOption } from '../../lib/columnFilter';
import './MultiSelectList.css';

interface Props {
  options: readonly FilterOption[];
  /** Currently selected values. */
  selected: readonly string[];
  /** Row counts per option value, if this list has them. Counts **exclude this column's own
   *  filter** — see `lib/filterCounts.ts`; passing displayed-row counts here is the bug that makes
   *  the list unreadable. */
  counts?: Record<string, number>;
  onToggle: (value: string) => void;
  onClear: () => void;
  /** Accessible name for the listbox. */
  label: string;
  /** Show the option-search box once the list is at least this long. */
  searchThreshold?: number;
  /** Flipped true once the surrounding popover is measured — see `takeFocus` in `ColumnFilterCell`.
   *  Focuses the option-search box **only when one renders**: a short list has no text entry, and
   *  moving focus onto an option would pick one of them for no reason. */
  takeFocus?: boolean;
}

const SEARCH_THRESHOLD = 8;

export function MultiSelectList({
  options,
  selected,
  counts,
  onToggle,
  onClear,
  label,
  searchThreshold = SEARCH_THRESHOLD,
  takeFocus,
}: Props) {
  const { t } = useTranslation('common');
  const [query, setQuery] = useState('');
  const listRef = useRef<HTMLDivElement>(null);
  const searchRef = useRef<HTMLInputElement>(null);
  const sel = useMemo(() => new Set(selected), [selected]);

  // `searchRef` is null when the list is too short for a search box, so this is also the "no text
  // entry ⇒ leave focus on the trigger" half of the rule. See `TextConditionEditor` for the timing.
  useLayoutEffect(() => {
    if (takeFocus) searchRef.current?.focus({ preventScroll: true });
  }, [takeFocus]);

  const visible = useMemo(() => {
    const q = query.trim().toLowerCase();
    return q === '' ? options : options.filter((o) => o.label.toLowerCase().includes(q));
  }, [options, query]);

  const showSearch = options.length >= searchThreshold;

  // Arrow keys walk the options. Enter/Space are the button's own, so they are not handled here.
  const onKeyDown = (e: React.KeyboardEvent<HTMLDivElement>) => {
    if (e.key !== 'ArrowDown' && e.key !== 'ArrowUp') return;
    const items = [...(listRef.current?.querySelectorAll<HTMLButtonElement>('[role="option"]') ?? [])];
    if (items.length === 0) return;
    e.preventDefault();
    const cur = items.indexOf(document.activeElement as HTMLButtonElement);
    const next =
      e.key === 'ArrowDown'
        ? cur < 0
          ? 0
          : (cur + 1) % items.length
        : cur < 0
          ? items.length - 1
          : (cur - 1 + items.length) % items.length;
    items[next]?.focus();
  };

  return (
    <div className="msel">
      {showSearch && (
        <input
          ref={searchRef}
          type="search"
          className="msel-search"
          value={query}
          placeholder={t('filter.optionSearch')}
          aria-label={t('filter.optionSearch')}
          onChange={(e) => setQuery(e.target.value)}
        />
      )}
      <div
        className="msel-list scroll-y"
        role="listbox"
        aria-multiselectable="true"
        aria-label={label}
        ref={listRef}
        onKeyDown={onKeyDown}
      >
        {visible.length === 0 && <div className="msel-none">{t('filter.noOptions')}</div>}
        {visible.map((o) => {
          const on = sel.has(o.value);
          return (
            <button
              key={o.value}
              type="button"
              role="option"
              aria-selected={on}
              className={on ? 'msel-opt on' : 'msel-opt'}
              onClick={() => onToggle(o.value)}
            >
              <span className="msel-box" aria-hidden="true">
                {on ? '✓' : ''}
              </span>
              <span className="msel-label">{o.label}</span>
              {counts && <span className="msel-count">{counts[o.value] ?? 0}</span>}
            </button>
          );
        })}
      </div>
      <div className="msel-foot">
        <button type="button" className="msel-clear" onClick={onClear} disabled={sel.size === 0}>
          {t('filter.clear')}
        </button>
      </div>
    </div>
  );
}
