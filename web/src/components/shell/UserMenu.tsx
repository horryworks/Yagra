// SPDX-License-Identifier: AGPL-3.0-only
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
  const setScope = useAuthStore((s) => s.setScope);
  const role = useAuthStore((s) => s.role);
  const scope = useAuthStore((s) => s.scope);
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
    // Fire the server-side revoke (request() captures the token synchronously before it's
    // cleared); the local UI state clears immediately without waiting on the round-trip.
    void api.logout();
    setAuthed(false);
    setRole(null);
    setScope(null);
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
            {/* Said out loud only when it restricts something. A scoped account's lists are simply
                shorter than the fleet, with nothing else on screen to distinguish "you can see
                three sites" from "there are three sites". */}
            {scope && scope !== 'All' && (
              <div className="usermenu-scope">
                {t('shell.scopedTo', { count: scope.Groups.length })}
              </div>
            )}
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
