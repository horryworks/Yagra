// SPDX-License-Identifier: AGPL-3.0-only
// Like RankedBars, but each value is a signed delta: the bar width is proportional to the
// magnitude and the value reads with a ▲/▼ and sign. Powers traffic spikes/drops. The fill is a
// series color (a delta direction is not a node status), passed in by the widget.

import { useTranslation } from 'react-i18next';
import './DeltaBars.css';

export interface DeltaRow {
  label: string;
  /** Signed magnitude (drives bar width via |value| and the ▲/▼ via sign). */
  value: number;
  /** Right-aligned display (e.g. "▲ +1.2 Gbps"); defaults to the signed number. */
  valueText?: string;
}

interface Props {
  rows: DeltaRow[];
  /** Fill color (CSS var); default series-4. */
  color?: string;
  empty?: string;
}

export function DeltaBars({ rows, color = 'var(--series-4)', empty }: Props) {
  const { t } = useTranslation('dashboard');
  if (rows.length === 0) return <p className="muted">{empty ?? t('primitives.deltaBars.empty')}</p>;
  const peak = Math.max(...rows.map((r) => Math.abs(r.value)), 0);
  return (
    <ul className="deltabars">
      {rows.map((r) => {
        const pct = peak > 0 ? Math.max(0, Math.min(100, (Math.abs(r.value) / peak) * 100)) : 0;
        const arrow = r.value > 0 ? '▲' : r.value < 0 ? '▼' : '◆';
        return (
          <li className="deltabar" key={r.label}>
            <span className="deltabar-label" title={r.label}>
              {r.label}
            </span>
            <span className="deltabar-track">
              <span className="deltabar-fill" style={{ width: `${pct}%`, background: color }} />
            </span>
            <span className="deltabar-value">
              <span className="deltabar-arrow" aria-hidden="true">
                {arrow}
              </span>
              {r.valueText ?? String(r.value)}
            </span>
          </li>
        );
      })}
    </ul>
  );
}
