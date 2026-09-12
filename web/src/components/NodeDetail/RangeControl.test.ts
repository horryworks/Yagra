// SPDX-License-Identifier: AGPL-3.0-only
// Pure-helper unit tests for RangeControl. Vitest runs in a node env (no DOM), so we cover only the
// exported pure helpers, not the component render. Importing the component module is safe: it merely
// defines functions/JSX (nothing renders here) and the CSS import is a no-op under Vitest.
//
// Every assertion is timezone-agnostic: the local-time helpers are exercised via round-trips and via
// inputs constructed with the same local Date fields they format back to.

import { afterEach, describe, expect, it, vi } from 'vitest';
import { readdirSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import {
  formatCompactRange,
  localInputToIso,
  localInputToUnix,
  pad2,
  rangeInputsValid,
  resolveRange,
  unixToLocalInput,
} from './RangeControl';

describe('resolveRange', () => {
  afterEach(() => {
    vi.useRealTimers();
  });

  it('returns absolute bounds unchanged', () => {
    expect(resolveRange({ kind: 'absolute', from: 100, to: 200 })).toEqual({ from: 100, to: 200 });
  });

  it('resolves a relative range against the current time', () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date('2026-06-17T12:00:00Z'));
    const now = Math.floor(Date.now() / 1000);
    expect(resolveRange({ kind: 'relative', secs: 3600 })).toEqual({ from: now - 3600, to: now });
  });
});

describe('datetime-local conversion', () => {
  it('round-trips unix → input → unix at minute resolution', () => {
    for (const base of [0, 1_700_000_000, 1_750_000_000]) {
      const s = base - (base % 60); // datetime-local has no seconds; align first
      expect(localInputToUnix(unixToLocalInput(s))).toBe(s);
    }
  });

  it('produces a YYYY-MM-DDTHH:MM shaped string', () => {
    expect(unixToLocalInput(1_750_000_000)).toMatch(/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}$/);
  });

  it('returns null for empty or unparseable input', () => {
    expect(localInputToUnix('')).toBeNull();
    expect(localInputToUnix('not-a-date')).toBeNull();
  });
});

describe('formatCompactRange', () => {
  it('formats local M/D HH:MM–M/D HH:MM with an en-dash and zero-padded time', () => {
    const from = Math.floor(new Date(2026, 5, 12, 9, 5).getTime() / 1000); // Jun 12 09:05 local
    const to = Math.floor(new Date(2026, 5, 17, 18, 30).getTime() / 1000); // Jun 17 18:30 local
    expect(formatCompactRange(from, to)).toBe('6/12 09:05–6/17 18:30');
  });
});

describe('rangeInputsValid', () => {
  it('is true only when both parse and from < to', () => {
    expect(rangeInputsValid('2026-06-12T12:00', '2026-06-17T09:30')).toBe(true);
  });

  it('is false for empty, unparseable, equal, or inverted inputs', () => {
    expect(rangeInputsValid('', '2026-06-17T09:30')).toBe(false);
    expect(rangeInputsValid('2026-06-12T12:00', '')).toBe(false);
    expect(rangeInputsValid('garbage', '2026-06-17T09:30')).toBe(false);
    expect(rangeInputsValid('2026-06-17T09:30', '2026-06-17T09:30')).toBe(false);
    expect(rangeInputsValid('2026-06-18T09:30', '2026-06-17T09:30')).toBe(false);
  });
});

// The `range URL params` suite is gone with the helpers it covered: the active window lives in
// `useRangeStore` (sessionStorage), not the query string. See the note in RangeControl.tsx.

describe('localInputToIso', () => {
  it('renders a parseable local input as RFC 3339 UTC', () => {
    const iso = localInputToIso('2024-03-01T12:30');
    expect(iso).toBeDefined();
    // Same instant as the seconds conversion its sibling produces — one parse rule, two shapes.
    expect(iso).toBe(new Date((localInputToUnix('2024-03-01T12:30') as number) * 1000).toISOString());
    expect(iso).toMatch(/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{3}Z$/);
  });

  it('is undefined for an empty or unparseable input, so the filter is simply omitted', () => {
    // `undefined` (not an empty string) is what lets the caller drop the bound from the query
    // rather than sending a malformed one.
    expect(localInputToIso('')).toBeUndefined();
    expect(localInputToIso('not a date')).toBeUndefined();
  });

  it('round-trips with the local-input formatter', () => {
    const secs = Math.floor(Date.UTC(2024, 5, 15, 8, 45) / 1000);
    const iso = localInputToIso(unixToLocalInput(secs));
    expect(iso).toBe(new Date(secs * 1000).toISOString());
  });
});

describe('pad2', () => {
  it('pads to two digits and leaves longer values alone', () => {
    expect(pad2(0)).toBe('00');
    expect(pad2(9)).toBe('09');
    expect(pad2(23)).toBe('23');
    expect(pad2(100)).toBe('100');
  });
});

// 🚨 Every pane that draws a range picker reads the SHARED window (ADR-134 決定 6).
//
// The defect this exists for: `RangeControl.tsx` says in its own comment that the window "is shared
// across the Overview / Interfaces / Flow / DNS panes", `store.ts` names the same set — and `FlowTab`
// held its own `useState<Range>(DEFAULT_RANGE)`. Picking 24h on Overview and opening Flow snapped
// silently back to 1h. **Two doc comments asserted it and nothing checked it.**
//
// Read as source text rather than by rendering: Vitest runs no `.tsx`, so the only way to ask "does
// this component read the store" from a test is to read the file. What that cannot tell is whether
// the value is then *used* — the panes' `resolveRange(range)` calls are covered by the browser walk.
describe('every range picker reads the shared window', () => {
  const DIR = join(__dirname);
  const panes = readdirSync(DIR)
    .filter((f) => f.endsWith('.tsx') && !f.includes('.test.'))
    .map((f) => ({ file: f, src: readFileSync(join(DIR, f), 'utf8') }))
    // The control's own module defines it; every *other* file that mentions it is a consumer.
    .filter((p) => p.file !== 'RangeControl.tsx' && p.src.includes('<RangeControl'));

  it('inspected the panes this product actually has', () => {
    // A floor on what was **inspected**, not on what passed: a detector that stopped matching
    // would otherwise report "no offenders" and be indistinguishable from a clean tree
    // (`floor-must-count-what-was-checked`). Four panes draw one today — Overview, Interfaces,
    // Collection and Flow — and DnsHealth renders inside Overview.
    expect(panes.map((p) => p.file).sort()).toContain('FlowTab.tsx');
    expect(panes.length).toBeGreaterThanOrEqual(4);
  });

  it('reads useRangeStore rather than holding a local Range', () => {
    const offenders = panes.filter((p) => !p.src.includes('useRangeStore'));
    expect(
      offenders.map((p) => p.file),
      'a pane draws a range picker over its own state, so the window it shows disagrees with every other pane',
    ).toEqual([]);
  });

  it('leaves no pane seeding its own state from DEFAULT_RANGE', () => {
    // The exact shape the defect took. `DEFAULT_RANGE` is the store's initial value and belongs to
    // `store.ts`; a pane naming it is a pane that has its own idea of "the window".
    const offenders = panes.filter((p) => p.src.includes('DEFAULT_RANGE'));
    expect(offenders.map((p) => p.file)).toEqual([]);
  });
});
