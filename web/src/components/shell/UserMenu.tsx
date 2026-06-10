// User menu (top-right, always present §2.1): shows the current principal's role and a
// logout action. Role comes from GET /api/v1/auth/me; in public-dashboard mode there may be
// no session, in which case it shows a sign-in affordance.

import { useEffect, useRef, useState } from 'react';
import { useNavigate } from 'react-router-dom';
import { api, getToken } from '../../services/api';
import { useAuthStore } from '../../store';
import './UserMenu.css';

export function UserMenu() {
  const authed = useAuthStore((s) => s.authed);
  const setAuthed = useAuthStore((s) => s.setAuthed);
  const navigate = useNavigate();
  const [role, setRole] = useState<string | null>(null);
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!authed || !getToken()) {
      setRole(null);
      return;
    }
    let cancelled = false;
    api
      .me()
      .then((r) => !cancelled && setRole(r.role))
      .catch(() => !cancelled && setRole(null));
    return () => {
      cancelled = true;
    };
  }, [authed]);

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
    setOpen(false);
    navigate('/dashboard');
  };

  const initial = (role ?? (authed ? 'U' : '?')).charAt(0).toUpperCase();

  return (
    <div className="usermenu" ref={ref}>
      <button className="usermenu-avatar" onClick={() => setOpen((o) => !o)} aria-label="User menu">
        {initial}
      </button>
      {open && (
        <div className="usermenu-pop">
          <div className="usermenu-head">
            <div className="usermenu-role">{role ? `Signed in · ${role}` : 'Not signed in'}</div>
          </div>
          {authed ? (
            <button className="usermenu-item" onClick={logout}>
              Log out
            </button>
          ) : (
            <button
              className="usermenu-item"
              onClick={() => {
                setOpen(false);
                navigate('/login');
              }}
            >
              Sign in
            </button>
          )}
        </div>
      )}
    </div>
  );
}
