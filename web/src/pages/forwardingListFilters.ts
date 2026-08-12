// SPDX-License-Identifier: AGPL-3.0-only
// Which rows the Settings ▸ Forwarding table shows.
//
// Named `…ListFilters` and not `forwardingFilters` on purpose: `forwardingOptions.ts` next door is
// about a destination's **own** filter — the conditions deciding which events it relays — and two
// files a letter apart meaning different things by "filter" is a trap for whoever reads this next.
//
// Client-side: the destination list is bounded by what an operator configured, not by fleet size
// (ui-conventions). In a `.ts` so a test can reach it (testing.md).

import { isFiltered as isFilteredAgainst, matchesEnabled, textMatch } from '../lib/filterQuery';
import type { EnabledState } from '../lib/filterQuery';
import type { ForwardDestKind, ForwardDestination } from '../types/api';

export interface ForwardingFilters {
  dest_kind: ForwardDestKind | '';
  enabled: EnabledState | '';
  /** Free text over the destination's name and where it sends to. */
  q: string;
}

export const DEFAULT_FORWARDING_FILTERS: ForwardingFilters = { dest_kind: '', enabled: '', q: '' };

/** Whether one destination survives the filter.
 *
 *  `target` is searched as well as the name because it is what an operator actually knows during an
 *  incident — "which of these points at 10.0.0.9" is the question, and the name is whatever someone
 *  typed months ago. */
export function matchesForwardDestination(d: ForwardDestination, f: ForwardingFilters): boolean {
  if (f.dest_kind && d.dest_kind !== f.dest_kind) return false;
  if (!matchesEnabled(f.enabled, d.enabled)) return false;
  return textMatch(f.q, d.name, d.target);
}

export function isForwardingFiltered(f: ForwardingFilters): boolean {
  return isFilteredAgainst(f, DEFAULT_FORWARDING_FILTERS);
}
