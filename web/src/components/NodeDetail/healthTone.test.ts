// SPDX-License-Identifier: AGPL-3.0-only
// The node Overview's health-card thresholds. Each decides what colour an operator sees against a
// live reading, and the expired-certificate case is the one that silently reads healthy if the
// bands are written as a range instead of a ladder.

import { describe, expect, it } from 'vitest';
import { certTone, httpToneVar, operLabel, operState } from './healthTone';
import type { TFunction } from 'i18next';

describe('certTone', () => {
  it('bands days-to-expiry at 7 and 30', () => {
    expect(certTone(90)).toBe('up');
    expect(certTone(30)).toBe('up'); // exactly 30 is not yet a warning
    expect(certTone(29)).toBe('warning');
    expect(certTone(7)).toBe('warning'); // exactly 7 is not yet critical
    expect(certTone(6)).toBe('critical');
  });

  it('reads an already-expired certificate as critical, not healthy', () => {
    // The failure a range check would produce: negative days falling outside `7..30` and landing
    // on the `up` branch, painting an expired certificate green.
    expect(certTone(0)).toBe('critical');
    expect(certTone(-1)).toBe('critical');
    expect(certTone(-400)).toBe('critical');
  });
});

describe('operState', () => {
  it('maps ifOperStatus 1 to ok and anything else to critical', () => {
    expect(operState(1)).toBe('ok');
    // 2=down, 3=testing, 5=dormant, 6=notPresent, 7=lowerLayerDown — none of them are "fine".
    for (const oper of [2, 3, 4, 5, 6, 7]) expect(operState(oper)).toBe('critical');
  });

  it('reports an unread interface as unknown rather than guessing', () => {
    expect(operState(null)).toBe('unknown');
  });
});

describe('httpToneVar', () => {
  it('resolves every tone to a shared status variable', () => {
    // ui-conventions: never an ad-hoc red/yellow/green, or the same node reads differently in the
    // table, the map and the chart.
    expect(httpToneVar('up')).toBe('var(--status-ok)');
    expect(httpToneVar('warning')).toBe('var(--status-warning)');
    expect(httpToneVar('critical')).toBe('var(--status-critical)');
  });
});

describe('operLabel', () => {
  const t = ((key: string) => key) as unknown as TFunction;

  it('has three answers, not two', () => {
    // `null` is "the walk has not reported this port", which is NOT down. Folding it into down
    // would draw an alarm for a port nobody has looked at yet.
    expect(operLabel(1, t)).toBe('interfaces.operUp');
    expect(operLabel(2, t)).toBe('interfaces.operDown');
    expect(operLabel(null, t)).toBe('interfaces.operUnknown');
  });

  it('treats every non-1 ifOperStatus as down', () => {
    // 3 testing / 4 unknown / 5 dormant / 6 notPresent / 7 lowerLayerDown — none of them is up, and
    // the tab's summary counts on exactly that.
    for (const v of [3, 4, 5, 6, 7, 0]) expect(operLabel(v, t)).toBe('interfaces.operDown');
  });
});
