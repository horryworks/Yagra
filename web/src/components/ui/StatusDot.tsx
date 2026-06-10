// Status indicator (§1.3): a colored dot + text label. Status color is a domain concept —
// it comes from stateColorVar (the canonical status palette) and is NEVER reused for
// decoration. Pairs color with text so it's not color-alone (accessibility).

import { stateColorVar, stateLabel } from '../../lib/format';
import type { NodeState } from '../../types/api';
import './StatusDot.css';

interface Props {
  state: NodeState;
  /** Show the text label next to the dot (default true). */
  withLabel?: boolean;
}

export function StatusDot({ state, withLabel = true }: Props) {
  return (
    <span className="statusdot-wrap">
      <span
        className="statusdot"
        style={{ background: stateColorVar(state) }}
        title={stateLabel(state)}
        aria-label={stateLabel(state)}
      />
      {withLabel && <span className="statusdot-label">{stateLabel(state)}</span>}
    </span>
  );
}
