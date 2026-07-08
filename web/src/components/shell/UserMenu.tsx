// User menu (top-right, always present §2.1): shows the current principal's role and a
// logout action. Role is resolved into the auth store (App bootstrap / login); in public-dashboard
// mode there may be no session, in which case it shows a sign-in affordance.

import { useEffect, useRef, useState } from 'react';
import { useNavigate } from 'react-router-dom';
import { useTranslation } from 'react-i18next';
import { api } from '../../services/api';
import { useAuthStore } from '../../store';
import './UserMenu.css';

export function UserMenu() {
  const { t } = useTranslation('nav');
  const authed = useAuthStore((s) => s.authed);
  const setAuthed = useAuthStore((s) => s.setAuthed);
  const setRole = useAuthStore((s) => s.setRole);
  const role = useAuthStore((s) => s.role);
  const navigate = useNavigate();
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const onDown = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) setOpen(false);
    };
    document.addEventListener('mousedown', onDown);
    return () => document.removeEventListener('mousedown', onDown);
  }, []);

  const logout = () => {
    api.logout();
    setAuthed(false);
    setRole(null);
    setOpen(false);
    navigate('/dashboard');
  };

  const initial = (role ?? (authed ? 'U' : '?')).charAt(0).toUpperCase();

  return (
    <div className="usermenu" ref={ref}>
      <button
        className="usermenu-avatar"
        onClick={() => setOpen((o) => !o)}
        aria-label={t('shell.userMenu')}
      >
        {initial}
      </button>
      {open && (
        <div className="usermenu-pop">
          <div className="usermenu-head">
            <div className="usermenu-role">
              {role ? t('shell.signedInAs', { role }) : t('shell.notSignedIn')}
            </div>
          </div>
          {authed ? (
            <button className="usermenu-item" onClick={logout}>
              {t('shell.logOut')}
            </button>
          ) : (
            <button
              className="usermenu-item"
              onClick={() => {
                setOpen(false);
                navigate('/login');
              }}
            >
              {t('shell.signIn')}
            </button>
          )}
        </div>
      )}
    </div>
  );
}
