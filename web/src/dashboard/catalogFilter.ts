// SPDX-License-Identifier: AGPL-3.0-only
// Searching the add-widget catalog.
//
// Forty-six widgets across nine sections, and until now the only way to find one was to read all of
// them. In a `.ts` because Vitest never executes a `.tsx` (testing.md).
//
// ⚠️ **The search is over the *rendered* words, not the registry's keys.** A widget's `title` and
// `blurb` are i18n keys (`widgets.alertVolume.title`), so matching them raw would mean an operator
// typing what they can see finds nothing while typing a key they have never seen works — and it
// would only ever match English-shaped keys, on a UI that ships in two languages. The caller passes
// its `t`, so the term is compared against exactly what is on the card.

import { textMatch } from '../lib/filterQuery';

/** The two localizable fields a catalog card shows. Structural, so this needs neither the registry
 *  nor its `WidgetType` union to be testable. */
export interface CatalogEntry {
  title: string;
  blurb: string;
}

/** A section with its widgets, as `catalogBySection()` returns it. */
export interface CatalogSection<T extends CatalogEntry> {
  section: string;
  widgets: T[];
}

/**
 * The catalog narrowed to a search term, with empty sections dropped.
 *
 * `translate` resolves a key to the visible string. Sections are dropped rather than shown empty:
 * a heading with nothing under it reads as "this section has no widgets", which is false.
 */
export function filterCatalog<T extends CatalogEntry>(
  sections: readonly CatalogSection<T>[],
  term: string,
  translate: (key: string) => string,
): CatalogSection<T>[] {
  if (term.trim() === '') return sections as CatalogSection<T>[];
  return sections
    .map((s) => ({
      section: s.section,
      // The section heading is searchable too — "flow" should find the Flow section's widgets even
      // where the widget's own words never say it.
      widgets: s.widgets.filter((w) =>
        textMatch(term, translate(w.title), translate(w.blurb), translate(s.section)),
      ),
    }))
    .filter((s) => s.widgets.length > 0);
}

/** How many widgets a (possibly filtered) catalog holds — for the "N of M" count. */
export function countWidgets<T extends CatalogEntry>(
  sections: readonly CatalogSection<T>[],
): number {
  return sections.reduce((n, s) => n + s.widgets.length, 0);
}
