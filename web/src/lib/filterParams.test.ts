// SPDX-License-Identifier: AGPL-3.0-only
// Pure-helper unit tests for the list-filter URL codecs (no DOM — Vitest node env). Moved from
// `components/EventLog/eventFilters.test.ts` with the helpers, and extended for the closed-set
// codec Alert history needs.

import { describe, expect, it } from 'vitest';
import { readEnumParam, readIdParam, writeEnumParam, writeIdParam } from './filterParams';

describe('id URL params', () => {
  const roundTrip = (id: string | null) => {
    const params = new URLSearchParams();
    writeIdParam(params, 'node_id', id);
    return readIdParam(params, 'node_id');
  };

  it('round-trips a real node id', () => {
    expect(roundTrip('3f6a1c2e-0000-4000-8000-000000000001')).toBe(
      '3f6a1c2e-0000-4000-8000-000000000001',
    );
  });

  it('returns null when the key is absent', () => {
    expect(readIdParam(new URLSearchParams(), 'node_id')).toBeNull();
  });

  it('returns null for an empty or whitespace-only value', () => {
    expect(readIdParam(new URLSearchParams('node_id='), 'node_id')).toBeNull();
    expect(readIdParam(new URLSearchParams('node_id=%20%20'), 'node_id')).toBeNull();
  });

  it('trims surrounding whitespace off a real id', () => {
    expect(readIdParam(new URLSearchParams('node_id=%20abc%20'), 'node_id')).toBe('abc');
  });

  it('deletes the key when cleared (write null)', () => {
    const params = new URLSearchParams('node_id=abc&kind=syslog');
    writeIdParam(params, 'node_id', null);
    expect(params.get('node_id')).toBeNull();
    // Unrelated params are left untouched.
    expect(params.get('kind')).toBe('syslog');
  });

  it('round-trips null back to null', () => {
    expect(roundTrip(null)).toBeNull();
  });

  it('keys are independent, so one screen can carry two id filters', () => {
    // Alert history filters by node *or* by group; the two must not collide.
    const params = new URLSearchParams();
    writeIdParam(params, 'node_id', 'n1');
    writeIdParam(params, 'group_id', 'g1');
    expect(readIdParam(params, 'node_id')).toBe('n1');
    expect(readIdParam(params, 'group_id')).toBe('g1');
    writeIdParam(params, 'node_id', null);
    expect(readIdParam(params, 'group_id')).toBe('g1');
  });
});

describe('closed-set URL params', () => {
  const SEVERITIES = ['', 'info', 'warning', 'critical'] as const;
  type Sev = (typeof SEVERITIES)[number];

  it('round-trips a member of the set', () => {
    const params = new URLSearchParams();
    writeEnumParam(params, 'severity', 'critical' as Sev, '');
    expect(readEnumParam(params, 'severity', SEVERITIES, '')).toBe('critical');
  });

  it('falls back rather than surfacing a value the control cannot render', () => {
    // A stale bookmark from an older release, or a hand-edited URL. The screen recovers by showing
    // the default view — unlike the API edge, where an unknown token is a 400 so a typo cannot
    // silently widen a search.
    expect(readEnumParam(new URLSearchParams('severity=fatal'), 'severity', SEVERITIES, '')).toBe('');
    expect(readEnumParam(new URLSearchParams(), 'severity', SEVERITIES, '')).toBe('');
  });

  it('writing the fallback deletes the key, so the default view has no query string', () => {
    const params = new URLSearchParams('severity=critical&node_id=abc');
    writeEnumParam(params, 'severity', '' as Sev, '');
    expect(params.get('severity')).toBeNull();
    expect(params.get('node_id')).toBe('abc');
  });

  it('a non-empty fallback is still the value that gets deleted', () => {
    // Ranges default to something other than '' (a bounded window), and that default must be the
    // one spelling that leaves no trace in the URL.
    const RANGES = ['24h', '7d', '30d', 'all'] as const;
    const params = new URLSearchParams();
    writeEnumParam(params, 'range', '7d', '7d');
    expect(params.toString()).toBe('');
    // …and an absent key still reads back as the default, so the round trip is closed.
    expect(readEnumParam(params, 'range', RANGES, '7d')).toBe('7d');
    writeEnumParam(params, 'range', 'all', '7d');
    expect(params.get('range')).toBe('all');
    expect(readEnumParam(params, 'range', RANGES, '7d')).toBe('all');
  });
});
