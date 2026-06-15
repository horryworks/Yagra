// Users & roles (Settings ▸ Users & roles). Admin-only CRUD over local auth accounts, rendered
// as the "identity list" (data-table standard v2, variant B): a roomy row per account with a
// role-colored monogram, inline instant role change, account status, and hover row-actions
// (change password / enable-disable / delete). The server enforces ManageUsers (admin) and guards
// against removing/demoting/disabling the last admin (409 last_admin); the UI surfaces those typed
// errors and reverts. Passwords are write-only — the API never returns a hash.

import { useCallback, useEffect, useMemo, useState } from 'react';
import { api, ApiError } from '../services/api';
import { useAuthStore } from '../store';
import type { Role, UserSummary } from '../types/api';
import { dateOnly, relativeTime } from '../lib/format';
import { PageHeader } from '../components/ui/PageHeader';
import { Card } from '../components/ui/Card';
import { Button } from '../components/ui/Button';
import { Modal } from '../components/ui/Modal';
import { TextInput, Select, RequiredMark } from '../components/ui/Field';
import { IconButton } from '../components/ui/IconButton';
import { TableToolbar, SearchInput, TableSpacer, ResultCount } from '../components/ui/TableToolbar';
import { Monogram } from '../components/ui/tableCells';
import { KeyIcon, TrashIcon, PowerIcon } from '../components/ui/icons';
import './UsersPage.css';

const ROLES: Role[] = ['viewer', 'operator', 'admin'];
const MIN_PW = 8;
const SEGMENTS: [string, string][] = [
  ['all', 'All'],
  ['admin', 'Admins'],
  ['operator', 'Operators'],
  ['viewer', 'Viewers'],
];

const errMsg = (e: unknown, fallback: string) =>
  e instanceof ApiError ? e.message : fallback;

export function UsersPage() {
  const authed = useAuthStore((s) => s.authed);
  const [rows, setRows] = useState<UserSummary[]>([]);
  const [me, setMe] = useState<string | null>(null);
  const [query, setQuery] = useState('');
  const [roleFilter, setRoleFilter] = useState('all');
  const [error, setError] = useState<string | null>(null);
  const [unavailable, setUnavailable] = useState(false);
  const [forbidden, setForbidden] = useState(false);
  const [loading, setLoading] = useState(true);
  // Open dialogs: add form, and the user targeted by a password change / delete.
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
    api.me().then((m) => setMe(m.username)).catch(() => setMe(null));
  }, [load]);

  const changeRole = (id: string, next: Role) => {
    setError(null);
    api
      .setUserRole(id, next)
      .then(load)
      .catch((e: unknown) => {
        setError(errMsg(e, 'failed to change role'));
        load(); // revert the optimistic-looking select to the server's truth
      });
  };

  const toggleEnabled = (u: UserSummary) => {
    setError(null);
    api
      .setUserEnabled(u.id, !u.enabled)
      .then(load)
      .catch((e: unknown) => setError(errMsg(e, 'failed to change account status')));
  };

  const list = useMemo(() => {
    const q = query.trim().toLowerCase();
    return rows.filter(
      (u) =>
        (q === '' || u.username.toLowerCase().includes(q)) &&
        (roleFilter === 'all' || u.role === roleFilter),
    );
  }, [rows, query, roleFilter]);

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
        <>
          <TableToolbar>
            <SearchInput
              value={query}
              onChange={setQuery}
              placeholder="Search username…"
              ariaLabel="Search users"
            />
            <div className="segmented" role="group" aria-label="Filter by role">
              {SEGMENTS.map(([key, label]) => (
                <button
                  key={key}
                  className={roleFilter === key ? 'on' : ''}
                  onClick={() => setRoleFilter(key)}
                >
                  {label}
                </button>
              ))}
            </div>
            <TableSpacer />
            <ResultCount shown={list.length} total={rows.length} noun="users" />
            {authed && (
              <Button variant="primary" onClick={() => setAdding(true)}>
                + Add user
              </Button>
            )}
          </TableToolbar>

          {error && <p className="form-error users-error">{error}</p>}

          <div className="identity-list">
            {list.length === 0 ? (
              <div className="il-empty">
                {loading ? 'Loading…' : rows.length === 0 ? 'No users yet.' : 'No users match.'}
              </div>
            ) : (
              list.map((u) => {
                const last = relativeTime(u.last_login_at);
                return (
                  <div className={u.enabled ? 'il-row' : 'il-row is-muted'} key={u.id}>
                    <Monogram name={u.username} role={u.role} lg />
                    <div className="il-id">
                      <div className="il-line1">
                        <span className="il-name">{u.username}</span>
                        {me === u.username && <span className="you-pill">You</span>}
                        <span className={u.enabled ? 'status-pill active' : 'status-pill disabled'}>
                          <span className="yt-status-dot" />
                          {u.enabled ? 'Active' : 'Disabled'}
                        </span>
                      </div>
                      <div className="il-line2">
                        <span>Created {dateOnly(u.created_at)}</span>
                        <span className="il-meta-sep">·</span>
                        <span title={u.last_login_at ?? undefined}>Last login {last}</span>
                      </div>
                    </div>
                    <div className="il-right">
                      {authed ? (
                        <select
                          className={`role-select role-${u.role}`}
                          value={u.role}
                          onChange={(e) => changeRole(u.id, e.target.value as Role)}
                          aria-label={`Role for ${u.username}`}
                        >
                          {ROLES.map((r) => (
                            <option key={r} value={r}>
                              {r}
                            </option>
                          ))}
                        </select>
                      ) : (
                        <span className="muted">{u.role}</span>
                      )}
                      {authed && (
                        <div className="il-actions">
                          <IconButton
                            title={u.enabled ? 'Disable account' : 'Enable account'}
                            onClick={() => toggleEnabled(u)}
                          >
                            <PowerIcon />
                          </IconButton>
                          <IconButton title="Change password" onClick={() => setPwUser(u)}>
                            <KeyIcon />
                          </IconButton>
                          <IconButton title="Delete user" danger onClick={() => setDelUser(u)}>
                            <TrashIcon />
                          </IconButton>
                        </div>
                      )}
                    </div>
                  </div>
                );
              })
            )}
          </div>
        </>
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
          <Button variant="outline" onClick={onClose} disabled={busy}>
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
          <Button variant="outline" onClick={onClose} disabled={busy}>
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
          <Button variant="outline" onClick={onClose} disabled={busy}>
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
