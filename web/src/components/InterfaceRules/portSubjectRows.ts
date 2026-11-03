// SPDX-License-Identifier: AGPL-3.0-only
// The port's threshold dialog as rows: one per thing a port rule can watch, each with the rule in
// force, where it came from, and the one action that makes sense for it (ADR-230 decision 7).
//
// The dialog used to list every rule that reached the port, a CPU rule among them, and a rule
// inherited from every node had no button at all — so an operator who wanted to loosen the optical
// receive level on one port could not see that they could. A fixed row per subject answers both:
// the five things are always there, and each says whether it is this port's own, inherited, or not
// watched.
//
// In a `.ts` because Vitest never executes a `.tsx` (testing.md).

import {
  PORT_RULE_SUBJECTS,
  newPortRuleForm,
  portRuleFrom,
  portSubjectOf,
  type PortRuleForm,
  type PortRuleSubject,
} from '../../lib/portRuleForm';
import type { InterfaceRow, InterfaceSeries, MatchingThreshold } from '../../types/api';
import { isOwnRule } from './interfaceRuleText';

/** What the row's button does. */
export type PortRowAction =
  /** This port's own rule, which the port form can express: edit it, or remove it. */
  | { kind: 'edit'; form: PortRuleForm }
  /** A broader rule is in force: make a port rule, starting from its bounds. */
  | { kind: 'override'; form: PortRuleForm }
  /** Nothing watches it: make a port rule from blank. */
  | { kind: 'set'; form: PortRuleForm }
  /** This port's own rule, in a shape the port form cannot say (a band on both sides): it is
   *  changed on the threshold rules screen, never rounded into the nearest shape here. */
  | { kind: 'elsewhere' };

export interface PortSubjectRow {
  subject: PortRuleSubject;
  /** The rules in force for this subject, the port's own first. Usually one; two for traffic when
   *  both a percentage and a rate rule are in force. Empty when nothing watches it. */
  inForce: MatchingThreshold[];
  /** The port's own rules for this subject, in force or not — what "remove" takes away. */
  own: MatchingThreshold[];
  action: PortRowAction;
}

export interface PortRows {
  subjects: PortSubjectRow[];
  /** Rules in force on this port for a per-interface metric no subject covers (errors, discards):
   *  read-only here, and listed rather than hidden, because a rule that fires on this port and
   *  appears nowhere in its dialog is a rule nobody will find. */
  other: MatchingThreshold[];
}

/** Whether this port carries an optical transceiver, as far as the dock can tell. A vendor may
 *  report neither a model nor a media name for one, so a light reading counts as well. */
export function isOpticalPort(
  row: Pick<InterfaceRow, 'transceiver_model' | 'if_media'>,
  series: Pick<InterfaceSeries, 'rx_power_dbm' | 'tx_power_dbm'> | null,
): boolean {
  if (row.transceiver_model || row.if_media) return true;
  const read = (xs: readonly (number | null)[] | undefined) => (xs ?? []).some((v) => v != null);
  return read(series?.rx_power_dbm) || read(series?.tx_power_dbm);
}

const OPTICAL: ReadonlySet<PortRuleSubject> = new Set(['optical_rx', 'optical_tx']);

/** The dialog's rows. `optical` hides the two light-level rows on a copper port, unless the port
 *  already has a rule of its own there — a rule that exists is never hidden. */
export function portSubjectRows(
  rows: readonly MatchingThreshold[],
  nodeId: string,
  ifindex: number,
  optical: boolean,
): PortRows {
  const bySubject = new Map<PortRuleSubject, MatchingThreshold[]>();
  const other: MatchingThreshold[] = [];
  for (const r of rows) {
    const subject = portSubjectOf(r.rule.metric);
    if (subject) bySubject.set(subject, [...(bySubject.get(subject) ?? []), r]);
    else if (r.in_force) other.push(r);
  }
  const subjects = PORT_RULE_SUBJECTS.flatMap((subject): PortSubjectRow[] => {
    const all = bySubject.get(subject) ?? [];
    const own = all.filter((r) => isOwnRule(r, nodeId, ifindex));
    if (OPTICAL.has(subject) && !optical && own.length === 0) return [];
    const inForce = all
      .filter((r) => r.in_force)
      .sort((a, b) => Number(isOwnRule(b, nodeId, ifindex)) - Number(isOwnRule(a, nodeId, ifindex)));
    return [{ subject, inForce, own, action: actionFor(subject, inForce, own) }];
  });
  return { subjects, other };
}

function actionFor(
  subject: PortRuleSubject,
  inForce: readonly MatchingThreshold[],
  own: readonly MatchingThreshold[],
): PortRowAction {
  const mine = own[0];
  if (mine) {
    const form = portRuleFrom(mine.rule);
    return form ? { kind: 'edit', form } : { kind: 'elsewhere' };
  }
  const broad = inForce[0];
  if (broad) {
    // A broad rule the form cannot say still gets an override; it starts blank rather than from
    // bounds the form would have to round.
    return { kind: 'override', form: portRuleFrom(broad.rule) ?? newPortRuleForm(subject) };
  }
  return { kind: 'set', form: newPortRuleForm(subject) };
}

/** How many of the port's own rules there are — the number on the dock's button. */
export function ownRuleCount(
  rows: readonly MatchingThreshold[],
  nodeId: string,
  ifindex: number,
): number {
  return rows.filter((r) => isOwnRule(r, nodeId, ifindex)).length;
}
