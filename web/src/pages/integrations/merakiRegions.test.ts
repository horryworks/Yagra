// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import { DEFAULT_MERAKI_BASE_URL, MERAKI_REGION_KEYS, MERAKI_REGIONS } from './merakiRegions';

describe('merakiRegions', () => {
  it('starts on the global shard, which is what the backend assumes when none is named', () => {
    expect(DEFAULT_MERAKI_BASE_URL).toBe('https://api.meraki.com');
    expect(MERAKI_REGIONS[0].key).toBe('global');
  });

  it('offers only https endpoints, each once', () => {
    // The API key rides on every request to this host, so plaintext is never an option — the
    // backend refuses it too, and would turn a typo here into a region that can never be used.
    for (const r of MERAKI_REGIONS) {
      expect(r.base_url.startsWith('https://'), r.key).toBe(true);
      expect(new URL(r.base_url).pathname, r.key).toBe('/');
    }
    const urls = MERAKI_REGIONS.map((r) => r.base_url);
    expect(new Set(urls).size).toBe(urls.length);
    expect(new Set(MERAKI_REGION_KEYS).size).toBe(MERAKI_REGION_KEYS.length);
  });

  it('still offers Canada', () => {
    // The region that was offered and refused (ADR-164). Removing it would also "fix" the
    // mismatch, so say out loud that the fix went the other way.
    expect(MERAKI_REGIONS.some((r) => r.base_url === 'https://api.meraki.ca')).toBe(true);
  });
});
