// SPDX-License-Identifier: AGPL-3.0-only
// The top bar's global search. Decision log §6 makes it a permanent affordance; it shipped
// disabled because no search endpoint existed at the time. `GET /api/v1/nodes/search` does exist,
// so it is wired.
//
// Scope is nodes only, and the popover says so — alerts, events and groups have no server-side
// search endpoint, and quietly searching one entity while looking like it searches everything is
// the kind of half-truth this pass exists to remove. The structure leaves room for a second
// section when there is a second thing to search.
//
// Behaviour (debounce, stale-response drop, roving keys, cap hint) mirrors NodePicker; the
// judgement lives in ./searchBox.ts so it is reachable by a test.

import { useEffect, useRef, useState } from 'react';
import { useNavigate } from 'react-router-dom';
import { useTranslation } from 'react-i18next';
import { api } from '../../services/api';
import type { NodeSearchResult } from '../../types/api';
import {
  MAX_RESULTS,
  SEARCH_DEBOUNCE_MS,
  isCapped,
  keyAction,
  moveActive,
  resultRoute,
  shouldFocusOnSlash,
} from './searchBox';
import './GlobalSearch.css';

export function GlobalSearch({ mobile = false }: { mobile?: boolean }) {
  const { t } = useTranslation('nav');
  const navigate = useNavigate();
  const [query, setQuery] = useState('');
  const [open, setOpen] = useState(false);
  const [active, setActive] = useState(0);
  const [results, setResults] = useState<NodeSearchResult[]>([]);
  const [loading, setLoading] = useState(false);
  const rootRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLInputElement>(null);

  // Click-outside closes. Escape is handled on the input so it also blurs.
  useEffect(() => {
    if (!open) return;
    const onDown = (e: MouseEvent) => {
      if (rootRef.current && !rootRef.current.contains(e.target as Node)) setOpen(false);
    };
    document.addEventListener('mousedown', onDown);
    return () => document.removeEventListener('mousedown', onDown);
  }, [open]);

  // Ctrl/Cmd+K anywhere, and a bare "/" only when the operator is not already typing in a field.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const el = document.activeElement as HTMLElement | null;
      const hotkey =
        (e.key === 'k' && (e.metaKey || e.ctrlKey)) ||
        (e.key === '/' &&
          !e.metaKey &&
          !e.ctrlKey &&
          !e.altKey &&
          shouldFocusOnSlash(el?.tagName, el?.isContentEditable ?? false));
      if (!hotkey) return;
      e.preventDefault();
      inputRef.current?.focus();
      setOpen(true);
    };
    document.addEventListener('keydown', onKey);
    return () => document.removeEventListener('keydown', onKey);
  }, []);

  // Query on each (debounced) keystroke while open. The cancelled flag drops a stale response so a
  // slow request cannot overwrite the results of a later keystroke.
  useEffect(() => {
    if (!open) return;
    let cancelled = false;
    const term = query.trim();
    setLoading(true);
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
            if (!cancelled) setLoading(false);
          });
      },
      term ? SEARCH_DEBOUNCE_MS : 0,
    );
    return () => {
      cancelled = true;
      clearTimeout(h);
    };
  }, [open, query]);

  const go = (r: NodeSearchResult) => {
    navigate(resultRoute(r));
    setOpen(false);
    setQuery('');
    inputRef.current?.blur();
  };

  const onKeyDown = (e: React.KeyboardEvent) => {
    const action = keyAction(e.key);
    if (!action) return;
    if (action === 'close') {
      setOpen(false);
      inputRef.current?.blur();
      return;
    }
    if (action === 'open') {
      const r = results[active];
      if (r) {
        e.preventDefault();
        go(r);
      }
      return;
    }
    e.preventDefault();
    setActive((a) => moveActive(a, action === 'down' ? 1 : -1, results.length));
  };

  return (
    <div className={mobile ? 'gsearch mobile' : 'gsearch'} ref={rootRef}>
      <input
        ref={inputRef}
        className="topbar-search"
        type="search"
        value={query}
        placeholder={t('shell.search')}
        aria-label={t('shell.globalSearch')}
        aria-expanded={open}
        aria-controls="gsearch-results"
        role="combobox"
        onFocus={() => setOpen(true)}
        onChange={(e) => {
          setQuery(e.target.value);
          setActive(0);
          setOpen(true);
        }}
        onKeyDown={onKeyDown}
      />
      {open && (
        <div className="gsearch-pop" id="gsearch-results">
          <div className="gsearch-section">{t('shell.searchNodes')}</div>
          <div className="gsearch-list" role="listbox" aria-label={t('shell.globalSearch')}>
            {loading && results.length === 0 ? (
              <div className="gsearch-empty">{t('shell.searchLoading')}</div>
            ) : results.length === 0 ? (
              <div className="gsearch-empty">{t('shell.searchNoMatch')}</div>
            ) : (
              results.map((r, i) => (
                <button
                  type="button"
                  key={r.id}
                  role="option"
                  aria-selected={i === active}
                  className={i === active ? 'gsearch-option active' : 'gsearch-option'}
                  onMouseEnter={() => setActive(i)}
                  onClick={() => go(r)}
                >
                  <span className="gsearch-opt-name">{r.name}</span>
                  <span className="gsearch-opt-addr mono">{r.address}</span>
                </button>
              ))
            )}
            {isCapped(results.length) && (
              <div className="gsearch-empty">{t('shell.searchCapped', { count: MAX_RESULTS })}</div>
            )}
          </div>
          {/* Say what is not searched, rather than letting a nodes-only result set read as
              "nothing else matched". */}
          <div className="gsearch-foot">{t('shell.searchScope')}</div>
        </div>
      )}
    </div>
  );
}
