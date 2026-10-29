// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import { buildCliLoginSecret, cliLoginReady, emptyCliLogin } from './cliLoginCredential';

describe('CLI login credential (ADR-210 stage 2)', () => {
  it('needs a password, and neither a user name nor an enable', () => {
    expect(cliLoginReady(emptyCliLogin())).toBe(false);
    expect(cliLoginReady({ username: 'ops', password: '', enable: 'e' })).toBe(false);
    expect(cliLoginReady({ username: '', password: 'x', enable: '' })).toBe(true);
    expect(cliLoginReady({ username: '  ', password: 'x', enable: '' })).toBe(true);
    expect(cliLoginReady({ username: 'ops', password: 'x', enable: '' })).toBe(true);
  });

  it('sends a blank user name as an empty string, not as a missing field', () => {
    expect(JSON.parse(buildCliLoginSecret({ username: '  ', password: 'pw', enable: '' }))).toEqual({
      username: '',
      password: 'pw',
    });
  });

  it('seals the shape the server parses, and leaves a blank enable out', () => {
    expect(JSON.parse(buildCliLoginSecret({ username: ' ops ', password: 'p w', enable: '' }))).toEqual({
      username: 'ops',
      password: 'p w',
    });
    expect(JSON.parse(buildCliLoginSecret({ username: 'ops', password: 'p', enable: 'en' }))).toEqual({
      username: 'ops',
      password: 'p',
      enable: 'en',
    });
  });
});
