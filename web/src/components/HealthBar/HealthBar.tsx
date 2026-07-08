// A proportional stacked health bar: one segment per node state, width = its share of the node
// set, colored from the canonical status palette (stateColorVar — never an ad-hoc color). Used for
// the per-group rollup on tree rows and the full-width Health section on the group-detail pane. An
// empty set renders just the neutral track. Status is conveyed by color + the title tooltip and is
// always paired with text counts elsewhere (legend / count pill), so it is not color-alone.

import { useTranslation } from 'react-i18next';
import type { NodeSummary } from '../../types/api';
import { stateColorVar, stateLabel } from '../../lib/format';
import { STATE_ORDER, tallyStates } from '../../lib/nodeTree';
import './HealthBar.css';

interface Props {
  nodes: NodeSummary[];
  className?: string;
}

export function HealthBar({ nodes, className }: Props) {
  const { t } = useTranslation();
  const { counts, total } = tallyStates(nodes);
  const segments = STATE_ORDER.filter((s) => counts[s] > 0);
  const title = segments.map((s) => `${counts[s]} ${stateLabel(s)}`).join(' · ') || t('healthbar.noNodes');
  return (
    <span className={['healthbar', className].filter(Boolean).join(' ')} title={title}>
      {total > 0 &&
        segments.map((s) => (
          <span
            key={s}
            className="healthbar-seg"
            style={{ width: `${(counts[s] / total) * 100}%`, background: stateColorVar(s) }}
          />
        ))}
    </span>
  );
}
