// SPDX-License-Identifier: AGPL-3.0-only
import { afterEach, describe, expect, it } from 'vitest';
import { MOBILE_BP, applyViewportMode, resolveViewportMode } from './viewport';

describe('resolveViewportMode', () => {
  it('auto follows the viewport: narrow ⇒ mobile, wide ⇒ desktop', () => {
    expect(resolveViewportMode('auto', true)).toBe('mobile');
    expect(resolveViewportMode('auto', false)).toBe('desktop');
  });

  it("the 'desktop' override forces desktop even on a narrow screen (there is no force-mobile)", () => {
    expect(resolveViewportMode('desktop', true)).toBe('desktop');
    expect(resolveViewportMode('desktop', false)).toBe('desktop');
  });
});

describe('MOBILE_BP', () => {
  it('is the documented canonical 768 boundary (single TS source; CSS mirrors the constant)', () => {
    expect(MOBILE_BP).toBe(768);
  });
});

describe('applyViewportMode', () => {
  const g = globalThis as unknown as { document?: unknown };
  afterEach(() => {
    delete g.document;
  });

  it('stamps <html data-viewport> so mode-dependent CSS applies (mirrors applyTheme)', () => {
    let attr: [string, string] | null = null;
    g.document = {
      documentElement: {
        setAttribute: (k: string, v: string) => {
          attr = [k, v];
        },
      },
    };
    applyViewportMode('mobile');
    expect(attr).toEqual(['data-viewport', 'mobile']);
    applyViewportMode('desktop');
    expect(attr).toEqual(['data-viewport', 'desktop']);
  });

  it('is a no-op (no throw) without a document — SSR / the Vitest node env', () => {
    delete g.document;
    expect(() => applyViewportMode('desktop')).not.toThrow();
  });
});
