// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import { trimLikeCore, utf8Length } from './coreText';

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
