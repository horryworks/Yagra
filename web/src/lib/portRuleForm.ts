// SPDX-License-Identifier: AGPL-3.0-only
// The port-shaped alert rule: what an operator picks, and the threshold rule it becomes
// (ADR-076 増分 5, 決定 8).
//
// The generic dialog asks for a metric name, a direction and two numbers. On a port that is the
// wrong shape of question in three ways, all three reported from a real deployment:
//
//  1. of the 88 metrics it offers, **7** can carry a rule on a port at all — the other 81 are
//     either node-wide or raw counters the API refuses outright;
//  2. adding the transmit side of a receive rule means re-picking a metric name, when the operator
//     is doing the same thing in the other direction;
//  3. nothing tells them that "alert when the link is down" is `if_oper_status above 1.5`. Written
//     the obvious way — `below 0.5` — it is a rule that can never fire, because `ifOperStatus`
//     reports down as **2** and never returns 0. This repo has already shipped that mistake once.
//
// So this module is the translation layer: (subject, basis, bounds) ⟷ a `ThresholdInput`. The
// stored rule is unchanged — one shape on the wire, one engine reading it — and the port dialog is
// a *presenter* over it, never a second kind of rule.
//
// ⚠️ It is nonetheless a second representation, which this repo treats as a thing that must be
// pinned rather than trusted (`extensibility.md` §2). The guard is `portRuleFrom ∘ portRuleTo`
// round-tripping every subject × basis, and the rule that anything which does not fit returns
// `null` rather than being rounded into the nearest shape — a rule shown as something it is not is
// worse than a rule shown as a metric name.
//
// In a `.ts` because Vitest never executes a `.tsx` (`testing.md`).

import { interfaceScopeId } from './interfaceScope';
import type { Direction, StoredThreshold, ThresholdInput } from '../types/api';
import { DEFAULT_DWELL } from '../pages/thresholdRequest';

/** What the operator says they want to watch. */
export const PORT_RULE_SUBJECTS = [
  'in_traffic',
  'out_traffic',
  'link_state',
  'optical_rx',
  'optical_tx',
] as const;

export type PortRuleSubject = (typeof PORT_RULE_SUBJECTS)[number];

/** How a traffic bound is expressed. */
export const PORT_RULE_BASES = ['percent', 'absolute'] as const;
export type PortRuleBasis = (typeof PORT_RULE_BASES)[number];

/** Units an absolute rate may be typed in. Stored bounds are always bits per second. */
export const RATE_UNITS = ['bps', 'kbps', 'Mbps', 'Gbps'] as const;
export type RateUnit = (typeof RATE_UNITS)[number];

const UNIT_FACTOR: Record<RateUnit, number> = {
  bps: 1,
  kbps: 1e3,
  Mbps: 1e6,
  Gbps: 1e9,
};

/** The two derived metrics for each traffic direction (they are computed, not collected). */
const TRAFFIC_METRICS: Record<'in_traffic' | 'out_traffic', Record<PortRuleBasis, string>> = {
  in_traffic: { percent: 'if_in_util_pct', absolute: 'if_in_bps' },
  out_traffic: { percent: 'if_out_util_pct', absolute: 'if_out_bps' },
};

/**
 * What each subject is, mechanically.
 *
 * `Record` keyed by the union so a new subject cannot be added without deciding all of it — the
 * rule this repo applies to every registry (`extensibility.md` §1).
 */
export interface PortSubjectSpec {
  /** Whether the operator chooses between a percentage and an absolute rate. */
  hasBasis: boolean;
  /** The metric this subject stores, given a basis. */
  metric: (basis: PortRuleBasis) => string;
  /** The direction its bounds read in. Fixed per subject: none of them is ambiguous. */
  direction: Direction;
  /**
   * Bounds the operator does not type, because the subject has no scale — link state is a code,
   * and the only useful question is "is it up". `1.5` sits between up (1) and every other value
   * (2…7), so `above 1.5` means "anything but up". It is not `below 0.5`, which never fires.
   */
  fixedBounds?: { warning?: number; critical: number };
  /** Whether a breach count is one evaluator tick (60s) or one poll. */
  cadence: 'minutes' | 'polls';
  /** Unit shown beside the bound inputs, for the subjects that type one. */
  unit?: '%' | 'dBm';
}

export const PORT_SUBJECT_SPECS: Record<PortRuleSubject, PortSubjectSpec> = {
  in_traffic: {
    hasBasis: true,
    metric: (b) => TRAFFIC_METRICS.in_traffic[b],
    direction: 'above',
    // The derived metrics are evaluated by a leader loop once a minute, so a breach count is a
    // count of minutes. Saying "polls" here would understate the delay by the poll interval.
    cadence: 'minutes',
    unit: '%',
  },
  out_traffic: {
    hasBasis: true,
    metric: (b) => TRAFFIC_METRICS.out_traffic[b],
    direction: 'above',
    cadence: 'minutes',
    unit: '%',
  },
  link_state: {
    hasBasis: false,
    metric: () => 'if_oper_status',
    direction: 'above',
    fixedBounds: { critical: 1.5 },
    cadence: 'polls',
  },
  // Optical levels are read downwards: a receive level falling is the failure. The upper bound of
  // the vendor's window cannot be expressed at all yet — a rule carries one direction, and the
  // range form is still deferred (ADR-062 決定 8 / Issue #66).
  optical_rx: {
    hasBasis: false,
    metric: () => 'if_rx_power_dbm',
    direction: 'below',
    cadence: 'polls',
    unit: 'dBm',
  },
  optical_tx: {
    hasBasis: false,
    metric: () => 'if_tx_power_dbm',
    direction: 'below',
    cadence: 'polls',
    unit: 'dBm',
  },
};

/** The dialog's fields. Strings, because that is what an `<input>` gives back. */
export interface PortRuleForm {
  subject: PortRuleSubject;
  basis: PortRuleBasis;
  warning: string;
  critical: string;
  /** The unit the two bounds are typed in, when the basis is absolute. */
  unit: RateUnit;
  dwell: string;
}

/** A new rule's starting state: the request this whole increment came from. */
export function newPortRuleForm(subject: PortRuleSubject = 'in_traffic'): PortRuleForm {
  return {
    subject,
    basis: 'percent',
    warning: '',
    critical: subject === 'in_traffic' || subject === 'out_traffic' ? '90' : '',
    unit: 'Mbps',
    dwell: String(DEFAULT_DWELL),
  };
}

/**
 * A number the operator did not type is absent, never zero.
 *
 * The same rule `thresholdRequest.ts` states and for the same reason: `Number('')` is `0`, which on
 * an `above` rule never fires and on a `below` rule fires forever.
 */
function optionalNumber(s: string): number | undefined {
  const trimmed = s.trim();
  if (trimmed === '') return undefined;
  const n = Number(trimmed);
  return Number.isFinite(n) ? n : undefined;
}

/** Whether the form can be submitted: it needs at least one bound, unless the subject fixes them. */
export function isPortRuleReady(f: PortRuleForm): boolean {
  const spec = PORT_SUBJECT_SPECS[f.subject];
  if (spec.fixedBounds) return true;
  return optionalNumber(f.warning) !== undefined || optionalNumber(f.critical) !== undefined;
}

/** Whether this subject can be evaluated on a port whose speed the device does not report.
 *
 *  A percentage cannot: the speed is its denominator, and without one the evaluator skips the port
 *  entirely — silently, which is the whole reason the absolute metrics exist. */
export function needsPortSpeed(f: PortRuleForm): boolean {
  return PORT_SUBJECT_SPECS[f.subject].hasBasis && f.basis === 'percent';
}

/** The bound, in the metric's own unit. Absolute rates are typed in kbps/Mbps/Gbps and stored in
 *  bits per second; everything else is stored as typed. */
function boundOf(f: PortRuleForm, which: 'warning' | 'critical'): number | undefined {
  const n = optionalNumber(f[which]);
  if (n === undefined) return undefined;
  const spec = PORT_SUBJECT_SPECS[f.subject];
  return spec.hasBasis && f.basis === 'absolute' ? n * UNIT_FACTOR[f.unit] : n;
}

/** The body sent to `POST /api/v1/thresholds` or `PUT /api/v1/thresholds/{id}`. */
export function portRuleToThreshold(
  f: PortRuleForm,
  nodeId: string,
  ifindex: number,
): ThresholdInput {
  const spec = PORT_SUBJECT_SPECS[f.subject];
  return {
    scope_level: 'interface',
    scope_id: interfaceScopeId(nodeId, ifindex),
    metric: spec.metric(f.basis),
    direction: spec.direction,
    warning: spec.fixedBounds ? spec.fixedBounds.warning : boundOf(f, 'warning'),
    critical: spec.fixedBounds ? spec.fixedBounds.critical : boundOf(f, 'critical'),
    dwell_samples: optionalNumber(f.dwell) ?? DEFAULT_DWELL,
  };
}

/** Every (subject, basis) pair, as the metric it stores. Built from the specs so it cannot drift. */
const METRIC_TO_SHAPE: Record<string, { subject: PortRuleSubject; basis: PortRuleBasis }> =
  Object.fromEntries(
    PORT_RULE_SUBJECTS.flatMap((subject) => {
      const spec = PORT_SUBJECT_SPECS[subject];
      const bases: PortRuleBasis[] = spec.hasBasis ? [...PORT_RULE_BASES] : ['percent'];
      return bases.map((basis) => [spec.metric(basis), { subject, basis }] as const);
    }),
  );

/** The largest unit that renders `bps` without a fraction, so 90,000,000 reads as `90 Mbps`. */
export function splitRate(bps: number): { value: number; unit: RateUnit } {
  for (const unit of ['Gbps', 'Mbps', 'kbps'] as const) {
    const v = bps / UNIT_FACTOR[unit];
    if (Math.abs(bps) >= UNIT_FACTOR[unit] && Number.isInteger(v)) return { value: v, unit };
  }
  // Not a whole number of any larger unit: show the raw rate rather than a rounded lie.
  return { value: bps, unit: 'bps' };
}

/**
 * A stored rule read back into the dialog's fields, or `null` when it does not fit this shape.
 *
 * `null` is the load-bearing half. A port-scoped rule on a metric this form has no subject for —
 * `if_admin_status`, an operator's own per-interface item — is real, applies, and must be *shown*;
 * it just cannot be shown in these words. The caller lists it read-only and points at the rules
 * screen. Coercing it into the nearest subject would silently retarget the rule on the next save.
 */
export function portRuleFrom(rule: StoredThreshold): PortRuleForm | null {
  const shape = METRIC_TO_SHAPE[rule.metric];
  if (!shape) return null;
  const spec = PORT_SUBJECT_SPECS[shape.subject];
  // A rule whose direction disagrees with the subject is not this subject: `if_rx_power_dbm above`
  // is a legitimate rule (a receive level too *high* also damages optics) that this form has no
  // way to express, so it stays a metric-name rule rather than being flipped on save.
  if (rule.direction !== spec.direction) return null;
  if (spec.fixedBounds) {
    // Link state has no bounds to show, so the stored ones must be the ones it writes — otherwise
    // reopening it would rewrite someone's deliberately different threshold.
    if (rule.critical !== spec.fixedBounds.critical) return null;
    if ((rule.warning ?? undefined) !== spec.fixedBounds.warning) return null;
    return {
      subject: shape.subject,
      basis: 'percent',
      warning: '',
      critical: '',
      unit: 'Mbps',
      dwell: String(rule.dwell_samples),
    };
  }
  if (shape.basis === 'absolute') {
    // Both bounds share one unit, so pick the unit from whichever is present and show the other in
    // it too. A rule whose two bounds have no common whole unit falls back to bits per second.
    const anchor = rule.critical ?? rule.warning ?? 0;
    const { unit } = splitRate(anchor);
    const text = (n: number | null | undefined) =>
      n == null ? '' : String(n / UNIT_FACTOR[unit]);
    return {
      subject: shape.subject,
      basis: 'absolute',
      warning: text(rule.warning),
      critical: text(rule.critical),
      unit,
      dwell: String(rule.dwell_samples),
    };
  }
  const text = (n: number | null | undefined) => (n == null ? '' : String(n));
  return {
    subject: shape.subject,
    basis: shape.basis,
    warning: text(rule.warning),
    critical: text(rule.critical),
    unit: 'Mbps',
    dwell: String(rule.dwell_samples),
  };
}

/** What a percentage bound means in bits per second on a port of this speed.
 *
 *  `null` when the port reports no usable speed — which is not a display detail: that is exactly
 *  the port a percentage rule cannot be evaluated on at all, so the dialog says so instead of
 *  showing a bound it knows will never be compared against anything.
 */
export function bpsOfPercent(pct: number, speedBps: number | null | undefined): number | null {
  if (speedBps == null || speedBps <= 0 || !Number.isFinite(pct)) return null;
  return (speedBps * pct) / 100;
}

/** A stored bound as it should read for its metric.
 *
 *  An absolute rate is stored in bits per second, so the rules table showed `800000000` — a number
 *  an operator has to count the digits of. Percentages and dBm are already in the unit they were
 *  typed in, and anything this module does not recognise is left exactly as stored rather than
 *  being given a unit it may not have.
 */
export function boundText(metric: string, value: number): string {
  const shape = METRIC_TO_SHAPE[metric];
  if (!shape) return String(value);
  const spec = PORT_SUBJECT_SPECS[shape.subject];
  if (spec.hasBasis && shape.basis === 'absolute') {
    const { value: v, unit } = splitRate(value);
    return String(v) + ' ' + unit;
  }
  if (spec.unit === '%') return String(value) + '%';
  return spec.unit ? String(value) + ' ' + spec.unit : String(value);
}

/** Every metric this form can express — what the port dialog can round-trip. */
export function portRuleMetrics(): string[] {
  return Object.keys(METRIC_TO_SHAPE);
}
