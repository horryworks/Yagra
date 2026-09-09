// SPDX-License-Identifier: AGPL-3.0-only
// User menu (top-right, always present §2.1): shows the current principal's role, opens the
// Preferences dialog, changes this account's password, and logs out. Role is resolved into the auth
// store (App bootstrap / login); in public-dashboard mode there may be no session, in which case it
// shows a sign-in affordance.
//
// Preferences lives here rather than in Settings (ADR-055 決定 9 / Inc.7): the account badge is by
// definition "the shelf that is only mine", which is the line the old `Personal` group header was
// drawn to make. It opens a dialog over whatever is on screen, because theme and language are
// changed *during* other work.
//
// Changing your own password is here for the same reason (ADR-122) — and it is the one thing on
// this shelf that writes to the server, which is why it is also the one that can be absent: an
// account signing in through a directory or an identity provider has no password Yagra holds. When
// it is absent the head says where the password actually lives, because hiding a control the
// operator came looking for and saying nothing is how a decision reads as a missing feature
// (ADR-055 R6).

import { useEffect, useRef, useState } from 'react';
import { useNavigate } from 'react-router-dom';
import { useTranslation } from 'react-i18next';
import { api } from '../../services/api';
import { useAuthStore, usePrefsDialogStore } from '../../store';
import { hasLocalPassword, passwordHomeKey } from '../../lib/password';
import { ChangeMyPasswordModal } from './ChangeMyPasswordModal';
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
  const accountKind = useAuthStore((s) => s.accountKind);
  const setAccountKind = useAuthStore((s) => s.setAccountKind);
  const prefsOpen = usePrefsDialogStore((s) => s.open);
  const setPrefsOpen = usePrefsDialogStore((s) => s.setOpen);
  const navigate = useNavigate();
  const [open, setOpen] = useState(false);
  const [pwOpen, setPwOpen] = useState(false);
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

  // Drop everything this browser knows about the session. Shared by the sign-out item and by a
  // successful password change, because the two differ only in whether the server still needs
  // telling — not in what the client forgets. Writing it twice is how one of them ends up keeping
  // a stale role or scope.
  const clearSession = () => {
    setAuthed(false);
    setRole(null);
    setScope(null);
    setAccountKind(null);
    setRoleMatrix(null);
  };

  const logout = () => {
    // Fire the server-side revoke (request() captures the token synchronously before it's
    // cleared); the local UI state clears immediately without waiting on the round-trip.
    void api.logout();
    clearSession();
    setOpen(false);
    navigate('/dashboard');
  };

  // The API has already revoked every session of this account, this one included, so there is no
  // logout to send — the stored token is dead. Go to /login rather than /dashboard: the operator
  // asked to change their password, and the next thing they need is to prove the new one works.
  const passwordChanged = () => {
    setPwOpen(false);
    clearSession();
    navigate('/login');
  };

  const passwordHome = passwordHomeKey(accountKind);

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
            {/* Removing the change-password item for an external account is only half the job —
                the other half is answering the person who opened this menu looking for it
                (ADR-055 R6). A hidden control with no explanation reads as a missing feature. */}
            {passwordHome && <div className="usermenu-scope">{t(passwordHome)}</div>}
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
          {/* Drawn only when Yagra actually holds this account's password. Not permission-gated —
              there is no privilege involved, every signed-in local account may change its own — but
              the same rule applies (ADR-056): a control whose write path answers 400 is a control
              that is not drawn. `hasLocalPassword` reads `null` (skeleton mode) as "no", so this
              fails closed rather than offering a button with no server behind it.
              ⚠️ It deliberately does **not** also ask `authed`. Not only because
              `permissions.test.ts` forbids that spelling — `accountKind` is resolved from
              `/auth/me`, which needs a session, and is cleared on sign-out, so asking twice would
              be a second answer to "is anyone signed in" that can disagree with the first. */}
          {hasLocalPassword(accountKind) && (
            <button
              className="usermenu-item"
              onClick={() => {
                setOpen(false);
                setPwOpen(true);
              }}
            >
              {t('shell.changePassword')}
            </button>
          )}
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
      {pwOpen && (
        <ChangeMyPasswordModal
          onChanged={passwordChanged}
          onClose={() => {
            setPwOpen(false);
            // Same reason as the Preferences dialog below: `Modal` restores focus to the menu item
            // that opened it, and that item unmounted with the menu.
            avatarRef.current?.focus();
          }}
        />
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
