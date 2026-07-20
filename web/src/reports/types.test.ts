// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import i18n from '../i18n';
import type { ReportSectionDef, ReportSpec } from '../types/api';
import {
  cadenceLabel,
  defaultSettings,
  emptySpec,
  newSection,
  sanitizeSpec,
  weekdayName,
} from './types';

// cadenceLabel/weekdayName resolve their display strings through i18next now; pass the real
// (English-bundled) translator so the expected output stays the human-readable label.
const t = i18n.t;

const topCpu: ReportSectionDef = {
  kind: 'top-cpu',
  title: 'Top CPU',
  blurb: '',
  group: 'Performance',
  settings: [
    { key: 'limit', label: 'Rows', kind: 'number', default: 10 },
    {
      key: 'agg',
      label: 'Aggregation',
      kind: 'select',
      default: 'max_1h',
      options: [
        { value: 'now', label: 'Current' },
        { value: 'max_1h', label: '1h peak' },
      ],
    },
  ],
};

describe('defaultSettings + newSection', () => {
  it('seeds settings from each catalog default', () => {
    expect(defaultSettings(topCpu)).toEqual({ limit: 10, agg: 'max_1h' });
  });

  it('makes a placed section with a unique id and the kind', () => {
    const a = newSection(topCpu);
    const b = newSection(topCpu);
    expect(a.kind).toBe('top-cpu');
    expect(a.settings).toEqual({ limit: 10, agg: 'max_1h' });
    expect(a.id).not.toBe(b.id);
  });
});

describe('sanitizeSpec', () => {
  it('returns an empty spec for null/garbage', () => {
    expect(sanitizeSpec(null)).toEqual(emptySpec());
  });

  it('keeps valid sections, drops malformed ones, defaults the range', () => {
    const dirty = {
      params: { range_secs: 0 }, // invalid → default
      sections: [
        { id: 's1', kind: 'top-cpu', settings: { limit: 5 } },
        { kind: 'top-rtt' }, // no id/settings → filled
        { settings: {} }, // no kind → dropped
      ],
    } as unknown as ReportSpec;
    const clean = sanitizeSpec(dirty);
    expect(clean.params.range_secs).toBe(7 * 86400);
    expect(clean.sections).toHaveLength(2);
    expect(clean.sections[0]).toEqual({ id: 's1', kind: 'top-cpu', settings: { limit: 5 } });
    expect(clean.sections[1].kind).toBe('top-rtt');
    expect(typeof clean.sections[1].id).toBe('string');
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
