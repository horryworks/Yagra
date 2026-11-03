// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import type { TFunction } from 'i18next';
import { filterSummaryText } from './filterWords';

// A `t` that names the key and its values, so each branch's choice of string is what is checked.
const t = ((key: string, opts?: Record<string, unknown>) =>
  opts ? `${key}(${JSON.stringify(opts)})` : key) as unknown as TFunction;

describe('filterSummaryText', () => {
  it('prints the one chosen value, or a preset, as its label', () => {
    expect(filterSummaryText({ kind: 'one', label: 'Critical' }, t)).toBe('Critical');
    expect(filterSummaryText({ kind: 'preset', label: 'Last 24 h' }, t)).toBe('Last 24 h');
  });

  it('prints the first of several values and how many more there are', () => {
    expect(filterSummaryText({ kind: 'many', label: 'Critical', more: 2 }, t)).toBe(
      'Critical common:filter.more({"count":2})',
    );
  });

  it('prints a text condition as its term, with no mark of its own for a negation', () => {
    expect(filterSummaryText({ kind: 'text', term: 'ge-0/0', mode: 'contains', not: true }, t)).toBe(
      'ge-0/0',
    );
  });

  it('prints a number range three ways, so a one-sided one reads as deliberate', () => {
    expect(filterSummaryText({ kind: 'number', min: 3, max: 8 }, t)).toBe('3 – 8');
    expect(filterSummaryText({ kind: 'number', min: 8, max: null }, t)).toBe(
      'common:filter.numMin({"value":8})',
    );
    expect(filterSummaryText({ kind: 'number', min: null, max: 3 }, t)).toBe(
      'common:filter.numMax({"value":3})',
    );
  });
});
