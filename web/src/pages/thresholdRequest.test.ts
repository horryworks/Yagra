// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import {
  DEFAULT_DWELL,
  isThresholdReady,
  scopeAcceptsMany,
  scopeIdKind,
  thresholdBody,
  thresholdFormFrom,
} from './thresholdRequest';
import { SCOPE_LEVELS, type ScopeLevel, type StoredThreshold } from '../types/api';
import { LIVENESS_METRIC } from '../lib/format';

/** A row shaped the way the server actually sends one since ADR-081.
 *
 *  The four bounds are **derived from `direction`/`warning`/`critical`** here, mirroring
 *  `ThresholdBounds::from_legacy`, so every case below still reads as "an above rule at 100/250"
 *  while carrying the shape the API returns. Writing only the legacy triple would build a fixture
 *  no endpoint produces, and the round-trip test would then be checking a row that cannot arrive. */
function rule(over: Partial<StoredThreshold> = {}): StoredThreshold {
  const base = {
    id: '00000000-0000-0000-0000-000000000001',
    scope_level: 'profile' as ScopeLevel,
    scope_ids: ['11111111-1111-1111-1111-111111111111'],
    metric: 'icmp_rtt_ms',
    direction: 'above' as const,
    warning: 100 as number | null,
    critical: 250 as number | null,
    dwell_samples: 3,
    ...over,
  };
  const down = base.direction === 'below';
  return {
    ...base,
    warning_below: down ? base.warning : null,
    critical_below: down ? base.critical : null,
    warning_above: down ? null : base.warning,
    critical_above: down ? null : base.critical,
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
      rule({ scope_level: 'group', scope_ids: ['tokyo'], direction: 'below', warning: 20 }),
      rule({ scope_level: 'group_id', scope_ids: ['22222222-2222-2222-2222-222222222222'] }),
      rule({ scope_level: 'global', scope_ids: [], metric: '__liveness__', warning: null, critical: null }),
      // ADR-078: several targets must survive the round trip too, in order — an edit that
      // dropped or reordered them would re-scope a rule the operator only opened to read.
      rule({
        scope_level: 'profile',
        scope_ids: [
          '33333333-3333-3333-3333-333333333333',
          '44444444-4444-4444-4444-444444444444',
        ],
      }),
      // ADR-081: a band rule must survive too. This is the case the old form structurally could
      // not hold — one `direction` field meant opening it and saving dropped a side.
      rule({
        metric: 'rx_dbm',
        warning_below: -18,
        critical_below: -20,
        warning_above: -5,
        critical_above: -3,
      }),
    ]) {
      expect(thresholdBody(thresholdFormFrom(r))).toEqual({
        scope_level: r.scope_level,
        scope_ids: r.scope_ids,
        metric: r.metric,
        direction: r.direction,
        warning_below: r.warning_below ?? undefined,
        critical_below: r.critical_below ?? undefined,
        warning_above: r.warning_above ?? undefined,
        critical_above: r.critical_above ?? undefined,
        dwell_samples: r.dwell_samples,
      });
    }
  });

  it('derives the direction from the bounds rather than carrying one', () => {
    // The defect ADR-081 removes: the form used to hold a `direction` beside the numbers, so it
    // could be saved saying `above` with bounds that only made sense downward — stored, listed,
    // and never fired. There is no field to disagree with now.
    const f = thresholdFormFrom();
    expect(thresholdBody({ ...f, metric: 'x', criticalBelow: '10' }).direction).toBe('below');
    expect(thresholdBody({ ...f, metric: 'x', criticalAbove: '90' }).direction).toBe('above');
    // A band reports its primary side, matching `ThresholdBounds::direction` on the server.
    expect(
      thresholdBody({ ...f, metric: 'x', criticalBelow: '10', criticalAbove: '90' }).direction,
    ).toBe('above');
  });

  it('will not submit a rule that names no bound, except liveness', () => {
    // A rule with no bound is stored, listed, and never fires. The server refuses one since
    // ADR-081; this keeps the button from enabling on a body it will 400. Liveness is the one
    // exemption on both sides — it asks whether the node answered, not whether a number is out of
    // range — and the two sides must name the same exception or the dialog and the edge disagree.
    const base = { ...thresholdFormFrom(), scopeIds: ['11111111-1111-1111-1111-111111111111'] };
    expect(isThresholdReady({ ...base, metric: 'cpu_util' })).toBe(false);
    expect(isThresholdReady({ ...base, metric: 'cpu_util', criticalAbove: '90' })).toBe(true);
    expect(isThresholdReady({ ...base, metric: 'cpu_util', warningBelow: '5' })).toBe(true);
    expect(isThresholdReady({ ...base, metric: LIVENESS_METRIC })).toBe(true);
  });

  it('sends an empty bound as absent, never as zero', () => {
    // `Number('')` is 0. A warning bound of 0 on an `above` rule never fires and on a `below` rule
    // fires on every sample, so the difference between `undefined` and `0` here is the difference
    // between "no warning" and "a permanent one".
    const body = thresholdBody({
      ...thresholdFormFrom(),
      metric: 'cpu_util',
      warningAbove: '',
      criticalAbove: '  ',
    });
    expect(body.warning_above).toBeUndefined();
    expect(body.critical_above).toBeUndefined();
    // A real zero still travels — 0 is a legitimate bound (`snmp_up below 0.5` has a sibling shape).
    expect(
      thresholdBody({ ...thresholdFormFrom(), metric: 'x', warningAbove: '0' }).warning_above,
    ).toBe(0);
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

  it('pins a global rule to no targets even when some were picked first', () => {
    // The operator picks "profile", ticks two, then switches to "every node". The stale targets
    // must not travel: two global rules differing only in a stray target both apply and look
    // identical in the list.
    const f = {
      ...thresholdFormFrom(),
      level: 'global' as const,
      scopeIds: ['left-over', 'and-another'],
      metric: 'x',
    };
    expect(thresholdBody(f).scope_ids).toEqual([]);
    // The receiving side: every other level keeps what was picked, trimmed, in order. Without
    // this the assertion above would also pass for a function that emptied every target list.
    for (const level of ['profile', 'group', 'group_id', 'node'] as const) {
      expect(thresholdBody({ ...f, level, scopeIds: ['  keep-me  ', ' and-me '] }).scope_ids).toEqual([
        'keep-me',
        'and-me',
      ]);
    }
    // A blank entry is dropped rather than sent: the edge refuses an empty target, and the one
    // that could produce one is the legacy tag box being cleared.
    expect(thresholdBody({ ...f, level: 'group', scopeIds: ['a', '  ', 'b'] }).scope_ids).toEqual([
      'a',
      'b',
    ]);
  });

  it('says which levels accept more than one target', () => {
    // ADR-078 決定 3. `interface` is single because a rule there covers one port and is created
    // from that port's own screen; `group` is the legacy free-text scope. Driven from
    // `SCOPE_LEVELS` so a new level has to decide rather than inherit whatever came last.
    const many = SCOPE_LEVELS.filter((l) => scopeAcceptsMany(l));
    expect(many).toEqual(['profile', 'group_id', 'node']);
  });

  it('lets a global rule be saved with no target, and refuses the others', () => {
    // A bound is set so this stays a test about *targets*: since ADR-081 a rule with none is
    // refused, and without one every case below would be false for the wrong reason.
    const base = {
      ...thresholdFormFrom(),
      metric: 'icmp_rtt_ms',
      criticalAbove: '250',
      scopeIds: [],
    };
    expect(isThresholdReady({ ...base, level: 'global' })).toBe(true);
    for (const level of ['profile', 'group', 'group_id', 'node'] as const) {
      expect(isThresholdReady({ ...base, level })).toBe(false);
      expect(isThresholdReady({ ...base, level, scopeIds: ['x'] })).toBe(true);
      // Several is ready too — the case the feature exists for.
      expect(isThresholdReady({ ...base, level, scopeIds: ['x', 'y'] })).toBe(true);
      // A list of nothing but blanks is not a list of targets.
      expect(isThresholdReady({ ...base, level, scopeIds: ['  ', ''] })).toBe(false);
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
      interface: 'interface',
    });
    // The two "group" levels must never resolve to the same control — one picks a folder from the
    // inventory tree, the other is free-form tag text, and offering the tree for the tag level
    // would store a UUID that matches no tag at all.
    expect(scopeIdKind('group')).not.toBe(scopeIdKind('group_id'));
    // Exactly one level has no id, and readiness agrees with it.
    expect(SCOPE_LEVELS.filter((l) => scopeIdKind(l) === 'none')).toEqual(['global']);
  });

  it('judges a port rule scope id itself rather than letting the server 400 it', () => {
    // Every other level's id comes from a picker, so "non-empty" is the whole question. A port
    // rule's id is composed (`<node-uuid>:<ifindex>`), so the form has to know the shape — the
    // server refuses anything else, and a Save that turns into a 400 for a value the form could
    // have judged is a worse form than a disabled button.
    const base = {
      metric: 'if_in_util_pct',
      warningBelow: '',
      criticalBelow: '',
      warningAbove: '',
      criticalAbove: '90',
      dwell: '3',
    };
    const node = '6f1c9d2a-0b3e-4a71-9c8d-2e5f7a1b4c60';

    expect(isThresholdReady({ ...base, level: 'interface', scopeIds: [`${node}:7`] })).toBe(true);
    // Port 0 is a real ifIndex, so it must not read as "no port".
    expect(isThresholdReady({ ...base, level: 'interface', scopeIds: [`${node}:0`] })).toBe(true);

    for (const bad of ['', node, `${node}:`, `${node}:x`, `${node}:-1`, 'not-a-uuid:7']) {
      expect(isThresholdReady({ ...base, level: 'interface', scopeIds: [bad] })).toBe(false);
    }
    // Two ports in one rule is refused here as well as at the edge, so the button never enables
    // on a shape the server will 400 (ADR-078 決定 3).
    expect(
      isThresholdReady({
        ...base,
        level: 'interface',
        scopeIds: [`${node}:7`, `${node}:8`],
      }),
    ).toBe(false);

    // The strictness is confined to this level: a node rule still only needs a non-empty target,
    // so this did not quietly tighten every other level as well.
    expect(isThresholdReady({ ...base, level: 'node', scopeIds: ['anything'] })).toBe(true);
  });
});
