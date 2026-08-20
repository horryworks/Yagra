// SPDX-License-Identifier: AGPL-3.0-only
// Which sides of the band a stored rule bounds (ADR-081).
//
// In a `.ts` because Vitest never executes a `.tsx` (testing.md), and this is the judgement that
// decides whether the rules table prints "above" or "range" — a cell that gets it wrong tells the
// operator a rule watches one direction when it watches two.

import type { StoredThreshold } from '../types/api';

export type BoundSides = 'below' | 'above' | 'both' | 'none';

/** The sides `rule` actually names.
 *
 *  ⚠️ Reads the four bounds, never `direction`. That field carries the rule's **primary side** for
 *  clients written before ADR-081, so on a rule bounding both it answers `above` and describes
 *  half of what the rule does. `none` is reachable: reachability (`__liveness__`) is decided from
 *  the poll outcome rather than from a number, so it legitimately names no bound. */
export function boundSides(rule: Pick<
  StoredThreshold,
  'warning_below' | 'critical_below' | 'warning_above' | 'critical_above'
>): BoundSides {
  const below = rule.warning_below != null || rule.critical_below != null;
  const above = rule.warning_above != null || rule.critical_above != null;
  if (below && above) return 'both';
  if (below) return 'below';
  if (above) return 'above';
  return 'none';
}
