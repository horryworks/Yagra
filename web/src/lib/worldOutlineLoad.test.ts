// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import { loadOutline } from './worldOutlineLoad';

describe('loadOutline', () => {
  it('hands over the generated coastline, and the same one on every call', async () => {
    const first = await loadOutline();
    expect(first.land.length).toBeGreaterThan(100);
    expect(first.lakes.length).toBeGreaterThan(0);
    expect(await loadOutline()).toBe(first);
  });
});
