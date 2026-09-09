// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import {
  MIN_PW,
  hasLocalPassword,
  passwordHomeKey,
  validateOwnPasswordChange,
} from './password';
import { USER_KINDS } from '../types/api';

const ok = { current: 'the old one', next: 'a much better passphrase', confirm: 'a much better passphrase' };

describe('validateOwnPasswordChange', () => {
  it('accepts a filled, long-enough, matching, different password', () => {
    expect(validateOwnPasswordChange(ok)).toBeNull();
  });

  it('asks for the current password before judging the new one', () => {
    // Order matters: complaining that the *new* password is short, while the box above it is
    // empty, points the operator at the wrong field.
    expect(validateOwnPasswordChange({ current: '', next: 'x', confirm: 'y' })).toBe(
      'current-missing',
    );
  });

  it('refuses a new password one character below the floor and accepts it at the floor', () => {
    const short = 'a'.repeat(MIN_PW - 1);
    const exact = 'a'.repeat(MIN_PW);
    expect(validateOwnPasswordChange({ current: 'old', next: short, confirm: short })).toBe(
      'too-short',
    );
    expect(validateOwnPasswordChange({ current: 'old', next: exact, confirm: exact })).toBeNull();
  });

  it('refuses a new password identical to the current one', () => {
    // Not pedantry: a successful change signs the operator out, so accepting a no-op would end
    // their session for nothing.
    const same = 'a much better passphrase';
    expect(validateOwnPasswordChange({ current: same, next: same, confirm: same })).toBe(
      'unchanged',
    );
  });

  it('refuses a confirmation that does not match, including an empty one', () => {
    expect(validateOwnPasswordChange({ ...ok, confirm: 'something else' })).toBe('mismatch');
    expect(validateOwnPasswordChange({ ...ok, confirm: '' })).toBe('mismatch');
  });
});

describe('hasLocalPassword', () => {
  it('is true for exactly one kind, and false when the core could not answer', () => {
    // The `null` case is the one that matters: it is skeleton mode, and reading it as anything
    // but "no" draws a control whose write path is not there.
    expect(hasLocalPassword('local')).toBe(true);
    expect(hasLocalPassword(null)).toBe(false);
    for (const kind of USER_KINDS.filter((k) => k !== 'local')) {
      expect(hasLocalPassword(kind)).toBe(false);
    }
  });
});

describe('passwordHomeKey', () => {
  it('names where the password lives for the two kinds that have one elsewhere', () => {
    expect(passwordHomeKey('oidc')).toBe('shell.passwordAtIdp');
    expect(passwordHomeKey('ldap')).toBe('shell.passwordAtDirectory');
  });

  it('says nothing for the kinds with nothing to say', () => {
    expect(passwordHomeKey('local')).toBeNull();
    expect(passwordHomeKey('service')).toBeNull();
    expect(passwordHomeKey(null)).toBeNull();
  });

  it('answers for every kind the backend can send', () => {
    // A floor on what was inspected, not on what was found: if `USER_KINDS` ever stopped being the
    // full enum, every assertion above would still pass over a shorter list.
    expect(USER_KINDS.length).toBeGreaterThanOrEqual(4);
    for (const kind of USER_KINDS) {
      const key = passwordHomeKey(kind);
      expect(key === null || key.startsWith('shell.')).toBe(true);
    }
  });
});
