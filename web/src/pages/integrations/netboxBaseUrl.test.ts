// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import { addressChangeNeedsToken } from './netboxBaseUrl';

describe('addressChangeNeedsToken (ADR-178 決定 3)', () => {
  const stored = 'https://netbox.example.com';

  it('asks for the token when the host, scheme or port changes', () => {
    expect(addressChangeNeedsToken(stored, 'https://other.example.com')).toBe(true);
    expect(addressChangeNeedsToken(stored, 'http://netbox.example.com')).toBe(true);
    expect(addressChangeNeedsToken(stored, 'https://netbox.example.com:8443')).toBe(true);
  });

  it('does not ask when only the path, the case or a default port differs', () => {
    expect(addressChangeNeedsToken(stored, stored)).toBe(false);
    expect(addressChangeNeedsToken(stored, ' https://netbox.example.com/netbox/ ')).toBe(false);
    expect(addressChangeNeedsToken(stored, 'https://NetBox.Example.com:443/dcim/sites/')).toBe(false);
    expect(addressChangeNeedsToken('https://netbox.example.com/netbox', stored)).toBe(false);
  });

  it('leaves a half-typed or unparseable address to the backend', () => {
    expect(addressChangeNeedsToken(stored, 'https://')).toBe(false);
    expect(addressChangeNeedsToken(stored, 'netbox.example')).toBe(false);
    expect(addressChangeNeedsToken(stored, 'ftp://netbox.example.com')).toBe(false);
  });
});
