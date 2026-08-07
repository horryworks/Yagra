// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import {
  MAX_DAYS,
  MAX_HOURS,
  RETENTION_FIELDS,
  bandFor,
  formFromValues,
  isDirty,
  isHours,
  parseRetentionForm,
  rowField,
  rowMode,
  storeValue,
  type RetentionForm,
} from './retentionSettings';
import type { RetentionValues } from '../types/api';

const saved: RetentionValues = {
  alert_linked_days: 90,
  unmatched_event_hours: 24,
  report_run_days: 90,
  flow_days: 30,
  diagnostic_days: 90,
};

function form(over: Partial<RetentionForm> = {}): RetentionForm {
  return { ...formFromValues(saved), ...over };
}

describe('rowMode', () => {
  it('maps every token the backend emits', () => {
    expect(rowMode({ tunable: 'settings' })).toBe('editable');
    expect(rowMode({ tunable: 'store_flag_read_only' })).toBe('store-owned');
    expect(rowMode({ tunable: 'by_decision' })).toBe('unlimited');
  });

  // A newer core's token must not fall through to "editable": that would render a control that
  // writes to a field this build cannot send.
  it('treats an unrecognised token as unknown, not as editable', () => {
    expect(rowMode({ tunable: 'something_new' } as never)).toBe('unknown');
    expect(rowMode({ tunable: '' } as never)).toBe('unknown');
  });
});

describe('rowField', () => {
  it('accepts the four editable fields and rejects the two non-fields', () => {
    for (const f of RETENTION_FIELDS) {
      expect(rowField({ field: f })).toBe(f);
    }
    expect(rowField({ field: 'store_owned' })).toBeNull();
    expect(rowField({ field: 'unlimited' })).toBeNull();
    expect(rowField({ field: 'invented_later' } as never)).toBeNull();
  });
});

describe('storeValue', () => {
  it('reports what the store actually said', () => {
    expect(storeValue({ store_reported: '30d', store_configured: true })).toEqual({
      kind: 'reported',
      value: '30d',
    });
  });

  // The distinction that matters: an unconfigured optional store retains nothing, which is a
  // different statement from "configured but we could not read the number".
  it('separates not-configured from unknown', () => {
    expect(storeValue({ store_reported: null, store_configured: false })).toEqual({
      kind: 'not-configured',
    });
    expect(storeValue({ store_reported: null, store_configured: true })).toEqual({
      kind: 'unknown',
    });
    expect(storeValue({ store_reported: '   ', store_configured: true })).toEqual({
      kind: 'unknown',
    });
  });

  it('never invents a number when the store did not report one', () => {
    const v = storeValue({ store_reported: null, store_configured: true });
    expect(v).not.toHaveProperty('value');
  });
});

describe('units', () => {
  it('gives the unmatched-event window hours and everything else days', () => {
    expect(isHours('unmatched_event_hours')).toBe(true);
    expect(bandFor('unmatched_event_hours')).toEqual([1, MAX_HOURS]);
    for (const f of RETENTION_FIELDS.filter((x) => x !== 'unmatched_event_hours')) {
      expect(isHours(f)).toBe(false);
      expect(bandFor(f)).toEqual([1, MAX_DAYS]);
    }
  });
});

describe('parseRetentionForm', () => {
  it('accepts the defaults', () => {
    const r = parseRetentionForm(form());
    expect(r.ok).toBe(true);
    if (r.ok) expect(r.values).toEqual(saved);
  });

  it('rejects zero, blank, fractional and out-of-band values', () => {
    for (const bad of ['0', '', '  ', '1.5', 'abc', '-3', String(MAX_DAYS + 1)]) {
      const r = parseRetentionForm(form({ alert_linked_days: bad }));
      expect(r.ok, `"${bad}" should be rejected`).toBe(false);
    }
  });

  // The two units have different ceilings, so a generic message would be wrong half the time.
  it('names the offending field and its own band', () => {
    const r = parseRetentionForm(form({ unmatched_event_hours: '99999' }));
    expect(r.ok).toBe(false);
    if (!r.ok) {
      expect(r.field).toBe('unmatched_event_hours');
      expect(r.max).toBe(MAX_HOURS);
    }
    const d = parseRetentionForm(form({ flow_days: '99999' }));
    expect(d.ok).toBe(false);
    if (!d.ok) {
      expect(d.field).toBe('flow_days');
      expect(d.max).toBe(MAX_DAYS);
    }
  });

  it('accepts both ends of each band', () => {
    const r = parseRetentionForm({
      alert_linked_days: '1',
      unmatched_event_hours: String(MAX_HOURS),
      report_run_days: String(MAX_DAYS),
      flow_days: '1',
      diagnostic_days: String(MAX_DAYS),
    });
    expect(r.ok).toBe(true);
  });
});

describe('isDirty', () => {
  it('is false for an untouched form and for whitespace-only edits', () => {
    expect(isDirty(form(), saved)).toBe(false);
    expect(isDirty(form({ flow_days: ' 30 ' }), saved)).toBe(false);
  });

  it('is true once any window actually changes', () => {
    expect(isDirty(form({ flow_days: '7' }), saved)).toBe(true);
    expect(isDirty(form({ unmatched_event_hours: '48' }), saved)).toBe(true);
  });
});
