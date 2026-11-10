// SPDX-License-Identifier: AGPL-3.0-only
import { afterEach, describe, expect, it } from 'vitest';
import i18n from '../i18n';
import { localTimeLine } from './localTime';
import { setDisplayTimeZone } from './timeZone';

const t = i18n.getFixedT('en', 'nodes');
const at = (iso: string) => Date.parse(iso);
// Monday 2026-11-02 09:00 in Tokyo.
const MON_9_TOKYO = at('2026-11-02T00:00:00Z');

afterEach(() => setDisplayTimeZone(null));

describe("a site's local time on the sub line (ADR-252)", () => {
  it('shows no clock when no folder names a zone, and none for a zone this browser lacks', () => {
    expect(localTimeLine(null, null, MON_9_TOKYO, t, 'en').clock).toBeNull();
    expect(localTimeLine('Mars/Olympus_Mons', null, MON_9_TOKYO, t, 'en').clock).toBeNull();
  });

  it('shows the clock alone when no schedule reaches the place', () => {
    const line = localTimeLine('Asia/Tokyo', null, MON_9_TOKYO, t, 'en');
    expect(line.clock).toEqual({ time: '09:00', zone: 'Asia/Tokyo' });
    expect(line.delivery).toBeNull();
    expect(line.stale).toBe(false);
  });

  it('says until when delivery runs, in the site zone, with no weekday on the same day', () => {
    const line = localTimeLine(
      'Asia/Tokyo',
      { open: true, closes_at: '2026-11-02T08:20:00Z', opens_at: null },
      MON_9_TOKYO,
      t,
      'en',
    );
    expect(line.delivery).toEqual({ open: true, text: 'Delivering until 17:20' });
  });

  it('names the weekday when the hours open on another day', () => {
    // Friday 20:00 Tokyo, held until Monday 08:30 Tokyo.
    const line = localTimeLine(
      'Asia/Tokyo',
      { open: false, closes_at: null, opens_at: '2026-11-08T23:30:00Z' },
      at('2026-11-06T11:00:00Z'),
      t,
      'en',
    );
    expect(line.delivery).toEqual({ open: false, text: 'Held until Mon 08:30' });
  });

  it('says only "Delivering" when the hours never close within the look-ahead', () => {
    const line = localTimeLine('Asia/Tokyo', { open: true }, MON_9_TOKYO, t, 'en');
    expect(line.delivery?.text).toBe('Delivering');
  });

  it('reads the boundary in the display zone when the site has none', () => {
    setDisplayTimeZone('UTC');
    const line = localTimeLine(
      null,
      { open: true, closes_at: '2026-11-02T08:20:00Z' },
      MON_9_TOKYO,
      t,
      'en',
    );
    expect(line.clock).toBeNull();
    expect(line.delivery?.text).toBe('Delivering until 08:20');
  });

  it('hides an answer whose boundary has passed and asks for a fresh one', () => {
    const line = localTimeLine(
      'Asia/Tokyo',
      { open: true, closes_at: '2026-11-02T08:20:00Z' },
      at('2026-11-02T08:20:00Z'),
      t,
      'en',
    );
    expect(line.delivery).toBeNull();
    expect(line.stale).toBe(true);
    expect(line.clock?.time).toBe('17:20');
  });

  it('writes the Japanese forms', async () => {
    await i18n.changeLanguage('ja');
    try {
      const line = localTimeLine(
        'Asia/Tokyo',
        { open: false, opens_at: '2026-11-08T23:30:00Z' },
        at('2026-11-06T11:00:00Z'),
        i18n.getFixedT('ja', 'nodes'),
        'ja',
      );
      expect(line.delivery?.text).toBe('月 08:30 まで保留');
    } finally {
      await i18n.changeLanguage('en');
    }
  });
});
