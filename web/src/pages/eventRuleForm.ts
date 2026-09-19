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

// ── The three numbers: what they may be ─────────────────────────────────────────────────────────
//
// 🚨 **`Number('')` is `0`.** A number input that has been cleared holds the empty string, and the
// form sent `Number(ttl)` — so clearing "Clears after" asked the server for a rule that expires
// after zero seconds. The server refuses it, which is why nothing was ever stored wrongly, but the
// refusal arrived as its raw English sentence (`ttl_secs must be 60..=604800`) on a screen whose
// siblings all validate first and say so in the operator's language. `thresholdRequest.ts` writes
// the same trap down for the threshold form; this is the form that had not had the treatment.

/** The server's bounds — `crates/yagra-core/src/api/events.rs`. */
export const EVENT_RULE_BOUNDS = {
  ttl_secs: { min: 60, max: 604_800 },
  min_count: { min: 1, max: 100 },
  window_secs: { min: 1, max: 3_600 },
} as const;

export type EventRuleNumberField = keyof typeof EVENT_RULE_BOUNDS;
export const EVENT_RULE_NUMBER_FIELDS = Object.keys(EVENT_RULE_BOUNDS) as EventRuleNumberField[];

/** A whole number the operator typed, or `undefined` for an empty or unusable box. */
export function typedInteger(raw: string): number | undefined {
  const trimmed = raw.trim();
  if (trimmed === '') return undefined;
  const n = Number(trimmed);
  return Number.isInteger(n) ? n : undefined;
}

/** Which of the three is empty or out of range — the first, in form order — or `null`. */
export function eventRuleNumberProblem(
  values: Record<EventRuleNumberField, string>,
): EventRuleNumberField | null {
  for (const field of EVENT_RULE_NUMBER_FIELDS) {
    const n = typedInteger(values[field]);
    const { min, max } = EVENT_RULE_BOUNDS[field];
    if (n === undefined || n < min || n > max) return field;
  }
  return null;
}
