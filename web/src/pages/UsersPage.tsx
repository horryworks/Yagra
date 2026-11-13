// SPDX-License-Identifier: AGPL-3.0-only
// Users (Settings ▸ Users). Admin-only CRUD over local auth accounts, drawn as a `DataTable` like
// every other list (ADR-246 Inc.5): one row per account with a role-colored monogram, how it signs
// in, its status, an inline instant role change, and row actions (scope / enable-disable /
// password / delete). It used to be the "identity list" (variant B), a two-line card per account
// with no header row. The server enforces ManageUsers (admin) and guards against
// removing/demoting/disabling the last admin (409 last_admin); the UI surfaces those typed errors
// and reverts. Passwords are write-only — the API never returns a hash.

import { useCallback, useEffect, useMemo, useState } from 'react';
import { Trans, useTranslation } from 'react-i18next';
import { api, errMsg } from '../services/api';
import { useCan } from '../store';
import { ROLES, type NodeGroup, type Role, type Scope, type UserSummary } from '../types/api';
import {
  canHoldScope,
  sameScope,
  scopeFromSelection,
  scopeGroupIds,
  scopeLabelKey,
} from './userScope';
import { CREATABLE_USER_KINDS, kindInfoKey, type CreatableUserKind } from './userKinds';
import { dateOnly, relativeTime } from '../lib/format';
import { PageHeader } from '../components/ui/PageHeader';
import { Button } from '../components/ui/Button';
import { EmptyState } from '../components/ui/EmptyState';
import { ConfirmDeleteModal } from '../components/ui/ConfirmDeleteModal';
import { Modal } from '../components/ui/Modal';
import { Field, TextInput, Select } from '../components/ui/Field';
import { FieldGroup, Fields } from '../components/ui/FormLayout';
import { SelectAllBox } from '../components/ui/SelectAllBox';
import { Callout } from '../components/ui/Section';
import { OverflowMenu } from '../components/ui/OverflowMenu';
import { ListToolbar } from '../components/ui/ListToolbar';
import { columnLabels, serverToolbarFilters } from '../lib/listToolbar';
import { Page } from '../components/ui/Page';
import { DataTable, type Column } from '../components/ui/DataTable';
import { ClippedText } from '../components/ui/ClippedText';
import { rowActionsWidth } from '../lib/rowActions';

import { useFilterParams } from '../lib/useFilterParams';
import { facetCounts } from '../lib/filterCounts';
import { buildPredicate } from '../lib/filterPredicate';
import { userColumns } from './userFilters';
import { Monogram } from '../components/ui/tableCells';
import { InfoPress } from '../components/ui/InfoTip';
import { KeyIcon, TrashIcon, PowerIcon, BoxIcon } from '../components/ui/icons';
import { useLoad } from '../lib/useLoad';
// The floor is shared with the account badge's own change-password dialog — one literal, not
// three (ADR-122 decision 8).
import { MIN_PW } from '../lib/password';
import { LoadGate } from '../components/ui/LoadGate';
import './UsersPage.css';
import { done } from '../lib/submitState';
import { useSubmit } from '../lib/useSubmit';
import { FormError, FormFooter } from '../components/ui/FormFooter';

// The role *filter* moved to `pages/userFilters.ts` (ADR-053 Inc.6). It used to be a segmented
// radio group built from a `['all', ...ROLES.reverse()]` list here — one-of-four by construction, so
// "operators and admins" could not be asked for. `ROLES` is still imported: the per-row role
// dropdown below is a different control, and that one really is single-valued.

export function UsersPage() {
  const { t } = useTranslation('access');
  const canUsers = useCan('manage_users');
  const [me, setMe] = useState<string | null>(null);
  const filterCols = useMemo(() => userColumns(t), [t]);
  // In the URL (ADR-153), so a narrowed list survives a reload.
  const { filters, setFilters } = useFilterParams(filterCols);
  const [error, setError] = useState<string | null>(null);
  // Open dialogs: add form, and the user targeted by a password change / delete.
  const [adding, setAdding] = useState(false);
  const [pwUser, setPwUser] = useState<UserSummary | null>(null);
  const [delUser, setDelUser] = useState<UserSummary | null>(null);
  const [scopeUser, setScopeUser] = useState<UserSummary | null>(null);

  const users = useLoad(() => api.listUsers(), [], { initial: [] as UserSummary[] });
  const { data: rows, loading, reload: load } = users;

  useEffect(() => {
    api.me().then((m) => setMe(m.username)).catch(() => setMe(null));
  }, []);

  const changeRole = useCallback(
    (id: string, next: Role) => {
      setError(null);
      api
        .setUserRole(id, next)
        .then(load)
        .catch((e: unknown) => {
          setError(errMsg(e, t('users.err.changeRole')));
          load(); // revert the optimistic-looking select to the server's truth
        });
    },
    [load, t],
  );

  const toggleEnabled = useCallback(
    (u: UserSummary) => {
      setError(null);
      api
        .setUserEnabled(u.id, !u.enabled)
        .then(load)
        .catch((e: unknown) => setError(errMsg(e, t('users.err.changeStatus'))));
    },
    [load, t],
  );

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

  // Each filter sits on the column it narrows, so the column key IS the URL key (`q`, `kind`,
  // `status`, `role`), unchanged from the toolbar funnel these filters used to open from.
  const columns = useMemo(() => {
    const cols: Column<UserSummary>[] = [
      {
        key: 'q',
        header: t('users.cols.user'),
        width: '1fr',
        render: (u) => (
          <span className="yt-user">
            <Monogram name={u.username} role={u.role} />
            <ClippedText text={u.username} />
            {me === u.username && <span className="you-pill">{t('users.you')}</span>}
          </span>
        ),
      },
      {
        key: 'kind',
        header: t('users.cols.kind'),
        // Sized for the widest kind badge, "Service account" with its ▾.
        width: '160px',
        render: (u) => <KindBadge kind={u.auth_source} />,
      },
      {
        key: 'status',
        header: t('users.cols.status'),
        width: '110px',
        render: (u) => (
          <span className={u.enabled ? 'status-pill active' : 'status-pill disabled'}>
            <span className="yt-status-dot" />
            {u.enabled ? t('users.status.active') : t('users.status.disabled')}
          </span>
        ),
      },
      {
        key: 'role',
        header: t('users.cols.role'),
        width: '140px',
        render: (u) =>
          canUsers ? (
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
          ),
      },
      {
        key: 'last_login',
        header: t('users.cols.lastLogin'),
        width: '100px',
        render: (u) => (
          <span title={u.last_login_at ?? undefined}>{relativeTime(u.last_login_at ?? null)}</span>
        ),
      },
      {
        key: 'created',
        header: t('users.cols.created'),
        width: '100px',
        render: (u) => dateOnly(u.created_at),
      },
      {
        key: 'scope',
        header: t('users.cols.scope'),
        width: '120px',
        render: (u) => <ScopeSummary scope={u.scope} />,
      },
    ];
    if (canUsers) {
      cols.push({
        key: 'actions',
        header: t('users.cols.actions'),
        // Scope, enable/disable, password and delete; an admin's row draws three.
        width: rowActionsWidth(4),
        align: 'right',
        render: (u) => (
          <span className="ytable-actions">
            <OverflowMenu
              actions={[
                // Offered only where it can succeed: the API refuses to scope an admin (409
                // `admin_is_unscoped`) because admin permissions are fleet-wide, so showing the
                // action there is showing a button that must fail.
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
                  label: u.enabled ? t('users.action.disable') : t('users.action.enable'),
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
          </span>
        ),
      });
    }
    for (const c of cols) c.filter = filterCols.find((f) => f.key === c.key)?.filter;
    return cols;
  }, [t, me, canUsers, filterCols, changeRole, toggleEnabled]);

  return (
    <Page layout="list">
      <PageHeader
        title={t('nav:settings.users')}
        trail={[{ label: t('nav:sections.settings') }, { label: t('nav:settings.users') }]}
      />

      <LoadGate
        load={users}
        // Kept rather than falling back to the shared sentence: this one already names the role
        // that would let you in, which is what ADR-056 Increment 2 will give every screen.
        forbidden={t('users.forbidden')}
      >
        <ListToolbar
          list={serverToolbarFilters(filterCols, { filters, setFilters }, undefined, facets)}
          labels={columnLabels(columns)}
          count={{
            shown: list.length,
            total: rows.length,
            noun: (n) => t('common:noun.user', { count: n }),
          }}
        >
          {canUsers && (
            <Button variant="primary" onClick={() => setAdding(true)}>
              + {t('users.actions.addUser')}
            </Button>
          )}
        </ListToolbar>

        {error && <p className="form-error">{error}</p>}

        <DataTable
          tableId="settings.users"
          rows={list}
          columns={columns}
          rowKey={(u) => u.id}
          rowClass={(u) => (u.enabled ? undefined : 'is-muted')}
          filters={filters}
          onFiltersChange={setFilters}
          filterCounts={facets}
          loading={loading}
          empty={
            rows.length === 0 ? (
              <EmptyState
                text={t('users.empty.none')}
                action={
                  canUsers && (
                    <Button variant="primary" onClick={() => setAdding(true)}>
                      + {t('users.actions.addUser')}
                    </Button>
                  )
                }
              />
            ) : (
              t('users.empty.filtered')
            )
          }
        />
      </LoadGate>

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
    </Page>
  );
}

/** How an account signs in. A plain local account is the default and reads as a word; every other
 *  kind is a badge that explains itself when pressed — who holds the password and who decides the
 *  role (`USER_KIND_INFO`), since a hover could not be read on touch.
 *
 *  One badge driven by the kind, not a branch per kind: LDAP was the third member and would have
 *  been the third `===` comparison. The label is a runtime key, so `i18nEnumKeys.test.ts` demands
 *  EN and JA for any kind added later. */
function KindBadge({ kind }: { kind: string }) {
  const { t } = useTranslation('access');
  const label = t(`users.kind.${kind}`);
  if (kind === 'local') return <span>{label}</span>;
  const info = kindInfoKey(kind);
  return info ? (
    <InfoPress infoKey={info} className="you-pill users-kind">
      {label}
    </InfoPress>
  ) : (
    // A kind this build has no word for shows its raw label, which can outrun the column: the pill
    // is a flex box, which never draws an ellipsis, so the words go in a block that does.
    <span className="you-pill">
      <ClippedText text={label} />
    </span>
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
  const form = useSubmit({ errorFallback: t('users.err.create'), onDone });
  // A service account has no password to validate — it cannot sign in at all.
  const valid =
    username.trim().length > 0 && (kind === 'service' || password.length >= MIN_PW);

  const submit = () => {
    if (!valid) return;
    form.submit(() =>
      api
        .createUser({
          username: username.trim(),
          role,
          kind,
          // Omitted for a service account: the API rejects a password there rather than discarding
          // it, so that an admin cannot come away believing they set one.
          password: kind === 'service' ? undefined : password,
        })
        .then(() => done()),
    );
  };

  return (
    <Modal
      title={t('users.actions.addUser')}
      onClose={onClose}
      footer={
        <FormFooter
          form={form}
          onClose={onClose}
          onSubmit={submit}
          submitLabel={t('users.actions.addUser')}
          canSubmit={valid}
        />
      }
    >
      <Fields>
        <Field label={t('users.field.username')} required>
          <TextInput
            value={username}
            onChange={(e) => setUsername(e.target.value)}
            autoComplete="off"
            autoFocus
          />
        </Field>
        <Field label={t('users.field.kind')}>
          <Select value={kind} onChange={(e) => setKind(e.target.value as CreatableUserKind)}>
            {/* The sub-label says what the kind is for, in the option itself (ADR-200 kind 5). */}
            {CREATABLE_USER_KINDS.map((k) => (
              <option key={k} value={k}>
                {t(`users.kind.${k}`)} — {t(`users.kindSub.${k}`)}
              </option>
            ))}
          </Select>
        </Field>
        {kind !== 'service' && (
          <Field label={t('users.field.password', { min: MIN_PW })} required>
            <TextInput
              type="password"
              value={password}
              onChange={(e) => setPassword(e.target.value)}
              autoComplete="new-password"
            />
          </Field>
        )}
        <Field label={t('users.field.role')}>
          <Select value={role} onChange={(e) => setRole(e.target.value as Role)}>
            {/* What each role may do, in a word beside its name; Settings > Roles has the matrix. */}
            {ROLES.map((r) => (
              <option key={r} value={r}>
                {t(`role.${r}`)} — {t(`users.roleSub.${r}`)}
              </option>
            ))}
          </Select>
        </Field>
        <FormError form={form} />
      </Fields>
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
  const form = useSubmit({ errorFallback: t('users.err.changePw'), onDone });
  const tooShort = password.length < MIN_PW;
  const mismatch = confirm.length > 0 && password !== confirm;
  const valid = !tooShort && password === confirm;

  const submit = () => {
    if (!valid) return;
    form.submit(() =>
      api
        .setUserPassword(user.id, password)
        .then(() => done()),
    );
  };

  return (
    <Modal
      title={t('users.changePw.title', { name: user.username })}
      onClose={onClose}
      footer={
        <FormFooter
          form={form}
          onClose={onClose}
          onSubmit={submit}
          submitLabel={t('common:actions.save')}
          canSubmit={valid}
        />
      }
    >
      <Fields>
        <Field label={t('users.changePw.newPassword', { min: MIN_PW })}>
          <TextInput
            type="password"
            value={password}
            onChange={(e) => setPassword(e.target.value)}
            autoComplete="new-password"
            autoFocus
          />
        </Field>
        <Field
          label={t('users.changePw.confirm')}
          error={mismatch ? t('users.changePw.mismatch') : null}
        >
          <TextInput
            type="password"
            value={confirm}
            onChange={(e) => setConfirm(e.target.value)}
            autoComplete="new-password"
          />
        </Field>
        <FormError form={form} />
      </Fields>
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
  const form = useSubmit({ errorFallback: t('users.err.changeScope'), onDone });
  // The folder list failing to load is said on the same line a failed save is.
  const { refuse } = form;

  useEffect(() => {
    api
      .listNodeGroups()
      .then(setGroups)
      .catch((e: unknown) => {
        setGroups([]);
        refuse(errMsg(e, t('users.err.loadGroups')));
      });
  }, [t, refuse]);

  const next = scopeFromSelection(selected);
  // Saving revokes every session the account holds, so an unchanged selection must not be savable:
  // an admin who opens this and clicks Save would otherwise sign that person out for nothing.
  const changed = !sameScope(next, user.scope);

  const submit = () => {
    if (!changed) return;
    form.submit(() =>
      api
        .setUserScope(user.id, next)
        .then(() => done()),
    );
  };

  return (
    <Modal
      title={t('users.scopeModal.title', { name: user.username })}
      onClose={onClose}
      footer={
        <FormFooter
          form={form}
          onClose={onClose}
          onSubmit={submit}
          submitLabel={t('common:actions.save')}
          canSubmit={changed}
        />
      }
    >
      <Fields>
        {/* A label over a set of boxes, not over one control - a `FieldGroup`. The boxes stay a
            bounded, scrolling pick list (`users-scope-list`, shared with the token dialog): the
            folder tree grows with the estate, and each row is a hover band of touch height, which
            a `CheckField` does not draw. */}
        <FieldGroup label={t('users.scopeModal.groups')}>
          {groups === null ? (
            <p className="muted">{t('common:loading')}</p>
          ) : groups.length === 0 ? (
            <p className="muted">{t('users.scopeModal.noGroups')}</p>
          ) : (
            <div className="users-scope-list">
              {/* Every folder, or none (ADR-256 decision 10). None ticked means the whole fleet,
                  which the line under the list says. */}
              <SelectAllBox
                visibleKeys={groups.map((g) => g.id)}
                selected={new Set(selected)}
                onChange={(next) => setSelected(groups.map((g) => g.id).filter((id) => next.has(id)))}
                className="users-scope-row users-scope-head"
              >
                <span className="users-scope-name">{t('common:select.all')}</span>
              </SelectAllBox>
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
          <p className="form-status">
            {selected.length === 0
              ? t('users.scopeModal.hintAll')
              : t('users.scopeModal.hintGroups', { count: selected.length })}
          </p>
        </FieldGroup>
        <Callout tone="warning">{t('users.scopeModal.revokes')}</Callout>
        <FormError form={form} />
      </Fields>
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
