// SPDX-License-Identifier: AGPL-3.0-only
// Users (Settings ▸ Users). Admin-only CRUD over local auth accounts, rendered
// as the "identity list" (data-table standard v2, variant B): a roomy row per account with a
// role-colored monogram, inline instant role change, account status, and hover row-actions
// (change password / enable-disable / delete). The server enforces ManageUsers (admin) and guards
// against removing/demoting/disabling the last admin (409 last_admin); the UI surfaces those typed
// errors and reverts. Passwords are write-only — the API never returns a hash.

import { useCallback, useEffect, useMemo, useState } from 'react';
import { Trans, useTranslation } from 'react-i18next';
import { api, errMsg, ApiError } from '../services/api';
import { useAuthStore } from '../store';
import { ROLES, type NodeGroup, type Role, type Scope, type UserKind, type UserSummary } from '../types/api';
import {
  canHoldScope,
  sameScope,
  scopeFromSelection,
  scopeGroupIds,
  scopeLabelKey,
} from './userScope';

/** The account kinds an admin can create here.
 *
 *  A deliberate subset of `USER_KINDS`, the way `monitorKinds.ts` is a subset of `NodeKind`: an
 *  `oidc` account is provisioned by someone signing in through the identity provider, and the API
 *  refuses to create one directly. Offering it would be a choice that always fails. */
const CREATABLE_USER_KINDS = ['local', 'service'] as const satisfies readonly UserKind[];

type CreatableUserKind = (typeof CREATABLE_USER_KINDS)[number];
import { dateOnly, relativeTime } from '../lib/format';
import { PageHeader } from '../components/ui/PageHeader';
import { Card } from '../components/ui/Card';
import { Button } from '../components/ui/Button';
import { ConfirmDeleteModal } from '../components/ui/ConfirmDeleteModal';
import { Modal } from '../components/ui/Modal';
import { TextInput, Select, RequiredMark } from '../components/ui/Field';
import { OverflowMenu } from '../components/ui/OverflowMenu';
import { TableToolbar, TableSpacer, ResultCount } from '../components/ui/TableToolbar';
import { ClearFilters } from '../components/ui/ClearFilters';
import { FilterBar } from '../components/ui/FilterBar';
import { MobileFilterButton, MobileFilterSheet } from '../components/ui/MobileFilterSheet';
import { defaultFilters, type FilterState } from '../lib/columnFilter';
import { facetCounts } from '../lib/filterCounts';
import { buildPredicate } from '../lib/filterPredicate';
import { userColumns, userFilterLabels } from './userFilters';
import { Monogram } from '../components/ui/tableCells';
import { KeyIcon, TrashIcon, PowerIcon, BoxIcon } from '../components/ui/icons';
import './UsersPage.css';

const MIN_PW = 8;
// The role *filter* moved to `pages/userFilters.ts` (ADR-053 Inc.6). It used to be a segmented
// radio group built from a `['all', ...ROLES.reverse()]` list here — one-of-four by construction, so
// "operators and admins" could not be asked for. `ROLES` is still imported: the per-row role
// dropdown below is a different control, and that one really is single-valued.

export function UsersPage() {
  const { t } = useTranslation('access');
  const authed = useAuthStore((s) => s.authed);
  const [rows, setRows] = useState<UserSummary[]>([]);
  const [me, setMe] = useState<string | null>(null);
  const filterCols = useMemo(() => userColumns(t), [t]);
  const filterLabels = useMemo(() => userFilterLabels(t), [t]);
  const [filters, setFilters] = useState<FilterState>(() => defaultFilters(filterCols));
  const [sheet, setSheet] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [unavailable, setUnavailable] = useState(false);
  const [forbidden, setForbidden] = useState(false);
  const [loading, setLoading] = useState(true);
  // Open dialogs: add form, and the user targeted by a password change / delete.
  const [adding, setAdding] = useState(false);
  const [pwUser, setPwUser] = useState<UserSummary | null>(null);
  const [delUser, setDelUser] = useState<UserSummary | null>(null);
  const [scopeUser, setScopeUser] = useState<UserSummary | null>(null);

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

  const list = useMemo(
    () => rows.filter(buildPredicate(filterCols, filters, Date.now())),
    [rows, filterCols, filters],
  );
  const facets = useMemo(
    () =>
      Object.fromEntries(
        filterCols
          .filter((c) => c.filter.kind === 'enum')
          .map((c) => [c.key, facetCounts(rows, filterCols, filters, c.key, Date.now())]),
      ),
    [rows, filterCols, filters],
  );

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
            <MobileFilterButton
              columns={filterCols}
              filters={filters}
              onOpen={() => setSheet(true)}
            />
            <ClearFilters
              columns={filterCols}
              filters={filters}
              onClear={() => setFilters(defaultFilters(filterCols))}
            />
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

          {/* The identity list is a card per account with no header row, so the controls carry
              their own names rather than sitting under columns that do not exist (ADR-053 Inc.6
              decision E). Role went from a one-of-three segmented control to a set, which is what
              makes "operators and admins" — the accounts that can change anything — sayable. */}
          <FilterBar
            columns={filterCols}
            labels={filterLabels}
            filters={filters}
            onChange={setFilters}
            counts={facets}
          />
          {sheet && (
            <MobileFilterSheet
              columns={filterCols}
              labels={filterLabels}
              filters={filters}
              onChange={setFilters}
              counts={facets}
              onClose={() => setSheet(false)}
            />
          )}

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
                const last = relativeTime(u.last_login_at ?? null);
                return (
                  <div className={u.enabled ? 'il-row' : 'il-row is-muted'} key={u.id}>
                    <Monogram name={u.username} role={u.role} lg />
                    <div className="il-id">
                      <div className="il-line1">
                        <span className="il-name">{u.username}</span>
                        {me === u.username && <span className="you-pill">{t('users.you')}</span>}
                        {/* One badge driven by the kind, not a branch per kind: LDAP was the third
                            member and would have been the third `===` comparison. Both strings are
                            runtime keys, so `i18nEnumKeys.test.ts` demands EN and JA for any kind
                            added later — which EN/JA parity alone would not (a new kind is missing
                            from both locales, so parity passes and the badge shows a raw key). */}
                        {u.auth_source !== 'local' && (
                          <span className="you-pill" title={t(`users.kindHint.${u.auth_source}`)}>
                            {t(`users.kind.${u.auth_source}`)}
                          </span>
                        )}
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
                        <span className="il-meta-sep">·</span>
                        <ScopeSummary scope={u.scope} />
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
                          <OverflowMenu
                            actions={[
                              // Offered only where it can succeed: the API refuses to scope an
                              // admin (409 `admin_is_unscoped`) because admin permissions are
                              // fleet-wide, so showing the action there is showing a button that
                              // must fail.
                              ...(canHoldScope(u.role)
                                ? [
                                    {
                                      label: t('users.action.changeScope'),
                                      icon: <BoxIcon />,
                                      onClick: () => setScopeUser(u),
                                    },
                                  ]
                                : []),
                              {
                                label: u.enabled
                                  ? t('users.action.disable')
                                  : t('users.action.enable'),
                                icon: <PowerIcon />,
                                onClick: () => toggleEnabled(u),
                              },
                              {
                                label: t('users.action.changePassword'),
                                icon: <KeyIcon />,
                                onClick: () => setPwUser(u),
                              },
                              {
                                label: t('users.action.delete'),
                                icon: <TrashIcon />,
                                danger: true,
                                onClick: () => setDelUser(u),
                              },
                            ]}
                          />
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
      {scopeUser && (
        <ChangeScopeModal
          user={scopeUser}
          onClose={() => setScopeUser(null)}
          onDone={() => {
            setScopeUser(null);
            load();
          }}
        />
      )}
    </div>
  );
}

/** One account's visibility, as a single meta line. Reads its wording off `scopeLabelKey`, so the
 *  list, the modal and the account menu cannot describe the same scope differently. */
function ScopeSummary({ scope }: { scope: Scope }) {
  const { t } = useTranslation('access');
  const { key, n } = scopeLabelKey(scope);
  return <span>{t(`users.scope.${key}`, { count: n })}</span>;
}

/** Create a new account (focused-editing modal). */
function AddUserModal({ onClose, onDone }: { onClose: () => void; onDone: () => void }) {
  const { t } = useTranslation('access');
  const [username, setUsername] = useState('');
  const [password, setPassword] = useState('');
  const [role, setRole] = useState<Role>('viewer');
  // `oidc` is excluded: those accounts appear by signing in through the IdP, and the API refuses to
  // create one by hand. Offering it would be a choice that always fails.
  const [kind, setKind] = useState<CreatableUserKind>('local');
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  // A service account has no password to validate — it cannot sign in at all.
  const valid =
    username.trim().length > 0 && (kind === 'service' || password.length >= MIN_PW);

  const submit = () => {
    if (!valid) return;
    setBusy(true);
    setError(null);
    api
      .createUser({
        username: username.trim(),
        role,
        kind,
        // Omitted for a service account: the API rejects a password there rather than discarding
        // it, so that an admin cannot come away believing they set one.
        password: kind === 'service' ? undefined : password,
      })
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
        <label className="modal-field-label">{t('users.field.kind')}</label>
        <Select
          value={kind}
          onChange={(e) => setKind(e.target.value as CreatableUserKind)}
        >
          {CREATABLE_USER_KINDS.map((k) => (
            <option key={k} value={k}>
              {t(`users.kind.${k}`)}
            </option>
          ))}
        </Select>
        <span className="modal-hint">{t('users.field.kindHint')}</span>
      </div>
      {kind !== 'service' && (
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
      )}
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

/** Limit an account to a set of node groups, or restore fleet-wide visibility.
 *
 *  Ticking nothing means "the whole fleet" (`"All"`), not "nothing" — see `userScope.ts` for why
 *  those two must never be spelled the same way. Every judgement here lives in that module; this
 *  component is the form around it. */
function ChangeScopeModal({
  user,
  onClose,
  onDone,
}: {
  user: UserSummary;
  onClose: () => void;
  onDone: () => void;
}) {
  const { t } = useTranslation('access');
  const [groups, setGroups] = useState<NodeGroup[] | null>(null);
  const [selected, setSelected] = useState<string[]>(() => scopeGroupIds(user.scope));
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    api
      .listNodeGroups()
      .then(setGroups)
      .catch((e: unknown) => {
        setGroups([]);
        setError(errMsg(e, t('users.err.loadGroups')));
      });
  }, [t]);

  const next = scopeFromSelection(selected);
  // Saving revokes every session the account holds, so an unchanged selection must not be savable:
  // an admin who opens this and clicks Save would otherwise sign that person out for nothing.
  const changed = !sameScope(next, user.scope);

  const submit = () => {
    if (!changed) return;
    setBusy(true);
    setError(null);
    api
      .setUserScope(user.id, next)
      .then(onDone)
      .catch((e: unknown) => {
        setError(errMsg(e, t('users.err.changeScope')));
        setBusy(false);
      });
  };

  return (
    <Modal
      title={t('users.scopeModal.title', { name: user.username })}
      onClose={onClose}
      footer={
        <>
          <Button variant="outline" onClick={onClose} disabled={busy}>
            {t('common:actions.cancel')}
          </Button>
          <Button variant="primary" onClick={submit} disabled={!changed || busy}>
            {t('common:actions.save')}
          </Button>
        </>
      }
    >
      <p className="modal-hint users-scope-intro">{t('users.scopeModal.intro')}</p>
      <div className="modal-field">
        <label className="modal-field-label">{t('users.scopeModal.groups')}</label>
        {groups === null ? (
          <p className="muted">{t('common:loading')}</p>
        ) : groups.length === 0 ? (
          <p className="muted">{t('users.scopeModal.noGroups')}</p>
        ) : (
          <div className="users-scope-list">
            {groups.map((g) => (
              <label key={g.id} className="users-scope-row">
                <input
                  type="checkbox"
                  checked={selected.includes(g.id)}
                  onChange={(e) =>
                    setSelected((prev) =>
                      e.target.checked ? [...prev, g.id] : prev.filter((x) => x !== g.id),
                    )
                  }
                />
                <span className="users-scope-name">{g.name}</span>
              </label>
            ))}
          </div>
        )}
        <span className="modal-hint">
          {selected.length === 0
            ? t('users.scopeModal.hintAll')
            : t('users.scopeModal.hintGroups', { count: selected.length })}{' '}
          {t('users.scopeModal.revokes')}
        </span>
      </div>
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
  return (
    <ConfirmDeleteModal
      title={t('users.delete.title')}
      onConfirm={() => api.deleteUser(user.id)}
      errorFallback={t('users.err.delete')}
      onClose={onClose}
      onDone={onDone}
    >
      <Trans
        t={t}
        i18nKey="users.delete.confirm"
        values={{ name: user.username }}
        components={{ strong: <strong /> }}
      />
    </ConfirmDeleteModal>
  );
}
