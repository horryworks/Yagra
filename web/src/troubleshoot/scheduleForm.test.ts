// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import {
  BASELINE_SECS,
  blankSchedule,
  formFromSchedule,
  scheduleBody,
  scheduleFormError,
  schedulableTools,
  sliderFor,
  timeLabel,
  type ScheduleForm,
} from './scheduleForm';
import { sigmaFor } from './report/format';
import { TOOLS } from './data';
import type { AnalysisSchedule } from '../types/api';
import type { ScopeValue } from './scope';

const ALL: ScopeValue = { kind: 'all', id: null, label: 'All nodes' };

function form(over: Partial<ScheduleForm> = {}): ScheduleForm {
  return { ...blankSchedule(ALL), ...over };
}

function stored(over: Partial<AnalysisSchedule> = {}): AnalysisSchedule {
  return {
    id: 's1',
    tool: 'anomaly',
    scope_kind: 'all',
    scope_id: null,
    scope_label: 'All nodes',
    params: { window_secs: 86_400, sensitivity: 2.5, notify: true },
    frequency: 'weekly',
    day_of_week: 3,
    day_of_month: null,
    at_hour: 3,
    at_minute: 0,
    enabled: true,
    next_run_ms: 0,
    last_run_ms: null,
    last_status: null,
    ...over,
  };
}

describe('blankSchedule', () => {
  it('defaults an unattended analysis to silent', () => {
    // The launch drawer defaults notify ON because someone is waiting for that run. A nightly
    // schedule that pages every morning is how the feature gets switched off.
    expect(blankSchedule(ALL).notify).toBe(false);
  });
});

describe('sliderFor', () => {
  it('round-trips through the single sigma definition', () => {
    // The property that matters: opening an existing schedule and saving it unchanged must not
    // move its sensitivity. A second copy of the slider→σ table is exactly how that breaks.
    for (const pos of [1, 2, 3, 4, 5]) {
      expect(sliderFor(sigmaFor(pos))).toBe(pos);
    }
  });

  it('clamps a stored value outside the slider range', () => {
    expect(sliderFor(99)).toBe(1);
    expect(sliderFor(-99)).toBe(5);
  });
});

describe('formFromSchedule', () => {
  it('reads the stored params back without moving them', () => {
    const f = formFromSchedule(stored(), ALL);
    expect(f.windowSecs).toBe(86_400);
    expect(sigmaFor(f.sensitivity)).toBe(2.5);
    expect(f.notify).toBe(true);
    expect(f.dayOfWeek).toBe(3);
  });

  it('keeps an unrecognised cadence rather than quietly making it daily', () => {
    // Silently rewriting it would turn a monthly schedule into a nightly one the first time
    // someone opened the row and pressed save. `scheduleFormError` is what blocks the save.
    const f = formFromSchedule(stored({ frequency: 'unknown' }), ALL);
    expect(f.frequency).toBe('unknown');
    expect(scheduleFormError(f)).toBe('unknown_cadence');
  });

  it('falls back to a renderable tool when the row names one this build lacks', () => {
    expect(formFromSchedule(stored({ tool: 'teleport' }), ALL).tool).toBe('anomaly');
  });
});

describe('scheduleBody', () => {
  it('sends only the day field its cadence reads', () => {
    const weekly = scheduleBody(form({ frequency: 'weekly', dayOfWeek: 5 }), '7 days');
    expect(weekly.day_of_week).toBe(5);
    expect(weekly.day_of_month).toBeNull();

    const monthly = scheduleBody(form({ frequency: 'monthly', dayOfMonth: 12 }), '7 days');
    expect(monthly.day_of_month).toBe(12);
    expect(monthly.day_of_week).toBeNull();

    const daily = scheduleBody(form({ frequency: 'daily' }), '7 days');
    expect(daily.day_of_week).toBeNull();
    expect(daily.day_of_month).toBeNull();
  });

  it('carries the scope and the fixed baseline through', () => {
    const b = scheduleBody(
      form({ scope: { kind: 'group', id: 'g1', label: 'Group tokyo' } }),
      '7 days',
    );
    expect(b.scope_kind).toBe('group');
    expect(b.scope_id).toBe('g1');
    expect(b.scope_label).toBe('Group tokyo · 7 days');
    expect(b.baseline_secs).toBe(BASELINE_SECS);
  });
});

describe('scheduleFormError', () => {
  it('refuses a group or node scope with no id', () => {
    // The runner would resolve it to the whole fleet, which is not what the operator picked.
    expect(scheduleFormError(form({ scope: { kind: 'node', id: null, label: 'x' } }))).toBe(
      'missing_scope',
    );
    expect(scheduleFormError(form())).toBeNull();
  });
});

describe('schedulableTools', () => {
  it('hides the flow analyses when this deployment has no flow store', () => {
    // The backend refuses them (`flow_tier_off`), because each fire would write an empty run.
    // Offering an option that cannot be saved is worse than not offering it.
    const withFlow = schedulableTools(true);
    const without = schedulableTools(false);
    expect(withFlow).toEqual(TOOLS);
    expect(without.length).toBeLessThan(TOOLS.length);
    expect(without.some((t) => t.method === 'flow')).toBe(false);
    // …and everything else survives, so a missing flow store does not hide unrelated analyses.
    expect(without.length).toBe(TOOLS.filter((t) => t.method !== 'flow').length);
  });
});

describe('timeLabel', () => {
  it('zero-pads', () => {
    expect(timeLabel(3, 0)).toBe('03:00');
    expect(timeLabel(23, 59)).toBe('23:59');
  });
});
