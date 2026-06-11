// Users & roles (Settings ▸ Users & roles). Admin-only CRUD over local auth accounts:
// create with a role, change role inline, reset password, and delete. The server enforces
// ManageUsers (admin) and guards against removing/demoting the last admin (409 last_admin);
// the UI surfaces those typed errors rather than re-deriving the rule. Passwords are
// write-only — the API never returns a hash.

import { useCallback, useEffect, useState } from 'react';
import { api, ApiError } from '../services/api';
import { useAuthStore } from '../store';
import type { Role, UserSummary } from '../types/api';
import { PageHeader } from '../components/ui/PageHeader';
import { Card } from '../components/ui/Card';
import { Button } from '../components/ui/Button';
import { TextInput, Select } from '../components/ui/Field';
import './CrudList.css';

const ROLES: Role[] = ['viewer', 'operator', 'admin'];

export function UsersPage() {
  const authed = useAuthStore((s) => s.authed);
  const [rows, setRows] = useState<UserSummary[]>([]);
  const [username, setUsername] = useState('');
  const [password, setPassword] = useState('');
  const [role, setRole] = useState<Role>('viewer');
  const [error, setError] = useState<string | null>(null);
  const [unavailable, setUnavailable] = useState(false);
  const [forbidden, setForbidden] = useState(false);
  // Per-row password reset: the id whose reset input is open, and its draft value.
  const [resetId, setResetId] = useState<string | null>(null);
  const [resetPw, setResetPw] = useState('');

  const load = useCallback(() => {
    api
      .listUsers()
      .then((list) => {
        setRows(list);
        setUnavailable(false);
        setForbidden(false);
      })
      .catch((e: unknown) => {
        if (e instanceof ApiError && e.code === 'admin_unavailable') setUnavailable(true);
        else if (e instanceof ApiError && (e.code === 'forbidden' || e.status === 403))
          setForbidden(true);
      });
  }, []);

  useEffect(() => {
    load();
  }, [load]);

  const fail = (e: unknown, fallback: string) =>
    setError(e instanceof ApiError ? e.message : fallback);

  const add = () => {
    setError(null);
    api
      .createUser({ username: username.trim(), password, role })
      .then(() => {
        setUsername('');
        setPassword('');
        setRole('viewer');
        load();
      })
      .catch((e: unknown) => fail(e, 'failed to create user'));
  };

  const changeRole = (id: string, next: Role) => {
    setError(null);
    api
      .setUserRole(id, next)
      .then(load)
      .catch((e: unknown) => fail(e, 'failed to change role'));
  };

  const remove = (id: string) => {
    setError(null);
    api
      .deleteUser(id)
      .then(load)
      .catch((e: unknown) => fail(e, 'failed to delete user'));
  };

  const saveReset = (id: string) => {
    setError(null);
    api
      .setUserPassword(id, resetPw)
      .then(() => {
        setResetId(null);
        setResetPw('');
      })
      .catch((e: unknown) => fail(e, 'failed to reset password'));
  };

  return (
    <div>
      <PageHeader
        title="Users & roles"
        trail={[{ label: 'Settings' }, { label: 'Users & roles' }]}
        note="Local accounts for the northbound API. Roles: viewer (read-only), operator (ack/maintenance), admin (full control)."
      />

      {unavailable ? (
        <Card>
          <p className="muted">User management is unavailable in skeleton mode (no database).</p>
        </Card>
      ) : forbidden ? (
        <Card>
          <p className="muted">Managing users requires an admin account.</p>
        </Card>
      ) : (
        <Card title="Accounts">
          {authed && (
            <div className="crud-add form-row">
              <TextInput
                placeholder="Username"
                value={username}
                onChange={(e) => setUsername(e.target.value)}
                autoComplete="off"
              />
              <TextInput
                type="password"
                placeholder="Password (min 8 chars)"
                value={password}
                onChange={(e) => setPassword(e.target.value)}
                autoComplete="new-password"
              />
              <Select value={role} onChange={(e) => setRole(e.target.value as Role)}>
                {ROLES.map((r) => (
                  <option key={r} value={r}>
                    {r}
                  </option>
                ))}
              </Select>
              <Button
                variant="primary"
                onClick={add}
                disabled={!username.trim() || password.length < 8}
              >
                Add user
              </Button>
            </div>
          )}
          {error && <p className="form-error">{error}</p>}
          {rows.length === 0 ? (
            <p className="muted">No users yet.</p>
          ) : (
            <div className="crud-list">
              {rows.map((u) => (
                <div className="crud-row" key={u.id}>
                  <span className="crud-name">{u.username}</span>
                  {authed ? (
                    <Select
                      className="crud-role-select"
                      value={u.role}
                      onChange={(e) => changeRole(u.id, e.target.value as Role)}
                      aria-label={`Role for ${u.username}`}
                    >
                      {ROLES.map((r) => (
                        <option key={r} value={r}>
                          {r}
                        </option>
                      ))}
                    </Select>
                  ) : (
                    <span className="crud-kind">{u.role}</span>
                  )}
                  <span className="crud-id mono">{u.created_at}</span>
                  {authed &&
                    (resetId === u.id ? (
                      <div className="crud-reset form-row">
                        <TextInput
                          type="password"
                          placeholder="New password"
                          value={resetPw}
                          onChange={(e) => setResetPw(e.target.value)}
                          autoComplete="new-password"
                        />
                        <Button
                          variant="primary"
                          onClick={() => saveReset(u.id)}
                          disabled={resetPw.length < 8}
                        >
                          Save
                        </Button>
                        <Button
                          variant="ghost"
                          onClick={() => {
                            setResetId(null);
                            setResetPw('');
                          }}
                        >
                          Cancel
                        </Button>
                      </div>
                    ) : (
                      <>
                        <Button
                          variant="ghost"
                          onClick={() => {
                            setResetId(u.id);
                            setResetPw('');
                          }}
                        >
                          Reset password
                        </Button>
                        <Button variant="ghost" onClick={() => remove(u.id)}>
                          Delete
                        </Button>
                      </>
                    ))}
                </div>
              ))}
            </div>
          )}
        </Card>
      )}
    </div>
  );
}
