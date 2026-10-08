// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import { secretMode, secretToSend, staysReplacing } from './secretField';

describe('secretMode', () => {
  it('is a plain box when nothing is stored, whatever else is asked', () => {
    expect(secretMode(false, false)).toEqual({ kind: 'new' });
    expect(secretMode(false, true)).toEqual({ kind: 'new' });
    expect(secretMode(false, false, true)).toEqual({ kind: 'new' });
  });

  it('shows the stored mark until Replace is pressed, then a box that can go back', () => {
    expect(secretMode(true, false)).toEqual({ kind: 'stored' });
    expect(secretMode(true, true)).toEqual({ kind: 'replace', canKeep: true });
  });

  it('opens the box with no way back when the stored value may not be used', () => {
    expect(secretMode(true, false, true)).toEqual({ kind: 'replace', canKeep: false });
    expect(secretMode(true, true, true)).toEqual({ kind: 'replace', canKeep: false });
  });
});

describe('staysReplacing', () => {
  it('latches a box that mustReplace forced open, so a typed value never hides behind the mark', () => {
    expect(staysReplacing(true, false, true)).toBe(true);
    // mustReplace turned off again: the box was already latched, and stays.
    expect(staysReplacing(true, true, false)).toBe(true);
    expect(secretMode(true, staysReplacing(true, true, false), false)).toEqual({
      kind: 'replace',
      canKeep: true,
    });
  });

  it('leaves the stored mark alone when nothing forced or asked for the box', () => {
    expect(staysReplacing(true, false, false)).toBe(false);
  });

  it('has nothing to latch when nothing is stored', () => {
    expect(staysReplacing(false, false, true)).toBe(false);
  });
});

describe('secretToSend', () => {
  it('sends a typed value trimmed', () => {
    expect(secretToSend('  abc \n')).toBe('abc');
  });

  it('keeps the line breaks inside a multi-line secret (a pasted key file)', () => {
    expect(secretToSend('{\n  "k": 1\n}\n')).toBe('{\n  "k": 1\n}');
  });

  it('sends nothing for an empty or blank box, so the stored value is kept', () => {
    expect(secretToSend('')).toBeUndefined();
    expect(secretToSend('   ')).toBeUndefined();
  });
});
