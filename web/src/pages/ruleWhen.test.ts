// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import type { TFunction } from 'i18next';
import { whenBodyFrom, whenDraftFrom, whenText } from './ruleWhen';

const t = ((k: string, o?: Record<string, string>) =>
  o?.span ? `outside ${o.span}` : (k.split('.').pop() ?? k)) as unknown as TFunction;

describe('a rule\'s hours', () => {
  it('starts a rule with none at office hours on weekdays, switched to always', () => {
    expect(whenDraftFrom(undefined)).toEqual({
      mode: 'always',
      days: [1, 2, 3, 4, 5],
      start: '08:30',
      end: '17:20',
    });
  });

  it('sends no window for always, and refuses a window with no day', () => {
    expect(whenBodyFrom({ mode: 'always', days: [], start: '', end: '' })).toEqual({
      when: { mode: 'always' },
    });
    expect(whenBodyFrom({ mode: 'during', days: [], start: '08:30', end: '17:20' })).toEqual({
      error: 'days',
    });
    expect(whenBodyFrom({ mode: 'outside', days: [1], start: '8', end: '17:20' })).toEqual({
      error: 'time',
    });
  });

  it('round-trips a window, days in week order', () => {
    const body = whenBodyFrom({ mode: 'during', days: [5, 1], start: '08:30', end: '17:20' });
    expect(body).toEqual({
      when: { mode: 'during', days: [1, 5], start_minute: 510, end_minute: 1040 },
    });
    if ('when' in body) expect(whenDraftFrom(body.when).days).toEqual([1, 5]);
  });

  it('says always, a window, or the outside of one', () => {
    expect(whenText(t, { mode: 'always' })).toBe('always');
    // ADR-266: a rule that holds while closed says so after its hours; an always-open one cannot.
    const office = { mode: 'during' as const, days: [1, 2, 3, 4, 5], start_minute: 510, end_minute: 1040 };
    expect(whenText(t, office, 'hold')).toMatch(/08:30–17:20 · closedHeld$/);
    expect(whenText(t, office, 'next_rule')).not.toContain('closedHeld');
    expect(whenText(t, { mode: 'always' }, 'hold')).toBe('always');
    const w = { days: [1, 2, 3, 4, 5], start_minute: 510, end_minute: 1040 };
    expect(whenText(t, { mode: 'during', ...w })).toContain('08:30–17:20');
    expect(whenText(t, { mode: 'outside', ...w })).toMatch(/^outside /);
  });
});
