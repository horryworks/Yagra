// SPDX-License-Identifier: AGPL-3.0-only
// API tokens (Settings ▸ API tokens): long-lived bearer tokens for non-browser clients — in
// particular an AI/MCP client (Claude Code/Desktop) authenticating against the read-only MCP tool
// surface (ADR-028). ManageUsers-gated. The raw token is returned once on create and never again
// (only its hash is stored, security.md/ADR-018), so the create flow reveals it in a one-time modal.
//
// Data-table standard v2: a toolbar (New + count) over the shared `DataTable`; revoke is a per-row
// OverflowMenu action with a confirm modal. Modeled on AuditPage (table) + AuthSettingsPage (modals).

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import type { TFunction } from 'i18next';
import { api, errMsg, ApiError } from '../services/api';
import { useAuthStore, useCan } from '../store';
import { classifyLoadError, type LoadBlock } from '../lib/loadState';
import { LoadBlockNotice } from '../components/ui/LoadBlockNotice';
import {
  ROLES,
  TOKEN_SURFACES,
  type ApiTokenSummary,
  type CreatedApiToken,
  type NodeGroup,
  type Role,
  type TokenSurface,
  type UserSummary,
} from '../types/api';
import { scopeFromSelection, scopeLabelKey } from './userScope';
import './UsersPage.css';
import {
  canSubmit,
  daysUntilExpiry,
  DEFAULT_EXPIRY,
  EXPIRY_CHOICES,
  expiryFromChoice,
  ownerChoices,
  ownerIsScoped,
  toggleSurface,
  tokenState,
  type ExpiryChoice,
  type TokenState,
} from './tokenForm';
import { PageHeader } from '../components/ui/PageHeader';
import { Card } from '../components/ui/Card';
import { Button } from '../components/ui/Button';
import { ConfirmDeleteModal } from '../components/ui/ConfirmDeleteModal';
import { Badge } from '../components/ui/Badge';
import { Modal } from '../components/ui/Modal';
import { TextInput, Select } from '../components/ui/Field';
import { DataTable, type Column } from '../components/ui/DataTable';
import { sortRows, type SortState } from '../lib/tableSort';
import { TableToolbar, TableSpacer, ResultCount } from '../components/ui/TableToolbar';
import { ClearFilters } from '../components/ui/ClearFilters';
import { FilterButton, MobileFilterSheet } from '../components/ui/MobileFilterSheet';
import { useClientFilters } from '../lib/useClientFilters';
import { DEFAULT_TOKEN_SORT, tokenFilters, tokenSortValues } from './apiTokenFilters';
import { TimeCell } from '../components/ui/tableCells';
import { OverflowMenu } from '../components/ui/OverflowMenu';
import { TrashIcon } from '../components/ui/icons';
import './ApiTokensPage.css';

/** Elevated roles read as a warmer tone so a non-viewer token stands out in the list. */
const roleTone = (role: Role): 'neutral' | 'info' | 'warning' =>
  role === 'admin' ? 'warning' : role === 'operator' ? 'info' : 'neutral';

/** Tone per token state. Every non-active state reads as a problem, because each one means the
 *  token is refused — and an admin looking at the list needs "this does not work" to be visible
 *  without reading the column. `Record` so a new state cannot be added without deciding this. */
const STATE_TONE: Record<TokenState, 'up' | 'neutral' | 'warning'> = {
  active: 'up',
  revoked: 'neutral',
  expired: 'warning',
  'no-owner': 'warning',
  'owner-disabled': 'warning',
};

/** Table columns. Renderers close over `t`, so the caller rebuilds them on a language switch.
 *  `onRevoke` is `null` when the viewer isn't an admin (no row actions rendered then).
 *
 *  `now` is the caller's, not this function's: the filter specs and the status badge both derive a
 *  token's state from it, and two readings would let a row be filtered as `active` and rendered as
 *  `expired` in the same paint. */
function tokenColumns(
  t: TFunction,
  now: Date,
  onRevoke: ((row: ApiTokenSummary) => void) | null,
): Column<ApiTokenSummary>[] {
  const filters = tokenFilters(t, now);
  const cols: Column<ApiTokenSummary>[] = [
    {
      key: 'name',
      header: t('cols.name'),
      width: '1fr',
      sortable: true,
      render: (r) => <span className="tok-name">{r.name}</span>,
    },
    {
      key: 'surfaces',
      header: t('cols.surfaces'),
      width: '150px',
      render: (r) => (
        <span className="tok-surface-badges">
          {r.surfaces.map((s) => (
            <Badge key={s} tone={s === 'rest' ? 'info' : 'neutral'}>
              {t(`surface.${s}`)}
            </Badge>
          ))}
        </span>
      ),
    },
    {
      key: 'role',
      header: t('cols.role'),
      width: '120px',
      sortable: true,
      render: (r) => <Badge tone={roleTone(r.role)}>{t(`common:role.${r.role}`)}</Badge>,
    },
    {
      key: 'owner',
      header: t('cols.owner'),
      width: '160px',
      sortable: true,
      render: (r) =>
        r.owner ? (
          // The SSO note only appears for an owner that signs in through an IdP, because that is
          // the only case where going quiet ends the token. Showing it for a service account —
          // which never signs in — would read as a warning about the normal state.
          <span className="tok-owner" title={r.owner_last_login_at ? t('ssoIdle') : undefined}>
            {r.owner}
            {r.owner_last_login_at && <span className="tok-sso-dot" aria-hidden="true" />}
          </span>
        ) : (
          <span className="muted">{t('owner.none')}</span>
        ),
    },
    {
      key: 'scope',
      header: t('cols.scope'),
      width: '130px',
      render: (r) => {
        const { key, n } = scopeLabelKey(r.scope);
        return key === 'all' ? (
          <span className="muted">{t(`scope.${key}`, { count: n })}</span>
        ) : (
          <span>{t(`scope.${key}`, { count: n })}</span>
        );
      },
    },
    {
      key: 'status',
      header: t('cols.status'),
      width: '150px',
      sortable: true,
      render: (r) => {
        const state = tokenState(r, now);
        return (
          <Badge
            tone={STATE_TONE[state]}
            title={state === 'active' || state === 'revoked' ? undefined : t(`stateHint.${state}`)}
          >
            {t(`state.${state}`)}
          </Badge>
        );
      },
    },
    {
      key: 'expires',
      header: t('cols.expires'),
      width: '150px',
      sortable: true,
      render: (r) => {
        const days = daysUntilExpiry(r.expires_at, now);
        if (!r.expires_at) return <span className="muted">{t('expires.never')}</span>;
        if (days === null) return <TimeCell iso={r.expires_at} />;
        // Near the end, a countdown is what an operator can act on; a date is not.
        return days <= 14 ? (
          <span className="tok-expiring">{t('expires.in', { count: days })}</span>
        ) : (
          <TimeCell iso={r.expires_at} />
        );
      },
    },
    {
      key: 'created',
      header: t('cols.created'),
      width: '190px',
      sortable: true,
      render: (r) => <TimeCell iso={r.created_at} />,
    },
    {
      key: 'lastUsed',
      header: t('cols.lastUsed'),
      width: '190px',
      sortable: true,
      render: (r) =>
        r.last_used_at ? <TimeCell iso={r.last_used_at} /> : <span className="muted">{t('lastUsed.never')}</span>,
    },
  ];
  // Attached by key rather than written into each literal, so a column with no spec simply has no
  // filter control and a spec with no column is a visible mismatch here rather than a silent no-op.
  for (const c of cols) c.filter = filters[c.key];
  if (onRevoke) {
    cols.push({
      key: 'actions',
      header: t('cols.actions'),
      width: '96px',
      align: 'right',
      render: (r) =>
        r.revoked_at ? null : (
          <OverflowMenu
            actions={[
              {
                label: t('revoke.action'),
                icon: <TrashIcon />,
                danger: true,
                onClick: () => onRevoke(r),
              },
            ]}
          />
        ),
    });
  }
  return cols;
}

/** Create a token: name, role, surfaces, expiry and owner. Scope is omitted (global/All) — the only
 *  scope either surface accepts today. On success the parent reveals the once-shown raw token.
 *
 *  The judgement (what the presets mean, which owners may be offered, when this can be submitted)
 *  lives in `tokenForm.ts` so it is unit-tested; Vitest never runs a `.tsx`. */
function CreateTokenModal({
  owners,
  onClose,
  onCreated,
}: {
  owners: UserSummary[];
  onClose: () => void;
  onCreated: (created: CreatedApiToken) => void;
}) {
  const { t } = useTranslation('settings-tokens');
  const [username, setUsername] = useState("");
  // Which account is "me" in the owner picker. Read here rather than from the auth store, which
  // holds the role but not the name.
  useEffect(() => {
    api
      .me()
      .then((m) => setUsername(m.username))
      .catch(() => setUsername(""));
  }, []);
  const [name, setName] = useState('');
  const [role, setRole] = useState<Role>('viewer');
  const [surfaces, setSurfaces] = useState<TokenSurface[]>(['mcp']);
  const [expiry, setExpiry] = useState<ExpiryChoice>(DEFAULT_EXPIRY);
  const [owner, setOwner] = useState('');
  const [groups, setGroups] = useState<NodeGroup[]>([]);
  const [scopeGroups, setScopeGroups] = useState<string[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const choices = useMemo(() => ownerChoices(owners, username ?? ''), [owners, username]);
  const ready = canSubmit(name, surfaces);
  // A token owned by a group-scoped account inherits that scope, so the picker is not offered —
  // see `ownerIsScoped`. Recomputed when the owner changes, since that is what decides it.
  const inherits = ownerIsScoped(owners, owner, username ?? '');

  useEffect(() => {
    api.listNodeGroups().then(setGroups).catch(() => setGroups([]));
  }, []);

  const submit = () => {
    if (!ready) return;
    setBusy(true);
    setError(null);
    api
      .createApiToken({
        name: name.trim(),
        role,
        surfaces,
        expires_at: expiryFromChoice(expiry, new Date()),
        // Omitted means "me" — the server defaults the owner to the caller.
        owner_user_id: owner || undefined,
        // Omitted when the owner is scoped: the token inherits, and sending anything else is a
        // `400`. Otherwise an empty selection means the whole fleet, never an empty group set.
        scope: inherits ? undefined : scopeFromSelection(scopeGroups),
      })
      .then((created) => onCreated(created))
      .catch((e: unknown) => {
        setError(
          e instanceof ApiError && e.code === 'duplicate_name'
            ? t('err.duplicate')
            : errMsg(e, t('err.create')),
        );
        setBusy(false);
      });
  };

  return (
    <Modal
      title={t('add.title')}
      onClose={onClose}
      footer={
        <>
          <Button variant="outline" onClick={onClose} disabled={busy}>
            {t('common:actions.cancel')}
          </Button>
          <Button variant="primary" onClick={submit} disabled={!ready || busy}>
            {t('common:actions.save')}
          </Button>
        </>
      }
    >
      <div className="modal-field">
        <label className="modal-field-label">{t('field.name')}</label>
        <TextInput
          value={name}
          placeholder={t('field.namePlaceholder')}
          onChange={(e) => setName(e.target.value)}
          autoFocus
        />
      </div>
      <div className="modal-field">
        <label className="modal-field-label">{t('field.surfaces')}</label>
        <div className="tok-surfaces">
          {TOKEN_SURFACES.map((s) => (
            <label key={s} className="tok-surface">
              <input
                type="checkbox"
                checked={surfaces.includes(s)}
                onChange={() => setSurfaces(toggleSurface(surfaces, s, TOKEN_SURFACES))}
              />
              <span className="tok-surface-name">{t(`surface.${s}`)}</span>
              <span className="tok-surface-hint">{t(`surfaceHint.${s}`)}</span>
            </label>
          ))}
        </div>
        {!surfaces.length && <span className="modal-hint">{t('err.noSurface')}</span>}
      </div>
      <div className="modal-field">
        <label className="modal-field-label">{t('field.role')}</label>
        <Select value={role} onChange={(e) => setRole(e.target.value as Role)}>
          {ROLES.map((r) => (
            <option key={r} value={r}>
              {t(`common:role.${r}`)}
            </option>
          ))}
        </Select>
        <span className="modal-hint">{t('field.roleHint')}</span>
      </div>
      <div className="modal-field">
        <label className="modal-field-label">{t('field.owner')}</label>
        <Select value={owner} onChange={(e) => setOwner(e.target.value)}>
          {choices.map((u) => (
            <option key={u.id} value={u.username === username ? '' : u.id}>
              {u.username === username
                ? t('field.ownerSelf', { username: u.username })
                : u.username}
            </option>
          ))}
        </Select>
        <span className="modal-hint">{t('field.ownerHint')}</span>
      </div>
      <div className="modal-field">
        <label className="modal-field-label">{t('field.scope')}</label>
        {inherits ? (
          // Not a picker: a token owned by a scoped account inherits that account's scope, and the
          // API refuses any other value (`400 owner_is_scoped`). Saying so beats a control whose
          // every setting is rejected.
          <span className="modal-hint">{t('field.scopeInherited')}</span>
        ) : groups.length === 0 ? (
          <span className="modal-hint">{t('field.scopeNoGroups')}</span>
        ) : (
          <>
            <div className="users-scope-list">
              {groups.map((g) => (
                <label key={g.id} className="users-scope-row">
                  <input
                    type="checkbox"
                    checked={scopeGroups.includes(g.id)}
                    onChange={(e) =>
                      setScopeGroups((prev) =>
                        e.target.checked ? [...prev, g.id] : prev.filter((x) => x !== g.id),
                      )
                    }
                  />
                  <span className="users-scope-name">{g.name}</span>
                </label>
              ))}
            </div>
            <span className="modal-hint">
              {scopeGroups.length === 0 ? t('field.scopeAllHint') : t('field.scopeHint')}
            </span>
          </>
        )}
      </div>
      <div className="modal-field">
        <label className="modal-field-label">{t('field.expiry')}</label>
        <Select value={expiry} onChange={(e) => setExpiry(e.target.value as ExpiryChoice)}>
          {EXPIRY_CHOICES.map((c) => (
            <option key={c} value={c}>
              {t(`expiry.${c}`)}
            </option>
          ))}
        </Select>
        <span className="modal-hint">{t('field.expiryHint')}</span>
      </div>
      {error && <p className="form-error">{error}</p>}
    </Modal>
  );
}

/** Reveal the raw token exactly once, with copy + a ready-to-paste MCP client command. */
function RevealTokenModal({ created, onClose }: { created: CreatedApiToken; onClose: () => void }) {
  const { t } = useTranslation('settings-tokens');
  const [copied, setCopied] = useState(false);
  const origin = typeof window !== 'undefined' ? window.location.origin : 'https://yagra.example';
  const command = `claude mcp add --transport http yagra ${origin}/mcp --header "Authorization: Bearer ${created.token}"`;

  const copy = (text: string) => {
    void navigator.clipboard?.writeText(text);
    setCopied(true);
    setTimeout(() => setCopied(false), 1200);
  };

  return (
    <Modal
      title={t('token.title')}
      size="wide"
      onClose={onClose}
      footer={
        <Button variant="primary" onClick={onClose}>
          {t('token.done')}
        </Button>
      }
    >
      <p className="modal-confirm-text">{t('token.intro')}</p>
      <div className="modal-field">
        <label className="modal-field-label">{t('token.label')}</label>
        <div className="tok-copyrow">
          <code className="tok-token mono">{created.token}</code>
          <Button variant="outline" onClick={() => copy(created.token)}>
            {copied ? t('common:copy.copied') : t('token.copy')}
          </Button>
        </div>
      </div>
      <div className="modal-field">
        <label className="modal-field-label">{t('token.usageHint')}</label>
        <code className="tok-cmd mono">{command}</code>
      </div>
    </Modal>
  );
}

/** Confirm + revoke a token. */
function RevokeTokenModal({
  token,
  onClose,
  onDone,
}: {
  token: ApiTokenSummary;
  onClose: () => void;
  onDone: () => void;
}) {
  const { t } = useTranslation('settings-tokens');
  return (
    <ConfirmDeleteModal
      title={t('revoke.title')}
      confirmLabel={t('revoke.action')}
      onConfirm={() => api.revokeApiToken(token.id)}
      errorFallback={t('err.revoke')}
      onClose={onClose}
      onDone={onDone}
    >
      {t('revoke.confirm', { name: token.name })}
    </ConfirmDeleteModal>
  );
}

export function ApiTokensPage() {
  const { t } = useTranslation('settings-tokens');
  const authed = useAuthStore((s) => s.authed);
  const canUsers = useCan('manage_users');
  const [rows, setRows] = useState<ApiTokenSummary[]>([]);
  const [block, setBlock] = useState<LoadBlock | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [adding, setAdding] = useState(false);
  const [created, setCreated] = useState<CreatedApiToken | null>(null);
  const [revoking, setRevoking] = useState<ApiTokenSummary | null>(null);
  const [users, setUsers] = useState<UserSummary[]>([]);
  const [sheet, setSheet] = useState(false);
  // The table sorts in the browser, and legitimately: every token is here. `DataTable` renders the
  // header affordance and reports the click — it never reorders `rows` itself, so a keyset-paged
  // screen cannot accidentally sort a prefix and present it as the order (`lib/tableSort.ts`).
  const [sort, setSort] = useState<SortState>(DEFAULT_TOKEN_SORT);
  // One clock reading, shared by the filter specs, the sort and the status badge — see
  // `tokenColumns`. Re-read only when the rows are replaced, so a relative window ("used in the
  // last 24 hours") does not creep forward while the operator reads the screen. Same shape as
  // `useFilterParams`'s pinned `nowMs`, and a ref rather than `useMemo` because a memo keyed on
  // `rows` is a cache the runtime may drop, not a guarantee.
  const clock = useRef<{ rows: unknown; at: Date } | null>(null);
  if (!clock.current || clock.current.rows !== rows) clock.current = { rows, at: new Date() };
  const now = clock.current.at;

  const load = useCallback(() => {
    setError(null);
    api
      .listApiTokens()
      .then((list) => {
        setRows(list);
        setBlock(null);
      })
      .catch((e: unknown) => {
        const b = classifyLoadError(e);
        if (b) setBlock(b);
        else setError(errMsg(e, t('err.load')));
      })
      .finally(() => setLoading(false));
    // Owner candidates for the create dialog. Same ManageUsers gate as this page, so a caller who
    // can see the tokens can see the accounts; a failure just leaves the picker with the caller
    // themselves, which is the pre-service-account behaviour and still correct.
    api
      .listUsers()
      .then(setUsers)
      .catch(() => setUsers([]));
  }, [t]);

  useEffect(() => {
    if (authed) load();
    else setLoading(false);
  }, [authed, load]);

  const columns = useMemo(
    () => tokenColumns(t, now, canUsers ? (r) => setRevoking(r) : null),
    [t, now, canUsers],
  );
  // URL-backed: one table on this route, so the column keys are free and a filtered view can be
  // sent to someone. Counts are exact and free here — every token is already in the browser.
  const { filterCols, filters, setFilters, clear, shown: matched, counts, anyFiltered } =
    useClientFilters(columns, rows, { url: true });
  const shown = useMemo(
    () => sortRows(matched, sort, tokenSortValues(now)),
    [matched, sort, now],
  );

  return (
    <div className="page-fill">
      <PageHeader
        title={t('nav:settings.apiTokens')}
        trail={[{ label: t('nav:sections.settings') }, { label: t('nav:settings.apiTokens') }]}
        note={t('note')}
      />

      {!authed ? (
        <Card>
          <p className="muted">{t('signInPrompt')}</p>
        </Card>
      ) : block ? (
        <LoadBlockNotice block={block} unavailable={t('unavailable')} permission="manage_users" />
      ) : (
        <>
          <TableToolbar>
            <FilterButton
              columns={filterCols}
              filters={filters}
              onOpen={() => setSheet(true)}
            />
            <ClearFilters
              columns={filterCols}
              filters={filters}
              onClear={clear}
            />
            <TableSpacer />
            <ResultCount
              shown={shown.length}
              total={anyFiltered ? rows.length : undefined}
              noun={t('count', { count: shown.length })}
            />
            {canUsers && (
              <Button variant="primary" onClick={() => setAdding(true)}>
                + {t('add.button')}
              </Button>
            )}
          </TableToolbar>

          {error && <p className="form-error">{error}</p>}

          <DataTable
            rows={shown}
            columns={columns}
            sort={sort}
            onSortChange={setSort}
            filters={filters}
            onFiltersChange={setFilters}
            filterCounts={counts}
            rowKey={(r) => r.id}
            loading={loading}
            empty={anyFiltered ? t('common:filter.noMatch') : t('empty')}
          />
          {sheet && (
            <MobileFilterSheet
              columns={filterCols}
              filters={filters}
              onChange={setFilters}
              counts={counts}
              labels={Object.fromEntries(columns.map((c) => [c.key, t(`cols.${c.key}`)]))}
              onClose={() => setSheet(false)}
            />
          )}
        </>
      )}

      {adding && (
        <CreateTokenModal
          owners={users}
          onClose={() => setAdding(false)}
          onCreated={(c) => {
            setAdding(false);
            setCreated(c);
            load();
          }}
        />
      )}
      {created && <RevealTokenModal created={created} onClose={() => setCreated(null)} />}
      {revoking && (
        <RevokeTokenModal
          token={revoking}
          onClose={() => setRevoking(null)}
          onDone={() => {
            setRevoking(null);
            load();
          }}
        />
      )}
    </div>
  );
}
