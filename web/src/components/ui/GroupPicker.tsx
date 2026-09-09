// SPDX-License-Identifier: AGPL-3.0-only
// Pick one folder, by typing part of its name (ADR-124 決定 9).
//
// It replaces the plain `<select>` that six dialogs each built from `groupOptions()`. A `<select>`
// cannot be narrowed, so choosing a folder in a deployment with more than a screenful — which a
// NetBox sync creates on its own — meant scrolling a flat list looking for an indent level.
//
// **It knows nothing about folders.** It takes `GroupOption[]` and gives back an id, so each
// caller keeps the two things that genuinely differ between them: what the empty choice means
// (ungrouped / top level / "pick one"), and which folders are offered at all (`GroupModal` removes
// the folder being edited and its descendants; `DiscoveryPage` offers only ones carrying a
// prefix). Folding those in would have made this the seventh place that knows what a folder is.
//
// ⚠️ **`AnchoredPopover`, not `NodePicker`'s hand-rolled panel.** The reasons that one gives for
// staying `position: absolute` — its callers' outside-click tests, and living inside the
// dashboard's ⚙ popover — do not apply to a picker inside a modal, while the reasons for the
// shared one do: `escapeClosesDialog` puts `.apop` above `[role=dialog]`, so Escape closes this
// list and not the dialog behind it.
//
// ⚠️ **Focus waits for `onPlacedChange`.** The popover is `visibility: hidden` for the frame in
// which it is measured, and a hidden element cannot take focus, so React's `autoFocus` is silently
// dropped — which is exactly what left every column filter needing a second click for three
// increments.

import { useLayoutEffect, useMemo, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { AnchoredPopover } from './AnchoredPopover';
import { SearchInput } from './SearchInput';
import { SEARCH_THRESHOLD } from './MultiSelectList';
import { filterGroupOptions, type GroupOption } from '../../lib/nodeTree';
import './GroupPicker.css';

interface Props {
  options: readonly GroupOption[];
  /** Selected folder id, or `''` for the empty choice. */
  value: string;
  onChange: (id: string) => void;
  /** Label for the "no folder" row — `— Ungrouped —`, `— Top level —`. Omit and the list offers
   *  no way back to "none", which is right for a picker whose value is required. */
  emptyOption?: string;
  /** Trigger text while nothing is chosen and there is no empty option. */
  placeholder?: string;
  id?: string;
  disabled?: boolean;
  /** Focus the trigger on mount — for a dialog whose first decision is this field. */
  autoFocus?: boolean;
}

export function GroupPicker({
  options,
  value,
  onChange,
  emptyOption,
  placeholder,
  id,
  disabled,
  autoFocus,
}: Props) {
  const { t } = useTranslation('nodes');
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState('');
  const [placed, setPlaced] = useState(false);
  const [active, setActive] = useState(0);
  const anchorRef = useRef<HTMLDivElement>(null);
  const searchRef = useRef<HTMLDivElement>(null);

  const shown = useMemo(() => filterGroupOptions(options, query), [options, query]);
  const showSearch = options.length >= SEARCH_THRESHOLD;
  const filtering = query.trim() !== '';
  const selected = options.find((o) => o.id === value);

  // The trigger says what is chosen. A chosen folder shows its full path rather than its own name:
  // two sites can both hold a rack called "R1", and the trigger is the only place the difference
  // is visible once the list is closed.
  const triggerLabel = selected
    ? selected.path
    : value === ''
      ? (emptyOption ?? placeholder ?? t('groupPicker.none'))
      : value;

  useLayoutEffect(() => {
    if (open && placed && showSearch) {
      searchRef.current?.querySelector('input')?.focus({ preventScroll: true });
    }
  }, [open, placed, showSearch]);

  const openList = () => {
    setQuery('');
    setActive(0);
    setOpen(true);
  };

  const choose = (next: string) => {
    onChange(next);
    setOpen(false);
  };

  // The empty choice is row -1 so the arrow keys walk it like any other.
  const rows: (string | null)[] = [
    ...(emptyOption !== undefined ? [''] : []),
    ...shown.map((o) => o.id),
  ];

  const onKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === 'ArrowDown') {
      e.preventDefault();
      setActive((a) => Math.min(a + 1, rows.length - 1));
    } else if (e.key === 'ArrowUp') {
      e.preventDefault();
      setActive((a) => Math.max(a - 1, 0));
    } else if (e.key === 'Enter') {
      e.preventDefault();
      const pick = rows[active];
      if (pick !== undefined && pick !== null) choose(pick);
    }
  };

  return (
    <div className="grouppick" ref={anchorRef}>
      <button
        type="button"
        id={id}
        className="field grouppick-trigger"
        aria-haspopup="listbox"
        aria-expanded={open}
        disabled={disabled}
        // The caller decides: a dialog whose first decision is this field opens onto it.
        autoFocus={autoFocus}
        onClick={() => (open ? setOpen(false) : openList())}
      >
        <span className={selected ? 'grouppick-label' : 'grouppick-label muted'}>
          {triggerLabel}
        </span>
        <span className="grouppick-caret" aria-hidden="true">
          ▾
        </span>
      </button>

      <AnchoredPopover
        open={open}
        anchorRef={anchorRef}
        role="listbox"
        label={t('groupPicker.listAria')}
        align="start"
        className="grouppick-pop"
        onPlacedChange={setPlaced}
        onDismiss={() => setOpen(false)}
        onKeyDown={onKeyDown}
      >
        {showSearch && (
          <div className="grouppick-search" ref={searchRef}>
            <SearchInput
              value={query}
              onChange={(v) => {
                setQuery(v);
                setActive(0);
              }}
              placeholder={t('groupPicker.searchPlaceholder')}
              ariaLabel={t('groupPicker.searchAria')}
            />
          </div>
        )}
        <div className="grouppick-list scroll-y">
          {emptyOption !== undefined && (
            <button
              type="button"
              role="option"
              aria-selected={value === ''}
              className={`grouppick-opt${0 === active ? ' active' : value === '' ? ' selected' : ''}`}
              onMouseEnter={() => setActive(0)}
              onClick={() => choose('')}
            >
              <span className="grouppick-opt-name">{emptyOption}</span>
            </button>
          )}
          {shown.length === 0 && <div className="grouppick-empty">{t('groupPicker.noMatch')}</div>}
          {shown.map((o, i) => {
            const row = emptyOption !== undefined ? i + 1 : i;
            return (
              <button
                type="button"
                key={o.id}
                role="option"
                aria-selected={value === o.id}
                className={`grouppick-opt${row === active ? ' active' : value === o.id ? ' selected' : ''}`}
                // The indent is drawn, never baked into the label — and it is dropped while a
                // search is narrowing the list, because an indent measured from a parent the
                // filter has removed points at nothing.
                style={filtering ? undefined : { paddingLeft: 8 + o.depth * 14 }}
                onMouseEnter={() => setActive(row)}
                onClick={() => choose(o.id)}
              >
                <span className="grouppick-opt-name">{filtering ? o.path : o.label}</span>
              </button>
            );
          })}
        </div>
      </AnchoredPopover>
    </div>
  );
}
