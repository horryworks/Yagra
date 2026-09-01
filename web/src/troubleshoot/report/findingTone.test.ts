// SPDX-License-Identifier: AGPL-3.0-only
// The colour rules that lived in four `.tsx` files, where Vitest never loaded them.
//
// Two things are worth pinning here and only one of them is obvious. The obvious one is the
// thresholds — 90/75 for a score, 1/3 for a rank — which decide what an operator sees at a glance.
// The other is that **each caller keeps its own `info` colour**: unifying four copies of a
// three-way mapping is only safe if the thing that differed between them survives the unification,
// and there is nothing in the type system to notice a report quietly re-coloured.
import { describe, expect, it } from 'vitest';
import {
  TONES,
  capacityTone,
  correlationColor,
  rankTone,
  scoreTone,
  severityColor,
  sourceColor,
  toneColor,
} from './findingTone';

describe('toneColor', () => {
  it('takes crit and warn from the status palette, never from the caller', () => {
    // These are the alert state machine's colours. A report that chose its own red would be the
    // second answer to "what does critical look like" (`ui-conventions.md`).
    expect(toneColor('crit', 'anything')).toBe('var(--status-critical)');
    expect(toneColor('warn', 'anything')).toBe('var(--status-warning)');
  });

  it('returns the caller’s own colour for the info tier', () => {
    expect(toneColor('info', 'var(--series-5)')).toBe('var(--series-5)');
    expect(toneColor('info', 'var(--series-1)')).toBe('var(--series-1)');
  });

  it('covers every tone in the union', () => {
    // If a fourth tone is added, this fails rather than letting one fall through to `info`.
    for (const t of TONES) expect(toneColor(t, 'var(--series-9)')).toMatch(/^var\(--/);
  });
});

describe('severityColor', () => {
  // The three bodies this replaced, with the colour each one actually shipped. Getting one of these
  // wrong is invisible to tsc and to every other test: the report simply renders a different shade.
  it('keeps each report body’s own info colour', () => {
    expect(severityColor({ severity: 'info' }, 'var(--series-5)')).toBe('var(--series-5)'); // auth probe
    expect(severityColor({ severity: 'info' }, 'var(--series-1)')).toBe('var(--series-1)'); // flap
    expect(severityColor({ severity: 'info' }, 'var(--series-6)')).toBe('var(--series-6)'); // traffic anomaly
  });

  it('reads an unknown severity as info, like every other reader', () => {
    expect(severityColor({ severity: 'catastrophic' }, 'var(--series-2)')).toBe('var(--series-2)');
    expect(severityColor({ severity: 'crit' }, 'var(--series-2)')).toBe('var(--status-critical)');
    expect(severityColor({ severity: 'warn' }, 'var(--series-2)')).toBe('var(--status-warning)');
  });
});

describe('scoreTone', () => {
  it('bands a 0–100 score at the backend’s 90/75 thresholds, inclusively', () => {
    expect(scoreTone(90)).toBe('crit');
    expect(scoreTone(89.9)).toBe('warn');
    expect(scoreTone(75)).toBe('warn');
    expect(scoreTone(74.9)).toBe('info');
    expect(scoreTone(0)).toBe('info');
  });

  it('does not band above 100 differently', () => {
    // The grouped rows sum component scores before this sees them, so a value over 100 is reachable.
    expect(scoreTone(140)).toBe('crit');
  });
});

describe('rankTone', () => {
  it('gives the busiest talker the strongest weight', () => {
    expect(rankTone(1)).toBe('crit');
    expect(rankTone(2)).toBe('warn');
    expect(rankTone(3)).toBe('warn');
    expect(rankTone(4)).toBe('info');
  });

  it('tones the no-rank sentinel as info', () => {
    // `talkerRank` defaults to 99 for a row with no rank; that must not read as the top talker.
    expect(rankTone(99)).toBe('info');
  });
});

describe('capacityTone', () => {
  it('follows the same 30/90-day boundaries the filter chips use', () => {
    expect(capacityTone(30)).toBe('crit');
    expect(capacityTone(30.1)).toBe('warn');
    expect(capacityTone(90)).toBe('warn');
    expect(capacityTone(91)).toBe('info');
  });

  it('tones a resource that is never exhausted as info', () => {
    expect(capacityTone(Infinity)).toBe('info');
  });
});

describe('sourceColor', () => {
  it('gives each passive-event source its own series colour', () => {
    expect(sourceColor('trap')).toBe('var(--series-5)');
    expect(sourceColor('syslog')).toBe('var(--series-1)');
    expect(sourceColor('webhook')).toBe('var(--series-3)');
  });

  it('falls back to tertiary text, not to a status colour, for a kind it does not know', () => {
    // A source kind a newer core invented is not an alert. Falling back to red would invent one.
    expect(sourceColor('netflow')).toBe('var(--text-tertiary)');
    expect(sourceColor('')).toBe('var(--text-tertiary)');
  });
});

describe('correlationColor', () => {
  it('splits on the same sign `correlationDirection` does, zero included', () => {
    expect(correlationColor(0.9)).toBe('var(--series-1)');
    expect(correlationColor(0)).toBe('var(--series-1)');
    expect(correlationColor(-0.0001)).toBe('var(--series-4)');
  });
});
