// SPDX-License-Identifier: AGPL-3.0-only
// Turning the deployment's MIB catalog into the list a metric picker shows (ADR-075 増分 4).
//
// A `.ts` on purpose: Vitest runs `environment: 'node'` and never executes `.tsx`, so judgement
// left inside the component is judgement nothing tests (`testing.md`). Everything here is a pure
// function of (catalog rows, a translator) — the component only draws the result.
//
// Two sources, deliberately unequal:
//
//  - **Yagra's own check metrics** are hard-coded (`CHECK_METRICS`) because they have no catalog
//    row anywhere — they are emitted by the poller, not collected from a device. They lead the
//    list because they apply to every node, whatever it is.
//  - **Collected metrics** come from `GET /api/v1/mib-catalog` at runtime, so an operator's own
//    entries appear without this file knowing about them.
//
// ⚠️ **The catalog is what a deployment could collect, not what it does collect.** A Huawei-only
// fleet still sees the Cisco entries. That was already true of the free-text field this replaces,
// but a list of choices reads as "these are valid", so the grouping keeps the metric set visible
// and `picker.scopeNote` says it outright. Narrowing to what is actually collected needs an
// endpoint that does not exist (see the backlog).

import type { MibCatalogEntry } from '../types/api';
import { LIVENESS_METRIC } from './format';
import {
  CHECK_METRICS,
  DERIVED_METRICS,
  builtinMetric,
  metricMeaningKey,
} from './metricMeaning';

/** One row of the picker. */
export interface MetricOption {
  /** The metric name, i.e. the value that gets stored on the rule. */
  name: string;
  /**
   * What to show for it. The same as `name` everywhere except the liveness sentinel, which is an
   * internal token (`__liveness__`) rather than a series and is shown by its human name — the
   * rule table already does this, and a picker that showed the raw token would be teaching the
   * operator a string they must never type.
   */
  label: string;
  /** Heading this row is filed under. */
  group: string;
  /** The one-line meaning, or `''` when there is none. */
  meaning: string;
  /** Where it comes from: the OID, for a collected metric. Empty for Yagra's own checks. */
  oid: string;
  /** Whether it publishes one series per interface. `undefined` when nobody can say. */
  perInterface?: boolean;
}

/** How the picker's headings are labelled — supplied by the caller so this stays translator-free. */
export interface MetricLabels {
  /** Heading for Yagra's own checks. */
  checks: string;
  /** Heading for the metrics Yagra derives rather than collects (interface utilisation). */
  derived: string;
  /** Heading for catalog rows with no metric set named (the vendor-less standard set). */
  standard: string;
  /** The meaning of a metric, by key, or `''`. */
  meaning: (key: string) => string;
  /** The display name for the liveness sentinel, which is a check rather than a series. */
  liveness: string;
}

/**
 * Every metric a threshold rule may name, in the order the picker shows them.
 *
 * **Counters are dropped.** `POST /api/v1/thresholds` refuses them with `counter_metric` (a
 * monotonic value has no meaningful fixed bound, ADR-012), so offering one would be offering a
 * choice the save is going to reject. 13 of the 88 built-in catalog rows are counters.
 *
 * `current` is the value already on the rule being edited. If it is not otherwise in the list — an
 * operator-defined metric, or a catalog row an older release left behind — it is added at the top,
 * for the same reason the scope-level `<select>` keeps its current value: a control whose value is
 * absent from its options renders blank, and the next save silently stores something else.
 */
export function metricOptions(
  catalog: readonly MibCatalogEntry[],
  labels: MetricLabels,
  current?: string,
): MetricOption[] {
  const meaningOf = (name: string) => {
    const key = metricMeaningKey(name);
    return key ? labels.meaning(key) : '';
  };

  const checks: MetricOption[] = CHECK_METRICS.map((name) => ({
    name,
    label: name === LIVENESS_METRIC ? labels.liveness : name,
    group: labels.checks,
    meaning: meaningOf(name),
    oid: '',
  }));

  // Derived metrics (ADR-076). Listed beside the checks rather than among the collected
  // metrics because they are in neither the catalogue nor any time series — a reader who looked
  // for them under a vendor heading would not find them, and one who looked for their chart would
  // not find that either. `perInterface` is stated here rather than looked up, because the
  // generated catalogue only knows about metrics that are *collected*.
  const derived: MetricOption[] = DERIVED_METRICS.map((name) => ({
    name,
    label: name,
    group: labels.derived,
    meaning: meaningOf(name),
    oid: '',
    perInterface: true,
  }));

  const collected: MetricOption[] = catalog
    .filter((e) => e.metric_kind !== 'counter')
    .map((e) => ({
      name: e.metric_name,
      label: e.metric_name,
      group: e.vendor?.trim() || labels.standard,
      // The catalog's own `description` is the fallback, not the first choice: it is a single
      // column and this product is bilingual, so a translated sentence wins where there is one.
      // It is also empty on every seeded row — nothing writes it (see the backlog).
      meaning: meaningOf(e.metric_name) || e.description?.trim() || '',
      oid: e.oid,
      // Only the generated built-in list knows this. Whether a table's rows are interfaces is an
      // OID rule that lives in Rust, and guessing it from row keys is how chassis readings came to
      // be reported as per-port. An operator's own entry gets no badge rather than a wrong one.
      perInterface: builtinMetric(e.metric_name)?.per_interface,
    }));

  const out = [...checks, ...derived, ...collected];
  if (current && current.trim() && !out.some((o) => o.name === current)) {
    out.unshift({
      name: current,
      label: current === LIVENESS_METRIC ? labels.liveness : current,
      group: labels.checks,
      meaning: meaningOf(current),
      oid: '',
    });
  }
  return out;
}

/**
 * The rows matching `query`, case-insensitively, over the name, the heading and the meaning.
 *
 * The meaning is searched on purpose: an operator looking for "temperature" does not know whether
 * this vendor spells it `temp`, `temp_c` or `env_temp`, and the sentence is the only place the
 * English (or Japanese) word appears. A blank term returns everything.
 */
export function filterMetricOptions(
  options: readonly MetricOption[],
  query: string,
): MetricOption[] {
  const q = query.trim().toLowerCase();
  if (!q) return [...options];
  return options.filter(
    (o) =>
      o.name.toLowerCase().includes(q) ||
      o.label.toLowerCase().includes(q) ||
      o.group.toLowerCase().includes(q) ||
      o.meaning.toLowerCase().includes(q),
  );
}

/**
 * Whether the picker should offer to use `query` as a metric name it has never heard of.
 *
 * It must: an operator can attach a collection item with any valid metric name, and the item
 * writer never consults the catalog — so a closed list would block a metric that genuinely exists.
 * ⚠️ The *shape* of the name is deliberately not checked here. `is_valid_metric_name` lives in
 * Rust and the API enforces it; a regex copied into TypeScript would be a second rule with nothing
 * comparing the two (`extensibility.md` §2).
 */
export function offersCustomName(options: readonly MetricOption[], query: string): boolean {
  const q = query.trim();
  return q.length > 0 && !options.some((o) => o.name === q);
}

/** The rows grouped under their headings, in first-seen order. */
export function groupMetricOptions(
  options: readonly MetricOption[],
): { group: string; items: MetricOption[] }[] {
  const out: { group: string; items: MetricOption[] }[] = [];
  for (const o of options) {
    const last = out.find((g) => g.group === o.group);
    if (last) last.items.push(o);
    else out.push({ group: o.group, items: [o] });
  }
  return out;
}
