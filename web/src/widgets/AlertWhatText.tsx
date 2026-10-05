// SPDX-License-Identifier: AGPL-3.0-only
// "What fired" — the alert's name (ADR-196) and, for a threshold check, the condition it crossed
// and the observed value. Rendered identically on Alerts ▸ History and on the Active alerts triage row, which is the
// whole reason it lives here: the two screens read the same fact out of two different shapes (the
// history row flattens the breach into columns, the live alert nests it), so the *formatting*
// decision is `alertWhat`/`alertWhatOf` in lib/format.ts and the *markup* is this one component.
// Writing the eight lines twice is how the pair drifts.

import { alertWhatParts, type AlertWhat } from '../lib/format';

const PART_CLASS = { name: undefined, mono: 'mono', muted: 'muted', metric: 'mono muted' } as const;

/** Liveness up/down reads as "Node not responding" (never the raw `__liveness__` sentinel); a row
 *  with no captured metric — an alert raised before migration 0036 — reads as "—". Which pieces
 *  appear, and in what order, is `alertWhatParts`' decision (lib/format.ts), where a test reaches it.
 *
 *  🚨 **The parts are a list, and the tooltip is that list joined** — not a second sentence written
 *  beside the markup. The Active-alerts row clips this whole span at whatever width the widget
 *  happens to be, and ADR-088's first sweep found the condition and the observed value cut off
 *  entirely on three screens (`"above 1" is cut off by 76px`). The row's own `title` is the check
 *  id, which is a different question — a tooltip that answers something else is the failure mode
 *  that check is written to see through, so the answer had to be here, on the element that holds
 *  the words. Building the spans and the string from one array is what stops them drifting. */
export function AlertWhatText({ what }: { what: AlertWhat }) {
  const parts = alertWhatParts(what);
  if (parts.length === 0) return <span className="muted">—</span>;
  return (
    <span title={parts.map((p) => p.text).join(' ')}>
      {parts.map((p, i) => (
        <span key={i} className={PART_CLASS[p.style]}>
          {i > 0 ? ' ' : ''}
          {p.text}
        </span>
      ))}
    </span>
  );
}
