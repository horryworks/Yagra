// SPDX-License-Identifier: AGPL-3.0-only
// Alerts ▸ Metric alert rules — the add/edit form's state and the request it becomes.
//
// In a `.ts` because Vitest never executes a `.tsx` (testing.md), and every judgement below is one
// a test has to be able to reach:
//
//  - a `global` rule's `scope_id` is pinned empty (the server pins it too — this keeps the form
//    from *showing* an id it will not send);
//  - an empty bound is `undefined`, **not** `0`. `Number('')` is `0`, so the obvious rewrite sends
//    a warning bound of zero on every rule the operator left blank;
//  - prefilling an existing rule and saving it unchanged must produce that rule again. That is the
//    property that catches a field dropped from either direction of the round trip, and it is not
//    visible from either function on its own.
//
// One module for both modes on purpose. Two would be two answers to "what does this form send",
// and the add path and the edit path would drift the way every mirror in this repo has.

import type { Direction, ScopeLevel, StoredThreshold, ThresholdInput } from '../types/api';

/** What the dialog's inputs hold. Every field is a string because that is what an `<input>` gives
 *  back; the conversion to the wire's numbers happens in one place, below. */
export interface ThresholdForm {
  level: ScopeLevel;
  scopeId: string;
  metric: string;
  direction: Direction;
  warning: string;
  critical: string;
  dwell: string;
}

/** The breach count a new rule starts at. Matches the server's own default for an absent
 *  `dwell_samples`, so the form shows what would happen rather than a different number. */
export const DEFAULT_DWELL = 3;

/** A number the operator did not type is absent, never zero.
 *
 *  `Number('')` is `0` and `Number(' ')` is `0`, so the natural spelling turns "no warning bound"
 *  into "warn at zero" — which on an `above` rule never fires and on a `below` rule fires forever.
 *  A value that is not a number at all is also absent rather than `NaN`, which would serialize to
 *  `null` and be rejected by the edge. */
function optionalNumber(s: string): number | undefined {
  const trimmed = s.trim();
  if (trimmed === '') return undefined;
  const n = Number(trimmed);
  return Number.isFinite(n) ? n : undefined;
}

/** A number the form shows for a stored value — `null`/absent becomes an empty box, not "0". */
function numberText(n: number | null | undefined): string {
  return n == null ? '' : String(n);
}

/** The form's initial state: blank for a new rule, the row's own values for an edit.
 *
 *  ⚠️ `StoredThreshold` flattens the rule onto the row, so `metric`/`direction`/`warning`/
 *  `critical`/`dwell_samples` sit beside `scope_level`/`scope_id`. */
export function thresholdFormFrom(rule?: StoredThreshold): ThresholdForm {
  if (!rule) {
    return {
      level: 'profile',
      scopeId: '',
      metric: '',
      direction: 'above',
      warning: '',
      critical: '',
      dwell: String(DEFAULT_DWELL),
    };
  }
  return {
    level: rule.scope_level,
    scopeId: rule.scope_id,
    metric: rule.metric,
    direction: rule.direction,
    warning: numberText(rule.warning),
    critical: numberText(rule.critical),
    dwell: String(rule.dwell_samples),
  };
}

/** What the scope-id field *is* at a given level — which decides the control the dialog renders.
 *
 *  Named rather than branched on inline because two places need the same answer (which control to
 *  show, and whether the form is ready) and a second copy would let them disagree — a field that
 *  renders a picker while readiness still tests a text box is a Save button that never enables. */
export type ScopeIdKind = 'none' | 'profile' | 'folderGroup' | 'tag' | 'node';

/** ⚠️ Exhaustive over `ScopeLevel` on purpose: a new level must decide what its id is, rather than
 *  falling into a default that renders a free-text box for something the server will reject. */
export function scopeIdKind(level: ScopeLevel): ScopeIdKind {
  switch (level) {
    case 'global':
      return 'none';
    case 'profile':
      return 'profile';
    case 'group_id':
      return 'folderGroup';
    case 'group':
      return 'tag';
    case 'node':
      return 'node';
  }
}

/** Whether the form can be submitted.
 *
 *  A `global` rule needs no scope id — it targets every node — so demanding one would make the
 *  level unusable. Every other level needs one, because an empty scope id matches nothing and the
 *  rule would be stored, listed, and silently inert (the server refuses one since ADR-075 増分 3;
 *  this is the half that keeps the operator from getting a 400 for a field they can see is blank). */
export function isThresholdReady(f: ThresholdForm): boolean {
  return f.metric.trim() !== '' && (scopeIdKind(f.level) === 'none' || f.scopeId.trim() !== '');
}

/** The body sent to `POST /api/v1/thresholds` or `PUT /api/v1/thresholds/{id}`. */
export function thresholdBody(f: ThresholdForm): ThresholdInput {
  return {
    scope_level: f.level,
    // Pinned, not merely omitted: the operator may have typed an id and then switched the level to
    // "every node", and two global rules differing only in a stray scope id both apply, look
    // identical in the list, and cannot be told apart.
    scope_id: scopeIdKind(f.level) === 'none' ? '' : f.scopeId.trim(),
    metric: f.metric.trim(),
    direction: f.direction,
    warning: optionalNumber(f.warning),
    critical: optionalNumber(f.critical),
    dwell_samples: optionalNumber(f.dwell) ?? DEFAULT_DWELL,
  };
}
