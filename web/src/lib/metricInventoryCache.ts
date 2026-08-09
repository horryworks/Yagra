// SPDX-License-Identifier: AGPL-3.0-only
// One place that fetches a node's metric inventory — and the session's memory of what it saw.
//
// Four surfaces ask for the same list: the node Overview (twice, for the health cards and the scalar
// strip), its Collection tab, and the dashboard's metric widgets. Sharing the fetch means concurrent
// callers join one request instead of racing — opening a node used to issue the identical request
// twice — and a caller that can tolerate a slightly old answer (a dashboard widget re-rendering on
// the 15s tick) can say so instead of re-asking. Callers that must see an edit immediately pass no
// `maxAgeMs` and get a fresh read.
//
// The second job is what ADR-046 Inc.3 needs. `GET /metrics/top` ranks the fleet by any metric name,
// but there is deliberately no endpoint that enumerates the fleet's metric names: a fleet-wide series
// listing is exactly the cardinality query CLAUDE.md calls the single biggest design risk, so ADR-046
// decision 7 rules it out. The fleet Top-N widget therefore offers what this browser has actually
// seen — the inventories of nodes the operator has already looked at — rather than a list nobody can
// afford to compute. It is a memory, not a catalogue, which is why free text stays allowed beside it.

import { api } from '../services/api';
import type { MetricKind, NodeMetricEntry } from '../types/api';

/** A sane reuse window for a caller that polls (the dashboard). The inventory changes when someone
 *  edits a collection set or a new metric first arrives — not on the dashboard's 15s cadence — so
 *  polling it with the chart would be one wasted request per widget per tick. A few minutes is short
 *  enough that a metric added while the board is open still turns up without a reload. */
export const INVENTORY_TTL_MS = 180_000;

const cache = new Map<string, { at: number; entries: NodeMetricEntry[] }>();
const inflight = new Map<string, Promise<NodeMetricEntry[]>>();

/**
 * One node's metric inventory. Never rejects: a node the caller can no longer see, or a store that
 * is down, resolves to an empty inventory, which every surface already renders as "nothing to show".
 *
 * `maxAgeMs` is how old a cached answer may be; the default of 0 always re-reads. Concurrent callers
 * share one request either way.
 */
export function fetchNodeMetrics(nodeId: string, maxAgeMs = 0): Promise<NodeMetricEntry[]> {
  const hit = cache.get(nodeId);
  if (hit && Date.now() - hit.at < maxAgeMs) return Promise.resolve(hit.entries);
  const pending = inflight.get(nodeId);
  if (pending) return pending;
  const p = api
    .listNodeMetrics(nodeId)
    .then((entries) => {
      cache.set(nodeId, { at: Date.now(), entries });
      rememberMetrics(entries);
      return entries;
    })
    .catch(() => [] as NodeMetricEntry[])
    .finally(() => {
      inflight.delete(nodeId);
    });
  inflight.set(nodeId, p);
  return p;
}

// ── The session's metric-name memory ─────────────────────────────────────────

let kinds: ReadonlyMap<string, MetricKind> = new Map();
const listeners = new Set<() => void>();

/**
 * Fold one node's inventory into a name → kind memory.
 *
 * Returns `prev` **unchanged** when nothing was learned, so a subscriber is woken only when the
 * answer actually moved (and a `useSyncExternalStore` reader cannot loop on a fresh object).
 *
 * **A counter wins a disagreement**, and the disagreement is real rather than theoretical: the
 * inventory reports a metric with no collection item as a gauge, because a counter can only reach the
 * TSDB through one (ADR-046 Inc.1). So a metric whose collection item was removed from one node's
 * profile — while its series is still in the store — comes back from that node as a gauge and from
 * every other node as the counter it is. Ranking a counter's stored values ranks odometer readings
 * (ADR-012), so the direction that ends up refusing is the safe one to round towards.
 */
export function mergeMetricKinds(
  prev: ReadonlyMap<string, MetricKind>,
  entries: readonly NodeMetricEntry[],
): ReadonlyMap<string, MetricKind> {
  let next: Map<string, MetricKind> | null = null;
  for (const e of entries) {
    const known = prev.get(e.metric);
    if (known === e.metric_kind) continue;
    // Already remembered as a counter: a later gauge sighting does not downgrade it.
    if (known === 'counter') continue;
    next ??= new Map(prev);
    next.set(e.metric, e.metric_kind);
  }
  return next ?? prev;
}

/** Record what one node reported. Idempotent; notifies subscribers only on a real change. */
export function rememberMetrics(entries: readonly NodeMetricEntry[]): void {
  const next = mergeMetricKinds(kinds, entries);
  if (next === kinds) return;
  kinds = next;
  for (const l of listeners) l();
}

/** Every metric name seen this session, with the kind to read it as. */
export function metricKindsSnapshot(): ReadonlyMap<string, MetricKind> {
  return kinds;
}

/** Subscribe to changes (the `useSyncExternalStore` half); returns the unsubscribe. */
export function subscribeMetricKinds(onChange: () => void): () => void {
  listeners.add(onChange);
  return () => {
    listeners.delete(onChange);
  };
}
