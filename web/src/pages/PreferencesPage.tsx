// Preferences (Settings ▸ Preferences). Real, client-only: theme (light/dark, required by
// §1.2) and interface language, both persisted via the prefs store. Theme reflects onto
// <html data-theme>; language drives i18next (see App.tsx). No backend needed.

import { useTranslation } from 'react-i18next';
import { usePrefsStore, type Language, type Theme, type UiMode } from '../prefs';
import { PageHeader } from '../components/ui/PageHeader';
import { Card } from '../components/ui/Card';
import './PreferencesPage.css';

// Theme options carry an i18n key (resolved at render); language options show endonyms (native
// names, e.g. "日本語") which are intentionally NOT translated — you pick your language in its own.
const THEMES: { key: Theme; labelKey: 'prefs.themes.light' | 'prefs.themes.dark' }[] = [
  { key: 'light', labelKey: 'prefs.themes.light' },
  { key: 'dark', labelKey: 'prefs.themes.dark' },
];
const LANGUAGES: { key: Language; label: string }[] = [
  { key: 'en', label: 'English' },
  { key: 'ja', label: '日本語' },
];
// Layout mode (ADR-027): 'auto' follows the viewport width; 'desktop' pins the desktop shell on any
// screen. This is the reverse of the drawer's one-way "Desktop view" — a user can return to Auto here.
const UI_MODES: { key: UiMode; labelKey: 'prefs.layouts.auto' | 'prefs.layouts.desktop' }[] = [
  { key: 'auto', labelKey: 'prefs.layouts.auto' },
  { key: 'desktop', labelKey: 'prefs.layouts.desktop' },
];

export function PreferencesPage() {
  const { t } = useTranslation('settings');
  const theme = usePrefsStore((s) => s.theme);
  const setTheme = usePrefsStore((s) => s.setTheme);
  const language = usePrefsStore((s) => s.language);
  const setLanguage = usePrefsStore((s) => s.setLanguage);
  const uiMode = usePrefsStore((s) => s.uiMode);
  const setUiMode = usePrefsStore((s) => s.setUiMode);

  return (
    <div>
      <PageHeader
        title={t('prefs.title')}
        trail={[{ label: t('nav:sections.settings') }, { label: t('prefs.title') }]}
      />
      <Card title={t('prefs.appearance')}>
        <div className="pref-row">
          <div className="pref-label">
            <div className="pref-name">{t('prefs.theme')}</div>
            <div className="pref-help muted">{t('prefs.themeHelp')}</div>
          </div>
          <div className="pref-seg" role="radiogroup" aria-label={t('prefs.theme')}>
            {THEMES.map((o) => (
              <button
                key={o.key}
                role="radio"
                aria-checked={theme === o.key}
                className={theme === o.key ? 'pref-seg-btn active' : 'pref-seg-btn'}
                onClick={() => setTheme(o.key)}
              >
                {t(o.labelKey)}
              </button>
            ))}
          </div>
        </div>
        <div className="pref-row">
          <div className="pref-label">
            <div className="pref-name">{t('prefs.language')}</div>
            <div className="pref-help muted">{t('prefs.languageHelp')}</div>
          </div>
          <div className="pref-seg" role="radiogroup" aria-label={t('prefs.language')}>
            {LANGUAGES.map((o) => (
              <button
                key={o.key}
                role="radio"
                aria-checked={language === o.key}
                className={language === o.key ? 'pref-seg-btn active' : 'pref-seg-btn'}
                onClick={() => setLanguage(o.key)}
              >
                {o.label}
              </button>
            ))}
          </div>
        </div>
        <div className="pref-row">
          <div className="pref-label">
            <div className="pref-name">{t('prefs.layout')}</div>
            <div className="pref-help muted">{t('prefs.layoutHelp')}</div>
          </div>
          <div className="pref-seg" role="radiogroup" aria-label={t('prefs.layout')}>
            {UI_MODES.map((o) => (
              <button
                key={o.key}
                role="radio"
                aria-checked={uiMode === o.key}
                className={uiMode === o.key ? 'pref-seg-btn active' : 'pref-seg-btn'}
                onClick={() => setUiMode(o.key)}
              >
                {t(o.labelKey)}
              </button>
            ))}
          </div>
        </div>
      </Card>
    </div>
  );
}
