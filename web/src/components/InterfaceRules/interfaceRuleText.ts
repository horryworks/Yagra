// SPDX-License-Identifier: AGPL-3.0-only
// The two questions the per-interface threshold modal answers in prose: "is this rule this port's
// own, or inherited?" and "what does this rule actually say?".
//
// A `.ts` on purpose: Vitest runs `environment: 'node'` with `include: ['src/**/*.test.ts']`, so
// judgement left in the `.tsx` is judgement nothing tests. `boundSentence` in particular resolves a
// unit, converts a percentage against the port's own speed, and picks a dwell cadence — three
// decisions whose only visible output is a line of text that always *looks* plausible.

import { formatBps } from '../../lib/format';
import { interfaceScopeId } from '../../lib/interfaceScope';
import { PORT_SUBJECT_SPECS, bpsOfPercent, type PortRuleForm } from '../../lib/portRuleForm';
import type { MatchingThreshold, StoredThreshold } from '../../types/api';

/**
 * Whether a matching rule is attached to *this* interface rather than inherited from the node or
 * a group above it. Drives which list the row appears in and whether it may be edited here.
 *
 * `includes`, not `[0] ===`: a rule may name several targets since ADR-078. An interface rule is
 * capped at one server-side, so in practice this reads the single element — but asking the list is
 * what keeps the answer right if that cap is ever lifted.
 */
export function isOwnRule(row: MatchingThreshold, nodeId: string, ifindex: number): boolean {
  return (
    row.rule.scope_level === 'interface' &&
    row.rule.scope_ids.includes(interfaceScopeId(nodeId, ifindex))
  );
}

/**
 * One line summarising what a stored rule fires on: direction, its bounds in the subject's own
 * unit, and how long the condition must hold.
 *
 * Three things are decided here rather than displayed:
 *
 * 1. **The unit comes from the subject, not from the rule.** A rule on a rate subject whose form
 *    basis is `absolute` is in bits per second and is formatted as such; otherwise the subject's
 *    declared unit applies. `form` is `null` while the subject is still being resolved, and then
 *    every bound falls back to a bare number rather than claiming a unit it does not know.
 * 2. **A percentage is shown against the port's real speed when there is one** (`70% (700 Mbps)`).
 *    With no known speed the percentage stands alone — inventing a bit rate from a missing
 *    `ifSpeed` is the ADR-063 accident in another place.
 * 3. **Dwell is counted in whatever the subject counts in.** A minutes-cadence subject says
 *    minutes; everything else says polls. Saying "3 polls" about a minutes-cadence rule is wrong by
 *    the poll interval, which is exactly the kind of error nobody notices in a summary line.
 *
 * A subject with fixed bounds has no bounds to state, so the sentence says what the rule is instead.
 */
export function boundSentence(
  rule: StoredThreshold,
  form: PortRuleForm | null,
  speedBps: number | null,
  t: (key: string, opts?: Record<string, unknown>) => string,
): string {
  if (form && PORT_SUBJECT_SPECS[form.subject].fixedBounds) {
    return t('interfaces.rules.linkNotUp');
  }
  const unit =
    form && PORT_SUBJECT_SPECS[form.subject].hasBasis && form.basis === 'absolute'
      ? null
      : (form && PORT_SUBJECT_SPECS[form.subject].unit) || '';
  const show = (v: number | null | undefined): string | null => {
    if (v == null) return null;
    if (unit === null) return formatBps(v);
    if (unit === '%') {
      const bps = bpsOfPercent(v, speedBps);
      return bps == null ? `${v}%` : `${v}% (${formatBps(bps)})`;
    }
    return `${v}${unit ? ` ${unit}` : ''}`;
  };
  const parts = [
    show(rule.warning) && t('interfaces.rules.warnIs', { value: show(rule.warning) }),
    show(rule.critical) && t('interfaces.rules.critIs', { value: show(rule.critical) }),
  ].filter(Boolean);
  const direction = t(
    `interfaces.rules.${rule.direction === 'above' ? 'aboveShort' : 'belowShort'}`,
  );
  const cadence =
    form && PORT_SUBJECT_SPECS[form.subject].cadence === 'minutes' ? 'Minutes' : 'Polls';
  const dwell = t(`interfaces.rules.dwellShort${cadence}`, { count: rule.dwell_samples });
  return `${direction} ${parts.join(' / ')} · ${dwell}`;
}
