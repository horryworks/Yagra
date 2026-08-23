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
 *  captured metric — an alert raised before migration 0036 — reads as "—".
 *
 *  🚨 **The parts are a list, and the tooltip is that list joined** — not a second sentence written
 *  beside the markup. The Active-alerts row clips this whole span at whatever width the widget
 *  happens to be, and ADR-088's first sweep found the condition and the observed value cut off
 *  entirely on three screens (`"above 1" is cut off by 76px`). The row's own `title` is the check
 *  id, which is a different question — a tooltip that answers something else is the failure mode
 *  that check is written to see through, so the answer had to be here, on the element that holds
 *  the words. Building the spans and the string from one array is what stops them drifting. */
export function AlertWhatText({ what }: { what: AlertWhat }) {
  const { t } = useTranslation();
  if (what.kind === 'none') return <span className="muted">—</span>;
  if (what.kind === 'liveness') {
    const liveness = t('format:liveness');
    return <span title={liveness}>{liveness}</span>;
  }
  // The port comes right after the metric because that is what the pair means — "this metric, on
  // this port" — and before the bound, so two alerts on one node read as different lines rather
  // than as a repeat.
  const parts: { text: string; mono: boolean }[] = [{ text: what.metric, mono: true }];
  if (what.ifindex != null) {
    parts.push({ text: t('format:alertOnPort', { ifindex: what.ifindex }), mono: true });
  }
  if (what.condition) parts.push({ text: what.condition, mono: false });
  if (what.observed) parts.push({ text: `(${what.observed})`, mono: false });
  return (
    <span title={parts.map((p) => p.text).join(' ')}>
      {parts.map((p, i) => (
        <span key={i} className={p.mono ? 'mono' : 'muted'}>
          {i > 0 ? ' ' : ''}
          {p.text}
        </span>
      ))}
    </span>
  );
}
