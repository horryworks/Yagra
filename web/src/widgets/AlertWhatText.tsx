// SPDX-License-Identifier: AGPL-3.0-only
// "What fired" — the metric and, for a threshold check, the condition it crossed and the observed
// value. Rendered identically on Alerts ▸ History and on the Active alerts triage row, which is the
// whole reason it lives here: the two screens read the same fact out of two different shapes (the
// history row flattens the breach into columns, the live alert nests it), so the *formatting*
// decision is `alertWhat`/`alertWhatOf` in lib/format.ts and the *markup* is this one component.
// Writing the eight lines twice is how the pair drifts.

import { useTranslation } from 'react-i18next';
import type { AlertWhat } from '../lib/format';

/** Liveness up/down reads as "Reachability" (never the raw `__liveness__` sentinel); a row with no
 *  captured metric — an alert raised before migration 0036 — reads as "—". */
export function AlertWhatText({ what }: { what: AlertWhat }) {
  const { t } = useTranslation();
  if (what.kind === 'none') return <span className="muted">—</span>;
  if (what.kind === 'liveness') return <span>{t('format:liveness')}</span>;
  return (
    <span>
      <span className="mono">{what.metric}</span>
      {what.condition && <span className="muted"> {what.condition}</span>}
      {what.observed && <span className="muted"> ({what.observed})</span>}
    </span>
  );
}
