// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import { boundSides } from './thresholdBounds';

const NONE = {
  warning_below: null,
  critical_below: null,
  warning_above: null,
  critical_above: null,
};

describe('boundSides', () => {
  it('names the side a one-sided rule bounds', () => {
    expect(boundSides({ ...NONE, critical_above: 90 })).toBe('above');
    expect(boundSides({ ...NONE, warning_above: 70 })).toBe('above');
    expect(boundSides({ ...NONE, critical_below: 10 })).toBe('below');
    expect(boundSides({ ...NONE, warning_below: 20 })).toBe('below');
  });

  it('calls a rule that bounds both sides a band', () => {
    // The case the rules table gets wrong if it prints `direction`: a band reports `above` as its
    // primary side, so the cell would say the rule watches one direction when it watches two.
    expect(boundSides({ ...NONE, critical_below: -20, critical_above: -3 })).toBe('both');
    expect(boundSides({ warning_below: -18, critical_below: -20, warning_above: -5, critical_above: -3 })).toBe(
      'both',
    );
    // One severity below and the other above is still a band — the sides are what count, not
    // whether they pair up.
    expect(boundSides({ ...NONE, warning_below: 5, critical_above: 95 })).toBe('both');
  });

  it('says none for a rule that bounds nothing', () => {
    // Reachability. It is decided from the poll outcome rather than from a number, so a bound-less
    // row is its correct shape — the table must not print a direction for it.
    expect(boundSides(NONE)).toBe('none');
  });

  it('ignores 0 as a value but not as a bound', () => {
    // `0` is a legitimate bound and `!= null` is the test, not truthiness — the natural
    // `rule.critical_below ?` spelling would read a zero bound as no bound at all.
    expect(boundSides({ ...NONE, critical_below: 0 })).toBe('below');
    expect(boundSides({ ...NONE, critical_above: 0 })).toBe('above');
  });
});
