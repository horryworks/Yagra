// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import { intlLocale } from './locale';

describe('intlLocale', () => {
  it('maps interface languages to BCP-47 locales', () => {
    expect(intlLocale('en')).toBe('en-US');
    expect(intlLocale('ja')).toBe('ja-JP');
  });

  it('falls back to en-US for an unknown language', () => {
    expect(intlLocale('xx')).toBe('en-US');
  });
});
