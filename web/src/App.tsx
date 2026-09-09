// SPDX-License-Identifier: AGPL-3.0-only
// App root: applies the persisted theme, discovers whether reads are gated, and mounts one of four
// things — a spinner, the login screen, the bare public board, or the full shell.
//
// ⚠️ Two things here changed with ADR-123 and the old behaviour is worth knowing, because both were
// wrong in ways nothing reported:
//
//  1. A failed config fetch used to fall back to `{ public_dashboard: true }`, so a core that was
//     down dropped every visitor into the app shell with no login screen and every panel erroring.
//     Unknown is now closed — see `appGate.ts`.
//  2. A public deployment used to render the **whole application** to anonymous visitors, because
//     every `RequireView` endpoint answered them. There is now one board and nothing else, and it
//     is somewhere they are *sent* rather than where they land: every anonymous URL answers with
//     the sign-in form, which carries a button through to the board (ADR-123 Inc.1).

import { useEffect } from 'react';
import { BrowserRouter } from 'react-router-dom';
import { LoginPage } from './pages/LoginPage';
import { OidcCallbackPage } from './pages/OidcCallbackPage';
import { AppRoutes } from './routes';
import { useTranslation } from 'react-i18next';
import { api, getToken, setUnauthorizedHandler } from './services/api';
import { appView } from './appGate';
import { PublicShell } from './dashboard/PublicDashboardPage';
import { applyLanguage, applyTheme, usePrefsStore } from './prefs';
import { loadServerPrefs, resetServerPrefs } from './serverPrefs';
import { applyViewportMode, useViewportMode } from './lib/viewport';
import i18n from './i18n';
import { useAuthStore, useConfigStore } from './store';

export function App() {
  const { t } = useTranslation();
  const authed = useAuthStore((s) => s.authed);
  const role = useAuthStore((s) => s.role);
  const setRole = useAuthStore((s) => s.setRole);
  const scope = useAuthStore((s) => s.scope);
  const setScope = useAuthStore((s) => s.setScope);
  const setAccountKind = useAuthStore((s) => s.setAccountKind);
  const setRoleMatrix = useAuthStore((s) => s.setRoleMatrix);
  const theme = usePrefsStore((s) => s.theme);
  const language = usePrefsStore((s) => s.language);
  const viewportMode = useViewportMode();
  // In a store rather than local state: the dashboard layout stores read `public_dashboard` from
  // outside React to decide whether there is a row to fetch (`layoutAccess.ts`).
  const config = useConfigStore((s) => s.config);
  const configStatus = useConfigStore((s) => s.status);
  const setConfig = useConfigStore((s) => s.setConfig);
  const setUnreachable = useConfigStore((s) => s.setUnreachable);

  // Resolve the current principal's role, visibility scope and permissions once we're authenticated
  // but don't yet know them (after a page reload the token is in localStorage but none of them is),
  // and after a login, which returns the role but not the rest. Role-, scope- and permission-gated
  // UI read them from the auth store. All three clear when signed out.
  //
  // The permissions come from `GET /api/v1/roles` — the server's `Permission::ALL × Role::ALL`
  // matrix — looked up by this principal's role, so the UI never holds a permission table of its
  // own (ADR-056 Inc.2). Two requests rather than one added field on `/auth/me` deliberately:
  // `/roles` has existed since ADR-014, so a newer WebUI in front of an N-1 core still resolves
  // permissions instead of hiding every write control it has.
  //
  // On failure nothing is set to a non-null value, which is what stops this retrying forever: a
  // failed resolve leaves the state exactly as it found it, so no re-render is triggered.
  useEffect(() => {
    if (!authed || !getToken()) {
      setRole(null);
      setScope(null);
      setAccountKind(null);
      setRoleMatrix(null);
      return;
    }
    if (role != null && scope != null) return;
    let cancelled = false;
    Promise.all([api.me(), api.listRoles()])
      .then(([me, matrix]) => {
        if (cancelled) return;
        setRole(me.role);
        setScope(me.scope);
        setAccountKind(me.kind ?? null);
        setRoleMatrix(matrix);
      })
      .catch(() => !cancelled && setRole(null));
    return () => {
      cancelled = true;
    };
  }, [authed, role, scope, setRole, setScope, setAccountKind, setRoleMatrix]);

  // Pull this account's server-side preferences once per sign-in (ADR-058), so a setting made on
  // another machine is in place here. Deliberately not part of the effect above: that one is a
  // precondition for rendering role-gated UI, whereas this one only refines values the local store
  // already holds — it must never gate, retry or report. Signing out resets the sync so the next
  // account does not inherit the previous one's "endpoint unsupported" verdict.
  useEffect(() => {
    if (!authed || !getToken()) {
      resetServerPrefs();
      return;
    }
    void loadServerPrefs();
  }, [authed]);

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

  // Retry a few times before settling on `unreachable`: a core that is still starting answers
  // nothing for a few seconds, and the old code papered over exactly that case by pretending the
  // deployment was public. Retrying is the honest version of the same intent.
  useEffect(() => {
    let cancelled = false;
    let attempt = 0;
    const tryOnce = () => {
      api
        .getConfig()
        .then((c) => {
          if (!cancelled) setConfig(c);
        })
        .catch(() => {
          if (cancelled) return;
          attempt += 1;
          if (attempt < 3) setTimeout(tryOnce, 5000);
          else setUnreachable();
        });
    };
    tryOnce();
    return () => {
      cancelled = true;
    };
  }, [setConfig, setUnreachable]);

  // Read once, outside the router: this decides whether a router is mounted at all.
  const path = typeof window !== 'undefined' ? window.location.pathname : '/';
  const view = appView(configStatus, config?.public_dashboard === true, authed, path);
  // The OIDC redirect lands here before a session exists — handle it regardless of the login gate
  // (otherwise the gate would swap in the login screen and drop the code/state).
  const isOidcCallback =
    typeof window !== 'undefined' && window.location.pathname === '/auth/callback';

  return (
    <BrowserRouter>
      {isOidcCallback ? (
        <OidcCallbackPage />
      ) : view === 'loading' ? (
        <div className="app-loading muted">{t('loading')}</div>
      ) : view === 'login' ? (
        <LoginPage />
      ) : view === 'public' ? (
        <PublicShell />
      ) : (
        <AppRoutes />
      )}
    </BrowserRouter>
  );
}
