// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import type { TFunction } from 'i18next';
import { addressText, hasAddress } from './nodeAddress';

const t = ((key: string) => `<${key}>`) as unknown as TFunction;

// ADR-175: the unspecified address is how "no address" is stored, never an address to show.
describe('node address', () => {
  it('treats both families of the unspecified address as no address', () => {
    for (const none of ['0.0.0.0', '::', '0:0:0:0:0:0:0:0', ' 0.0.0.0 ', '', null, undefined]) {
      expect(hasAddress(none), String(none)).toBe(false);
    }
    for (const real of ['192.0.2.1', '2001:db8::1', '10.0.0.0']) {
      expect(hasAddress(real), real).toBe(true);
    }
  });

  it('shows the address itself, or says there is none — naming a mesh repeater as the reason', () => {
    expect(addressText('192.0.2.1', t)).toBe('192.0.2.1');
    expect(addressText('192.0.2.1', t, { meshRepeater: true })).toBe('192.0.2.1');
    expect(addressText('0.0.0.0', t)).toBe('<nodes:address.none>');
    expect(addressText('::', t, { meshRepeater: true })).toBe('<nodes:address.noneRepeater>');
  });
});
