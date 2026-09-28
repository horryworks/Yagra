// SPDX-License-Identifier: AGPL-3.0-only
// The one mapping from a passive event's kind and action to a colour (ADR-184 increment 7).
//
// Before this module a trap was `--series-2` on the dashboard and `--series-5` in Troubleshoot,
// because the mapping was written out in three files. The first half pins the values; the second
// refuses a fourth copy, which is how the first three appeared.
import { readFileSync } from 'node:fs';
import { sourceFiles as walkSources } from '../../testSupport/sources';
import { join } from 'node:path';
import { describe, expect, it } from 'vitest';
import { EVENT_ACTIONS, EVENT_KINDS } from '../../types/api';
import {
  ACTION_COLOR,
  ACTION_TONE,
  KIND_COLOR,
  actionColorOf,
  actionToneOf,
  kindColorOf,
} from './eventTone';

describe('event tone tables', () => {
  it('has an entry for every kind and every action, and nothing else', () => {
    // `Record<Union, …>` already makes a missing member a compile error. This catches the other
    // direction: a stale key left behind after a member is removed from the union.
    expect(Object.keys(KIND_COLOR).sort()).toEqual([...EVENT_KINDS].sort());
    expect(Object.keys(ACTION_COLOR).sort()).toEqual([...EVENT_ACTIONS].sort());
    expect(Object.keys(ACTION_TONE).sort()).toEqual([...EVENT_ACTIONS].sort());
  });

  it('draws a trap in series-5, the colour Troubleshoot always used', () => {
    expect(KIND_COLOR.trap).toBe('var(--series-5)');
    expect(KIND_COLOR.syslog).toBe('var(--series-1)');
    expect(KIND_COLOR.webhook).toBe('var(--series-3)');
  });

  it('gives each kind its own colour, and none of them a status colour', () => {
    // A kind is a category, not a verdict: a trap is not worse than a syslog line.
    const colours = Object.values(KIND_COLOR);
    expect(new Set(colours).size).toBe(colours.length);
    for (const c of colours) expect(c).toMatch(/^var\(--series-\d+\)$/);
  });

  it('puts the alert lifecycle on the status channel', () => {
    expect(ACTION_TONE.fired).toBe('critical');
    expect(ACTION_TONE.refreshed).toBe('warning');
    expect(ACTION_TONE.cleared).toBe('up');
    expect(ACTION_COLOR.fired).toBe('var(--status-critical)');
    expect(ACTION_COLOR.refreshed).toBe('var(--status-warning)');
    expect(ACTION_COLOR.cleared).toBe('var(--status-ok)');
  });

  it('keeps the other actions off the status channel', () => {
    // `none` / `suppressed` / `info` raised nothing; a red or an amber would claim they did.
    for (const a of ['none', 'suppressed', 'info'] as const) {
      expect(ACTION_COLOR[a]).toMatch(/^var\(--series-\d+\)$/);
      expect(['critical', 'warning', 'up']).not.toContain(ACTION_TONE[a]);
    }
  });

  it('answers undefined for a key this build does not know, so each caller keeps its fallback', () => {
    expect(kindColorOf('trap')).toBe('var(--series-5)');
    expect(kindColorOf('netflow')).toBeUndefined();
    expect(actionColorOf('fired')).toBe('var(--status-critical)');
    expect(actionToneOf('cleared')).toBe('up');
    expect(actionColorOf('escalated')).toBeUndefined();
    expect(actionToneOf('escalated')).toBeUndefined();
    // An index into a plain object would hand these back as functions.
    for (const k of ['constructor', 'toString', '__proto__']) {
      expect(kindColorOf(k)).toBeUndefined();
      expect(actionColorOf(k)).toBeUndefined();
      expect(actionToneOf(k)).toBeUndefined();
    }
  });
});

/**
 * The guard. A second kind→colour or action→tone table is how a trap came to be two colours, and a
 * hand-written crit/warn ternary is how three report bodies kept their own copy of `toneColor`.
 *
 * ⚠️ **The needles are assembled at runtime** from `EVENT_KINDS` / `EVENT_ACTIONS`, so no line of
 * this file can match them, and a kind added to the union is searched for without an edit here.
 */
describe('no other file re-declares the event or severity colour mapping', () => {
  const SRC = join(__dirname, '..', '..');
  const HOME = join(__dirname, 'eventTone.ts');
  const FINDING_TONE = join(SRC, 'troubleshoot', 'report', 'findingTone.ts');

  const sourceFiles = (dir: string) => walkSources(dir);

  const alt = (xs: readonly string[]) => xs.join('|');
  // `trap: 'var(--series-5)'` — a kind keyed to a CSS colour.
  const KIND_ENTRY = new RegExp(`^\\s*['"]?(?:${alt(EVENT_KINDS)})['"]?\\s*:\\s*['"]var\\(--`, 'm');
  // `kind === 'trap' ? 'var(--…)'` — the same table written as a ternary.
  const KIND_TERNARY = new RegExp(`===\\s*['"](?:${alt(EVENT_KINDS)})['"]\\s*\\?\\s*['"]var\\(--`);
  // `fired: 'critical'` or `fired: 'var(--status-critical)'` — a lifecycle action keyed to a tone
  // or a colour. Only the three lifecycle actions: `info: 'var(--…)'` is also the shape of any
  // severity table, and would be a false hit.
  const LIFECYCLE = EVENT_ACTIONS.filter((a) => a === 'fired' || a === 'refreshed' || a === 'cleared');
  const ACTION_ENTRY = new RegExp(
    `^\\s*['"]?(?:${alt(LIFECYCLE)})['"]?\\s*:\\s*['"](?:critical|warning|up|var\\(--)`,
    'm',
  );
  // `sevOf(f) === 'crit' ? 'var(--status-critical)'`, or a `crit:` entry in a table — the
  // hand-written severity colour that `toneColor` / `severityColor` replace.
  const CRIT = ['c', 'rit'].join('');
  const SEV_TERNARY = new RegExp(`===\\s*['"]${CRIT}['"]\\s*\\?\\s*['"]var\\(--status-`);
  const SEV_ENTRY = new RegExp(`^\\s*['"]?${CRIT}['"]?\\s*:\\s*['"]var\\(--status-`, 'm');
  const NEEDLES = [KIND_ENTRY, KIND_TERNARY, ACTION_ENTRY, SEV_TERNARY, SEV_ENTRY];

  it('every event colour comes from eventTone.ts and every severity colour from toneColor', () => {
    const offenders = sourceFiles(SRC)
      .filter((p) => p !== HOME && p !== FINDING_TONE)
      .map((p) => [p, readFileSync(p, 'utf8')] as const)
      .filter(([, src]) => NEEDLES.some((n) => n.test(src)))
      .map(([p]) => p.slice(SRC.length + 1).replace(/\\/g, '/'));

    expect(
      offenders,
      `these files map an event kind, an event action or a finding severity to a colour by hand, ` +
        `which is how a trap came to be two colours (ADR-184 increment 7). Use KIND_COLOR / ` +
        `ACTION_COLOR / ACTION_TONE from components/EventLog/eventTone.ts, or toneColor / ` +
        `severityColor from troubleshoot/report/findingTone.ts:\n  ${offenders.join('\n  ')}`,
    ).toEqual([]);
  });

  it('finds the sources it is supposed to be reading', () => {
    // A broken path would turn the check above into one that scans nothing and passes.
    const all = sourceFiles(SRC);
    expect(all.length).toBeGreaterThan(300);
    expect(all).toContain(HOME);
    expect(all).toContain(FINDING_TONE);
    // The needles have to match the homes they protect, or they would match nothing anywhere.
    const home = readFileSync(HOME, 'utf8');
    expect(KIND_ENTRY.test(home)).toBe(true);
    expect(ACTION_ENTRY.test(home)).toBe(true);
    expect(readFileSync(FINDING_TONE, 'utf8')).toContain('export function toneColor');
    // …and the mapping has to be in use, not merely declared.
    const importers = all.filter((p) =>
      /from '[./]*(?:components\/EventLog\/)?eventTone'/.test(readFileSync(p, 'utf8')),
    );
    expect(importers.length).toBeGreaterThanOrEqual(3);
  });
});
