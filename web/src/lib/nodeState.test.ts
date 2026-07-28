// SPDX-License-Identifier: AGPL-3.0-only
import { describe, it, expect } from 'vitest';
import {
  DISPLAY_ORDER,
  PROBLEM_STATES,
  SEVERITY_ORDER,
  emptyStateCounts,
  isNodeState,
} from './nodeState';

// The two orders are the whole point of this module: they must enumerate the same six states, in
// deliberately different sequences. Before this file existed the same lists lived in four places
// under three names; these tests are what stops a new state from being added to one and not the
// other (which previously showed up as a state silently missing from a legend or a roll-up).
describe('node state vocabulary', () => {
  it('both orders enumerate exactly the same states', () => {
    expect([...SEVERITY_ORDER].sort()).toEqual([...DISPLAY_ORDER].sort());
  });

  it('has no duplicate entries in either order', () => {
    expect(new Set(SEVERITY_ORDER).size).toBe(SEVERITY_ORDER.length);
    expect(new Set(DISPLAY_ORDER).size).toBe(DISPLAY_ORDER.length);
  });

  it('orders severity worst-first and display best-first', () => {
    expect(SEVERITY_ORDER[0]).toBe('critical');
    expect(SEVERITY_ORDER.at(-1)).toBe('ok');
    expect(DISPLAY_ORDER[0]).toBe('ok');
  });

  it('treats every problem state as a real state', () => {
    for (const s of PROBLEM_STATES) expect(SEVERITY_ORDER).toContain(s);
    // `ok` and `maintenance` are deliberately NOT "needs attention".
    expect(PROBLEM_STATES.has('ok')).toBe(false);
    expect(PROBLEM_STATES.has('maintenance')).toBe(false);
  });

  it('narrows only known states (guards the SSE payload)', () => {
    for (const s of SEVERITY_ORDER) expect(isNodeState(s)).toBe(true);
    expect(isNodeState('bogus')).toBe(false);
    expect(isNodeState('OK')).toBe(false);
    expect(isNodeState('')).toBe(false);
  });

  it('zero-fills every state so a missing state reads as 0, not undefined', () => {
    const counts = emptyStateCounts();
    expect(Object.keys(counts).sort()).toEqual([...SEVERITY_ORDER].sort());
    for (const s of SEVERITY_ORDER) expect(counts[s]).toBe(0);
  });

  it('returns a fresh object each call (callers mutate it)', () => {
    const a = emptyStateCounts();
    a.ok += 1;
    expect(emptyStateCounts().ok).toBe(0);
  });
});
