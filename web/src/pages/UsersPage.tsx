// Users & roles (Settings ▸ Users & roles). Admin-only CRUD over local auth accounts,
// presented as a headered table: create via an "Add user" modal, change role inline, change
// password and delete via modals (delete behind a confirmation, per ui-conventions). The
// server enforces ManageUsers (admin) and guards against removing/demoting the last admin
// (409 last_admin); the UI surfaces those typed errors. Passwords are write-only — the API
// never returns a hash.

import { useCallback, useEffect, useState } from 'react';
import { api, ApiError } from '../services/api';
import { useAuthStore } from '../store';
import type { Role, UserSummary } from '../types/api';
import { PageHeader } from '../components/ui/PageHeader';
import { Card } from '../components/ui/Card';
import { Button } from '../components/ui/Button';
import { Modal } from '../components/ui/Modal';
import { TextInput, Select, RequiredMark } from '../components/ui/Field';
import './UsersPage.css';

const ROLES: Role[] = ['viewer', 'operator', 'admin'];
const MIN_PW = 8;

const errMsg = (e: unknown, fallback: string) =>
  e instanceof ApiError ? e.message : fallback;

export function UsersPage() {
  const authed = useAuthStore((s) => s.authed);
  const [rows, setRows] = useState<UserSummary[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [unavailable, setUnavailable] = useState(false);
  const [forbidden, setForbidden] = useState(false);
  const [loading, setLoading] = useState(true);
  // Open dialogs: the add form, and the user targeted by a password change / delete.
  const [adding, setAdding] = useState(false);
  const [pwUser, setPwUser] = useState<UserSummary | null>(null);
  const [delUser, setDelUser] = useState<UserSummary | null>(null);

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
      })
      .finally(() => setLoading(false));
  }, []);

  useEffect(() => {
    load();
  }, [load]);

  const changeRole = (id: string, next: Role) => {
    setError(null);
    api
      .setUserRole(id, next)
      .then(load)
      .catch((e: unknown) => setError(errMsg(e, 'failed to change role')));
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
        <Card
          title="Accounts"
          actions={
            authed && (
              <Button variant="primary" onClick={() => setAdding(true)}>
                + Add user
              </Button>
            )
          }
        >
          {error && <p className="form-error">{error}</p>}
          <div className="ytable users-table">
            <div className="ytable-head">
              <div className="ytable-h">Username</div>
              <div className="ytable-h">Role</div>
              <div className="ytable-h">Created</div>
              <div className="ytable-h">Last login</div>
              <div className="ytable-h right">Actions</div>
            </div>
            {rows.length === 0 ? (
              <div className="users-empty">{loading ? 'Loading…' : 'No users yet.'}</div>
            ) : (
              rows.map((u) => (
                <div className="ytable-row" key={u.id}>
                  <div className="ytable-cell ellipsis users-name">{u.username}</div>
                  <div className="ytable-cell">
                    {authed ? (
                      <Select
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
                      <span>{u.role}</span>
                    )}
                  </div>
                  <div className="ytable-cell ellipsis users-created mono">{u.created_at}</div>
                  <div className="ytable-cell ellipsis users-created">
                    {u.last_login_at ? (
                      <span className="mono">{u.last_login_at}</span>
                    ) : (
                      'Never'
                    )}
                  </div>
                  <div className="ytable-cell right users-actions">
                    {authed && (
                      <>
                        <Button variant="ghost" onClick={() => setPwUser(u)}>
                          Change password
                        </Button>
                        <Button variant="ghost" onClick={() => setDelUser(u)}>
                          Delete
                        </Button>
                      </>
                    )}
                  </div>
                </div>
              ))
            )}
          </div>
        </Card>
      )}

      {adding && (
        <AddUserModal
          onClose={() => setAdding(false)}
          onDone={() => {
            setAdding(false);
            load();
          }}
        />
      )}
      {pwUser && (
        <ChangePasswordModal user={pwUser} onClose={() => setPwUser(null)} onDone={() => setPwUser(null)} />
      )}
      {delUser && (
        <DeleteUserModal
          user={delUser}
          onClose={() => setDelUser(null)}
          onDone={() => {
            setDelUser(null);
            load();
          }}
        />
      )}
    </div>
  );
}

/** Create a new account (focused-editing modal). */
function AddUserModal({ onClose, onDone }: { onClose: () => void; onDone: () => void }) {
  const [username, setUsername] = useState('');
  const [password, setPassword] = useState('');
  const [role, setRole] = useState<Role>('viewer');
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const valid = username.trim().length > 0 && password.length >= MIN_PW;

  const submit = () => {
    if (!valid) return;
    setBusy(true);
    setError(null);
    api
      .createUser({ username: username.trim(), password, role })
      .then(onDone)
      .catch((e: unknown) => {
        setError(errMsg(e, 'failed to create user'));
        setBusy(false);
      });
  };

  return (
    <Modal
      title="Add user"
      onClose={onClose}
      footer={
        <>
          <Button variant="outline" onClick={onClose}>
            Cancel
          </Button>
          <Button variant="primary" onClick={submit} disabled={!valid || busy}>
            Add user
          </Button>
        </>
      }
    >
      <div className="modal-field">
        <label className="modal-field-label">
          Username <RequiredMark />
        </label>
        <TextInput
          value={username}
          onChange={(e) => setUsername(e.target.value)}
          autoComplete="off"
          autoFocus
        />
      </div>
      <div className="modal-field">
        <label className="modal-field-label">
          Password (min {MIN_PW} chars) <RequiredMark />
        </label>
        <TextInput
          type="password"
          value={password}
          onChange={(e) => setPassword(e.target.value)}
          autoComplete="new-password"
        />
      </div>
      <div className="modal-field">
        <label className="modal-field-label">Role</label>
        <Select value={role} onChange={(e) => setRole(e.target.value as Role)}>
          {ROLES.map((r) => (
            <option key={r} value={r}>
              {r}
            </option>
          ))}
        </Select>
      </div>
      {error && <p className="form-error">{error}</p>}
    </Modal>
  );
}

/** Change a user's password (with confirmation, focused-editing modal). */
function ChangePasswordModal({
  user,
  onClose,
  onDone,
}: {
  user: UserSummary;
  onClose: () => void;
  onDone: () => void;
}) {
  const [password, setPassword] = useState('');
  const [confirm, setConfirm] = useState('');
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const tooShort = password.length < MIN_PW;
  const mismatch = confirm.length > 0 && password !== confirm;
  const valid = !tooShort && password === confirm;

  const submit = () => {
    if (!valid) return;
    setBusy(true);
    setError(null);
    api
      .setUserPassword(user.id, password)
      .then(onDone)
      .catch((e: unknown) => {
        setError(errMsg(e, 'failed to change password'));
        setBusy(false);
      });
  };

  return (
    <Modal
      title={`Change password — ${user.username}`}
      onClose={onClose}
      footer={
        <>
          <Button variant="outline" onClick={onClose}>
            Cancel
          </Button>
          <Button variant="primary" onClick={submit} disabled={!valid || busy}>
            Save
          </Button>
        </>
      }
    >
      <div className="modal-field">
        <label className="modal-field-label">New password (min {MIN_PW} chars)</label>
        <TextInput
          type="password"
          value={password}
          onChange={(e) => setPassword(e.target.value)}
          autoComplete="new-password"
          autoFocus
        />
      </div>
      <div className="modal-field">
        <label className="modal-field-label">Confirm new password</label>
        <TextInput
          type="password"
          value={confirm}
          onChange={(e) => setConfirm(e.target.value)}
          autoComplete="new-password"
        />
      </div>
      {mismatch && <p className="form-error">passwords do not match</p>}
      {error && <p className="form-error">{error}</p>}
    </Modal>
  );
}

/** Confirm + delete a user (destructive-consent modal). */
function DeleteUserModal({
  user,
  onClose,
  onDone,
}: {
  user: UserSummary;
  onClose: () => void;
  onDone: () => void;
}) {
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const submit = () => {
    setBusy(true);
    setError(null);
    api
      .deleteUser(user.id)
      .then(onDone)
      .catch((e: unknown) => {
        setError(errMsg(e, 'failed to delete user'));
        setBusy(false);
      });
  };

  return (
    <Modal
      title="Delete user"
      onClose={onClose}
      footer={
        <>
          <Button variant="outline" onClick={onClose}>
            Cancel
          </Button>
          <Button variant="danger" onClick={submit} disabled={busy}>
            Delete
          </Button>
        </>
      }
    >
      <p className="modal-confirm-text">
        Delete user <strong>{user.username}</strong>? Their audit-log entries remain for the
        record, but the account cannot be recovered.
      </p>
      {error && <p className="form-error">{error}</p>}
    </Modal>
  );
}
