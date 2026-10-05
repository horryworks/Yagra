// SPDX-License-Identifier: AGPL-3.0-only
// Turning a stored classification rule back into the shape the edit form submits, and the one
// warning the form gives about a sysObjectID prefix.
//
// Same shape and same hazard as `eventRuleForm.ts`: a hand-written projection of the stored row
// onto its input type, where a field added upstream and forgotten here is **reset to its default
// on the next save** rather than being a compile error. Worth extra care on this one — a
// classification rule decides which device profile a discovered node is bound to, so a silently
// dropped `sysobjectid_prefix` re-classifies devices on the next sweep.

import type { ClassificationRule, ClassificationRuleInput } from '../types/api';

/** Every editable field of a stored rule, ready to submit again unchanged. */
export function ruleToInput(r: ClassificationRule): ClassificationRuleInput {
  return {
    priority: r.priority,
    sysobjectid_prefix: r.sysobjectid_prefix,
    sysdescr_regex: r.sysdescr_regex,
    profile_id: r.profile_id,
    vendor: r.vendor,
    model: r.model,
    enabled: r.enabled,
  };
}

/** Whether a typed sysObjectID prefix lacks its trailing dot.
 *
 *  The classifier compares as text, so `1.3.6.1.4.1.9` also matches `1.3.6.1.4.1.91…` — another
 *  vendor's subtree. Only a typed value is judged: a blank prefix means "match on sysDescr alone".
 *  It warns rather than refuses, because a prefix that deliberately stops mid-number is legal. */
export function prefixLacksDot(prefix: string): boolean {
  const p = prefix.trim();
  return p !== '' && !p.endsWith('.');
}
