// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import {
  DEFAULT_DWELL,
  isThresholdReady,
  scopeIdKind,
  thresholdBody,
  thresholdFormFrom,
} from './thresholdRequest';
import { SCOPE_LEVELS, type ScopeLevel, type StoredThreshold } from '../types/api';

function rule(over: Partial<StoredThreshold> = {}): StoredThreshold {
  return {
    id: '00000000-0000-0000-0000-000000000001',
    scope_level: 'profile',
    scope_id: '11111111-1111-1111-1111-111111111111',
    metric: 'icmp_rtt_ms',
    direction: 'above',
    warning: 100,
    critical: 250,
    dwell_samples: 3,
    ...over,
  };
}

describe('thresholdFormFrom + thresholdBody', () => {
  it('round-trips a stored rule unchanged', () => {
    // The property that matters for editing: open a rule, change nothing, save — and the server
    // receives what it already had. A field dropped from either function is invisible to both of
    // them separately, and shows up as a form that saves and silently clears a bound.
    for (const r of [
      rule(),
      rule({ scope_level: 'node', warning: null, critical: 0.5, dwell_samples: 2 }),
      rule({ scope_level: 'group', scope_id: 'tokyo', direction: 'below', warning: 20 }),
      rule({ scope_level: 'group_id', scope_id: '22222222-2222-2222-2222-222222222222' }),
      rule({ scope_level: 'global', scope_id: '', metric: '__liveness__', warning: null, critical: null }),
    ]) {
      expect(thresholdBody(thresholdFormFrom(r))).toEqual({
        scope_level: r.scope_level,
        scope_id: r.scope_id,
        metric: r.metric,
        direction: r.direction,
        warning: r.warning ?? undefined,
        critical: r.critical ?? undefined,
        dwell_samples: r.dwell_samples,
      });
    }
  });

  it('sends an empty bound as absent, never as zero', () => {
    // `Number('')` is 0. A warning bound of 0 on an `above` rule never fires and on a `below` rule
    // fires on every sample, so the difference between `undefined` and `0` here is the difference
    // between "no warning" and "a permanent one".
    const body = thresholdBody({ ...thresholdFormFrom(), metric: 'cpu_util', warning: '', critical: '  ' });
    expect(body.warning).toBeUndefined();
    expect(body.critical).toBeUndefined();
    // A real zero still travels — 0 is a legitimate bound (`snmp_up below 0.5` has a sibling shape).
    expect(thresholdBody({ ...thresholdFormFrom(), metric: 'x', warning: '0' }).warning).toBe(0);
  });

  it('falls back to the default breach count rather than sending nothing', () => {
    // An empty box must not mean "no anti-flap": dwell 0 would commit on the first sample, which is
    // the hysteresis switched off by a blank field.
    expect(thresholdBody({ ...thresholdFormFrom(), metric: 'x', dwell: '' }).dwell_samples).toBe(
      DEFAULT_DWELL,
    );
    expect(thresholdBody({ ...thresholdFormFrom(), metric: 'x', dwell: 'abc' }).dwell_samples).toBe(
      DEFAULT_DWELL,
    );
    expect(thresholdBody({ ...thresholdFormFrom(), metric: 'x', dwell: '5' }).dwell_samples).toBe(5);
  });

  it('pins a global rule to no scope id even when one was typed first', () => {
    // The operator picks "profile", types an id, then switches to "every node". The stale id must
    // not travel: two global rules differing only in a stray scope id both apply and look identical.
    const f = { ...thresholdFormFrom(), level: 'global' as const, scopeId: 'left-over', metric: 'x' };
    expect(thresholdBody(f).scope_id).toBe('');
    // The receiving side: every other level keeps what was typed, trimmed. Without this the test
    // above would also pass for a function that emptied every scope id.
    for (const level of ['profile', 'group', 'group_id', 'node'] as const) {
      expect(thresholdBody({ ...f, level, scopeId: '  keep-me  ' }).scope_id).toBe('keep-me');
    }
  });

  it('lets a global rule be saved with no scope id, and refuses the others', () => {
    const base = { ...thresholdFormFrom(), metric: 'icmp_rtt_ms', scopeId: '' };
    expect(isThresholdReady({ ...base, level: 'global' })).toBe(true);
    for (const level of ['profile', 'group', 'group_id', 'node'] as const) {
      expect(isThresholdReady({ ...base, level })).toBe(false);
      expect(isThresholdReady({ ...base, level, scopeId: 'x' })).toBe(true);
    }
    // A rule with no metric is never ready — including a global one, which the clause above would
    // otherwise wave through.
    expect(isThresholdReady({ ...base, level: 'global', metric: '   ' })).toBe(false);
  });

  it('gives every scope level a scope-id control, and only the fleet-wide one none', () => {
    // Driven from `SCOPE_LEVELS` rather than a list written out here: a level added to the backend
    // and forgotten in the dialog would render whatever the last `case` returned, which for a
    // free-text fallback is a box the server then rejects.
    const kinds = Object.fromEntries(SCOPE_LEVELS.map((l) => [l, scopeIdKind(l)])) as Record<
      ScopeLevel,
      ReturnType<typeof scopeIdKind>
    >;
    expect(kinds).toEqual({
      global: 'none',
      profile: 'profile',
      group: 'tag',
      group_id: 'folderGroup',
      node: 'node',
    });
    // The two "group" levels must never resolve to the same control — one picks a folder from the
    // inventory tree, the other is free-form tag text, and offering the tree for the tag level
    // would store a UUID that matches no tag at all.
    expect(scopeIdKind('group')).not.toBe(scopeIdKind('group_id'));
    // Exactly one level has no id, and readiness agrees with it.
    expect(SCOPE_LEVELS.filter((l) => scopeIdKind(l) === 'none')).toEqual(['global']);
  });
});
