// Status summary widget (§8 NOC board): a roll-up of node counts by state. Color is the
// canonical status palette (status = state only). Today the inventory endpoint derives a
// coarse state (ok when a recent RTT exists, else unknown); richer states light up as the
// alert/threshold engine feeds back.

import { useTranslation } from 'react-i18next';
import { stateColorVar, stateLabel } from '../lib/format';
import type { NodeState, NodeSummary } from '../types/api';
import './StatusSummary.css';

const ORDER: NodeState[] = ['critical', 'unreachable', 'warning', 'unknown', 'maintenance', 'ok'];

export function StatusSummary({
  nodes,
  loading,
}: {
  nodes: NodeSummary[];
  loading?: boolean;
}) {
  const { t } = useTranslation();
  const counts = nodes.reduce<Record<string, number>>((acc, n) => {
    acc[n.state] = (acc[n.state] ?? 0) + 1;
    return acc;
  }, {});
  const present = ORDER.filter((s) => counts[s]);

  return (
    <div className="statussummary">
      <div className="statussummary-total">
        <div className="statussummary-num">{nodes.length}</div>
        <div className="statussummary-cap">{t('noun.node', { count: nodes.length })}</div>
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
