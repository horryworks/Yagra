// SPDX-License-Identifier: AGPL-3.0-only
// Turning a stored classification rule back into the shape the edit form submits.
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
