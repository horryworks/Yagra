// SPDX-License-Identifier: AGPL-3.0-only
// Which rows the two tables on Nodes ▸ Discovery show — the sweep's candidates, and the endpoints
// seen passively on the network.
//
// Client-side: a sweep's result set is bounded by the range an operator typed in, and the endpoint
// table is server-paged already, so this narrows the page in hand (ui-conventions). In a `.ts` so a
// test can reach it (testing.md).
//
// **ADR-053 Inc.6 (decision F) put both on the shared column model.** The import grid keeps its own
// markup — it is the matrix-shaped screen `ui-conventions` exempts from `DataTable`, with a
// horizontally scrolling body and a sticky select column — so what moved is the controls.

import {
  specColumns,
  TEXT_MODES,
  type ColumnFilterSpec,
  type FilterableColumn,
} from '../lib/columnFilter';
import { isUnmonitored } from './discoveredEndpoints';
import type { DiscoveredEndpoint, DiscoveryCandidate } from '../types/api';
import type { TFunction } from 'i18next';

// ───────────────────────────────────────────────────────────── sweep candidates

/** Whether a swept address answered anything at all. Its own token set rather than a boolean so the
 *  URL and the control read as what they mean, and so "only the silent ones" — the addresses worth
 *  chasing after a sweep — is as sayable as its opposite. */
export const CANDIDATE_REACH = ['reachable', 'silent'] as const;

/**
 * The sweep-results filter row.
 *
 * Only the first three columns carry a control. Name, profile and credential are the operator's
 * *input* for the import about to happen — a filter on a field you are in the middle of typing
 * would remove the row out from under the cursor.
 */
export function candidateFilters(
  t: TFunction,
): Record<string, ColumnFilterSpec<DiscoveryCandidate>> {
  return {
    reach: {
      kind: 'enum',
      options: CANDIDATE_REACH.map((v) => ({ value: v, label: t(`discovery.filter.reach.${v}`) })),
      readValue: (c) => (c.reachable ? 'reachable' : 'silent'),
      allLabel: t('discovery.filter.allReach'),
      counts: 'client',
    },
    address: {
      kind: 'text',
      modes: TEXT_MODES,
      not: true,
      readText: (c) => [c.address],
      containsSemantics: 'substring',
      placeholder: t('discovery.cols.address'),
    },
    // The identity cell stacks four device-reported strings, so the filter reads all four — a term
    // that is on screen has to find its row whichever line it is on.
    identity: {
      kind: 'text',
      modes: TEXT_MODES,
      not: true,
      readText: (c) => [c.sysname, c.sysdescr, c.vendor, c.model],
      containsSemantics: 'substring',
      placeholder: t('discovery.cols.identity'),
    },
  };
}

export function candidateColumns(t: TFunction): FilterableColumn<DiscoveryCandidate>[] {
  return specColumns(candidateFilters(t));
}

export function candidateLabels(t: TFunction): Record<string, string> {
  return {
    reach: t('discovery.filter.allReach'),
    address: t('discovery.cols.address'),
    identity: t('discovery.cols.identity'),
  };
}

// ──────────────────────────────────────────────────────── discovered endpoints

/** Whether a seen endpoint is already a monitored node. */
export const ENDPOINT_MONITORED = ['unmonitored', 'monitored'] as const;

/**
 * The default view, and note that it is **not** "everything".
 *
 * The table has always hidden already-imported endpoints, unconditionally and with nothing on
 * screen saying so. Keeping that as the default preserves the behaviour operators know, and making
 * it a control is the actual improvement: an endpoint that vanished from the list because someone
 * else imported it was previously indistinguishable from one that stopped being seen.
 *
 * ⚠️ This is the one column on either table whose default *narrows*, which is why it is spelled as
 * a value here rather than left to `defaultFilters` — see `RangeFilterSpec.defaultPreset` for the
 * other place a narrowing default is deliberate, and its warning about the empty state.
 */
export const ENDPOINT_DEFAULT_MONITORED = 'unmonitored';

/**
 * The seen-endpoints filter row.
 *
 * "Unmonitored" is `isUnmonitored` — the same test the row's own Import affordance makes, so the
 * filter and the button cannot disagree about which rows are still outstanding.
 */
export function endpointFilters(
  t: TFunction,
): Record<string, ColumnFilterSpec<DiscoveredEndpoint>> {
  return {
    ip: {
      kind: 'text',
      modes: TEXT_MODES,
      not: true,
      readText: (e) => [e.ip],
      containsSemantics: 'substring',
      placeholder: t('discovery.seen.cols.address'),
    },
    mac: {
      kind: 'text',
      modes: TEXT_MODES,
      not: true,
      readText: (e) => [e.mac],
      containsSemantics: 'substring',
      placeholder: t('discovery.seen.cols.mac'),
    },
    via: {
      kind: 'text',
      modes: TEXT_MODES,
      not: true,
      readText: (e) => [e.via_node],
      containsSemantics: 'substring',
      placeholder: t('discovery.seen.cols.seenBy'),
    },
    monitored: {
      kind: 'enum',
      options: ENDPOINT_MONITORED.map((v) => ({
        value: v,
        label: t(`discovery.seen.filter.state.${v}`),
      })),
      readValue: (e) => (isUnmonitored(e) ? 'unmonitored' : 'monitored'),
      allLabel: t('discovery.seen.filter.allStates'),
      counts: 'client',
    },
  };
}

export function endpointColumns(t: TFunction): FilterableColumn<DiscoveredEndpoint>[] {
  return specColumns(endpointFilters(t));
}

export function endpointLabels(t: TFunction): Record<string, string> {
  return {
    ip: t('discovery.seen.cols.address'),
    mac: t('discovery.seen.cols.mac'),
    via: t('discovery.seen.cols.seenBy'),
    monitored: t('discovery.seen.filter.allStates'),
  };
}
