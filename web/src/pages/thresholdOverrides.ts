// SPDX-License-Identifier: AGPL-3.0-only
// Alerts ▸ Metric alert rules — on how many nodes a narrower rule on the same metric takes over from
// each row (ADR-200 Inc.14, counted by the server since Inc.29). It replaces the page note "the most
// specific scope wins": instead of saying the rule, the row that loses somewhere says where.
//
// The count is the server's (`ThresholdPage.overridden`): it knows which profile, label and folder
// each node is in, which the rule list does not carry, and it counts against every rule rather than
// the filtered, capped page on screen. This file only reads it.

import type { ThresholdPage } from '../types/api';

/** Each overridden row's id, with the number of nodes it is overridden on. Rows overridden nowhere
 *  are absent. A missing map (an answer from a core that predates the field) marks nothing. */
export function overriddenRows(page: Partial<Pick<ThresholdPage, 'overridden'>>): Map<string, number> {
  const out = new Map<string, number>();
  for (const [id, count] of Object.entries(page.overridden ?? {})) {
    if (count > 0) out.set(id, count);
  }
  return out;
}
