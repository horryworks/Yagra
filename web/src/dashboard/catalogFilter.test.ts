// SPDX-License-Identifier: AGPL-3.0-only
// Unit tests for the add-widget catalog search (no DOM — Vitest node env).

import { describe, expect, it } from 'vitest';
import { countWidgets, filterCatalog, type CatalogSection } from './catalogFilter';

/** Keys in, visible words out — the same indirection the real catalog has. */
const STRINGS: Record<string, string> = {
  'sections.alerts': 'Alerts',
  'sections.flow': 'Traffic flow',
  'widgets.alertVolume.title': 'Alert volume',
  'widgets.alertVolume.blurb': 'How many alerts fired per hour',
  'widgets.flapping.title': 'Flapping watchlist',
  'widgets.flapping.blurb': 'Checks that keep changing state',
  'widgets.topTalkers.title': 'Top talkers',
  'widgets.topTalkers.blurb': 'Busiest conversations on the network',
};
const tr = (k: string) => STRINGS[k] ?? k;

const catalog: CatalogSection<{ title: string; blurb: string }>[] = [
  {
    section: 'sections.alerts',
    widgets: [
      { title: 'widgets.alertVolume.title', blurb: 'widgets.alertVolume.blurb' },
      { title: 'widgets.flapping.title', blurb: 'widgets.flapping.blurb' },
    ],
  },
  {
    section: 'sections.flow',
    widgets: [{ title: 'widgets.topTalkers.title', blurb: 'widgets.topTalkers.blurb' }],
  },
];

describe('filterCatalog', () => {
  it('returns the whole catalog when nothing is typed', () => {
    expect(filterCatalog(catalog, '', tr)).toEqual(catalog);
    expect(filterCatalog(catalog, '   ', tr)).toEqual(catalog);
  });

  it('searches the words on the card, not the i18n keys behind them', () => {
    // This is the whole point: matching keys would mean typing what you can see finds nothing,
    // and would only ever work in English.
    expect(countWidgets(filterCatalog(catalog, 'Flapping watchlist', tr))).toBe(1);
    expect(countWidgets(filterCatalog(catalog, 'widgets.flapping', tr))).toBe(0);
  });

  it('searches the blurb as well as the title', () => {
    const hit = filterCatalog(catalog, 'per hour', tr);
    expect(countWidgets(hit)).toBe(1);
    expect(hit[0].widgets[0].title).toBe('widgets.alertVolume.title');
  });

  it('searches the section heading, so a section name finds its widgets', () => {
    const hit = filterCatalog(catalog, 'traffic', tr);
    expect(countWidgets(hit)).toBe(1);
    expect(hit[0].section).toBe('sections.flow');
  });

  it('is case-insensitive', () => {
    expect(countWidgets(filterCatalog(catalog, 'ALERT VOLUME', tr))).toBe(1);
  });

  it('drops sections that match nothing rather than showing an empty heading', () => {
    // A heading with nothing under it reads as "this section has no widgets", which is false.
    const hit = filterCatalog(catalog, 'talkers', tr);
    expect(hit).toHaveLength(1);
    expect(hit[0].section).toBe('sections.flow');
  });

  it('returns nothing at all when the term matches nothing', () => {
    expect(filterCatalog(catalog, 'zzz', tr)).toEqual([]);
    expect(countWidgets(filterCatalog(catalog, 'zzz', tr))).toBe(0);
  });
});

describe('countWidgets', () => {
  it('counts across sections', () => {
    expect(countWidgets(catalog)).toBe(3);
    expect(countWidgets([])).toBe(0);
  });
});
