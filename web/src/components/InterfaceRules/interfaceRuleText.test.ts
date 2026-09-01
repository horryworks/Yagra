// SPDX-License-Identifier: AGPL-3.0-only
// Both of these lived in `InterfaceRulesModal.tsx`, where Vitest never loaded them
// (`include: ['src/**/*.test.ts']`). `boundSentence` is the one that matters: it resolves a unit,
// converts a percentage against the port's own speed, and picks a dwell cadence, and every wrong
// answer it can give still renders as a plausible sentence.
import { describe, expect, it } from 'vitest';
import { boundSentence, isOwnRule } from './interfaceRuleText';
import { interfaceScopeId } from '../../lib/interfaceScope';
import { newPortRuleForm } from '../../lib/portRuleForm';
import type { MatchingThreshold, StoredThreshold } from '../../types/api';

const NODE = '11111111-1111-4111-8111-111111111111';

function rule(over: Partial<StoredThreshold> = {}): StoredThreshold {
  return {
    id: 'r1',
    metric: 'if_in_util_pct',
    scope_level: 'interface',
    scope_ids: [interfaceScopeId(NODE, 3)],
    direction: 'above',
    warning: 70,
    critical: 90,
    dwell_samples: 3,
    ...over,
  } as StoredThreshold;
}

const match = (r: StoredThreshold): MatchingThreshold => ({ rule: r }) as MatchingThreshold;

/** i18n stand-in: returns the key plus its interpolations, so an assertion can name both. */
const t = (key: string, opts?: Record<string, unknown>) =>
  opts && Object.keys(opts).length ? `${key}(${JSON.stringify(opts)})` : key;

describe('isOwnRule', () => {
  it('claims a rule scoped to this exact port', () => {
    expect(isOwnRule(match(rule()), NODE, 3)).toBe(true);
  });

  it('rejects the same node’s other port, and another node’s same port', () => {
    expect(isOwnRule(match(rule()), NODE, 4)).toBe(false);
    expect(isOwnRule(match(rule()), '22222222-2222-4222-8222-222222222222', 3)).toBe(false);
  });

  it('rejects an inherited rule even when its scope somehow names this port', () => {
    // The level is what decides "may this dialog edit it", not the id list.
    expect(isOwnRule(match(rule({ scope_level: 'node' })), NODE, 3)).toBe(false);
    expect(isOwnRule(match(rule({ scope_level: 'global', scope_ids: [] })), NODE, 3)).toBe(false);
  });

  it('finds this port among several targets, not only as the first', () => {
    // ADR-078 lets a rule name several scopes. Reading `scope_ids[0]` would call a multi-target
    // rule inherited and hide its Edit button.
    const r = rule({ scope_ids: [interfaceScopeId(NODE, 9), interfaceScopeId(NODE, 3)] });
    expect(isOwnRule(match(r), NODE, 3)).toBe(true);
  });
});

describe('boundSentence', () => {
  it('shows a percentage bound against the port’s real speed', () => {
    const form = newPortRuleForm('in_traffic'); // hasBasis, basis 'percent', unit '%'
    const s = boundSentence(rule(), form, 1_000_000_000, t);
    expect(s).toContain('70% (700 Mbps)');
    expect(s).toContain('90% (900 Mbps)');
  });

  it('leaves a percentage alone when the link speed is unknown', () => {
    // Inventing a bit rate from a missing `ifSpeed` is the ADR-063 accident in another place.
    const form = newPortRuleForm('in_traffic');
    const s = boundSentence(rule(), form, null, t);
    expect(s).toContain('70%');
    // No bit rate is quoted at all — the '(' below belongs to the i18n stand-in's own output.
    expect(s).not.toContain('bps');
  });

  it('formats an absolute rate as bits per second, not as a bare number', () => {
    const form = { ...newPortRuleForm('in_traffic'), basis: 'absolute' as const };
    const s = boundSentence(rule({ warning: 8_000_000, critical: 9_000_000 }), form, null, t);
    expect(s).toContain('8.0 Mbps');
    expect(s).toContain('9.0 Mbps');
  });

  it('appends the subject’s own unit for a non-rate subject', () => {
    const form = newPortRuleForm('optical_rx'); // unit dBm, cadence polls, direction below
    const s = boundSentence(rule({ direction: 'below', warning: -14, critical: -18 }), form, null, t);
    expect(s).toContain('-14 dBm');
    expect(s).toContain('-18 dBm');
    expect(s).toContain('belowShort');
  });

  it('counts dwell in minutes for a minutes-cadence subject and polls for the rest', () => {
    // Saying "3 polls" about a minutes-cadence rule is wrong by the poll interval — invisible in a
    // summary line, and the reason the cadence lives on the subject spec at all.
    expect(boundSentence(rule(), newPortRuleForm('in_traffic'), null, t)).toContain(
      'dwellShortMinutes',
    );
    expect(boundSentence(rule(), newPortRuleForm('optical_rx'), null, t)).toContain(
      'dwellShortPolls',
    );
  });

  it('says what the rule is, not what its bounds are, when the subject fixes them', () => {
    const s = boundSentence(rule(), newPortRuleForm('link_state'), null, t);
    expect(s).toBe('interfaces.rules.linkNotUp');
  });

  it('omits a bound the rule does not carry', () => {
    const s = boundSentence(rule({ warning: null }), newPortRuleForm('in_traffic'), 1e9, t);
    expect(s).not.toContain('warnIs');
    expect(s).toContain('critIs');
  });

  it('claims no unit at all while the subject is still unresolved', () => {
    // `form` is null between opening the dialog and adopting the stored rule. A bare number is
    // honest there; guessing '%' would relabel a dBm rule for that render.
    const s = boundSentence(rule({ warning: 70, critical: 90 }), null, 1e9, t);
    expect(s).toContain('70');
    expect(s).not.toContain('%');
    expect(s).toContain('dwellShortPolls');
  });
});
