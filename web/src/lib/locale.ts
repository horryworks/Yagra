// Maps the app's interface language to a BCP-47 locale for Intl-based formatting (dates, numbers).
// Kept tiny and dependency-free so both format.ts (via the global i18next language) and any
// component can derive the same locale. Add a case here when a new language is added.

import type { Language } from '../prefs';

/** BCP-47 locale for a given interface language. */
export function intlLocale(language: string): string {
  switch (language as Language) {
    case 'ja':
      return 'ja-JP';
    case 'en':
    default:
      return 'en-US';
  }
}
