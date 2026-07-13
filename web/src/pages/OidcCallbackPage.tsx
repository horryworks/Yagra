// OIDC redirect landing (§3.6): the IdP sends the user back here with `?code&state`. We forward
// them to the backend, which exchanges the code, validates the ID token, maps groups → role, and
// returns a session — then we store it like a local login and go to the dashboard. Rendered by the
// App root (outside the login gate) so a returning SSO user isn't swallowed by the login screen.

import { useEffect, useRef, useState } from 'react';
import { useNavigate, useSearchParams } from 'react-router-dom';
import { useTranslation } from 'react-i18next';
import { api, ApiError } from '../services/api';
import { useAuthStore } from '../store';
import './LoginPage.css';

export function OidcCallbackPage() {
  const { t } = useTranslation('auth');
  const [params] = useSearchParams();
  const navigate = useNavigate();
  const setAuthed = useAuthStore((s) => s.setAuthed);
  const setRole = useAuthStore((s) => s.setRole);
  const [error, setError] = useState<string | null>(null);
  const ran = useRef(false);

  useEffect(() => {
    // The authorization code is one-shot (the server consumes its flight state), so guard against
    // React StrictMode's double-invoke to avoid a spurious second "already used" failure.
    if (ran.current) return;
    ran.current = true;

    const idpError = params.get('error');
    if (idpError) {
      setError(params.get('error_description') ?? idpError);
      return;
    }
    const code = params.get('code');
    const state = params.get('state');
    if (!code || !state) {
      setError(t('ssoMissingParams'));
      return;
    }
    api
      .oidcCallback(code, state)
      .then((res) => {
        setRole(res.role);
        setAuthed(true);
        navigate('/dashboard', { replace: true });
      })
      .catch((x: unknown) => {
        setError(x instanceof ApiError ? x.message : t('ssoFailed'));
      });
  }, [params, navigate, setAuthed, setRole, t]);

  return (
    <div className="login">
      <div className="login-card">
        <div className="login-form-side">
          {error ? (
            <>
              <h1 className="login-title">{t('ssoFailedTitle')}</h1>
              <p className="login-error login-error-danger">{error}</p>
              <button
                className="login-sso"
                type="button"
                onClick={() => navigate('/login', { replace: true })}
              >
                {t('backToSignIn')}
              </button>
            </>
          ) : (
            <p className="muted">{t('ssoCompleting')}</p>
          )}
        </div>
      </div>
    </div>
  );
}
