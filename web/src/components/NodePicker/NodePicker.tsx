// A node-only typeahead filter control: a field-styled trigger opens a popover with a scale-aware
// node search (lazy-loaded inventory, in-memory filtered, capped — never a flat dropdown). Emits a
// plain { id, name } | null. Distinct from the troubleshoot ScopePicker (which also offers All/Group
// modes) because the events API filters by node_id only. Reuses the generic scope data loader,
// filter, popover/roving-key pattern, and SearchInput.

import { useEffect, useMemo, useRef, useState } from 'react';
import { SearchInput } from '../ui/TableToolbar';
import { useScopeData } from '../../troubleshoot/useScopeData';
import { filterNodes } from '../../troubleshoot/scope';
import './NodePicker.css';

/** Rendered node-result cap — filter first, then show this many (keep typing to narrow). */
const MAX_RESULTS = 50;

interface Props {
  /** Selected node id (the URL/parent is the source of truth), or null for "no filter". */
  value: string | null;
  /** Resolved human name for the trigger; falls back to the raw id / placeholder. */
  valueLabel?: string;
  onChange: (node: { id: string; name: string } | null) => void;
  placeholder?: string;
  id?: string;
  className?: string;
}

export function NodePicker({ value, valueLabel, onChange, placeholder = 'All nodes', id, className }: Props) {
  const { nodes, nodesLoaded, loadNodes } = useScopeData();
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState('');
  const [active, setActive] = useState(0);
  const ref = useRef<HTMLDivElement>(null);
  const boxRef = useRef<HTMLDivElement>(null);

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

  const filtered = useMemo(() => filterNodes(nodes, query), [nodes, query]);
  const shown = filtered.slice(0, MAX_RESULTS);

  const openPopover = () => {
    setQuery('');
    setActive(0);
    loadNodes();
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

  const triggerLabel = value ? (valueLabel ?? value) : placeholder;

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
          <button type="button" className="nodepick-clear" onClick={clear} aria-label="Clear node filter">
            ×
          </button>
        ) : (
          <span className="nodepick-caret" aria-hidden="true">
            ▾
          </span>
        )}
      </div>

      {open && (
        <div className="nodepick-pop">
          <div ref={boxRef} onKeyDown={onKeyDown}>
            <div className="nodepick-search">
              <SearchInput
                value={query}
                onChange={(v) => {
                  setQuery(v);
                  setActive(0);
                }}
                placeholder="Search nodes by name or address…"
                ariaLabel="Search nodes"
              />
            </div>
            <div className="nodepick-list" role="listbox" aria-label="Nodes">
              {!nodesLoaded ? (
                <div className="nodepick-empty">Loading nodes…</div>
              ) : shown.length === 0 ? (
                <div className="nodepick-empty">No matching nodes.</div>
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
              {nodesLoaded && filtered.length > MAX_RESULTS && (
                <div className="nodepick-empty">
                  +{filtered.length - MAX_RESULTS} more — keep typing to narrow
                </div>
              )}
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
