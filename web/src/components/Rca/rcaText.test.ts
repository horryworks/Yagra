// SPDX-License-Identifier: AGPL-3.0-only
// "Explain this incident" text helpers (ADR-029). The refusal mapping is the point: each branch is
// a different next action for the reader, and collapsing them into one "AI failed" message is how
// an unconfigured install gets reported as a broken feature.

import { describe, expect, it } from 'vitest';
import { ApiError } from '../../services/api';
import type { RcaNodeFacts } from '../../types/api';
import { formatWindow, nodeLine, refusalText } from './rcaText';

/** Echoes the key (plus any interpolated message) so a test asserts on the branch taken. */
const t = (key: string, opts?: Record<string, unknown>) =>
  opts?.message != null ? `${key}:${String(opts.message)}` : key;

const node = (over: Partial<RcaNodeFacts> = {}): RcaNodeFacts => ({
  name: 'core-sw-01',
  address: '10.0.0.1',
  vendor: null,
  model: null,
  pool: null,
  tags: [],
  ...over,
});

describe('formatWindow', () => {
  it('uses the coarsest exact unit', () => {
    expect(formatWindow(3600)).toBe('1h');
    expect(formatWindow(6 * 3600)).toBe('6h');
    expect(formatWindow(86400)).toBe('1d');
    expect(formatWindow(1800)).toBe('30m');
  });

  it('falls back to minutes for an inexact window', () => {
    expect(formatWindow(5400)).toBe('90m');
    // Guard against `0 % 3600 === 0` rendering a nonsensical "0h".
    expect(formatWindow(0)).toBe('0m');
  });
});

describe('nodeLine', () => {
  it('shows name and address, and appends hardware when known', () => {
    expect(nodeLine(node())).toBe('core-sw-01 (10.0.0.1)');
    expect(nodeLine(node({ vendor: 'Cisco', model: 'C9300' }))).toBe(
      'core-sw-01 (10.0.0.1) · Cisco C9300',
    );
    expect(nodeLine(node({ vendor: 'Cisco' }))).toBe('core-sw-01 (10.0.0.1) · Cisco');
  });
});

describe('refusalText', () => {
  it('tells an unauthorized caller about the permission, whatever the code says', () => {
    expect(refusalText(new ApiError('rca_not_configured', 'nope', 403), t)).toBe('err.forbidden');
    expect(refusalText(new ApiError('unauthorized', 'nope', 401), t)).toBe('err.forbidden');
  });

  it('separates "nobody set this up" from "what is set up is wrong"', () => {
    // Different next actions: one is a first-time setup, the other is a field to fix.
    expect(refusalText(new ApiError('rca_not_configured', 'x', 503), t)).toBe('err.notConfigured');
    expect(refusalText(new ApiError('rca_misconfigured', 'no key', 503), t)).toBe(
      'err.misconfigured:no key',
    );
  });

  it('maps the remaining typed refusals to their own sentence', () => {
    expect(refusalText(new ApiError('rate_limited', 'x', 429), t)).toBe('err.rateLimited');
    expect(refusalText(new ApiError('no_incident', 'gone', 404), t)).toBe('err.noIncident:gone');
    expect(refusalText(new ApiError('provider_error', 'timeout', 502), t)).toBe(
      'err.provider:timeout',
    );
    expect(refusalText(new ApiError('model_refused', 'declined', 502), t)).toBe(
      'err.provider:declined',
    );
  });

  it('shows the server message for an unrecognised code rather than swallowing it', () => {
    expect(refusalText(new ApiError('something_new', 'a reason', 500), t)).toBe('a reason');
    expect(refusalText(new ApiError('something_new', '', 500), t)).toBe('err.generic');
  });

  it('falls back generically for a non-API failure (e.g. the network dropped)', () => {
    expect(refusalText(new TypeError('Failed to fetch'), t)).toBe('err.generic');
    expect(refusalText(undefined, t)).toBe('err.generic');
  });
});
