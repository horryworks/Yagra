// SPDX-License-Identifier: AGPL-3.0-only
// The numbers above the active-alert list that say what it is not seeing (ADR-227): silenced rows,
// nodes in maintenance, nodes not watched. Only the non-zero ones are drawn, so an all-clear
// fleet looks exactly as it did before. The judgement is `lib/watchCounts.ts`.

import { Link } from 'react-router-dom';
import { useTranslation } from 'react-i18next';
import type { WatchChip } from '../lib/watchCounts';
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
      {shown.map((c) => {
        const label = t(`watch.${c.key}`, { count: c.count });
        return c.to ? (
          <Link key={c.key} to={c.to} className="watch-chip">
            {label}
          </Link>
        ) : (
          <button
            key={c.key}
            type="button"
            className="watch-chip"
            aria-pressed={onlySilenced}
            onClick={onToggleSilenced}
          >
            {label}
          </button>
        );
      })}
    </div>
  );
}
