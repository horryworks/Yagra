// SPDX-License-Identifier: AGPL-3.0-only
// What colour a passive event is drawn in: by its kind (syslog / trap / webhook) and by what the
// pipeline did with it (its action).
//
// These mappings used to be written out three times — the dashboard's event widgets, the event
// log's Result column and Troubleshoot's rule-gap report — and the copies disagreed: a trap was
// `--series-2` on the dashboard and `--series-5` in Troubleshoot. The dashboard copies were also
// keyed by `string`, so an action the backend added later fell through to a default without a
// compile error. Keyed by the generated unions now, a new member is a type error here and nowhere
// else. `eventTone.test.ts` refuses a second copy anywhere under `src/`.
//
// ⚠️ **Kinds are categories, not statuses.** A trap is not worse than a syslog line, so the kind
// colours come from the series palette. Actions are different: `fired` / `refreshed` / `cleared`
// are the alert lifecycle and take the status palette, like everywhere else in the product.

import type { Tone as BadgeTone } from '../ui/Badge';
import { EVENT_ACTIONS, EVENT_KINDS, type EventAction, type EventKind } from '../../types/api';

/** An action as a `Badge` tone — the event log's Result column and the dashboard feed. */
export const ACTION_TONE: Record<EventAction, BadgeTone> = {
  fired: 'critical',
  refreshed: 'warning',
  cleared: 'up',
  info: 'info',
  suppressed: 'neutral',
  none: 'neutral',
};

/** An action as a chart colour: the lifecycle on the status channel, the rest on the series one. */
export const ACTION_COLOR: Record<EventAction, string> = {
  fired: 'var(--status-critical)',
  refreshed: 'var(--status-warning)',
  cleared: 'var(--status-ok)',
  info: 'var(--series-3)',
  suppressed: 'var(--series-4)',
  none: 'var(--series-5)',
};

/** A kind as a chart colour. Categorical — see the module header. */
export const KIND_COLOR: Record<EventKind, string> = {
  syslog: 'var(--series-1)',
  trap: 'var(--series-5)',
  webhook: 'var(--series-3)',
};

function isKind(key: string): key is EventKind {
  return (EVENT_KINDS as readonly string[]).includes(key);
}

function isAction(key: string): key is EventAction {
  return (EVENT_ACTIONS as readonly string[]).includes(key);
}

/** `KIND_COLOR` for a key that arrived as a plain string (a stats bucket, a finding's detail).
 *  `undefined` for a kind this build does not know — a newer core's — so each caller keeps its own
 *  fallback. An own-key check rather than an index, so `'constructor'` is not a colour. */
export function kindColorOf(key: string): string | undefined {
  return isKind(key) ? KIND_COLOR[key] : undefined;
}

/** `ACTION_COLOR` for a plain-string key; `undefined` for an unknown action. */
export function actionColorOf(key: string): string | undefined {
  return isAction(key) ? ACTION_COLOR[key] : undefined;
}

/** `ACTION_TONE` for a plain-string key; `undefined` for an unknown action. */
export function actionToneOf(key: string): BadgeTone | undefined {
  return isAction(key) ? ACTION_TONE[key] : undefined;
}
