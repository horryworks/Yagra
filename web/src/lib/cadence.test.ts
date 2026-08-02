// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import i18n from '../i18n';
import { CADENCES } from '../types/api';
import { CADENCE, SELECTABLE_CADENCES, cadenceLabel, weekdayName } from './cadence';

// The label builders resolve through i18next; pass the real (English-bundled) translator so the
// expected output stays the human-readable label rather than a key.
const t = i18n.t;

describe('cadence presentation', () => {
  it('covers every cadence', () => {
    // The Record type already forces this at compile time; asserting it at runtime is what catches
    // the union and the generated schema drifting apart after a regeneration.
    expect(Object.keys(CADENCE).sort()).toEqual([...CADENCES].sort());
  });

  it('never presents an unrecognised cadence as daily', () => {
    // The bug this module exists to fix: a `default:` arm rendered a cadence written by a newer
    // core as "Daily", so an operator believed a monthly schedule fired every night.
    expect(CADENCE.unknown.labelKey).not.toBe(CADENCE.daily.labelKey);
  });

  it('offers every cadence except the storage-only one', () => {
    // A deliberate subset: `unknown` means "written by a newer core", so an operator picking it
    // would be asking for a cadence the scheduler silently treats as daily.
    expect([...SELECTABLE_CADENCES]).toEqual(['daily', 'weekly', 'monthly']);
    expect(SELECTABLE_CADENCES).not.toContain('unknown');
    // Pin the subset relation, so a fourth cadence has to choose a side.
    for (const f of SELECTABLE_CADENCES) expect(CADENCES).toContain(f);
    expect(SELECTABLE_CADENCES.length).toBe(CADENCES.length - 1);
  });

  it('names which extra field each cadence needs', () => {
    // Drives whether the schedule form shows a weekday picker or a day-of-month picker; getting it
    // wrong means a weekly schedule asking for a day of the month.
    expect(CADENCE.daily.part).toBe('none');
    expect(CADENCE.weekly.part).toBe('day');
    expect(CADENCE.monthly.part).toBe('dom');
  });
});

describe('cadenceLabel', () => {
  it('formats daily/weekly/monthly with UTC time', () => {
    expect(
      cadenceLabel(t, {
        frequency: 'daily',
        day_of_week: null,
        day_of_month: null,
        at_hour: 9,
        at_minute: 0,
      }),
    ).toBe('Daily · 09:00 UTC');
    expect(
      cadenceLabel(t, {
        frequency: 'weekly',
        day_of_week: 1,
        day_of_month: null,
        at_hour: 8,
        at_minute: 30,
      }),
    ).toBe('Weekly · Monday 08:30 UTC');
    expect(
      cadenceLabel(t, {
        frequency: 'monthly',
        day_of_week: null,
        day_of_month: 15,
        at_hour: 6,
        at_minute: 5,
      }),
    ).toBe('Monthly · day 15 06:05 UTC');
  });

  it('weekdayName clamps out-of-range indices', () => {
    expect(weekdayName(t, 0)).toBe('Sunday');
    expect(weekdayName(t, 6)).toBe('Saturday');
    expect(weekdayName(t, 99)).toBe('Saturday');
  });
});
