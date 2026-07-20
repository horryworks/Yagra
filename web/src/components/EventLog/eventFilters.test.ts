// SPDX-License-Identifier: AGPL-3.0-only
// Pure-helper unit tests for the Events node-filter URL codec (no DOM — Vitest node env).

import { describe, expect, it } from 'vitest';
import { readNodeIdParam, writeNodeIdParam } from './eventFilters';

describe('node_id URL params', () => {
  const roundTrip = (nodeId: string | null) => {
    const params = new URLSearchParams();
    writeNodeIdParam(params, nodeId);
    return readNodeIdParam(params);
  };

  it('round-trips a real node id', () => {
    expect(roundTrip('3f6a1c2e-0000-4000-8000-000000000001')).toBe(
      '3f6a1c2e-0000-4000-8000-000000000001',
    );
  });

  it('returns null when node_id is absent', () => {
    expect(readNodeIdParam(new URLSearchParams())).toBeNull();
  });

  it('returns null for an empty or whitespace-only node_id', () => {
    expect(readNodeIdParam(new URLSearchParams('node_id='))).toBeNull();
    expect(readNodeIdParam(new URLSearchParams('node_id=%20%20'))).toBeNull();
  });

  it('trims surrounding whitespace off a real id', () => {
    expect(readNodeIdParam(new URLSearchParams('node_id=%20abc%20'))).toBe('abc');
  });

  it('deletes the key when cleared (write null)', () => {
    const params = new URLSearchParams('node_id=abc&kind=syslog');
    writeNodeIdParam(params, null);
    expect(params.get('node_id')).toBeNull();
    // Unrelated params are left untouched.
    expect(params.get('kind')).toBe('syslog');
  });

  it('round-trips null back to null', () => {
    expect(roundTrip(null)).toBeNull();
  });
});
