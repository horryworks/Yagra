// Users (Settings ▸ Users). Admin-only CRUD over local auth accounts, rendered
// as the "identity list" (data-table standard v2, variant B): a roomy row per account with a
// role-colored monogram, inline instant role change, account status, and hover row-actions
// (change password / enable-disable / delete). The server enforces ManageUsers (admin) and guards
// against removing/demoting/disabling the last admin (409 last_admin); the UI surfaces those typed
// errors and reverts. Passwords are write-only — the API never returns a hash.

import { useCallback, useEffect, useMemo, useState } from 'react';
import { Trans, useTranslation } from 'react-i18next';
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
// Role filter segments. The filter key drives the query; the label is resolved with `t` in render.
const SEGMENTS = ['all', 'admin', 'operator', 'viewer'] as const;

const errMsg = (e: unknown, fallback: string) =>
  e instanceof ApiError ? e.message : fallback;

export function UsersPage() {
  const { t } = useTranslation('access');
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
        setError(errMsg(e, t('users.err.changeRole')));
        load(); // revert the optimistic-looking select to the server's truth
      });
  };

  const toggleEnabled = (u: UserSummary) => {
    setError(null);
    api
      .setUserEnabled(u.id, !u.enabled)
      .then(load)
      .catch((e: unknown) => setError(errMsg(e, t('users.err.changeStatus'))));
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
        title={t('nav:settings.users')}
        trail={[{ label: t('nav:sections.settings') }, { label: t('nav:settings.users') }]}
        note={t('users.note')}
      />

      {unavailable ? (
        <Card>
          <p className="muted">{t('users.unavailable')}</p>
        </Card>
      ) : forbidden ? (
        <Card>
          <p className="muted">{t('users.forbidden')}</p>
        </Card>
      ) : (
        <>
          <TableToolbar>
            <SearchInput
              value={query}
              onChange={setQuery}
              placeholder={t('users.searchPlaceholder')}
              ariaLabel={t('users.searchAria')}
            />
            <div className="segmented" role="group" aria-label={t('users.filterAria')}>
              {SEGMENTS.map((key) => (
                <button
                  key={key}
                  className={roleFilter === key ? 'on' : ''}
                  onClick={() => setRoleFilter(key)}
                >
                  {t(`users.filter.${key}`)}
                </button>
              ))}
            </div>
            <TableSpacer />
            <ResultCount
              shown={list.length}
              total={rows.length}
              noun={t('common:noun.user', { count: rows.length })}
            />
            {authed && (
              <Button variant="primary" onClick={() => setAdding(true)}>
                + {t('users.actions.addUser')}
              </Button>
            )}
          </TableToolbar>

          {error && <p className="form-error users-error">{error}</p>}

          <div className="identity-list">
            {list.length === 0 ? (
              <div className="il-empty">
                {loading
                  ? t('common:loading')
                  : rows.length === 0
                    ? t('users.empty.none')
                    : t('users.empty.filtered')}
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
                        {me === u.username && <span className="you-pill">{t('users.you')}</span>}
                        <span className={u.enabled ? 'status-pill active' : 'status-pill disabled'}>
                          <span className="yt-status-dot" />
                          {u.enabled ? t('users.status.active') : t('users.status.disabled')}
                        </span>
                      </div>
                      <div className="il-line2">
                        <span>{t('users.created', { date: dateOnly(u.created_at) })}</span>
                        <span className="il-meta-sep">·</span>
                        <span title={u.last_login_at ?? undefined}>
                          {t('users.lastLogin', { time: last })}
                        </span>
                      </div>
                    </div>
                    <div className="il-right">
                      {authed ? (
                        <select
                          className={`role-select role-${u.role}`}
                          value={u.role}
                          onChange={(e) => changeRole(u.id, e.target.value as Role)}
                          aria-label={t('users.roleFor', { name: u.username })}
                        >
                          {ROLES.map((r) => (
                            <option key={r} value={r}>
                              {t(`role.${r}`)}
                            </option>
                          ))}
                        </select>
                      ) : (
                        <span className="muted">{t(`role.${u.role}`)}</span>
                      )}
                      {authed && (
                        <div className="il-actions">
                          <IconButton
                            title={u.enabled ? t('users.action.disable') : t('users.action.enable')}
                            onClick={() => toggleEnabled(u)}
                          >
                            <PowerIcon />
                          </IconButton>
                          <IconButton title={t('users.action.changePassword')} onClick={() => setPwUser(u)}>
                            <KeyIcon />
                          </IconButton>
                          <IconButton title={t('users.action.delete')} danger onClick={() => setDelUser(u)}>
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
  const { t } = useTranslation('access');
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
        setError(errMsg(e, t('users.err.create')));
        setBusy(false);
      });
  };

  return (
    <Modal
      title={t('users.actions.addUser')}
      onClose={onClose}
      footer={
        <>
          <Button variant="outline" onClick={onClose} disabled={busy}>
            {t('common:actions.cancel')}
          </Button>
          <Button variant="primary" onClick={submit} disabled={!valid || busy}>
            {t('users.actions.addUser')}
          </Button>
        </>
      }
    >
      <div className="modal-field">
        <label className="modal-field-label">
          {t('users.field.username')} <RequiredMark />
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
          {t('users.field.password', { min: MIN_PW })} <RequiredMark />
        </label>
        <TextInput
          type="password"
          value={password}
          onChange={(e) => setPassword(e.target.value)}
          autoComplete="new-password"
        />
      </div>
      <div className="modal-field">
        <label className="modal-field-label">{t('users.field.role')}</label>
        <Select value={role} onChange={(e) => setRole(e.target.value as Role)}>
          {ROLES.map((r) => (
            <option key={r} value={r}>
              {t(`role.${r}`)}
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
  const { t } = useTranslation('access');
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
        setError(errMsg(e, t('users.err.changePw')));
        setBusy(false);
      });
  };

  return (
    <Modal
      title={t('users.changePw.title', { name: user.username })}
      onClose={onClose}
      footer={
        <>
          <Button variant="outline" onClick={onClose} disabled={busy}>
            {t('common:actions.cancel')}
          </Button>
          <Button variant="primary" onClick={submit} disabled={!valid || busy}>
            {t('common:actions.save')}
          </Button>
        </>
      }
    >
      <div className="modal-field">
        <label className="modal-field-label">{t('users.changePw.newPassword', { min: MIN_PW })}</label>
        <TextInput
          type="password"
          value={password}
          onChange={(e) => setPassword(e.target.value)}
          autoComplete="new-password"
          autoFocus
        />
      </div>
      <div className="modal-field">
        <label className="modal-field-label">{t('users.changePw.confirm')}</label>
        <TextInput
          type="password"
          value={confirm}
          onChange={(e) => setConfirm(e.target.value)}
          autoComplete="new-password"
        />
      </div>
      {mismatch && <p className="form-error">{t('users.changePw.mismatch')}</p>}
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
  const { t } = useTranslation('access');
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const submit = () => {
    setBusy(true);
    setError(null);
    api
      .deleteUser(user.id)
      .then(onDone)
      .catch((e: unknown) => {
        setError(errMsg(e, t('users.err.delete')));
        setBusy(false);
      });
  };

  return (
    <Modal
      title={t('users.delete.title')}
      onClose={onClose}
      footer={
        <>
          <Button variant="outline" onClick={onClose} disabled={busy}>
            {t('common:actions.cancel')}
          </Button>
          <Button variant="danger" onClick={submit} disabled={busy}>
            {t('common:actions.delete')}
          </Button>
        </>
      }
    >
      <p className="modal-confirm-text">
        <Trans
          t={t}
          i18nKey="users.delete.confirm"
          values={{ name: user.username }}
          components={{ strong: <strong /> }}
        />
      </p>
      {error && <p className="form-error">{error}</p>}
    </Modal>
  );
}
