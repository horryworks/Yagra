import { describe, it, expect } from 'vitest';
import { PROFILE_CATEGORIES, categoryLabel } from './profileCategories';

describe('profileCategories', () => {
  it('covers the 14 ProfileCategory tokens', () => {
    expect(PROFILE_CATEGORIES).toHaveLength(14);
    expect(PROFILE_CATEGORIES.map((c) => c.token)).toContain('generic-snmp');
    expect(PROFILE_CATEGORIES.map((c) => c.token)).toContain('l3-switch');
  });

  it('maps a token to its label and falls back to the raw token', () => {
    expect(categoryLabel('firewall')).toBe('Firewall');
    expect(categoryLabel('l3-switch')).toBe('L3 switch');
    expect(categoryLabel('unknown-token')).toBe('unknown-token');
  });

  it('has unique tokens', () => {
    const tokens = PROFILE_CATEGORIES.map((c) => c.token);
    expect(new Set(tokens).size).toBe(tokens.length);
  });
});
