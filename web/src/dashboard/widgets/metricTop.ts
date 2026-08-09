// SPDX-License-Identifier: AGPL-3.0-only
// What the "Fleet Top-N by metric" widget should do, given its persisted metric name and what this
// browser has learned about the fleet's metrics (ADR-046 Inc.3).
//
// The endpoint behind this widget already ranks the fleet by any metric name — the increment is the
// picker, and the picker is the hard part, for two reasons.
//
// The first is that **nothing may enumerate the fleet's metric names.** A fleet-wide series listing
// is the cardinality query CLAUDE.md names as the single biggest design risk, so ADR-046 decision 7
// rules one out; `top_nodes` answers "which nodes have metric X", never "which metrics exist". The
// field is therefore free text, offering what this session has actually seen (`metricInventoryCache`)
// as suggestions rather than pretending to hold the catalogue.
//
// The second is that **a counter cannot be ranked here.** `/metrics/top` has no rate mode: it ranks
// `max by (node)` of the stored values, which for a counter is a ranking of odometer readings — a
// plausible-looking list in which the node that has been up longest wins. That is the ADR-012
// accident wearing a different surface, so a metric this session knows to be a counter is refused
// rather than ranked. What free text costs is that an unseen name cannot be checked; what it must
// not cost is ranking a counter we *do* know about.
//
// A `.ts` on purpose: Vitest runs `environment: 'node'` with `include: ['src/**/*.test.ts']`, so
// judgement left in the `.tsx` is judgement nothing tests.

import { METRIC_PRESETS } from '../../lib/suppression';
import type { MetricKind, MetricTopAgg } from '../../types/api';
import type { WidgetSettings } from '../types';
import { topAggOf } from './util';

/** The widget's persisted selection: which metric to rank by, over which window. */
export interface MetricTopSelection {
  metric: string | null;
  agg: MetricTopAgg;
}

/** Read a selection out of the opaque settings bag.
 *
 *  The bag is `Record<string, unknown>` because the layout document is user-editable JSON that has
 *  round-tripped through storage and the server, so anything that is not a non-empty string degrades
 *  to "nothing picked" rather than becoming part of a query. Whitespace is trimmed: a metric name
 *  never has any, and ` icmp_rtt_ms` would otherwise be refused by the edge's identifier check as if
 *  the operator had typed something else entirely. */
export function readTopSelection(settings: WidgetSettings | undefined): MetricTopSelection {
  const raw = typeof settings?.metric === 'string' ? settings.metric.trim() : '';
  return { metric: raw === '' ? null : raw, agg: topAggOf(settings) };
}

/**
 * The metric names to offer under the field.
 *
 * Two sources, both honest about being partial: the shared presets every node emits
 * (`lib/suppression`, the same list the threshold and mute forms offer), and every metric this
 * session has seen on a node the operator opened. Anything known to be a counter is dropped from
 * both — suggesting a name the widget will then refuse to rank is worse than not suggesting it.
 */
export function metricSuggestions(known: ReadonlyMap<string, MetricKind>): string[] {
  const names = new Set<string>([...METRIC_PRESETS, ...known.keys()]);
  return [...names].filter((n) => known.get(n) !== 'counter').sort();
}

/** What the widget body should render this pass. */
export type MetricTopPlan =
  /** Nothing typed yet. */
  | { kind: 'pick-metric' }
  /** This session has seen the metric on a node and it is a raw counter — see the header. */
  | { kind: 'counter'; metric: string }
  /** Rank it. */
  | { kind: 'rank'; metric: string; agg: MetricTopAgg };

/**
 * Decide what to render.
 *
 * `known` is a memory, not an inventory, so an absent name means "not seen", never "does not exist"
 * — an unrecognised metric is ranked and the empty result speaks for itself. Only a positive counter
 * sighting refuses.
 */
export function metricTopPlan(
  sel: MetricTopSelection,
  known: ReadonlyMap<string, MetricKind>,
): MetricTopPlan {
  if (!sel.metric) return { kind: 'pick-metric' };
  if (known.get(sel.metric) === 'counter') return { kind: 'counter', metric: sel.metric };
  return { kind: 'rank', metric: sel.metric, agg: sel.agg };
}
