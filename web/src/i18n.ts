// SPDX-License-Identifier: AGPL-3.0-only
// i18next bootstrap. Default + fallback language is English, bundled statically so first paint
// never waits on a fetch; other languages (Japanese today) are code-split and lazy-loaded the
// first time the user switches. The active language is owned by the persisted `prefs` store — this
// module only mirrors it into i18next (see App.tsx's language effect + `applyLanguage`).
//
// No `<I18nextProvider>` is needed: react-i18next binds this global instance, so `useTranslation()`
// works app-wide once this module has been imported (side-effect import in main.tsx).

import i18n from 'i18next';
import { initReactI18next } from 'react-i18next';
import resourcesToBackend from 'i18next-resources-to-backend';
import { usePrefsStore } from './prefs';

// English namespaces are imported eagerly so they ship in the main chunk (zero extra fetch on the
// default path). Add one import + `resources.en` entry per namespace as areas are localized.
import enCommon from './locales/en/common.json';
import enNav from './locales/en/nav.json';
import enFormat from './locales/en/format.json';
import enSettings from './locales/en/settings.json';
import enAuth from './locales/en/auth.json';
import enAlerts from './locales/en/alerts.json';
import enNodes from './locales/en/nodes.json';
import enMonitoring from './locales/en/monitoring.json';
import enDashboard from './locales/en/dashboard.json';
import enAlertsConfig from './locales/en/alertsConfig.json';
import enSuppression from './locales/en/suppression.json';
import enTopology from './locales/en/topology.json';
import enReports from './locales/en/reports.json';
import enTroubleshoot from './locales/en/troubleshoot.json';
import enAccess from './locales/en/access.json';
import enSystem from './locales/en/system.json';
import enSettingsAuth from './locales/en/settings-auth.json';
import enSettingsTokens from './locales/en/settings-tokens.json';
import enSettingsForwarding from './locales/en/settings-forwarding.json';
import enSettingsAi from './locales/en/settings-ai.json';
import enSettingsTls from './locales/en/settings-tls.json';
import enSettingsUpgrade from './locales/en/settings-upgrade.json';
import enRca from './locales/en/rca.json';

/** All translation namespaces. Keep in sync with the files under `locales/<lng>/`. */
export const NAMESPACES = [
  'common',
  'nav',
  'format',
  'settings',
  'auth',
  'alerts',
  'nodes',
  'monitoring',
  'dashboard',
  'alertsConfig',
  'suppression',
  'topology',
  'reports',
  'troubleshoot',
  'access',
  'system',
  'settings-auth',
  'settings-tokens',
  'settings-forwarding',
  'settings-ai',
  'settings-tls',
  'settings-upgrade',
  'rca',
] as const;

void i18n
  // Non-English resources are fetched on demand via dynamic import → Vite code-splits each
  // `locales/<lng>/<ns>.json` into its own chunk. i18next only calls this for languages/namespaces
  // absent from `resources` below, i.e. never for English at runtime.
  //
  // Vite emits a build warning that the English JSONs are "both statically and dynamically
  // imported" — expected and harmless: the glob for this dynamic import also matches the `en/`
  // files that `resources` imports eagerly, so Vite keeps English in the main chunk (zero extra
  // fetch on the default path) while still splitting the other languages out. Don't "fix" it by
  // dropping the eager English imports — that would make the default path wait on a fetch.
  .use(resourcesToBackend((lng: string, ns: string) => import(`./locales/${lng}/${ns}.json`)))
  .use(initReactI18next)
  .init({
    lng: usePrefsStore.getState().language, // initial language from the persisted pref
    fallbackLng: 'en', // any missing key (e.g. an untranslated JA string) falls back to English
    defaultNS: 'common',
    ns: NAMESPACES,
    resources: {
      en: {
        common: enCommon,
        nav: enNav,
        format: enFormat,
        settings: enSettings,
        auth: enAuth,
        alerts: enAlerts,
        nodes: enNodes,
        monitoring: enMonitoring,
        dashboard: enDashboard,
        alertsConfig: enAlertsConfig,
        suppression: enSuppression,
        topology: enTopology,
        reports: enReports,
        troubleshoot: enTroubleshoot,
        access: enAccess,
        system: enSystem,
        'settings-auth': enSettingsAuth,
        'settings-tokens': enSettingsTokens,
        'settings-forwarding': enSettingsForwarding,
        'settings-ai': enSettingsAi,
        'settings-tls': enSettingsTls,
        'settings-upgrade': enSettingsUpgrade,
        rca: enRca,
      },
    },
    partialBundledLanguages: true, // mix the eager EN resources above with lazily-loaded languages
    interpolation: { escapeValue: false }, // React already escapes rendered values
    react: { useSuspense: false }, // render immediately (EN fallback) while a language chunk loads
  });

export default i18n;
