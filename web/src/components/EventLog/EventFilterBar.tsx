// SPDX-License-Identifier: AGPL-3.0-only
// Shared filter controls for the event log, used by both the Alerts ▸ Events page and the
// NodeDetail ▸ Events tab (same shared-module pattern as useEventLog / eventColumns). Renders the
// kind / matched selects, an optional node picker, a text search with a regex toggle, and a time
// range: a preset (default "Last 24 hours"), with the From/To instants appearing under "Custom".
//
// ⚠️ The range default is **bounded**, and that is what pays for the search being
// case-insensitive — see `eventRange.ts`. It used to be "all time" with a From/To pair and nothing
// else; widening it back turns every search on a busy deployment into a ten-second wait.
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
import { localInputToIso } from '../NodeDetail/RangeControl';
import {
  boundsFor,
  DEFAULT_EVENT_RANGE,
  EVENT_RANGES,
  rangeIsNarrowing,
  type EventRange,
} from './eventRange';
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
  const [range, setRange] = useState<EventRange>(DEFAULT_EVENT_RANGE);
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

  // The default range has to reach the parent, not just sit in this state — otherwise the first
  // fetch is unbounded and only the second one is narrowed. Resolved once here rather than per
  // request: see `boundsFor` for why a lower bound that creeps forward breaks keyset paging.
  useEffect(() => {
    const b = boundsFor(DEFAULT_EVENT_RANGE, {}, Date.now());
    onRangeChange(b.start, b.end);
    // Mount only; every later change goes through `emit`.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const emit = (next: EventRange, from: string, to: string) => {
    const b = boundsFor(next, { from: localInputToIso(from), to: localInputToIso(to) }, Date.now());
    onRangeChange(b.start, b.end);
  };

  const pickRange = (next: EventRange) => {
    setRange(next);
    // Leaving Custom drops the two instants: keeping them would mean a preset and an absolute
    // window silently ANDed, so "Last 24 hours" could return nothing because a month-old To was
    // still set on a control no longer on screen.
    if (next !== 'custom') {
      setFromDraft('');
      setToDraft('');
      emit(next, '', '');
    } else {
      emit(next, fromDraft, toDraft);
    }
  };

  const clearRange = () => {
    setFromDraft('');
    setToDraft('');
    emit('custom', '', '');
  };

  const hasRange = fromDraft !== '' || toDraft !== '';

  // Badge on the collapsed mobile toggle so applied filters are visible without expanding.
  const activeCount =
    (kind !== '' ? 1 : 0) +
    (matched !== '' ? 1 : 0) +
    (searchDraft.trim() !== '' ? 1 : 0) +
    (regex ? 1 : 0) +
    (rangeIsNarrowing(range, { from: fromDraft, to: toDraft }) ? 1 : 0) +
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
        {/* No extra class: `Select` already carries `.field`, which is what the mobile collapse
            sizes, and this reads as one more toolbar filter beside kind/matched. */}
        <Select
          value={range}
          onChange={(e) => pickRange(e.target.value as EventRange)}
          aria-label={t('common:range.timeRange')}
        >
          {EVENT_RANGES.map((r) => (
            <option key={r} value={r}>
              {t(`events.range.${r}`)}
            </option>
          ))}
        </Select>
        {/* The two instants belong to Custom only. Kept mounted-on-demand rather than disabled:
            a From/To pair that is on screen but ignored by the active preset is the control that
            makes an operator distrust the whole toolbar. */}
        {range === 'custom' && (
          <div className="event-range">
            <input
              className="field event-range-input"
              type="datetime-local"
              value={fromDraft}
              aria-label={t('events.filters.from')}
              title={t('events.filters.from')}
              onChange={(e) => {
                setFromDraft(e.target.value);
                emit('custom', e.target.value, toDraft);
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
                emit('custom', fromDraft, e.target.value);
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
        )}
      </div>
    </>
  );
}
