// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import { eqIgnoreAsciiCase, trimLikeCore, utf8Length } from './coreText';

describe('trimLikeCore', () => {
  it('strips what Rust strips: White_Space, including U+0085 and U+3000', () => {
    expect(trimLikeCore(' \t\n a b \r ')).toBe('a b');
    expect(trimLikeCore('\u0085x　')).toBe('x');
  });

  it('keeps what Rust keeps: U+FEFF is not White_Space', () => {
    expect(trimLikeCore('﻿x﻿')).toBe('﻿x﻿');
    // ...where JavaScript's own trim would have dropped it.
    expect('﻿x'.trim()).toBe('x');
  });
});

describe('utf8Length', () => {
  it('counts bytes, not UTF-16 units', () => {
    expect(utf8Length('abc')).toBe(3);
    expect(utf8Length('é')).toBe(2);
    expect(utf8Length('回線')).toBe(6);
    expect(utf8Length('😀')).toBe(4);
  });
});

describe('eqIgnoreAsciiCase', () => {
  it('folds A-Z and nothing else', () => {
    expect(eqIgnoreAsciiCase('Yagra.Example.NET', 'yagra.example.net')).toBe(true);
    expect(eqIgnoreAsciiCase('2001:DB8::10', '2001:db8::10')).toBe(true);
    expect(eqIgnoreAsciiCase('a', 'b')).toBe(false);
  });

  it('answers false where toLowerCase would answer true', () => {
    // Each of these is a value core would refuse and a screen comparing with `toLowerCase` would
    // report as accepted -- the permissive direction, which is the one that sends someone to a site.
    expect('\u212a'.toLowerCase()).toBe('k');
    expect(eqIgnoreAsciiCase('\u212a', 'k')).toBe(false);
    expect(eqIgnoreAsciiCase('\u0130', 'i')).toBe(false);
    expect(eqIgnoreAsciiCase('\u03a3', '\u03c3')).toBe(false);
    expect(eqIgnoreAsciiCase('sw-\u00c9', 'sw-\u00e9')).toBe(false);
  });
});
