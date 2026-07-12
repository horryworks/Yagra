// Shared filter controls for the event log, used by both the Alerts ▸ Events page and the
// NodeDetail ▸ Events tab (same shared-module pattern as useEventLog / eventColumns). Renders the
// kind / matched selects, an optional node picker, a text search with a regex toggle, and a
// From/To time-range (both sides optional — empty = unbounded, so the default is "all events").
//
// The component owns only ephemeral input drafts (the search text and the From/To datetime-local
// strings); the resolved filter values flow up through typed callbacks. Search is debounced here
// so the parent stores only the settled term. Range bounds are emitted as RFC 3339 (the events
// API's time format), converted from the local-wall-clock <input type="datetime-local"> via the
// RangeControl helper so all date parsing lives in one place.

import { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import type { EventKind } from '../../types/api';
import { Select } from '../ui/Field';
import { SearchInput } from '../ui/TableToolbar';
import { NodePicker } from '../NodePicker/NodePicker';
import { localInputToUnix } from '../NodeDetail/RangeControl';
import './EventFilterBar.css';

export type KindFilter = '' | EventKind;
export type MatchedFilter = '' | 'matched' | 'unmatched';

interface Props {
  kind: KindFilter;
  onKindChange: (v: KindFilter) => void;
  matched: MatchedFilter;
  onMatchedChange: (v: MatchedFilter) => void;
  regex: boolean;
  onRegexChange: (v: boolean) => void;
  /** Called with the debounced, trimmed search term (empty string when cleared). */
  onSearchChange: (term: string) => void;
  /** Emit the resolved range bounds (RFC 3339, or undefined for an unbounded side). */
  onRangeChange: (start: string | undefined, end: string | undefined) => void;
  /** Show the node picker (Alerts ▸ Events); hidden on a single node's Events tab. */
  showNodePicker?: boolean;
  nodeId?: string | null;
  nodeLabel?: string;
  onNodeChange?: (node: { id: string; name: string } | null) => void;
}

/** local-wall-clock 'YYYY-MM-DDTHH:MM' → RFC 3339 (UTC), or undefined if empty/unparseable. */
function localInputToIso(local: string): string | undefined {
  const secs = localInputToUnix(local);
  return secs == null ? undefined : new Date(secs * 1000).toISOString();
}

export function EventFilterBar({
  kind,
  onKindChange,
  matched,
  onMatchedChange,
  regex,
  onRegexChange,
  onSearchChange,
  onRangeChange,
  showNodePicker = false,
  nodeId,
  nodeLabel,
  onNodeChange,
}: Props) {
  const { t } = useTranslation(['alerts', 'common', 'nav']);
  const [searchDraft, setSearchDraft] = useState('');
  const [fromDraft, setFromDraft] = useState('');
  const [toDraft, setToDraft] = useState('');
  // Mobile only: the filter controls collapse behind a "Filters" toggle so the event list gets the
  // screen height (the controls stack ~4 rows tall otherwise). Desktop ignores this — the toggle is
  // hidden and `.event-filters` is `display: contents`, so the controls stay inline in the toolbar.
  const [open, setOpen] = useState(false);

  // Debounce the text box so we don't refetch on every keystroke (matches the MIB search).
  useEffect(() => {
    const id = setTimeout(() => onSearchChange(searchDraft.trim()), 200);
    return () => clearTimeout(id);
    // onSearchChange is a stable setter from the parent; depend only on the draft.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [searchDraft]);

  const emitRange = (from: string, to: string) =>
    onRangeChange(localInputToIso(from), localInputToIso(to));

  const clearRange = () => {
    setFromDraft('');
    setToDraft('');
    onRangeChange(undefined, undefined);
  };

  const hasRange = fromDraft !== '' || toDraft !== '';

  // Badge on the collapsed mobile toggle so applied filters are visible without expanding.
  const activeCount =
    (kind !== '' ? 1 : 0) +
    (matched !== '' ? 1 : 0) +
    (searchDraft.trim() !== '' ? 1 : 0) +
    (regex ? 1 : 0) +
    (hasRange ? 1 : 0) +
    (showNodePicker && nodeId ? 1 : 0);

  return (
    <>
      <button
        type="button"
        className="event-filters-toggle"
        aria-expanded={open}
        onClick={() => setOpen((v) => !v)}
      >
        {t('events.filters.toggle')}
        {activeCount > 0 && <span className="event-filters-count">{activeCount}</span>}
      </button>
      <div className="event-filters" data-open={open ? 'true' : 'false'}>
        <Select value={kind} onChange={(e) => onKindChange(e.target.value as KindFilter)}>
          <option value="">{t('events.filters.allKinds')}</option>
          <option value="syslog">syslog</option>
          <option value="trap">trap</option>
          <option value="webhook">webhook</option>
        </Select>
        <Select value={matched} onChange={(e) => onMatchedChange(e.target.value as MatchedFilter)}>
          <option value="">{t('events.filters.allEvents')}</option>
          <option value="matched">{t('events.filters.matched')}</option>
          <option value="unmatched">{t('events.filters.unmatched')}</option>
        </Select>
        {showNodePicker && (
          <NodePicker
            value={nodeId ?? null}
            valueLabel={nodeId ? nodeLabel : undefined}
            onChange={(n) => onNodeChange?.(n)}
            placeholder={t('nav:nodes.all')}
          />
        )}
        <div className="event-search">
          <SearchInput
            value={searchDraft}
            onChange={setSearchDraft}
            placeholder={
              regex ? t('events.filters.searchPlaceholderRegex') : t('events.searchPlaceholder')
            }
            ariaLabel={t('events.searchAria')}
          />
          <button
            type="button"
            className={`event-regex-toggle${regex ? ' active' : ''}`}
            aria-pressed={regex}
            title={t('events.filters.regexAria')}
            aria-label={t('events.filters.regexAria')}
            onClick={() => onRegexChange(!regex)}
          >
            {t('events.filters.regexLabel')}
          </button>
        </div>
        <div className="event-range">
          <input
            className="field event-range-input"
            type="datetime-local"
            value={fromDraft}
            aria-label={t('events.filters.from')}
            title={t('events.filters.from')}
            onChange={(e) => {
              setFromDraft(e.target.value);
              emitRange(e.target.value, toDraft);
            }}
          />
          <span className="event-range-sep">–</span>
          <input
            className="field event-range-input"
            type="datetime-local"
            value={toDraft}
            aria-label={t('events.filters.to')}
            title={t('events.filters.to')}
            onChange={(e) => {
              setToDraft(e.target.value);
              emitRange(fromDraft, e.target.value);
            }}
          />
          {hasRange && (
            <button
              type="button"
              className="event-range-clear"
              title={t('events.filters.clearRange')}
              aria-label={t('events.filters.clearRange')}
              onClick={clearRange}
            >
              ×
            </button>
          )}
        </div>
      </div>
    </>
  );
}
