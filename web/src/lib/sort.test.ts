// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import { byNewestCreated } from './sort';

describe('byNewestCreated', () => {
  it('puts the most recently created first and leaves equal times in place', () => {
    const rows = [
      { id: 'a', created_ms: 100 },
      { id: 'b', created_ms: 300 },
      { id: 'c', created_ms: 200 },
      { id: 'd', created_ms: 200 },
    ];
    expect([...rows].sort(byNewestCreated).map((r) => r.id)).toEqual(['b', 'c', 'd', 'a']);
  });
});
