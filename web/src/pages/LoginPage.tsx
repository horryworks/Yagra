// Login (§3.6): split card — 藍 brand panel (家紋) on the left, sign-in on the right. SSO is
// the intended primary path but no IdP backend exists yet, so it's shown disabled (primary
// affordance, inert) above the local form (secondary). 朱 appears only on focus. Errors are
// color-split: danger = hard failure (bad credentials / unavailable), warning = recoverable.
// MFA is a backend gap, so no MFA step here yet.

import { useState } from 'react';
import type { ChangeEvent, FormEvent } from 'react';
import { useNavigate } from 'react-router-dom';
import { useTranslation } from 'react-i18next';
import { api, ApiError } from '../services/api';
import { useAuthStore } from '../store';
import { Logo } from '../components/shell/Logo';
import { Button } from '../components/ui/Button';
import { TextInput } from '../components/ui/Field';
import './LoginPage.css';

type ErrTone = 'danger' | 'warning';

export function LoginPage({ embedded = false }: { embedded?: boolean }) {
  const { t } = useTranslation('auth');
  const setAuthed = useAuthStore((s) => s.setAuthed);
  const setRole = useAuthStore((s) => s.setRole);
  const navigate = useNavigate();
  const [username, setUsername] = useState('');
  const [password, setPassword] = useState('');
  const [error, setError] = useState<{ tone: ErrTone; message: string } | null>(null);
  const [busy, setBusy] = useState(false);

  const text = (set: (v: string) => void) => (e: ChangeEvent<HTMLInputElement>) =>
    set(e.target.value);

  const submit = (e: FormEvent) => {
    e.preventDefault();
    setError(null);
    setBusy(true);
    api
      .login(username, password)
      .then((res) => {
        setPassword('');
        setRole(res.role);
        setAuthed(true);
        if (!embedded) navigate('/dashboard');
      })
      .catch((x: unknown) => {
        // Lockout/expiry are recoverable (warning); everything else is a hard danger.
        const recoverable =
          x instanceof ApiError && (x.code === 'locked_out' || x.code === 'expired');
        setError({
          tone: recoverable ? 'warning' : 'danger',
          message: x instanceof ApiError ? x.message : t('signInFailed'),
        });
      })
      .finally(() => setBusy(false));
  };

  return (
    <div className={embedded ? 'login login-embedded' : 'login'}>
      <div className="login-card">
        <aside className="login-brand">
          <Logo size={56} variant="mark" />
          <div className="login-brand-name">{t('nav:shell.wordmark')}</div>
          <div className="login-brand-tag">{t('brandTag')}</div>
        </aside>
        <div className="login-form-side">
          <h1 className="login-title">{t('signIn')}</h1>

          <button className="login-sso" disabled title={t('ssoDisabledTitle')}>
            {t('continueWithSso')}
            <span className="login-sso-note">{t('ssoNotConfigured')}</span>
          </button>

          <div className="login-divider">
            <span>{t('orSignInLocally')}</span>
          </div>

          <form className="login-form" onSubmit={submit}>
            <label className="login-label">
              {t('username')}
              <TextInput value={username} onChange={text(setUsername)} autoComplete="username" />
            </label>
            <label className="login-label">
              {t('password')}
              <TextInput
                type="password"
                value={password}
                onChange={text(setPassword)}
                autoComplete="current-password"
              />
            </label>
            <Button variant="primary" type="submit" disabled={busy}>
              {busy ? t('signingIn') : t('signIn')}
            </Button>
          </form>

          {error && <p className={`login-error login-error-${error.tone}`}>{error.message}</p>}
        </div>
      </div>
    </div>
  );
}
