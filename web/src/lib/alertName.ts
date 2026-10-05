// SPDX-License-Identifier: AGPL-3.0-only
// What an alert is called on screen — "SNMP not responding" rather than `snmp_up` (ADR-196).
//
// The names are not written here. English is generated from `metric_meaning.rs` into
// `locales/en/alertNames.json`, Japanese is the hand-written translation beside it, and which
// metrics are 0/1 answers comes from the generated `api/alertNameFlags.json`. So this module only
// answers the lookup, and a metric Yagra has no name for (an operator's own collection item, a
// metric a newer core introduced) answers `null` and keeps its raw spelling.
//
// A `.ts` file on purpose: Vitest never executes a `.tsx` (`testing.md`).

import i18n from '../i18n';
import enAlertNames from '../locales/en/alertNames.json';
import alertNameFlags from '../api/alertNameFlags.json';

/** The prefix an event rule's alert carries in front of the rule's name (`events/mod.rs`). */
export const EVENT_METRIC_PREFIX = 'event:';

const NAMED: ReadonlySet<string> = new Set(Object.keys(enAlertNames));
const FLAGS: ReadonlySet<string> = new Set(alertNameFlags);

export interface AlertTitle {
  /** The name in the operator's language. */
  text: string;
  /** A 0/1 answer: the name states the fault, so the condition and value are left off. */
  flag: boolean;
}

/** The i18n key for `metric`'s alert name, or `null` when Yagra has none for it. */
export function alertNameKey(metric: string): string | null {
  return NAMED.has(metric) ? `alertNames:${metric}` : null;
}

/** What an alert on `metric` is called, or `null` when Yagra has no name for it. */
export function alertTitle(metric: string): AlertTitle | null {
  if (metric.startsWith(EVENT_METRIC_PREFIX)) {
    // An event rule has no threshold, so there is no condition to hide — `flag` only says the
    // name stands on its own.
    return {
      text: i18n.t('format:alertEventRule', { name: metric.slice(EVENT_METRIC_PREFIX.length) }),
      flag: true,
    };
  }
  const key = alertNameKey(metric);
  return key ? { text: i18n.t(key), flag: FLAGS.has(metric) } : null;
}
