// SPDX-License-Identifier: AGPL-3.0-only
import { describe, it, expect } from 'vitest';
import type { TFunction } from 'i18next';
import { PROFILE_CATEGORIES, categoryLabel } from './profileCategories';
import enMonitoring from '../locales/en/monitoring.json';

// Resolve a `monitoring:categories.*` key against the bundled English strings, mirroring how
// i18next would — enough for the label assertions without booting the full i18n stack.
const t = ((key: string): string => {
  const path = key.replace(/^monitoring:/, '').split('.');
  let cur: unknown = enMonitoring;
  for (const seg of path) cur = (cur as Record<string, unknown> | undefined)?.[seg];
  return typeof cur === 'string' ? cur : key;
}) as unknown as TFunction;

describe('profileCategories', () => {
  it('covers the 16 ProfileCategory tokens', () => {
    expect(PROFILE_CATEGORIES).toHaveLength(16);
    expect(PROFILE_CATEGORIES.map((c) => c.token)).toContain('generic-snmp');
    expect(PROFILE_CATEGORIES.map((c) => c.token)).toContain('l3-switch');
    // The endpoint-monitor kinds: without these the Device profiles page renders a raw token.
    expect(PROFILE_CATEGORIES.map((c) => c.token)).toContain('url-check');
    expect(PROFILE_CATEGORIES.map((c) => c.token)).toContain('dns-check');
  });

  it('maps a token to its label and falls back to the raw token', () => {
    expect(categoryLabel('firewall', t)).toBe('Firewall');
    expect(categoryLabel('l3-switch', t)).toBe('L3 switch');
    expect(categoryLabel('dns-check', t)).toBe('DNS monitor');
    expect(categoryLabel('unknown-token', t)).toBe('unknown-token');
  });

  it('has unique tokens', () => {
    const tokens = PROFILE_CATEGORIES.map((c) => c.token);
    expect(new Set(tokens).size).toBe(tokens.length);
  });
});
