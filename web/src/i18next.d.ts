// SPDX-License-Identifier: AGPL-3.0-only
// react-i18next type augmentation.
//
// We deliberately do NOT constrain `t()` to a literal union of keys. The nav IA (`nav.ts`) and the
// format helpers store/compute translation keys as plain strings resolved at render, and several
// call sites build keys dynamically (e.g. `state.${state}`, `nav:${key}`) — all fundamentally
// incompatible with literal-key typing. Key correctness is enforced instead at the data layer: the
// EN/JA parity check (`npm run i18n:check`) + the i18n resolution tests, backed by i18next's runtime
// `fallbackLng: 'en'` so a missing/typo'd key never renders blank.
//
// We only pin `defaultNS` so an un-namespaced `t('loading')` resolves against `common`.

import 'i18next';

declare module 'i18next' {
  interface CustomTypeOptions {
    defaultNS: 'common';
  }
}
