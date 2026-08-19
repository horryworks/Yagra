// SPDX-License-Identifier: AGPL-3.0-only
// What a metric actually measures, in one sentence — for every screen where an operator meets a
// bare metric name and has no other way to look it up.
//
// Two of those exist. The alert-rule table's "measures" column, where the row
// `Reachability | below | (no bounds) | 3 breaches` says how the rule behaves and nothing about
// what it is watching. And the metric picker (ADR-075 増分 4), where the sentence has to be read
// *before* choosing — a threshold on `bgp_peer_state` is `above 3` or `below 3` depending on which
// of 1–6 means established, and no OID tells anyone that.
//
// **Scope, and how it moved.** This module first covered only the metrics Yagra emits itself, on
// the argument that a metric with a collection item "is already explained where it is defined".
// That was right about the OID and the gauge/counter kind — copying those here would be a second
// place for one fact to rot — and wrong about the meaning, because **a collection item holds an
// OID and a kind, not a sentence**. So the line is redrawn rather than removed:
//
//   the meaning lives here · where it comes from (OID, which metric set, per-port or per-device)
//   comes from the catalog
//
// The picker shows both, and falls back to the catalog facts alone for a metric this module has
// nothing to say about — an operator's own collection item, or a `mib_catalog` row left behind by
// an older release.
//
// A `.ts` file on purpose: Vitest runs `environment: 'node'` and never executes `.tsx`, so a lookup
// written inside the page component is a lookup nothing tests (`testing.md`).

import { LIVENESS_METRIC } from './format';
import builtinCatalog from '../api/metricCatalog.json';

/**
 * Metrics Yagra's own checks emit — the reachability probes and the URL / DNS / Meraki monitors.
 *
 * Hand-written, and it has to be: these names are constants and literals scattered across
 * `yagra-common`, `yagra-transport` and the poller, with **no catalog on the Rust side** to
 * generate from. They have no `mib_catalog` row either, which is exactly why they need listing —
 * nothing else knows they exist.
 */
export const CHECK_METRICS = [
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

/** One row of the generated built-in catalog (`web/src/api/metricCatalog.json`). */
export interface BuiltinMetric {
  metric_name: string;
  metric_kind: 'gauge' | 'counter';
  /** Whether this metric publishes one series per interface. Decided by an OID rule in Rust. */
  per_interface: boolean;
}

/**
 * Every metric collected by a built-in metric set, generated from the Rust catalog.
 *
 * Generated rather than transcribed (`extensibility.md` §2): a hand-kept copy of 88 names drifts
 * the moment someone adds a template, and the drift here is **silent** — the picker just shows an
 * OID where a sentence should be. `mib.rs::the_committed_metric_catalog_is_current` regenerates it.
 */
export const BUILTIN_METRICS: readonly BuiltinMetric[] = builtinCatalog as BuiltinMetric[];

/** The generated row for `metric`, or `undefined` for an operator-defined one. */
export function builtinMetric(metric: string): BuiltinMetric | undefined {
  return BUILTIN_METRICS.find((m) => m.metric_name === metric);
}

/**
 * Every metric this module owes an explanation for, as a runtime array so
 * `i18nEnumKeys.test.ts` can demand a string for each in **both** locales.
 *
 * That test is the whole point of the array: the key is built at runtime
 * (`` t(`metrics:meaning.${metric}`) ``), so a member with no strings renders a raw key in *both*
 * languages — which EN/JA parity passes, because both are equally missing it
 * (`extensibility.md` §4). Deriving the second half from the generated catalog is what makes
 * adding a metric in Rust fail a WebUI test until the sentences are written.
 *
 * **Gauges only**, and derived rather than listed. A counter can carry no threshold rule (a fixed
 * bound cannot be evaluated against a monotonic value, ADR-012), so the picker never offers one and
 * the rule table can never show one — a sentence written for `if_hc_in_octets` is a sentence nobody
 * can reach, and an unread string is what drifts. That is the same reasoning the scope-id
 * placeholder check already applies to the `global` level. A counter falls back to its catalog
 * facts like any other unexplained name.
 */
export const EXPLAINED_METRICS: readonly string[] = [
  ...CHECK_METRICS,
  ...BUILTIN_METRICS.filter((m) => m.metric_kind === 'gauge').map((m) => m.metric_name),
];

const EXPLAINED = new Set<string>(EXPLAINED_METRICS);

/**
 * The i18n key for `metric`'s one-line meaning, or `null` when Yagra has nothing to say about it.
 *
 * `null` rather than a generic fallback sentence: inventing prose here — "a metric collected from
 * this device" — would fill the column with words that carry no information. The callers do
 * something better with the `null` than a sentence could: the table shows an em dash, and the
 * picker shows the catalog facts (which metric set, which OID) it does have.
 */
export function metricMeaningKey(metric: string): string | null {
  return EXPLAINED.has(metric) ? `metrics:meaning.${metric}` : null;
}
