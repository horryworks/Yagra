// SPDX-License-Identifier: AGPL-3.0-only
// The chips above the active-alert list that narrow it (ADR-227): today one, the rows grouped into
// an outage. Only a non-zero chip is drawn, so an all-clear fleet looks exactly as it did before.
// The judgement is `lib/watchCounts.ts`. What is not a filter on this list — nodes sending no data —
// is the sentence `NotWatchedLine` draws instead (ADR-227 Inc.2).

import { Link } from 'react-router-dom';
import { Trans, useTranslation } from 'react-i18next';
import type { NotWatchedLine as Line, WatchChip } from '../lib/watchCounts';
import { Callout } from '../components/ui/Section';
import './WatchChips.css';

export function WatchChips({
  chips,
  onlySilenced,
  onToggleSilenced,
}: {
  chips: WatchChip[];
  onlySilenced: boolean;
  onToggleSilenced: () => void;
}) {
  const { t } = useTranslation('alerts');
  // Kept while narrowed even at zero, so the control that undoes the narrowing never vanishes.
  const shown = onlySilenced && !chips.some((c) => c.key === 'silenced')
    ? [{ key: 'silenced' as const, count: 0 }, ...chips]
    : chips;
  if (shown.length === 0) return null;
  return (
    <div className="watch-chips">
      {shown.map((c) => (
        <button
          key={c.key}
          type="button"
          className="watch-chip"
          aria-pressed={onlySilenced}
          onClick={onToggleSilenced}
        >
          {t('watch.silenced', { count: c.count })}
        </button>
      ))}
    </div>
  );
}

/** "N nodes are sending no data" with a link to where to start. */
export function NotWatchedLine({ line }: { line: Line | null }) {
  if (!line) return null;
  return (
    <Callout tone="warning" status className="not-watched">
      <Trans
        i18nKey="alerts:watch.notWatched"
        count={line.count}
        components={{ lnk: <Link to={line.to} /> }}
      />
    </Callout>
  );
}
