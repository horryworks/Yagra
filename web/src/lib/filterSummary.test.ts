// SPDX-License-Identifier: AGPL-3.0-only
// Unit tests for the filter-trigger summary (no DOM — Vitest node env).

import { describe, expect, it } from 'vitest';
import { TEXT_MODES, type ColumnFilterSpec } from './columnFilter';
import { summarize, summaryIsActive } from './filterSummary';

interface Row {
  kind: string;
  message: string;
}

const ENUM: ColumnFilterSpec<Row> = {
  kind: 'enum',
  options: [
    { value: 'syslog', label: 'Syslog' },
    { value: 'trap', label: 'SNMP trap' },
    { value: 'webhook', label: 'Webhook' },
  ],
  readValue: (r) => r.kind,
  allLabel: 'All kinds',
};

const TEXT: ColumnFilterSpec<Row> = {
  kind: 'text',
  modes: TEXT_MODES,
  not: true,
  readText: (r) => [r.message],
};

const RANGE: ColumnFilterSpec<Row> = {
  kind: 'range',
  presets: [
    { value: '1h', label: 'Last hour', seconds: 3600 },
    { value: '24h', label: 'Last 24 hours', seconds: 86400 },
    { value: 'all', label: 'All time', seconds: null },
  ],
  defaultPreset: '24h',
};

const NUMBER: ColumnFilterSpec<Row> = { kind: 'number' };

describe('enum summaries', () => {
  it('says nothing when nothing is selected', () => {
    expect(summarize(ENUM, '')).toEqual({ kind: 'none' });
  });

  it('shows the label, not the token', () => {
    expect(summarize(ENUM, 'trap')).toEqual({ kind: 'one', label: 'SNMP trap' });
  });

  it('shows the first plus a count, so the trigger width does not follow the selection', () => {
    // "Syslog +2", never "Syslog, SNMP trap, Webhook" — the track can be 90px wide.
    expect(summarize(ENUM, 'syslog,trap,webhook')).toEqual({
      kind: 'many',
      label: 'Syslog',
      more: 2,
    });
  });

  it('falls back to the raw token for a value the spec no longer offers', () => {
    expect(summarize(ENUM, 'kafka')).toEqual({ kind: 'one', label: 'kafka' });
  });
});

describe('text summaries', () => {
  it('decomposes the condition so the trigger need not re-parse it', () => {
    expect(summarize(TEXT, '!~^%LINK')).toEqual({
      kind: 'text',
      term: '^%LINK',
      mode: 'regex',
      not: true,
    });
  });

  it('says nothing for a term that does not narrow', () => {
    expect(summarize(TEXT, '')).toEqual({ kind: 'none' });
  });
});

describe('range summaries', () => {
  it('shows the window even at the default', () => {
    // The default narrows (24h), so a trigger reading "All time" or nothing at all while a
    // day-long window hides rows is the same lie the empty state had to be fixed for.
    expect(summarize(RANGE, '')).toEqual({ kind: 'preset', label: 'Last 24 hours' });
    expect(summarize(RANGE, '24h')).toEqual({ kind: 'preset', label: 'Last 24 hours' });
    expect(summarize(RANGE, 'all')).toEqual({ kind: 'preset', label: 'All time' });
  });
});

describe('number summaries (ADR-053 Inc.6)', () => {
  it('says nothing when both ends are open', () => {
    expect(summarize(NUMBER, '')).toEqual({ kind: 'none' });
    expect(summarize(NUMBER, ':')).toEqual({ kind: 'none' });
  });

  it('reports the two bounds separately so the component can word all three readings', () => {
    // The wording is the component's, not this module's: "8 and up" / "3 – 5" / "5 and below" each
    // need EN and JA, and a one-sided interval rendered as "3 – " reads as unfinished input.
    expect(summarize(NUMBER, '3:5')).toEqual({ kind: 'number', min: 3, max: 5 });
    expect(summarize(NUMBER, '8:')).toEqual({ kind: 'number', min: 8, max: null });
    expect(summarize(NUMBER, ':5')).toEqual({ kind: 'number', min: null, max: 5 });
  });

  it('is active on a bound of zero', () => {
    // The one that would fail with a truthiness check on the decoded bound.
    expect(summaryIsActive(NUMBER, '0:')).toBe(true);
    expect(summarize(NUMBER, '0:')).toEqual({ kind: 'number', min: 0, max: null });
  });
});

describe('summaryIsActive is not "the summary said something"', () => {
  it('is false for a range at its default, which still shows a label', () => {
    // Otherwise the trigger offers a clear button that does nothing.
    expect(summarize(RANGE, '').kind).not.toBe('none');
    expect(summaryIsActive(RANGE, '')).toBe(false);
    expect(summaryIsActive(RANGE, '24h')).toBe(false);
    expect(summaryIsActive(RANGE, '1h')).toBe(true);
  });

  it('tracks the summary for the other kinds', () => {
    expect(summaryIsActive(ENUM, '')).toBe(false);
    expect(summaryIsActive(ENUM, 'trap')).toBe(true);
    expect(summaryIsActive(TEXT, '')).toBe(false);
    expect(summaryIsActive(TEXT, 'link')).toBe(true);
  });
});
