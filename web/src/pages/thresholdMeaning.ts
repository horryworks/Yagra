// SPDX-License-Identifier: AGPL-3.0-only
// What a metric actually measures, for the one screen where an operator meets a bare metric name
// with no other way to look it up.
//
// The rule row `Reachability | below | (no bounds) | 3 breaches` is unreadable without this: it
// says how the rule behaves and nothing about what it is watching. Every other column on that
// screen is a property of the *rule*; this is the only one that is a property of the *metric*.
//
// **Scope, and why it is a real boundary rather than "the ones we got round to".** These are the
// metrics **Yagra emits itself**, which by construction have no entry in Nodes ▸ Metric sets — the
// reachability probes and the URL / DNS / Meraki monitors. A metric that *does* have a collection
// item is already explained where it is defined (name, OID, kind), and duplicating that here would
// be a second place for the same fact to rot. So: no collection item ⇒ it belongs here; has one ⇒
// it does not. `snmp_sys_uptime_ticks` is the near miss that shows the line is real — it is offered
// as a preset and is in five built-in templates, so it is explained there.
//
// A `.ts` file on purpose: Vitest runs `environment: 'node'` and never executes `.tsx`, so a lookup
// written inside the page component is a lookup nothing tests (`testing.md`).

import { LIVENESS_METRIC } from '../lib/format';

/**
 * Every metric this module explains, as a runtime array so `i18nEnumKeys.test.ts` can demand a
 * string for each in **both** locales.
 *
 * That test is the whole reason this is an array rather than a bare `Record`: the key is built at
 * runtime (`` t(`thresholds.meaning.${metric}`) ``), so a member with no strings renders a raw key
 * in *both* languages — which EN/JA parity passes, because both are equally missing it
 * (`extensibility.md` §4).
 */
export const EXPLAINED_METRICS = [
  LIVENESS_METRIC,
  'icmp_rtt_ms',
  'icmp_loss_pct',
  'snmp_up',
  'snmp_neighbor_count',
  'snmp_l3_address_count',
  'snmp_routing_adjacency_count',
  'snmp_arp_entry_count',
  'http_up',
  'http_status_code',
  'http_response_time_ms',
  'http_body_match',
  'ssl_cert_days_to_expiry',
  'dns_up',
  'dns_resolve_ms',
  'dns_answer_count',
  'dns_chain_length',
  'meraki_device_up',
] as const;

/** A metric this module has a sentence for. */
export type ExplainedMetric = (typeof EXPLAINED_METRICS)[number];

const EXPLAINED = new Set<string>(EXPLAINED_METRICS);

/**
 * The i18n key for `metric`'s one-line meaning, or `null` when Yagra has nothing to say about it.
 *
 * `null` rather than a generic fallback sentence: a rule on an operator's own collected metric is
 * explained by the collection item that defines it, and inventing prose here — "a metric collected
 * from this device" — would fill the column with words that carry no information. An em dash is a
 * more honest answer than a sentence that says nothing.
 */
export function metricMeaningKey(metric: string): string | null {
  return EXPLAINED.has(metric) ? `thresholds.meaning.${metric}` : null;
}
