// SPDX-License-Identifier: AGPL-3.0-only
/** Which metrics a device profile collects that no *baseline* alert rule names (ADR-106).
 *
 *  ADR-078 決定 1 moved the twenty vendor-specific default rules from `global` down to `profile`
 *  scope, and each names the built-in profiles it was written for. An operator who builds their own
 *  profile and attaches the same metric sets collects those metrics and **no rule reaches them** —
 *  the device is polled, the series is stored, and nothing can ever fire. Nothing in the product
 *  said so, which is the gap this module answers.
 *
 *  ⚠️ **"Baseline" is `global` or `profile` scope, and nothing narrower — that is a decision, not
 *  an approximation.** A `group` / `group_id` / `node` / `interface` rule reaches *some* of the
 *  nodes carrying a profile, so at the profile dimension there is no answer to give; and more
 *  importantly `resolve_effective` takes **only the most specific level present**, so a node-scoped
 *  rule does not add to a profile-scoped one — it replaces it. A metric with neither a `global` nor
 *  a `profile` rule has no floor under those overrides. The label the caller renders has to say
 *  that much rather than claiming the metric is unmonitored, or it calls an operator who is
 *  managing the metric node-by-node a liar (ADR-055 R1).
 *
 *  🚨 **Only a metric some rule already bounds can be a gap, and that is measured, not declared.**
 *  Most of what a profile collects is never meant to carry a node-dimension threshold: raw counters
 *  (`if_hc_in_octets`, `snmp_sys_uptime_ticks`), per-port values that are bounded at `interface`
 *  scope instead (`if_oper_status`), and the raw halves a derived metric divides (`cisco_mem_used`
 *  is collected; the rule sits on `cisco_mem_used_pct`). Asking "does every collected metric have a
 *  rule" answered **53 of 59 profiles with a warning** against the real fleet, essentially all of
 *  them false — a check nobody would read twice. So a metric counts here only when some `global` or
 *  `profile` rule names it **somewhere**: that is exactly "Yagra has a bound for this, and your
 *  profile is not one of the ones it was written for", which is the hole ADR-078 決定 1 left.
 *  The same fleet answers that question with 24 profiles covered and **0 gaps**, and a profile built
 *  from scratch with a vendor's metric set gets the vendor's metrics named.
 *  ⚠️ `kind` / `metric_kind` cannot stand in for this — `table/gauge` covers both `if_admin_status`
 *  (never bounded) and `cisco_cpu_5min` (always bounded), measured on the same data.
 *
 *  🚨 **A truncated rule list can never produce a gap.** `GET /api/v1/thresholds` caps at
 *  `THRESHOLDS_MAX` (500) and reports `truncated`; a rule past the cap is a rule this module cannot
 *  see, so "no rule names this metric" would be a guess. It answers `unchecked` instead. Same for a
 *  metric set whose items failed to load: a set that reads as empty would make its profile look
 *  *more* covered than it is, which is the one direction this must never fall in. */

import type { StoredThreshold } from '../types/api';

/** Whether one stored rule gives `profileId` a baseline — see the ⚠️ note above for why the two
 *  narrower halves of `ScopeLevel` are deliberately not consulted. */
export function ruleIsBaselineFor(rule: StoredThreshold, profileId: string): boolean {
  if (rule.scope_level === 'global') return true;
  return rule.scope_level === 'profile' && rule.scope_ids.includes(profileId);
}

/** What the panel knows about the metrics behind a profile's attached sets. `loading` and `failed`
 *  are kept apart on purpose: one renders nothing and resolves itself, the other is a standing
 *  admission that the question was not answered. */
export type CollectedMetrics =
  | { state: 'ready'; metrics: string[] }
  | { state: 'loading' }
  | { state: 'failed' };

/** Fold the per-set item lists into the distinct metric names a profile collects.
 *
 *  A set still in flight yields `loading`; a set that failed to load yields `failed`. Neither is
 *  allowed to shrink the metric list, because a short list is what makes a gap disappear. */
export function collectedMetrics(
  attached: readonly string[],
  loaded: ReadonlyMap<string, readonly string[]>,
  failed: ReadonlySet<string>,
): CollectedMetrics {
  if (attached.some((id) => failed.has(id))) return { state: 'failed' };
  if (attached.some((id) => !loaded.has(id))) return { state: 'loading' };
  const seen = new Set<string>();
  for (const id of attached) for (const name of loaded.get(id) ?? []) seen.add(name);
  return { state: 'ready', metrics: [...seen].sort() };
}

/** The answer the panel renders.
 *
 *  `empty` — the profile collects nothing any rule bounds — is separate from `covered` with a count
 *  of zero so the caller can stay silent rather than claim "0 metrics, all covered". It is the
 *  common answer (35 of 59 profiles on the real fleet), and silence is right there: a profile that
 *  collects only raw counters has nothing to report either way. `covered` exists at all so that
 *  "checked, nothing missing" does not look like "never checked"
 *  (`empty-list-is-not-evidence-until-settled`). */
export type RuleGap =
  | { kind: 'unchecked' }
  | { kind: 'empty' }
  | { kind: 'covered'; total: number }
  | { kind: 'gaps'; missing: string[]; total: number };

/** Which of the metrics **anyone bounds** no baseline rule names for this profile.
 *
 *  `metrics` is expected to be distinct and sorted — {@link collectedMetrics} produces it that way,
 *  and the missing list inherits that order so the rendered line is stable across reloads. The
 *  "anyone bounds" filter is derived from `rules` rather than passed in, so there is no second list
 *  of alertable metric names to keep in step with the seeded defaults. */
export function profileRuleGap(args: {
  metrics: readonly string[];
  rules: readonly StoredThreshold[] | null;
  rulesTruncated: boolean;
  profileId: string;
}): RuleGap {
  const { metrics, rules, rulesTruncated, profileId } = args;
  if (rules === null || rulesTruncated) return { kind: 'unchecked' };
  // Bounded somewhere in the fleet — the filter is re-applied here rather than trusted from the
  // caller's query, so passing a wider list cannot silently widen what counts as a gap.
  const bounded = new Set(
    rules.filter((r) => r.scope_level === 'global' || r.scope_level === 'profile').map((r) => r.metric),
  );
  const relevant = metrics.filter((m) => bounded.has(m));
  if (relevant.length === 0) return { kind: 'empty' };
  const covered = new Set(
    rules.filter((r) => ruleIsBaselineFor(r, profileId)).map((r) => r.metric),
  );
  const missing = relevant.filter((m) => !covered.has(m));
  return missing.length === 0
    ? { kind: 'covered', total: relevant.length }
    : { kind: 'gaps', missing, total: relevant.length };
}
