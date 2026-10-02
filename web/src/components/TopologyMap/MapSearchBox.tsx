// SPDX-License-Identifier: AGPL-3.0-only
// The search box over one level of the network map (ADR-191 Inc.11, Inc.13): the column filter's
// editor, how many it found, and Enter / Shift+Enter stepping through the hits. The full map puts it
// in its page header; a folder's pane puts it in the map's own heading row (`inline`).

import { useTranslation } from 'react-i18next';
import { TextConditionEditor } from '../ui/TextConditionEditor';
import { isImeComposing } from '../../lib/ime';
import type { MapSearchState } from './useMapSearch';
import './MapSearchBox.css';

interface Props {
  state: MapSearchState;
  /** One row beside a heading rather than a stacked block in a page header. */
  inline?: boolean;
}

export function MapSearchBox({ state, inline }: Props) {
  const { t } = useTranslation('topology');
  const { cond, setCond, search, step } = state;
  return (
    <div
      className={inline ? 'mapsearch inline' : 'mapsearch'}
      title={t('map.search.stepHint')}
      onKeyDown={(e) => {
        // The editor commits on Enter itself; here Enter also moves to the next hit.
        if (e.key !== 'Enter' || isImeComposing(e)) return;
        const box = e.target as HTMLInputElement;
        if (box.tagName !== 'INPUT') return;
        step(e.shiftKey ? -1 : 1, box.value);
      }}
    >
      <TextConditionEditor
        value={cond}
        onChange={setCond}
        modes={['contains', 'regex']}
        allowNot
        placeholder={t('map.search.placeholder')}
      />
      {search.matched && (
        <p className="mapsearch-count" aria-live="polite">
          {t('map.search.count', { count: search.total })}
          {search.undrawn > 0 && ' ' + t('map.search.undrawn', { count: search.undrawn })}
        </p>
      )}
    </div>
  );
}
