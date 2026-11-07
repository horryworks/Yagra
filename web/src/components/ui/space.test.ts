// SPDX-License-Identifier: AGPL-3.0-only
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { describe, expect, it } from 'vitest';
import { SRC } from '../../testSupport/sources';
import { SPACE, cx, isShown, spaceVar } from './space';

describe('the spacing scale the layout parts take (ADR-246)', () => {
  it('is exactly the scale tokens.css declares', () => {
    // Two spellings of one scale; a step in one and not the other would be a part asking for a
    // token that does not exist, which CSS resolves to nothing at all - silently.
    const tokens = readFileSync(join(SRC, 'styles', 'tokens.css'), 'utf8');
    const declared = Object.fromEntries(
      [...tokens.matchAll(/(--space-[\d-]+):\s*(\d+)px;/g)].map(([, name, px]) => [px, name]),
    );
    expect(declared).toEqual(Object.fromEntries(Object.entries(SPACE)));
  });

  it('turns a step into its token, and joins only the class names that are set', () => {
    expect(spaceVar(6)).toBe('var(--space-1-5)');
    expect(spaceVar(32)).toBe('var(--space-6)');
    expect(cx('a', false, undefined, 'b', null, '')).toBe('a b');
  });

  it('draws an optional slot only when it holds something', () => {
    for (const empty of [undefined, null, false, true, '']) expect(isShown(empty)).toBe(false);
    for (const full of [0, 'x', ['a']]) expect(isShown(full)).toBe(true);
  });
});
