// Preferences (Settings ▸ Preferences). Real, client-only: theme (light/dark, required by
// §1.2) persisted via the prefs store and reflected onto <html data-theme>. No backend needed.

import { usePrefsStore, type Theme } from '../prefs';
import { PageHeader } from '../components/ui/PageHeader';
import { Card } from '../components/ui/Card';
import './PreferencesPage.css';

const THEMES: { key: Theme; label: string }[] = [
  { key: 'light', label: 'Light' },
  { key: 'dark', label: 'Dark' },
];

export function PreferencesPage() {
  const theme = usePrefsStore((s) => s.theme);
  const setTheme = usePrefsStore((s) => s.setTheme);

  return (
    <div>
      <PageHeader title="Preferences" trail={[{ label: 'Settings' }, { label: 'Preferences' }]} />
      <Card title="Appearance">
        <div className="pref-row">
          <div className="pref-label">
            <div className="pref-name">Theme</div>
            <div className="pref-help muted">Light and dark both use the Yagra token palette.</div>
          </div>
          <div className="pref-seg" role="radiogroup" aria-label="Theme">
            {THEMES.map((t) => (
              <button
                key={t.key}
                role="radio"
                aria-checked={theme === t.key}
                className={theme === t.key ? 'pref-seg-btn active' : 'pref-seg-btn'}
                onClick={() => setTheme(t.key)}
              >
                {t.label}
              </button>
            ))}
          </div>
        </div>
      </Card>
    </div>
  );
}
