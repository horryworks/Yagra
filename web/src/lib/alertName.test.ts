// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import i18n from '../i18n';
import enAlertNames from '../locales/en/alertNames.json';
import jaAlertNames from '../locales/ja/alertNames.json';
import alertNameFlags from '../api/alertNameFlags.json';
import { alertNameKey, alertNameTexts, alertTitle } from './alertName';

describe('alert names (ADR-196)', () => {
  it('names every flag it lists, and lists more than a handful of each shape', () => {
    const named = Object.keys(enAlertNames);
    const flags = Object.keys(alertNameFlags);
    for (const flag of flags) expect(named).toContain(flag);
    // A floor in both directions: the generator emitting nothing must not read as "no names",
    // and every metric becoming a flag would hide every condition on every alert.
    expect(named.length).toBeGreaterThan(150);
    expect(flags.length).toBeGreaterThan(5);
    expect(flags.length).toBeLessThan(named.length / 4);
  });

  it('answers in the operator language and keeps a raw spelling it has no name for', async () => {
    try {
      expect(alertTitle('snmp_up')).toEqual({ text: 'SNMP not responding', flag: true });
      expect(alertTitle('huawei_temp')).toEqual({ text: 'Temperature', flag: false });
      expect(alertTitle('vendor_x_new_gauge')).toBeNull();
      expect(alertNameKey('vendor_x_new_gauge')).toBeNull();
      expect(alertTitle('event:Link down')).toEqual({ text: 'Event rule: Link down', flag: true });

      await i18n.changeLanguage('ja');
      expect(alertTitle('snmp_up')?.text).toBe(jaAlertNames.snmp_up);
      expect(alertTitle('snmp_up')?.text).toBe('SNMP が応答しない');
      expect(alertTitle('event:Link down')?.text).toBe('イベントルール: Link down');
    } finally {
      await i18n.changeLanguage('en');
    }
  });

  it('lists every name an alert goes by, the shown one first (ADR-196 decision 7)', async () => {
    try {
      expect(alertNameTexts('__liveness__')).toEqual([
        'Node not responding',
        'Node not responding',
        jaAlertNames.__liveness__,
      ]);
      // No name, no texts: the metric itself is matched by the caller.
      expect(alertNameTexts('vendor_x_new_gauge')).toEqual([]);
      // An event rule's "Event rule: ..." is not a name core can match, so it is not one here.
      expect(alertNameTexts('event:Link down')).toEqual([]);
      await i18n.changeLanguage('ja');
      expect(alertNameTexts('snmp_up')[0]).toBe(jaAlertNames.snmp_up);
    } finally {
      await i18n.changeLanguage('en');
    }
  });
});
