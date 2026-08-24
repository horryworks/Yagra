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
//
// **The sentences themselves moved to Rust (ADR-079 決定 4).** They used to live only in
// `locales/{en,ja}/metrics.json`, which made them something the WebUI knew and `/mcp` did not —
// the alert-rule table has a "What it measures" column and an MCP client reading the same
// ruleset got a bare metric name. English is now canonical in
// `crates/yagra-core/src/metric_meaning.rs`, `locales/en/metricMeanings.json` is generated from
// it, and `locales/ja/metricMeanings.json` is the translation. Nothing here is hand-kept in step
// with Rust: the explained set below is read straight off the generated file.

import { LIVENESS_METRIC } from './format';
import builtinCatalog from '../api/metricCatalog.json';
import enMetricMeanings from '../locales/en/metricMeanings.json';

/**
 * Metrics Yagra's own checks emit — the reachability probes and the URL / DNS / Meraki monitors.
 *
 * These names are constants and literals scattered across `yagra-common`, `yagra-transport` and
 * the poller, and they have no `mib_catalog` row, which is exactly why they need listing —
 * nothing else knows they exist.
 *
 * ⚠️ **This list is now duplicated in Rust** (`metric_meaning::CHECK_METRICS`), where it makes
 * the sentence table checkable. It survives here because the picker's *grouping* is a WebUI
 * concern — "Yagra’s own checks" is a heading, not a fact about the backend — and a test pins it
 * to be a subset of the generated meanings, so the two cannot come to disagree about a name.
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

/**
 * Metrics Yagra **derives** rather than collects: one port's traffic, as a percentage of its own
 * speed and as an absolute rate (ADR-076).
 *
 * A category of their own, not members of `CHECK_METRICS`, because they differ from those in the
 * two ways a picker has to show: they are per-interface, and they exist in no time series at all —
 * they are computed at evaluation time from a counter rate and `interfaces.if_speed` (ADR-012), so
 * they will never appear in the metric list of a node, nor on a chart reached from one.
 *
 * Receive and transmit are separate names because a link is asymmetric far more often than not,
 * and "which direction is congested" is the first thing an operator asks.
 *
 * Percentage and absolute are separate names for a harder reason: **the percentage cannot be
 * evaluated at all on a port whose speed the device does not report**, because that speed is its
 * denominator. Before the absolute pair existed, such a port could carry no traffic rule of any
 * kind — the octet counters are counters, and a fixed bound on a monotonic value is refused
 * outright (ADR-012). They are the reason "800 Mbps" is expressible at all.
 */
export const DERIVED_METRICS = [
  'if_in_util_pct',
  'if_out_util_pct',
  'if_in_bps',
  'if_out_bps',
  // Node-level (ADR-105). Devices report two raw numbers — a total and a free, or a used and a
  // free — and the percentage everyone actually monitors on exists in no series at all. Same
  // deal as the four above: computed at evaluation time, so `query_metrics` cannot return them.
  'cisco_cemp_mem_used_pct',
  'cisco_cpu_mem_used_pct',
  'cisco_mem_used_pct',
  'hr_storage_used_pct',
  'huawei_mem_used_pct',
  'poe_power_used_pct',
  'ucd_cpu_used_pct',
  'ucd_load_per_core',
  'ucd_mem_used_pct',
  'ucd_swap_used_pct',
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
 * (`` t(`metricMeanings:${metric}`) ``), so a member with no strings renders a raw key in *both*
 * languages — which EN/JA parity passes, because both are equally missing it
 * (`extensibility.md` §4). English can no longer be the missing half (it is generated), so what
 * this now guards is the Japanese translation of a metric Rust has just started explaining.
 *
 * **Gauges only**, and *derived in Rust* rather than listed. A counter can carry no threshold rule
 * (a fixed bound cannot be evaluated against a monotonic value, ADR-012), so the picker never
 * offers one and the rule table can never show one — a sentence written for `if_hc_in_octets` is a
 * sentence nobody can reach, and an unread string is what drifts. A counter falls back to its
 * catalog facts like any other unexplained name.
 *
 * Reading the keys off the generated file rather than recomputing the union here is what makes
 * "add a template in Rust, forget the sentence" a **Rust** test failure
 * (`every_metric_a_rule_can_name_has_a_sentence_and_no_others_do`) instead of a silently missing
 * column. `metricMeaning.test.ts` still checks the union from this side, which now compares two
 * generated artefacts against each other rather than a hand-list against itself.
 */
export const EXPLAINED_METRICS: readonly string[] = Object.keys(enMetricMeanings);

const EXPLAINED = new Set<string>(EXPLAINED_METRICS);

/**
 * The i18n key for `metric`'s one-line meaning, or `null` when Yagra has nothing to say about it.
 *
 * `null` rather than a generic fallback sentence: inventing prose here — "a metric collected from
 * this device" — would fill the column with words that carry no information. The callers do
 * something better with the `null` than a sentence could: the table shows an em dash, and the
 * picker shows the catalog facts (which metric set, which OID) it does have.
 *
 * A namespace of its own rather than `metrics:meaning.*`: the English half is a generated build
 * output and the `picker.*` strings beside it are hand-written, so sharing one file would mean
 * the generator either clobbering hand-written text or having to merge into it.
 */
export function metricMeaningKey(metric: string): string | null {
  return EXPLAINED.has(metric) ? `metricMeanings:${metric}` : null;
}
