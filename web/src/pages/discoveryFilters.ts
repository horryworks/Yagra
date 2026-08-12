// SPDX-License-Identifier: AGPL-3.0-only
// Which rows the two tables on Nodes ▸ Discovery show — the sweep's candidates, and the endpoints
// seen passively on the network.
//
// Client-side: a sweep's result set is bounded by the range an operator typed in, and the endpoint
// table is server-paged already, so this narrows the page in hand (ui-conventions). In a `.ts` so a
// test can reach it (testing.md).

import { isFiltered as isFilteredAgainst, textMatch } from '../lib/filterQuery';
import { isUnmonitored } from './discoveredEndpoints';
import type { DiscoveredEndpoint, DiscoveryCandidate } from '../types/api';

// ───────────────────────────────────────────────────────────── sweep candidates

export interface CandidateFilters {
  /** Hide the addresses that answered nothing — the bulk of any wide sweep. */
  reachableOnly: boolean;
  /** Free text over the address and what the device said it is. */
  q: string;
}

export const DEFAULT_CANDIDATE_FILTERS: CandidateFilters = { reachableOnly: false, q: '' };

export function matchesCandidate(c: DiscoveryCandidate, f: CandidateFilters): boolean {
  if (f.reachableOnly && !c.reachable) return false;
  return textMatch(f.q, c.address, c.sysname, c.vendor, c.model, c.sysdescr);
}

export function isCandidateFiltered(f: CandidateFilters): boolean {
  return isFilteredAgainst(f, DEFAULT_CANDIDATE_FILTERS);
}

// ──────────────────────────────────────────────────────── discovered endpoints

export interface EndpointFilters {
  /** Hide the ones already promoted to a monitored node — what is left is the work list. */
  unmonitoredOnly: boolean;
  /** Free text over the address, the MAC and the neighbour it was seen through. */
  q: string;
}

/**
 * The default view, and note that it is **not** "everything".
 *
 * The table has always hidden already-imported endpoints, unconditionally and with nothing on
 * screen saying so. Keeping that as the default preserves the behaviour operators know, and making
 * it a control is the actual improvement: an endpoint that vanished from the list because someone
 * else imported it was previously indistinguishable from one that stopped being seen.
 */
export const DEFAULT_ENDPOINT_FILTERS: EndpointFilters = { unmonitoredOnly: true, q: '' };

/** Whether one endpoint survives the filter.
 *
 *  "Unmonitored" is `isUnmonitored` — the same test the row's own Import affordance makes, so the
 *  filter and the button cannot disagree about which rows are still outstanding. */
export function matchesEndpoint(e: DiscoveredEndpoint, f: EndpointFilters): boolean {
  if (f.unmonitoredOnly && !isUnmonitored(e)) return false;
  return textMatch(f.q, e.ip, e.mac, e.via_node);
}

export function isEndpointFiltered(f: EndpointFilters): boolean {
  return isFilteredAgainst(f, DEFAULT_ENDPOINT_FILTERS);
}
