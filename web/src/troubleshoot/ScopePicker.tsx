// Scope picker for Troubleshoot analyses: choose All nodes / a Group / a single Node, producing
// a { kind, id, label }. A field-styled trigger opens a popover (mode tabs + a list), following
// the CredentialPicker popover/click-outside pattern. The Node list is a scale-aware typeahead
// (lazy-loaded, in-memory filtered, capped) — never a flat dropdown of the whole inventory.

import { useEffect, useMemo, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { SearchInput } from '../components/ui/TableToolbar';
import { api } from '../services/api';
import { groupOptions } from '../lib/nodeTree';
import type { NodeSearchResult } from '../types/api';
import { Segmented } from './Segmented';
import { useScopeData } from './useScopeData';
import { allScope, groupScopeLabel, nodeScopeLabel, type ScopeValue } from './scope';
import './ScopePicker.css';

type Mode = 'all' | 'group' | 'node';
/** Server search cap — request (and show) at most this many hits; keep typing to narrow. */
const MAX_RESULTS = 50;
/** Debounce so a fast typist fires one search request, not one per keystroke. */
const SEARCH_DEBOUNCE_MS = 200;

interface Props {
  value: ScopeValue;
  onChange: (v: ScopeValue) => void;
  id?: string;
  className?: string;
  disabled?: boolean;
}

export function ScopePicker({ value, onChange, id, className, disabled }: Props) {
  const { t } = useTranslation('troubleshoot');
  const { groups } = useScopeData();
  const MODES = useMemo(
    () => [
      { value: 'all', label: t('scope.modes.all') },
      { value: 'group', label: t('scope.modes.group') },
      { value: 'node', label: t('scope.modes.node') },
    ],
    [t],
  );
  const [open, setOpen] = useState(false);
  const [mode, setMode] = useState<Mode>(value.kind);
  const [query, setQuery] = useState('');
  const [active, setActive] = useState(0);
  const [results, setResults] = useState<NodeSearchResult[]>([]);
  const [nodesLoading, setNodesLoading] = useState(false);
  const ref = useRef<HTMLDivElement>(null);
  const nodeBoxRef = useRef<HTMLDivElement>(null);

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

  // Focus the search box when Node mode is showing.
  useEffect(() => {
    if (open && mode === 'node') nodeBoxRef.current?.querySelector('input')?.focus();
  }, [open, mode]);

  // Server-side node search: (re)query on entering Node mode and on each (debounced) keystroke —
  // never a whole-inventory client load. Empty query returns the first page by name so the list
  // isn't blank before typing. A cancelled flag drops a stale response from an earlier keystroke.
  useEffect(() => {
    if (!open || mode !== 'node') return;
    let cancelled = false;
    const term = query.trim();
    setNodesLoading(true);
    const h = setTimeout(
      () => {
        api
          .searchNodes(term, MAX_RESULTS)
          .then((r) => {
            if (!cancelled) setResults(r);
          })
          .catch(() => {
            if (!cancelled) setResults([]);
          })
          .finally(() => {
            if (!cancelled) setNodesLoading(false);
          });
      },
      term ? SEARCH_DEBOUNCE_MS : 0,
    );
    return () => {
      cancelled = true;
      clearTimeout(h);
    };
  }, [open, mode, query]);

  const groupItems = useMemo(() => groupOptions(groups), [groups]);
  const nameById = useMemo(() => new Map(groups.map((g) => [g.id, g.name])), [groups]);

  const shown = results;

  const openPopover = () => {
    if (disabled) return;
    setMode(value.kind);
    setQuery('');
    setActive(0);
    setResults([]);
    setOpen(true);
  };

  const changeMode = (m: Mode) => {
    setMode(m);
    setActive(0);
    setQuery('');
    if (m === 'all') {
      onChange(allScope(t));
      setOpen(false);
    } else if (m === 'node') {
      setResults([]);
    }
  };

  const pickGroup = (gid: string) => {
    const name = nameById.get(gid) ?? gid;
    onChange({ kind: 'group', id: gid, label: groupScopeLabel(name, t) });
    setOpen(false);
  };

  const pickNode = (nid: string, name: string) => {
    onChange({ kind: 'node', id: nid, label: nodeScopeLabel(name, t) });
    setOpen(false);
  };

  // Arrow-key roving over the node results (Enter selects the active row).
  const onNodeKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === 'ArrowDown') {
      e.preventDefault();
      setActive((a) => Math.min(a + 1, shown.length - 1));
    } else if (e.key === 'ArrowUp') {
      e.preventDefault();
      setActive((a) => Math.max(a - 1, 0));
    } else if (e.key === 'Enter') {
      e.preventDefault();
      const n = shown[active];
      if (n) pickNode(n.id, n.name);
    }
  };

  return (
    <div className={['scope-picker', className].filter(Boolean).join(' ')} ref={ref}>
      <button
        type="button"
        id={id}
        className="scope-picker-control field"
        disabled={disabled}
        aria-haspopup="listbox"
        aria-expanded={open}
        onClick={() => (open ? setOpen(false) : openPopover())}
      >
        <span className="scope-picker-label">{value.label}</span>
        <span className="scope-picker-caret" aria-hidden="true">
          ▾
        </span>
      </button>

      {open && (
        <div className="scope-pop">
          <div className="scope-pop-modes">
            <Segmented options={MODES} value={mode} onChange={(m) => changeMode(m as Mode)} ariaLabel={t('scope.modeAria')} />
          </div>

          {mode === 'all' && (
            <button type="button" className="scope-option" onClick={() => changeMode('all')}>
              <span className="scope-opt-name">{t('scope.all')}</span>
            </button>
          )}

          {mode === 'group' && (
            <div className="scope-list" role="listbox" aria-label={t('scope.groupsAria')}>
              {groupItems.length === 0 ? (
                <div className="scope-empty">{t('scope.noGroups')}</div>
              ) : (
                groupItems.map((g) => (
                  <button
                    type="button"
                    key={g.id}
                    role="option"
                    aria-selected={value.kind === 'group' && value.id === g.id}
                    className={
                      value.kind === 'group' && value.id === g.id
                        ? 'scope-option selected'
                        : 'scope-option'
                    }
                    onClick={() => pickGroup(g.id)}
                  >
                    <span className="scope-opt-name scope-opt-group">{g.label}</span>
                  </button>
                ))
              )}
            </div>
          )}

          {mode === 'node' && (
            <div ref={nodeBoxRef} onKeyDown={onNodeKeyDown}>
              <div className="scope-search">
                <SearchInput
                  value={query}
                  onChange={(v) => {
                    setQuery(v);
                    setActive(0);
                  }}
                  placeholder={t('common:nodePicker.searchPlaceholder')}
                  ariaLabel={t('common:nodePicker.searchAria')}
                />
              </div>
              <div className="scope-list" role="listbox" aria-label={t('common:nodePicker.listAria')}>
                {nodesLoading && shown.length === 0 ? (
                  <div className="scope-empty">{t('common:nodePicker.loading')}</div>
                ) : shown.length === 0 ? (
                  <div className="scope-empty">{t('common:nodePicker.noMatch')}</div>
                ) : (
                  shown.map((n, i) => (
                    <button
                      type="button"
                      key={n.id}
                      role="option"
                      aria-selected={value.kind === 'node' && value.id === n.id}
                      className={
                        i === active
                          ? 'scope-option active'
                          : value.kind === 'node' && value.id === n.id
                            ? 'scope-option selected'
                            : 'scope-option'
                      }
                      onMouseEnter={() => setActive(i)}
                      onClick={() => pickNode(n.id, n.name)}
                    >
                      <span className="scope-opt-name">{n.name}</span>
                      <span className="scope-opt-addr mono">{n.address}</span>
                    </button>
                  ))
                )}
                {/* Server returned a full page ⇒ there may be more matches; prompt to narrow. */}
                {results.length >= MAX_RESULTS && (
                  <div className="scope-empty">
                    {t('common:nodePicker.capped', { count: MAX_RESULTS })}
                  </div>
                )}
              </div>
            </div>
          )}
        </div>
      )}
    </div>
  );
}
