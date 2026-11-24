// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import alertNameFlags from '../api/alertNameFlags.json';
import { FLAG_BOUND, flagBounds, flagFault, flagSeverity } from './flagRule';
import {
  formFlagSeverity,
  thresholdBody,
  thresholdFormFrom,
  withFlagSeverity,
  type ThresholdForm,
} from '../pages/thresholdRequest';

const form = (metric: string, over: Partial<ThresholdForm> = {}): ThresholdForm => ({
  ...thresholdFormFrom(),
  metric,
  ...over,
});

describe('flagFault (ADR-264 decision 3)', () => {
  it('reads the fault side from the generated flags, not from the name', () => {
    expect(flagFault('snmp_up')).toBe(0);
    expect(flagFault('http_up')).toBe(0);
    expect(flagFault('meraki_uplink_failed')).toBe(1);
    // Named like an up/down answer, but it counts tunnels.
    expect(flagFault('fortinet_vpn_tunnels_up')).toBeNull();
    expect(flagFault('cisco_cpu_5min')).toBeNull();
  });

  it('leaves reachability out: it has no bounds to write', () => {
    expect(flagFault('__liveness__')).toBeNull();
  });

  it('knows both fault sides, so neither branch below is dead', () => {
    const values = new Set(Object.values(alertNameFlags));
    expect(values).toEqual(new Set([0, 1]));
  });
});

describe('flagSeverity', () => {
  it('reads the seeded snmp_up rule as critical', () => {
    expect(flagSeverity(0, { criticalBelow: 0.5 })).toBe('critical');
    expect(flagSeverity(0, { warningBelow: 0.5 })).toBe('warning');
  });

  it('accepts any bound that catches the fault and not the healthy value', () => {
    expect(flagSeverity(0, { criticalBelow: 0 })).toBe('critical');
    expect(flagSeverity(1, { warningAbove: 1 })).toBe('warning');
  });

  it('keeps the boxes for a bound that would also catch the healthy value', () => {
    // `<= 1` fires on 1 as well: the "URL monitor permanently Critical" shape.
    expect(flagSeverity(0, { criticalBelow: 1 })).toBeNull();
    expect(flagSeverity(1, { criticalAbove: 0 })).toBeNull();
  });

  it('keeps the boxes for a bound on the wrong side or two bounds', () => {
    expect(flagSeverity(0, { criticalAbove: 0.5 })).toBeNull();
    expect(flagSeverity(0, { warningBelow: 0.5, criticalBelow: 0.5 })).toBeNull();
  });

  it('says unset when nothing is written yet', () => {
    expect(flagSeverity(0, {})).toBe('unset');
  });

  it('stores a severity as one bound on the fault side, which it reads back', () => {
    for (const fault of [0, 1] as const) {
      for (const sev of ['warning', 'critical'] as const) {
        const b = flagBounds(fault, sev);
        expect(Object.values(b)).toEqual([FLAG_BOUND]);
        expect(flagSeverity(fault, b)).toBe(sev);
      }
    }
  });
});

describe('the dialog form', () => {
  it('draws boxes for a numeric metric and a severity for a 0/1 one', () => {
    expect(formFlagSeverity(form('cisco_cpu_5min', { warningAbove: '80' }))).toBe('boxes');
    expect(formFlagSeverity(form('snmp_up', { criticalBelow: '0.5' }))).toBe('critical');
    expect(formFlagSeverity(form('snmp_up'))).toBe('unset');
    expect(formFlagSeverity(form('snmp_up', { criticalBelow: '1' }))).toBe('boxes');
  });

  it('a picked severity replaces every box, so switching cannot leave the first behind', () => {
    const crit = withFlagSeverity(form('snmp_up'), 'critical');
    const warn = withFlagSeverity(crit, 'warning');
    expect(warn).toMatchObject({
      warningBelow: '0.5',
      criticalBelow: '',
      warningAbove: '',
      criticalAbove: '',
    });
    const body = thresholdBody(warn);
    expect(body.warning_below).toBe(0.5);
    expect(body.critical_below).toBeUndefined();
    expect(body.direction).toBe('below');
  });

  it('writes the bound above for a metric whose fault is 1', () => {
    const body = thresholdBody(withFlagSeverity(form('meraki_uplink_failed'), 'critical'));
    expect(body.critical_above).toBe(0.5);
    expect(body.direction).toBe('above');
  });

  it('leaves a numeric metric alone', () => {
    const f = form('cisco_cpu_5min', { warningAbove: '80' });
    expect(withFlagSeverity(f, 'critical')).toBe(f);
  });
});
