// SPDX-License-Identifier: AGPL-3.0-only
// Turning a stored event rule back into the shape the edit form submits.
//
// A `.ts` because Vitest never loads a `.tsx` (`testing.md`), and because of what this function
// silently decides: **which fields survive a round trip through the dialog.** It is a hand-written
// projection of `EventRule` onto `EventRuleInput`, so a field added to the rule and forgotten here
// is not a compile error — it is a field that reads correctly, edits correctly, and is **reset to
// its default the moment anyone saves that rule from this screen**. That failure is invisible in
// review and looks like the operator's own edit.

import type { EventRule, EventRuleInput } from '../types/api';

/** Every editable field of a stored rule, ready to submit again unchanged. */
export function ruleToInput(r: EventRule): EventRuleInput {
  return {
    name: r.name,
    enabled: r.enabled,
    source_kind: r.source_kind,
    source_id: r.source_id,
    node_id: r.node_id,
    match_kind: r.match_kind,
    pattern: r.pattern,
    clear_pattern: r.clear_pattern,
    severity: r.severity,
    ttl_secs: r.ttl_secs,
    min_count: r.min_count,
    window_secs: r.window_secs,
  };
}
