// SPDX-License-Identifier: AGPL-3.0-only
// The Tag section of the inventory tree's filter menu (ADR-265): a search box over the tags the
// caller's nodes carry, the shared option list with a node count beside each, and, once two or more
// are chosen, whether a node must carry any of them or every one (ADR-267).
//
// Search first because a deployment may carry thousands of tags; what to offer is decided in
// `tagPicker.ts`. The box is a filter editor's own text entry (ui-conventions: `SearchField`), and
// it does not take focus when the menu opens: focus stays on the trigger, as the menu's doc says.

import { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Button } from '../components/ui/Button';
import { MultiSelectList } from '../components/ui/MultiSelectList';
import { SearchField } from '../components/ui/SearchField';
import { Segmented } from '../components/ui/Segmented';
import { SEARCH_DEBOUNCE_MS, useDebouncedValue } from '../lib/useDebouncedValue';
import { api } from '../services/api';
import { TAG_MATCHES, type TagMatch, type TagSuggestions } from '../types/api';
import { TAG_MATCH_LABEL } from '../lib/tagMatch';
import { decodeSet } from '../lib/columnFilter';
import { tagPickerView, tagRequestLimit, toggleTag } from './tagPicker';

export function TagFilterSection({
  label,
  value,
  onChange,
  match,
  onMatch,
}: {
  label: string;
  /** The URL's comma-joined set of chosen tags. */
  value: string;
  onChange: (next: string) => void;
  /** Whether a node must carry any of the chosen tags or every one. */
  match: TagMatch;
  onMatch: (next: TagMatch) => void;
}) {
  const { t } = useTranslation('nodes');
  const [typed, setTyped] = useState('');
  const [answer, setAnswer] = useState<TagSuggestions | null>(null);
  // A failed read is shown as a Retry button rather than as an empty list, which would read as
  // "no node carries a tag".
  const [failed, setFailed] = useState(false);
  const [attempt, setAttempt] = useState(0);
  const settled = useDebouncedValue(typed, typed.trim() ? SEARCH_DEBOUNCE_MS : 0);
  const chosen = decodeSet(value);
  const limit = tagRequestLimit(settled, chosen.length);

  useEffect(() => {
    let cancelled = false;
    api
      .suggestNodeTags(settled.trim(), limit, value)
      .then((a) => {
        if (cancelled) return;
        setAnswer(a);
        setFailed(false);
      })
      .catch(() => {
        // The chosen tags still show; only the offer is missing.
        if (cancelled) return;
        setAnswer(null);
        setFailed(true);
      });
    return () => {
      cancelled = true;
    };
  }, [settled, limit, value, attempt]);

  const view = tagPickerView(answer, value, settled);
  return (
    <fieldset className="invf-sec">
      <legend className="invf-h">{label}</legend>
      <SearchField
        value={typed}
        placeholder={t('inventory.tagFilter.search')}
        aria-label={t('inventory.tagFilter.search')}
        onChange={(e) => setTyped(e.target.value)}
        onClear={() => setTyped('')}
      />
      <MultiSelectList
        options={view.options.map((v) => ({ value: v, label: v }))}
        selected={chosen}
        counts={view.counts}
        onToggle={(tag) => onChange(toggleTag(value, tag))}
        onClear={() => onChange('')}
        label={label}
        searchThreshold={Number.POSITIVE_INFINITY}
      />
      {failed && (
        <Button type="button" variant="ghost" onClick={() => setAttempt((n) => n + 1)}>
          {t('common:actions.retry')}
        </Button>
      )}
      {view.more > 0 && (
        <p className="form-status">
          {t('inventory.tagFilter.more', { count: view.more })}
        </p>
      )}
      {/* Under the list, so a row ticked under the pointer does not move (ADR-267 decision 6). */}
      {chosen.length >= 2 && (
        <Segmented
          size="sm"
          ariaLabel={t('common:tagMatch.label')}
          options={TAG_MATCHES.map((m) => ({ value: m, label: t(TAG_MATCH_LABEL[m]) }))}
          value={match}
          onChange={(v) => {
            const m = TAG_MATCHES.find((x) => x === v);
            if (m) onMatch(m);
          }}
        />
      )}
    </fieldset>
  );
}
