// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import {
  canSubmit,
  DEFAULT_EXPIRY,
  daysUntilExpiry,
  EXPIRY_CHOICES,
  expiryFromChoice,
  ownerChoices,
  tokenState,
  toggleSurface,
} from './tokenForm';
import { TOKEN_SURFACES } from '../types/api';
import type { UserSummary } from '../types/api';

const NOW = new Date('2026-08-02T00:00:00Z');

function user(over: Partial<UserSummary> & { username: string }): UserSummary {
  return {
    id: over.username,
    role: 'viewer',
    created_at: '2026-01-01T00:00:00Z',
    last_login_at: null,
    enabled: true,
    auth_source: 'local',
    ...over,
  } as UserSummary;
}

describe('expiry', () => {
  it('resolves each preset to an instant that far ahead', () => {
    expect(expiryFromChoice('30d', NOW)).toBe('2026-09-01T00:00:00.000Z');
    expect(expiryFromChoice('90d', NOW)).toBe('2026-10-31T00:00:00.000Z');
    expect(expiryFromChoice('365d', NOW)).toBe('2027-08-02T00:00:00.000Z');
  });

  // No expiry has to stay expressible: a service account driving CI should not die on a date
  // nobody wrote down. `undefined` (not null) so the field is omitted from the request body.
  it('maps never to an omitted field', () => {
    expect(expiryFromChoice('never', NOW)).toBeUndefined();
  });

  it('offers a bounded default rather than never', () => {
    expect(DEFAULT_EXPIRY).not.toBe('never');
    expect(EXPIRY_CHOICES).toContain(DEFAULT_EXPIRY);
  });

  it('counts remaining days, and reports nothing for no-expiry or lapsed', () => {
    expect(daysUntilExpiry('2026-08-12T00:00:00Z', NOW)).toBe(10);
    expect(daysUntilExpiry(null, NOW)).toBeNull();
    expect(daysUntilExpiry('2026-08-01T00:00:00Z', NOW)).toBeNull();
  });
});

describe('surfaces', () => {
  it('refuses a token that names none', () => {
    expect(canSubmit('ci', [])).toBe(false);
    expect(canSubmit('ci', ['rest'])).toBe(true);
  });

  it('refuses a blank name', () => {
    expect(canSubmit('   ', ['rest'])).toBe(false);
  });

  it('toggles while keeping the declared order', () => {
    const both = toggleSurface(['rest'], 'mcp', TOKEN_SURFACES);
    expect(both).toEqual(['mcp', 'rest']);
    expect(toggleSurface(both, 'mcp', TOKEN_SURFACES)).toEqual(['rest']);
  });
});

describe('owner choices', () => {
  const users = [
    user({ username: 'alice' }),
    user({ username: 'svc-ci', auth_source: 'service' }),
    user({ username: 'svc-backup', auth_source: 'service' }),
    user({ username: 'bob' }),
  ];

  // Service accounts first: that ordering *is* the recommendation, made visible at the moment the
  // choice is made rather than in documentation nobody reads while minting a credential.
  it('lists service accounts before the signed-in user', () => {
    expect(ownerChoices(users, 'alice').map((u) => u.username)).toEqual([
      'svc-backup',
      'svc-ci',
      'alice',
    ]);
  });

  it('never offers another person', () => {
    expect(ownerChoices(users, 'alice').map((u) => u.username)).not.toContain('bob');
  });

  // A token owned by a disabled account cannot authenticate, so offering one would mint something
  // dead on arrival.
  it('excludes disabled accounts', () => {
    const withDisabled = [...users, user({ username: 'svc-old', auth_source: 'service', enabled: false })];
    expect(ownerChoices(withDisabled, 'alice').map((u) => u.username)).not.toContain('svc-old');
  });
});

describe('token state', () => {
  const live = {
    revoked_at: null,
    expires_at: '2027-01-01T00:00:00Z',
    owner: 'svc-ci',
    owner_active: true,
  };

  it('reports an active token', () => {
    expect(tokenState(live, NOW)).toBe('active');
    expect(tokenState({ ...live, expires_at: null }, NOW)).toBe('active');
  });

  // The order matters: a revoked *and* expired token is revoked, because that is the reason a human
  // acted. Checked in the same order the server checks, so this label matches the 401 they saw.
  it('prefers revocation over every other reason', () => {
    expect(
      tokenState({ ...live, revoked_at: '2026-07-01T00:00:00Z', expires_at: '2026-01-01T00:00:00Z' }, NOW),
    ).toBe('revoked');
  });

  it('distinguishes expired, orphaned, and disabled-owner', () => {
    expect(tokenState({ ...live, expires_at: '2026-01-01T00:00:00Z' }, NOW)).toBe('expired');
    expect(tokenState({ ...live, owner: null }, NOW)).toBe('no-owner');
    expect(tokenState({ ...live, owner_active: false }, NOW)).toBe('owner-disabled');
  });

  // An owner-less token is what the 0057 backfill leaves behind when the issuing account is gone.
  // It cannot authenticate, so showing it as active would hide exactly the case this change exists
  // to surface.
  it('never calls an orphaned token active', () => {
    expect(tokenState({ revoked_at: null, expires_at: null, owner: null, owner_active: false }, NOW)).toBe(
      'no-owner',
    );
  });
});
