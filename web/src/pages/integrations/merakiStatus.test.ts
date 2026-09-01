// SPDX-License-Identifier: AGPL-3.0-only
// The integrations catalogue's Meraki tile. Both functions lived in the `.tsx`, where nothing ran
// them — including the exhaustive `switch`, whose whole value is that it has no `default`.
import { describe, expect, it } from 'vitest';
import type { TFunction } from 'i18next';
import { merakiStatusLabel, merakiStatusTone, type MerakiStatus } from './merakiStatus';

/** i18n stand-in: the key, plus its interpolations when there are any. */
const t = ((key: string, opts?: Record<string, unknown>) =>
  opts && Object.keys(opts).length ? `${key}(${JSON.stringify(opts)})` : key) as unknown as TFunction;

const ALL: MerakiStatus[] = [
  { kind: 'loading' },
  { kind: 'unavailable' },
  { kind: 'forbidden' },
  { kind: 'not-configured' },
  { kind: 'connected', orgs: 2, pollingOn: true },
  { kind: 'connected', orgs: 2, pollingOn: false },
];

describe('merakiStatusLabel', () => {
  it('names each state distinctly', () => {
    expect(merakiStatusLabel({ kind: 'loading' }, t)).toBe('integrations.status.checking');
    expect(merakiStatusLabel({ kind: 'unavailable' }, t)).toBe('integrations.status.unavailable');
    expect(merakiStatusLabel({ kind: 'forbidden' }, t)).toBe('integrations.status.forbidden');
    expect(merakiStatusLabel({ kind: 'not-configured' }, t)).toBe(
      'integrations.status.notConfigured',
    );
  });

  it('keeps "cannot reach it" and "you may not ask" apart', () => {
    // Collapsing these would tell an operator to go and fix a server that is fine.
    expect(merakiStatusLabel({ kind: 'unavailable' }, t)).not.toBe(
      merakiStatusLabel({ kind: 'forbidden' }, t),
    );
  });

  it('carries the org count when connected, and says so when polling is paused', () => {
    expect(merakiStatusLabel({ kind: 'connected', orgs: 3, pollingOn: true }, t)).toBe(
      'integrations.status.connected({"count":3})',
    );
    // Configured but paused is not configured and running.
    expect(merakiStatusLabel({ kind: 'connected', orgs: 3, pollingOn: false }, t)).toBe(
      'integrations.status.pollingPaused',
    );
  });

  it('answers for every state the union can be in', () => {
    // The switch has no `default`, so a new variant is a compile error rather than `undefined` on
    // the tile — this asserts the runtime half of that.
    for (const s of ALL) expect(merakiStatusLabel(s, t)).toMatch(/^integrations\.status\./);
  });
});

describe('merakiStatusTone', () => {
  it('is ok only while it is actually polling', () => {
    expect(merakiStatusTone({ kind: 'connected', orgs: 1, pollingOn: true })).toBe('ok');
    expect(merakiStatusTone({ kind: 'connected', orgs: 1, pollingOn: false })).toBe('paused');
  });

  it('mutes what could not be answered and idles what was never set up', () => {
    expect(merakiStatusTone({ kind: 'unavailable' })).toBe('muted');
    expect(merakiStatusTone({ kind: 'forbidden' })).toBe('muted');
    expect(merakiStatusTone({ kind: 'not-configured' })).toBe('idle');
    expect(merakiStatusTone({ kind: 'loading' })).toBe('idle');
  });

  it('never returns a node-state tone', () => {
    // An integration that cannot be reached is not a node that is down. Colouring it that way puts
    // a red thing on the screen that no alert corresponds to.
    for (const s of ALL) expect(['ok', 'paused', 'muted', 'idle']).toContain(merakiStatusTone(s));
  });
});
