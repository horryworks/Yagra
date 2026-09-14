// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import { memberFetchState, membersTrailer } from './groupMembers';

describe('memberFetchState', () => {
  it('is loading until the folder is in the loaded set', () => {
    expect(memberFetchState('g1', new Set(), new Set())).toBe('loading');
    expect(memberFetchState('g1', new Set(['g2']), new Set())).toBe('loading');
  });

  it('is loaded once the folder is in the loaded set', () => {
    expect(memberFetchState('g1', new Set(['g1']), new Set())).toBe('loaded');
  });

  it('reports a failure ahead of anything else', () => {
    expect(memberFetchState('g1', new Set(), new Set(['g1']))).toBe('failed');
    expect(memberFetchState('g1', new Set(['g1']), new Set(['g1']))).toBe('failed');
  });
});

describe('membersTrailer', () => {
  it('never calls a folder empty while its nodes are still arriving', () => {
    expect(membersTrailer('loading', 0)).toBe('loading');
  });

  it('never calls a folder empty when its nodes could not be fetched', () => {
    expect(membersTrailer('failed', 0)).toBe('failed');
  });

  it('keeps saying the nodes are pending under subfolder rows already drawn', () => {
    expect(membersTrailer('loading', 3)).toBe('loading');
    expect(membersTrailer('failed', 3)).toBe('failed');
  });

  it('calls a folder empty only after a finished fetch found nothing', () => {
    expect(membersTrailer('loaded', 0)).toBe('empty');
    expect(membersTrailer('loaded', 1)).toBeNull();
  });
});
