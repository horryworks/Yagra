// SPDX-License-Identifier: AGPL-3.0-only
// Status summary widget (§8 NOC board): a roll-up of node counts by state. Color is the
// canonical status palette (status = state only). Today the inventory endpoint derives a
// coarse state (ok when a recent RTT exists, else unknown); richer states light up as the
// alert/threshold engine feeds back.

import { useTranslation } from 'react-i18next';
import { stateColorVar, stateLabel } from '../lib/format';
import { SEVERITY_ORDER } from '../lib/nodeState';
import type { NodeState } from '../types/api';
import './StatusSummary.css';

/** Roll-up of node counts by state. `counts` + `total` are computed server-side over the whole
 *  fleet (`/fleet/summary`), so this stays correct beyond the first page of nodes (S12). */
export function StatusSummary({
  counts,
  total,
  loading,
}: {
  counts: Partial<Record<NodeState, number>>;
  total: number;
  loading?: boolean;
}) {
  const { t } = useTranslation();
  const present = SEVERITY_ORDER.filter((s) => counts[s]);

  return (
    <div className="statussummary">
      <div className="statussummary-total">
        <div className="statussummary-num">{total}</div>
        <div className="statussummary-cap">{t('noun.node', { count: total })}</div>
      </div>
      <div className="statussummary-grid">
        {present.length === 0 && (
          <span className="muted">
            {loading ? t('statusSummary.loading') : t('statusSummary.empty')}
          </span>
        )}
        {present.map((s) => (
          <div className="statussummary-item" key={s}>
            <span className="statussummary-dot" style={{ background: stateColorVar(s) }} />
            <span className="statussummary-count">{counts[s]}</span>
            <span className="statussummary-label">{stateLabel(s)}</span>
          </div>
        ))}
      </div>
    </div>
  );
}
