// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import {
  ALERT_RULE_TABS,
  alertRuleTabOf,
  alertRuleTabPath,
  EVENT_RULES_MOVED_FROM,
} from './alertRules';
import { navItemForPath, rememberableRoute } from '../nav';

describe('alert rule tabs (ADR-232)', () => {
  it('the threshold tab is the hub itself, so every link written before still lands on it', () => {
    expect(alertRuleTabPath('thresholds')).toBe('/alerts/rules');
    expect(alertRuleTabOf('/alerts/rules')).toBe('thresholds');
    expect(alertRuleTabOf('/alerts/rules/events')).toBe('events');
    expect(alertRuleTabOf('/alerts/rules/nonsense')).toBe('thresholds');
  });

  it('every tab is the one menu item, and is remembered as the tab', () => {
    for (const tab of ALERT_RULE_TABS) {
      const path = alertRuleTabPath(tab);
      expect(navItemForPath(path)?.item.path).toBe('/alerts/rules');
      expect(rememberableRoute(path, '?target_level=folder')?.route).toBe(
        `${path}?target_level=folder`,
      );
    }
  });

  it('the old event rules path is no menu item any more', () => {
    expect(navItemForPath(EVENT_RULES_MOVED_FROM)).toBeNull();
  });
});
