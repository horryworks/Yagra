// SPDX-License-Identifier: AGPL-3.0-only
// User menu (top-right, always present §2.1): shows the current principal's role, opens the
// Preferences dialog, and logs out. Role is resolved into the auth store (App bootstrap / login);
// in public-dashboard mode there may be no session, in which case it shows a sign-in affordance.
//
// Preferences lives here rather than in Settings (ADR-055 決定 9 / Inc.7): the account badge is by
// definition "the shelf that is only mine", which is the line the old `Personal` group header was
// drawn to make. It opens a dialog over whatever is on screen, because theme and language are
// changed *during* other work.

import { useEffect, useRef, useState } from 'react';
import { useNavigate } from 'react-router-dom';
import { useTranslation } from 'react-i18next';
import { api } from '../../services/api';
import { useAuthStore, usePrefsDialogStore } from '../../store';
import { PreferencesModal } from './PreferencesModal';
import './UserMenu.css';

export function UserMenu() {
  const { t } = useTranslation('nav');
  const authed = useAuthStore((s) => s.authed);
  const setAuthed = useAuthStore((s) => s.setAuthed);
  const setRole = useAuthStore((s) => s.setRole);
  const setScope = useAuthStore((s) => s.setScope);
  const setRoleMatrix = useAuthStore((s) => s.setRoleMatrix);
  const role = useAuthStore((s) => s.role);
  const scope = useAuthStore((s) => s.scope);
  const prefsOpen = usePrefsDialogStore((s) => s.open);
  const setPrefsOpen = usePrefsDialogStore((s) => s.setOpen);
  const navigate = useNavigate();
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);
  const avatarRef = useRef<HTMLButtonElement>(null);

  // Dismissal, in the shape every other menu in the app uses (`OverflowMenu`). Two things were
  // missing until ADR-073: Escape did nothing — this and `CredentialPicker` were the only two
  // popovers in the product without it — and the outside-click listener was mounted unconditionally,
  // so a menu that is closed 99% of the time still inspected every click on every screen.
  useEffect(() => {
    if (!open) return;
    const onDown = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) setOpen(false);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') setOpen(false);
    };
    document.addEventListener('mousedown', onDown);
    document.addEventListener('keydown', onKey);
    return () => {
      document.removeEventListener('mousedown', onDown);
      document.removeEventListener('keydown', onKey);
    };
  }, [open]);

  const logout = () => {
    // Fire the server-side revoke (request() captures the token synchronously before it's
    // cleared); the local UI state clears immediately without waiting on the round-trip.
    void api.logout();
    setAuthed(false);
    setRole(null);
    setScope(null);
    setRoleMatrix(null);
    setOpen(false);
    navigate('/dashboard');
  };

  const initial = (role ?? (authed ? 'U' : '?')).charAt(0).toUpperCase();

  return (
    <div className="usermenu" ref={ref}>
      <button
        ref={avatarRef}
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
          {/* Not permission-gated, deliberately: these settings are this browser's, so there is no
              privilege to hold. It sits above the sign-out item for the same reason every other
              menu does — leaving is the last thing on the list. `userMenu.spec.ts` pins the order,
              and goes red when the two are swapped. */}
          <button
            className="usermenu-item"
            onClick={() => {
              setOpen(false);
              setPrefsOpen(true);
            }}
          >
            {t('shell.preferences')}
          </button>
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
      {prefsOpen && (
        <PreferencesModal
          onClose={() => {
            setPrefsOpen(false);
            // `Modal` restores focus to whatever had it when the dialog mounted — which here is the
            // menu item, and that unmounts with the menu. Put focus back on the badge instead, or
            // it lands on <body> and the keyboard operator restarts from the top of the page.
            avatarRef.current?.focus();
          }}
        />
      )}
    </div>
  );
}
