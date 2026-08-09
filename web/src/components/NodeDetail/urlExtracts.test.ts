// SPDX-License-Identifier: AGPL-3.0-only
// Formatting an extracted JSON value has to work for numbers whose units and precision nobody
// declared, so each case here is a shape a real API has returned.

import { describe, expect, it } from 'vitest';
import { extractMetricKey, formatExtractedValue, metricsFromKey } from './urlExtracts';

describe('extractMetricKey / metricsFromKey', () => {
  it('round-trips a rule list', () => {
    // The regression this pins: the key was built with one separator and parsed with another, so
    // the effect asked the API for a single metric named "queue_depth<sep>error_rate" and every
    // extracted value vanished from the card.
    const rules = [{ metric: 'queue_depth' }, { metric: 'error_rate' }, { metric: 'p99_ms' }];
    expect(metricsFromKey(extractMetricKey(rules))).toEqual([
      'queue_depth',
      'error_rate',
      'p99_ms',
    ]);
  });

  it('round-trips the one-rule case that hid the bug', () => {
    // A single name has no separator in it at all, so a mismatched pair still worked here — which
    // is why the two-rule failure went unnoticed.
    expect(metricsFromKey(extractMetricKey([{ metric: 'queue_depth' }]))).toEqual(['queue_depth']);
  });

  it('treats no rules as no metrics, not as one empty name', () => {
    // `''.split(sep)` is `['']`, and fetching a metric named '' is a request nobody meant to make.
    expect(extractMetricKey([])).toBe('');
    expect(metricsFromKey('')).toEqual([]);
  });

  it('keeps the key stable for an unchanged rule list', () => {
    // The key is a `useEffect` dependency; an unstable one re-runs the fetch on every render.
    expect(extractMetricKey([{ metric: 'a' }, { metric: 'b' }])).toBe(
      extractMetricKey([{ metric: 'a' }, { metric: 'b' }]),
    );
  });
});

describe('formatExtractedValue', () => {
  it('keeps whole numbers whole', () => {
    expect(formatExtractedValue(42)).toBe('42');
    expect(formatExtractedValue(0)).toBe('0');
    expect(formatExtractedValue(-7)).toBe('-7');
  });

  it('does not print a float’s full expansion', () => {
    // The case this exists for: a dashboard reading `0.30000000000000004` where the service said
    // `0.3`. Three decimals is enough for a number with no declared unit.
    expect(formatExtractedValue(0.1 + 0.2)).toBe('0.3');
    expect(formatExtractedValue(1.5)).toBe('1.5');
    expect(formatExtractedValue(1.23456)).toBe('1.235');
  });

  it('does not round a small magnitude away to a misleading zero', () => {
    // `0.000004` is a real reading; showing it as `0` would say the opposite of what was measured.
    expect(formatExtractedValue(0.000004)).toBe('4.00e-6');
    expect(formatExtractedValue(-0.0000001)).toBe('-1.00e-7');
  });

  it('has something to show for a value that is not a number', () => {
    // The poller refuses to record NaN/Infinity, so this should be unreachable — but a dashboard
    // that prints "NaN" reads as a broken monitor rather than as a broken assumption.
    expect(formatExtractedValue(Number.NaN)).toBe('—');
    expect(formatExtractedValue(Number.POSITIVE_INFINITY)).toBe('—');
  });
});
