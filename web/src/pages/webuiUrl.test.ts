// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import { isWebuiUrlDirty, webuiUrlBody, webuiUrlDraft, webuiUrlPlaceholder } from './webuiUrl';

describe('the WebUI address field (ADR-260)', () => {
  it('shows nothing when no address is set', () => {
    expect(webuiUrlDraft(null)).toBe('');
    expect(webuiUrlDraft({ url: null })).toBe('');
    expect(webuiUrlDraft({ url: 'https://yagra.example.com' })).toBe('https://yagra.example.com');
  });

  it('sends null for an empty field, so Save clears the setting', () => {
    expect(webuiUrlBody('')).toEqual({ url: null });
    expect(webuiUrlBody('   ')).toEqual({ url: null });
    expect(webuiUrlBody(' https://yagra.example.com ')).toEqual({ url: 'https://yagra.example.com' });
  });

  it('is dirty only when the trimmed value differs from what is stored', () => {
    const stored = { url: 'https://yagra.example.com' };
    expect(isWebuiUrlDirty(stored, 'https://yagra.example.com ')).toBe(false);
    expect(isWebuiUrlDirty(stored, '')).toBe(true);
    expect(isWebuiUrlDirty({ url: null }, '')).toBe(false);
    expect(isWebuiUrlDirty(null, 'https://a.example')).toBe(true);
  });

  it('offers the address the page was opened at as the example', () => {
    expect(webuiUrlPlaceholder('https://192.0.2.10')).toBe('https://192.0.2.10');
    expect(webuiUrlPlaceholder('https://yagra.example.com/')).toBe('https://yagra.example.com');
  });
});
