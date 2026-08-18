// SPDX-License-Identifier: AGPL-3.0-only
// A node-only typeahead filter control: a field-styled trigger opens a popover with a scale-aware
// node search that queries the server on each keystroke (debounced), capped — never a flat dropdown
// and never a whole-inventory client load (A-2). Emits a plain { id, name } | null. Distinct from
// the troubleshoot ScopePicker (which also offers All/Group modes) because the events API filters by
// node_id only. Reuses the popover/roving-key pattern and SearchInput.

import { useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { SearchInput } from '../ui/SearchInput';
import { useNodeSearch } from '../../lib/useNodeSearch';
import './NodePicker.css';

/** Server search cap: request (and show) at most this many hits — keep typing to narrow. */
const MAX_RESULTS = 50;

/** Gap between the trigger and the list, in px. Mirrors the `4px` in `NodePicker.css` — the
 *  measurement below has to account for the same offset the stylesheet applies. */
const GAP = 4;

interface Props {
  /** Selected node id (the URL/parent is the source of truth), or null for "no filter". */
  value: string | null;
  /** Resolved human name for the trigger; falls back to the raw id / placeholder. */
  valueLabel?: string;
  onChange: (node: { id: string; name: string } | null) => void;
  placeholder?: string;
  id?: string;
  className?: string;
  /** Node ids to hide from the results (e.g. self + descendants when picking a dependency
   *  upstream, to avoid offering a cycle-forming choice). */
  exclude?: ReadonlySet<string>;
}

export function NodePicker({
  value,
  valueLabel,
  onChange,
  placeholder,
  id,
  className,
  exclude,
}: Props) {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState('');
  const [active, setActive] = useState(0);
  const ref = useRef<HTMLDivElement>(null);
  const boxRef = useRef<HTMLDivElement>(null);
  const popRef = useRef<HTMLDivElement>(null);
  /** Open upwards: there is no room below the trigger and there is more above it. */
  const [dropUp, setDropUp] = useState(false);

  // Click-outside + Escape close.
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

  // Focus the search box when the popover opens.
  useEffect(() => {
    if (open) boxRef.current?.querySelector('input')?.focus();
  }, [open]);

  // The debounce, the empty-term-is-a-real-query rule and the stale-response guard are all in
  // `useNodeSearch` — this was one of three byte-identical copies of them.
  const { results, loading } = useNodeSearch(open, query, MAX_RESULTS);

  const shown = useMemo(
    () => (exclude && exclude.size ? results.filter((node) => !exclude.has(node.id)) : results),
    [results, exclude],
  );

  /** Decide which way the list opens.
   *
   *  It is `position: absolute` and stays that way deliberately: portalling it the way
   *  `AnchoredPopover` does would take it out of the subtree its callers' outside-click tests ask
   *  about, and clicking a node inside a picker that sits in a popover (the dashboard's ⚙ panel)
   *  would close that popover instead of choosing. The cost of staying absolute is that nothing
   *  clamps it to the viewport, so this does — with the same rule the shared popover applies to
   *  itself.
   *
   *  ⚠️ It runs in a layout effect so the first painted frame is already on the right side; a
   *  `useEffect` here paints downwards once and then jumps. And it re-measures on capture-phase
   *  scroll, because scroll does not bubble and the trigger moves with its container.
   *
   *  Only flips when the list genuinely does not fit below **and** there is more room above —
   *  trading a clipped bottom edge for a clipped top edge is not a fix. */
  useLayoutEffect(() => {
    if (!open) {
      setDropUp(false);
      return;
    }
    const place = () => {
      const trigger = ref.current?.getBoundingClientRect();
      const panel = popRef.current?.getBoundingClientRect();
      if (!trigger || !panel) return;
      const below = window.innerHeight - trigger.bottom;
      const above = trigger.top;
      setDropUp(panel.height + GAP > below && above > below);
    };
    place();
    window.addEventListener('scroll', place, true);
    window.addEventListener('resize', place);
    return () => {
      window.removeEventListener('scroll', place, true);
      window.removeEventListener('resize', place);
    };
    // `shown.length` is in here because the panel's height is its rows: measuring once on open
    // would decide from the empty list and never revisit it.
  }, [open, shown.length, loading]);

  const openPopover = () => {
    // The hook re-queries on open (the empty term is a real query), so there is nothing to clear
    // here — clearing would blank the list for one frame before the same rows came back.
    setQuery('');
    setActive(0);
    setOpen(true);
  };

  const pick = (nid: string, name: string) => {
    onChange({ id: nid, name });
    setOpen(false);
  };

  const clear = () => {
    onChange(null);
    setOpen(false);
  };

  // Arrow-key roving over the results (Enter selects the active row).
  const onKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === 'ArrowDown') {
      e.preventDefault();
      setActive((a) => Math.min(a + 1, shown.length - 1));
    } else if (e.key === 'ArrowUp') {
      e.preventDefault();
      setActive((a) => Math.max(a - 1, 0));
    } else if (e.key === 'Enter') {
      e.preventDefault();
      const n = shown[active];
      if (n) pick(n.id, n.name);
    }
  };

  const triggerLabel = value ? (valueLabel ?? value) : (placeholder ?? t('nav:nodes.all'));

  return (
    <div className={['nodepick', className].filter(Boolean).join(' ')} ref={ref}>
      <div className={value ? 'nodepick-control field on' : 'nodepick-control field'}>
        <button
          type="button"
          id={id}
          className="nodepick-trigger"
          aria-haspopup="listbox"
          aria-expanded={open}
          onClick={() => (open ? setOpen(false) : openPopover())}
        >
          <span className={value ? 'nodepick-label' : 'nodepick-label muted'}>{triggerLabel}</span>
        </button>
        {value ? (
          <button type="button" className="nodepick-clear" onClick={clear} aria-label={t('nodePicker.clear')}>
            ×
          </button>
        ) : (
          <span className="nodepick-caret" aria-hidden="true">
            ▾
          </span>
        )}
      </div>

      {open && (
        <div ref={popRef} className={dropUp ? 'nodepick-pop drop-up' : 'nodepick-pop'}>
          <div ref={boxRef} onKeyDown={onKeyDown}>
            <div className="nodepick-search">
              <SearchInput
                value={query}
                onChange={(v) => {
                  setQuery(v);
                  setActive(0);
                }}
                placeholder={t('nodePicker.searchPlaceholder')}
                ariaLabel={t('nodePicker.searchAria')}
              />
            </div>
            <div className="nodepick-list" role="listbox" aria-label={t('nodePicker.listAria')}>
              {loading && results.length === 0 ? (
                <div className="nodepick-empty">{t('nodePicker.loading')}</div>
              ) : shown.length === 0 ? (
                <div className="nodepick-empty">{t('nodePicker.noMatch')}</div>
              ) : (
                shown.map((n, i) => (
                  <button
                    type="button"
                    key={n.id}
                    role="option"
                    aria-selected={value === n.id}
                    className={
                      i === active
                        ? 'nodepick-option active'
                        : value === n.id
                          ? 'nodepick-option selected'
                          : 'nodepick-option'
                    }
                    onMouseEnter={() => setActive(i)}
                    onClick={() => pick(n.id, n.name)}
                  >
                    <span className="nodepick-opt-name">{n.name}</span>
                    <span className="nodepick-opt-addr mono">{n.address}</span>
                  </button>
                ))
              )}
              {/* Server returned a full page ⇒ there may be more matches; prompt to narrow. */}
              {results.length >= MAX_RESULTS && (
                <div className="nodepick-empty">{t('nodePicker.capped', { count: MAX_RESULTS })}</div>
              )}
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
