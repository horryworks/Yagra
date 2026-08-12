// SPDX-License-Identifier: AGPL-3.0-only
// Unit tests for the Settings ▸ Forwarding list filter (no DOM — Vitest node env).

import { describe, expect, it } from 'vitest';
import type { ForwardDestination } from '../types/api';
import {
  DEFAULT_FORWARDING_FILTERS,
  isForwardingFiltered,
  matchesForwardDestination,
  type ForwardingFilters,
} from './forwardingListFilters';

const dest = (over: Partial<ForwardDestination> = {}): ForwardDestination => ({
  id: 'd1',
  name: 'siem relay',
  source_kind: 'syslog',
  dest_kind: 'syslog_udp',
  target: '10.0.0.9:514',
  enabled: true,
  verbatim: true,
  has_secret: false,
  ca_cert: null,
  filter: { mode: 'all', conditions: [] },
  pool: null,
  rate_limit_per_sec: null,
  ...over,
});

const f = (over: Partial<ForwardingFilters>): ForwardingFilters => ({
  ...DEFAULT_FORWARDING_FILTERS,
  ...over,
});

describe('matchesForwardDestination', () => {
  it('shows everything when nothing is set', () => {
    expect(matchesForwardDestination(dest(), DEFAULT_FORWARDING_FILTERS)).toBe(true);
  });

  it('filters by destination kind', () => {
    expect(matchesForwardDestination(dest(), f({ dest_kind: 'syslog_udp' }))).toBe(true);
    expect(matchesForwardDestination(dest(), f({ dest_kind: 'bigquery' }))).toBe(false);
  });

  it('filters by enabled', () => {
    expect(matchesForwardDestination(dest({ enabled: false }), f({ enabled: 'disabled' }))).toBe(true);
    expect(matchesForwardDestination(dest({ enabled: false }), f({ enabled: 'enabled' }))).toBe(false);
  });

  it('searches the target as well as the name', () => {
    // The address is what an operator knows during an incident; the name is whatever someone typed
    // months ago.
    expect(matchesForwardDestination(dest(), f({ q: '10.0.0.9' }))).toBe(true);
    expect(matchesForwardDestination(dest(), f({ q: 'SIEM' }))).toBe(true);
    expect(matchesForwardDestination(dest(), f({ q: '10.0.0.8' }))).toBe(false);
  });

  it('flips isFiltered for every field', () => {
    expect(isForwardingFiltered(DEFAULT_FORWARDING_FILTERS)).toBe(false);
    for (const x of [f({ dest_kind: 'bigquery' }), f({ enabled: 'disabled' }), f({ q: 'x' })]) {
      expect(isForwardingFiltered(x)).toBe(true);
    }
  });
});
