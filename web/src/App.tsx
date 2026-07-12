// App root: applies the persisted theme, discovers whether reads are gated, and mounts the
// router. In private-dashboard mode (reads require auth) and not logged in, the whole app is
// gated behind the login screen; otherwise the shell + routes render. On a config-fetch
// error we fall back to an open dashboard so a transient/older server never hard-locks the UI.

import { useEffect, useState } from 'react';
import { BrowserRouter } from 'react-router-dom';
import { LoginPage } from './pages/LoginPage';
import { AppRoutes } from './routes';
import { useTranslation } from 'react-i18next';
import { api, getToken, setUnauthorizedHandler, type ClientConfig } from './services/api';
import { applyLanguage, applyTheme, usePrefsStore } from './prefs';
import { applyViewportMode, useViewportMode } from './lib/viewport';
import i18n from './i18n';
import { useAuthStore } from './store';

export function App() {
  const { t } = useTranslation();
  const authed = useAuthStore((s) => s.authed);
  const role = useAuthStore((s) => s.role);
  const setRole = useAuthStore((s) => s.setRole);
  const theme = usePrefsStore((s) => s.theme);
  const language = usePrefsStore((s) => s.language);
  const viewportMode = useViewportMode();
  const [config, setConfig] = useState<ClientConfig | null>(null);

  // Resolve the current principal's role once we're authenticated but don't yet know it (e.g. after
  // a page reload, where the token is in localStorage but the role isn't). Role-gated UI reads it
  // from the auth store. Clears when signed out.
  useEffect(() => {
    if (!authed || !getToken()) {
      setRole(null);
      return;
    }
    if (role != null) return;
    let cancelled = false;
    api
      .me()
      .then((r) => !cancelled && setRole(r.role))
      .catch(() => !cancelled && setRole(null));
    return () => {
      cancelled = true;
    };
  }, [authed, role, setRole]);

  // Reflect the persisted theme onto <html data-theme> (and keep it in sync on change).
  useEffect(() => {
    applyTheme(theme);
  }, [theme]);

  // Reflect the resolved layout mode onto <html data-viewport> so mobile CSS applies (ADR-027).
  // Recomputes when the viewport crosses 768px or the uiMode override changes.
  useEffect(() => {
    applyViewportMode(viewportMode);
  }, [viewportMode]);

  // Reflect the persisted language into i18next + <html lang> (immediate switch, no reload). A
  // non-English language lazy-loads its chunks here; the EN fallback shows until they arrive.
  useEffect(() => {
    applyLanguage(language);
    void i18n.changeLanguage(language);
  }, [language]);

  // A stale/expired token (e.g. after a core restart wiped in-memory sessions) makes writes
  // 401 even though localStorage still has a token. Drop auth state on that signal so the UI
  // re-prompts for sign-in instead of showing write actions that fail.
  useEffect(() => {
    setUnauthorizedHandler(() => useAuthStore.getState().setAuthed(false));
    return () => setUnauthorizedHandler(null);
  }, []);

  useEffect(() => {
    api
      .getConfig()
      .then(setConfig)
      .catch(() =>
        setConfig({
          public_dashboard: true,
          auth_available: false,
          default_poll_interval_secs: 30,
        }),
      );
  }, []);

  const gated = config != null && !config.public_dashboard && !authed;

  return (
    <BrowserRouter>
      {config == null ? (
        <div className="app-loading muted">{t('loading')}</div>
      ) : gated ? (
        <LoginPage />
      ) : (
        <AppRoutes />
      )}
    </BrowserRouter>
  );
}
