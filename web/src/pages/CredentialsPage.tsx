// SPDX-License-Identifier: AGPL-3.0-only
// Credentials & secrets (Settings ▸ Credentials & secrets). Stores monitoring secrets
// (SNMP communities, v3 creds, API tokens) — the crown jewels. The list NEVER includes secret
// values (the API returns metadata only); the secret is write-only here and sent over the
// encrypted-at-rest create endpoint. ManageCredentials-gated.
//
// Data-table standard v2: a toolbar (search + type filter + count + "+ Add credential") over the
// shared `.ytable`. Add/edit/delete all go through modals — the type select drives the add form
// (snmp_v3 reveals the USM sub-form). snmp_v3 secrets are structured (USM): the form collects
// user / level / auth / privacy fields and serializes them into the JSON document the backend
// validates and seals. Edit: name is always editable; the secret is never returned, so it's left
// intact unless the operator opts to replace it (then kind + secret are re-entered and re-sealed).

import { useCallback, useEffect, useMemo, useState } from 'react';
import { Trans, useTranslation } from 'react-i18next';
import { api, errMsg } from '../services/api';
import { useCan } from '../store';
import type { CredentialSummary } from '../types/api';
import { PageHeader } from '../components/ui/PageHeader';
import { Button } from '../components/ui/Button';
import { ConfirmDeleteModal } from '../components/ui/ConfirmDeleteModal';
import { Modal } from '../components/ui/Modal';
import { TextInput, Select } from '../components/ui/Field';
import { OverflowMenu } from '../components/ui/OverflowMenu';
import { TableToolbar, TableSpacer, ResultCount } from '../components/ui/TableToolbar';
import { DataTable, type Column } from '../components/ui/DataTable';
import { ClearFilters } from '../components/ui/ClearFilters';
import { FilterButton, MobileFilterSheet } from '../components/ui/MobileFilterSheet';
import { useClientFilters } from '../lib/useClientFilters';
import { sortRows, type SortState } from '../lib/tableSort';
import { SealedSecret, CopyableId } from '../components/ui/tableCells';
import { HashIcon, ShieldIcon, KeyIcon, EditIcon, TrashIcon } from '../components/ui/icons';
import type { ComponentType } from 'react';
import {
  buildV3Secret,
  emptyV3,
  v3Ready,
  V3_AUTH_PROTOCOLS,
  V3_LEVELS,
  V3_PRIV_PROTOCOLS,
  type V3State,
} from './v3Credential';
import {
  buildHttpAuthSecret,
  emptyHttpAuth,
  httpAuthReady,
  HTTP_AUTH_SCHEMES,
  type HttpAuthScheme,
  type HttpAuthState,
} from './httpAuthCredential';
import {
  credentialFilters,
  credentialSortValues,
  DEFAULT_CREDENTIAL_SORT,
} from './credentialList';
import { CREDENTIAL_KINDS, type CredentialKind } from '../lib/credentialKinds';
import './CredentialsPage.css';
import { classifyLoadError, type LoadBlock } from '../lib/loadState';
import { LoadBlockNotice } from '../components/ui/LoadBlockNotice';
import { kindLabel, usageLabel } from './credentialList';

/** The creatable kinds and their type come from `lib/credentialKinds.ts`, which is also what the
 *  node-binding, URL-monitor and discovery pickers filter on — one list, so a kind added here
 *  cannot be one those pickers refuse to offer. */
const KINDS = CREDENTIAL_KINDS;
type Kind = CredentialKind;

// Kind → { i18n label key, icon }. The label is resolved with `t` at the call site (never at
// module load) so it follows the active language.
/** The icon per credential kind. ⚠️ The **label** half lives in `credentialList.ts` — icons are
 *  components, and that file is loaded by a test in a node environment. `kindIcons` in this file
 *  and `CREDENTIAL_KIND_LABEL_KEYS` there are keyed by the same strings, and
 *  `credentialList.test.ts` pins that they agree. */
const KIND_ICONS: Record<string, ComponentType> = {
  snmp_v2c: HashIcon,
  snmp_v3: ShieldIcon,
  http_auth: KeyIcon,
  api_token: KeyIcon,
  // Meraki keys are created via Settings ▸ Integrations; shown here read-only.
  meraki_api: KeyIcon,
};



/** The SNMPv3 (USM) sub-form. Controlled — the same fields back the add and edit modals. */
/** The HTTP-auth sub-form. Only the selected scheme's fields render, but every field stays in
 *  state, so switching scheme and back does not discard what was typed. */
function HttpAuthFields({
  value,
  onChange,
}: {
  value: HttpAuthState;
  onChange: (v: HttpAuthState) => void;
}) {
  const { t } = useTranslation('access');
  const set = (patch: Partial<HttpAuthState>) => onChange({ ...value, ...patch });
  return (
    <>
      <div className="modal-field">
        <label className="modal-field-label">{t('cred.http.scheme')}</label>
        <Select
          value={value.scheme}
          onChange={(e) => set({ scheme: e.target.value as HttpAuthScheme })}
        >
          {HTTP_AUTH_SCHEMES.map((s) => (
            <option key={s} value={s}>
              {t(`cred.http.schemeName.${s}`)}
            </option>
          ))}
        </Select>
      </div>
      {value.scheme === 'basic' && (
        <>
          <div className="modal-field">
            <label className="modal-field-label">{t('cred.http.username')}</label>
            <TextInput value={value.username} onChange={(e) => set({ username: e.target.value })} />
          </div>
          <div className="modal-field">
            <label className="modal-field-label">{t('cred.http.password')}</label>
            <TextInput
              className="mono"
              type="password"
              value={value.password}
              onChange={(e) => set({ password: e.target.value })}
              autoComplete="new-password"
            />
          </div>
        </>
      )}
      {value.scheme === 'bearer' && (
        <div className="modal-field">
          <label className="modal-field-label">{t('cred.http.token')}</label>
          <TextInput
            className="mono"
            type="password"
            value={value.token}
            onChange={(e) => set({ token: e.target.value })}
            autoComplete="new-password"
          />
        </div>
      )}
      {value.scheme === 'header' && (
        <>
          <div className="modal-field">
            <label className="modal-field-label">{t('cred.http.headerName')}</label>
            <TextInput
              className="mono"
              placeholder="X-API-Key"
              value={value.headerName}
              onChange={(e) => set({ headerName: e.target.value })}
            />
            <span className="modal-hint">{t('cred.http.headerNameHint')}</span>
          </div>
          <div className="modal-field">
            <label className="modal-field-label">{t('cred.http.headerValue')}</label>
            <TextInput
              className="mono"
              type="password"
              value={value.headerValue}
              onChange={(e) => set({ headerValue: e.target.value })}
              autoComplete="new-password"
            />
          </div>
        </>
      )}
      <span className="modal-hint">{t('cred.http.hint')}</span>
    </>
  );
}

function V3Fields({ value, onChange }: { value: V3State; onChange: (v: V3State) => void }) {
  const { t } = useTranslation('access');
  const needsAuth = value.level !== 'noauth';
  const needsPriv = value.level === 'authpriv';
  const set = (patch: Partial<V3State>) => onChange({ ...value, ...patch });
  return (
    <>
      <div className="modal-field">
        <label className="modal-field-label">{t('cred.v3.usmUser')}</label>
        <TextInput
          className="mono"
          placeholder={t('cred.v3.usmUserPlaceholder')}
          value={value.user}
          onChange={(e) => set({ user: e.target.value })}
        />
      </div>
      <div className="modal-field">
        <label className="modal-field-label">{t('cred.v3.securityLevel')}</label>
        <Select
          value={value.level}
          onChange={(e) => set({ level: e.target.value as (typeof V3_LEVELS)[number] })}
        >
          {V3_LEVELS.map((l) => (
            <option key={l} value={l}>
              {l}
            </option>
          ))}
        </Select>
      </div>
      {needsAuth && (
        <div className="cred-v3-pair">
          <div className="modal-field">
            <label className="modal-field-label">{t('cred.v3.authProtocol')}</label>
            <Select value={value.authProto} onChange={(e) => set({ authProto: e.target.value })}>
              {V3_AUTH_PROTOCOLS.map((p) => (
                <option key={p} value={p}>
                  {p}
                </option>
              ))}
            </Select>
          </div>
          <div className="modal-field">
            <label className="modal-field-label">{t('cred.v3.authPassphrase')}</label>
            <TextInput
              className="mono"
              type="password"
              value={value.authKey}
              onChange={(e) => set({ authKey: e.target.value })}
              autoComplete="new-password"
            />
          </div>
        </div>
      )}
      {needsPriv && (
        <div className="cred-v3-pair">
          <div className="modal-field">
            <label className="modal-field-label">{t('cred.v3.privacyProtocol')}</label>
            <Select value={value.privProto} onChange={(e) => set({ privProto: e.target.value })}>
              {V3_PRIV_PROTOCOLS.map((p) => (
                <option key={p} value={p}>
                  {p}
                </option>
              ))}
            </Select>
          </div>
          <div className="modal-field">
            <label className="modal-field-label">{t('cred.v3.privacyPassphrase')}</label>
            <TextInput
              className="mono"
              type="password"
              value={value.privKey}
              onChange={(e) => set({ privKey: e.target.value })}
              autoComplete="new-password"
            />
          </div>
        </div>
      )}
    </>
  );
}

/** Create a credential (type-driven focused-editing modal). */
function AddCredentialModal({ onClose, onSaved }: { onClose: () => void; onSaved: () => void }) {
  const { t } = useTranslation('access');
  const [name, setName] = useState('');
  const [kind, setKind] = useState<Kind>('snmp_v2c');
  const [secret, setSecret] = useState('');
  const [v3, setV3] = useState<V3State>(emptyV3);
  const [httpAuth, setHttpAuth] = useState<HttpAuthState>(emptyHttpAuth);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const isV3 = kind === 'snmp_v3';
  const isHttpAuth = kind === 'http_auth';
  const ready =
    name.trim() !== '' &&
    (isV3 ? v3Ready(v3) : isHttpAuth ? httpAuthReady(httpAuth) : secret !== '');

  const submit = () => {
    if (!ready) return;
    setBusy(true);
    setError(null);
    api
      .createCredential({
        name: name.trim(),
        kind,
        secret: isV3
          ? buildV3Secret(v3)
          : isHttpAuth
            ? buildHttpAuthSecret(httpAuth)
            : secret,
      })
      .then(() => {
        onSaved();
        onClose();
      })
      .catch((e: unknown) => {
        setError(errMsg(e, t('cred.err.add')));
        setBusy(false);
      });
  };

  return (
    <Modal
      title={t('cred.add.title')}
      onClose={onClose}
      footer={
        <>
          <Button variant="outline" onClick={onClose} disabled={busy}>
            {t('common:actions.cancel')}
          </Button>
          <Button variant="primary" onClick={submit} disabled={!ready || busy}>
            {t('cred.add.title')}
          </Button>
        </>
      }
    >
      <div className="modal-field">
        <label className="modal-field-label">{t('cred.field.name')}</label>
        <TextInput
          placeholder={t('cred.add.namePlaceholder')}
          value={name}
          onChange={(e) => setName(e.target.value)}
          autoFocus
        />
      </div>
      <div className="modal-field">
        <label className="modal-field-label">{t('cred.field.type')}</label>
        <Select value={kind} onChange={(e) => setKind(e.target.value as Kind)}>
          {KINDS.map((k) => (
            <option key={k} value={k}>
              {kindLabel(k, t)}
            </option>
          ))}
        </Select>
      </div>
      {isV3 ? (
        <V3Fields value={v3} onChange={setV3} />
      ) : isHttpAuth ? (
        <HttpAuthFields value={httpAuth} onChange={setHttpAuth} />
      ) : (
        <div className="modal-field">
          <label className="modal-field-label">{t('cred.field.secret')}</label>
          <TextInput
            className="mono"
            type="password"
            placeholder={
              kind === 'api_token'
                ? t('cred.add.secretPlaceholder.apiToken')
                : t('cred.add.secretPlaceholder.community')
            }
            value={secret}
            onChange={(e) => setSecret(e.target.value)}
            autoComplete="new-password"
          />
          <span className="modal-hint">{t('cred.add.secretHint')}</span>
        </div>
      )}
      {error && <p className="form-error">{error}</p>}
    </Modal>
  );
}

/** Edit a credential: rename always; optionally replace the (write-only) secret. */
function EditCredentialModal({
  cred,
  onClose,
  onSaved,
}: {
  cred: CredentialSummary;
  onClose: () => void;
  onSaved: () => void;
}) {
  const { t } = useTranslation('access');
  const [name, setName] = useState(cred.name);
  const [replace, setReplace] = useState(false);
  const [kind, setKind] = useState<Kind>((cred.kind as Kind) ?? 'snmp_v2c');
  const [secret, setSecret] = useState('');
  const [v3, setV3] = useState<V3State>(emptyV3);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const isV3 = kind === 'snmp_v3';
  const secretReady = !replace || (isV3 ? v3Ready(v3) : secret !== '');
  const ready = name.trim() !== '' && secretReady;

  const save = () => {
    setError(null);
    setBusy(true);
    const body = replace
      ? { name: name.trim(), kind, secret: isV3 ? buildV3Secret(v3) : secret }
      : { name: name.trim() };
    api
      .updateCredential(cred.id, body)
      .then(() => {
        onSaved();
        onClose();
      })
      .catch((e: unknown) => {
        setError(errMsg(e, t('cred.err.update')));
        setBusy(false);
      });
  };

  return (
    <Modal
      title={t('cred.edit.title')}
      onClose={onClose}
      footer={
        <>
          <Button variant="outline" onClick={onClose} disabled={busy}>
            {t('common:actions.cancel')}
          </Button>
          <Button variant="primary" onClick={save} disabled={!ready || busy}>
            {t('common:actions.save')}
          </Button>
        </>
      }
    >
      <div className="modal-field">
        <label className="modal-field-label">{t('cred.field.name')}</label>
        <TextInput value={name} onChange={(e) => setName(e.target.value)} autoFocus />
      </div>
      <label className="cred-replace">
        <input type="checkbox" checked={replace} onChange={(e) => setReplace(e.target.checked)} />
        <span>{t('cred.edit.replaceSecret')}</span>
        <span className="muted">— {t('cred.edit.replaceHint')}</span>
      </label>
      {replace && (
        <>
          <div className="modal-field">
            <label className="modal-field-label">{t('cred.field.type')}</label>
            <Select value={kind} onChange={(e) => setKind(e.target.value as Kind)}>
              {KINDS.map((k) => (
                <option key={k} value={k}>
                  {kindLabel(k, t)}
                </option>
              ))}
            </Select>
          </div>
          {isV3 ? (
            <V3Fields value={v3} onChange={setV3} />
          ) : (
            <div className="modal-field">
              <label className="modal-field-label">{t('cred.edit.newSecret')}</label>
              <TextInput
                className="mono"
                type="password"
                value={secret}
                onChange={(e) => setSecret(e.target.value)}
                autoComplete="new-password"
              />
            </div>
          )}
        </>
      )}
      {error && <p className="form-error">{error}</p>}
    </Modal>
  );
}

/** Confirm + delete a credential (destructive-consent modal). */
function DeleteCredentialModal({
  cred,
  onClose,
  onDone,
}: {
  cred: CredentialSummary;
  onClose: () => void;
  onDone: () => void;
}) {
  const { t } = useTranslation('access');
  return (
    <ConfirmDeleteModal
      title={t('cred.delete.title')}
      onConfirm={() => api.deleteCredential(cred.id)}
      errorFallback={t('cred.err.delete')}
      onClose={onClose}
      onDone={onDone}
    >
      <Trans
        t={t}
        i18nKey="cred.delete.confirm"
        values={{ name: cred.name }}
        components={{ strong: <strong /> }}
      />{' '}
      {cred.used_by > 0
        ? t('cred.delete.inUse', { usage: usageLabel(cred.used_by, t) })
        : t('cred.delete.unused')}{' '}
      {t('cred.delete.irreversible')}
    </ConfirmDeleteModal>
  );
}

export function CredentialsPage() {
  const { t } = useTranslation('access');
  const canCredentials = useCan('manage_credentials');
  const [rows, setRows] = useState<CredentialSummary[]>([]);
  const [sheet, setSheet] = useState(false);
  const [sort, setSort] = useState<SortState>(DEFAULT_CREDENTIAL_SORT);
  const [block, setBlock] = useState<LoadBlock | null>(null);
  const [loading, setLoading] = useState(true);
  const [adding, setAdding] = useState(false);
  const [editing, setEditing] = useState<CredentialSummary | null>(null);
  const [deleting, setDeleting] = useState<CredentialSummary | null>(null);

  const load = useCallback(() => {
    api
      .listCredentials()
      .then((list) => {
        setRows(list);
        setBlock(null);
      })
      .catch((e: unknown) => setBlock(classifyLoadError(e)))
      .finally(() => setLoading(false));
  }, []);

  useEffect(() => {
    load();
  }, [load]);

  const columns = useMemo<Column<CredentialSummary>[]>(() => {
    // Every kind present in the table, so a `meraki_api` or `http_auth` row — neither of which the
    // old three-option dropdown could select — is filterable.
    const kinds = [...new Set(rows.map((c) => c.kind))].sort();
    const specs = credentialFilters(t, kinds, (k) => kindLabel(k, t));
    const cols: Column<CredentialSummary>[] = [
      {
        key: 'name',
        header: t('cred.cols.name'),
        width: '1.7fr',
        sortable: true,
        render: (c) => {
          const Icon = KIND_ICONS[c.kind] ?? KeyIcon;
          return (
            <span className="yt-name">
              <span className="yt-typeicon" title={kindLabel(c.kind, t)}>
                <Icon />
              </span>
              <span className="yt-name-txt">{c.name}</span>
            </span>
          );
        },
      },
      {
        key: 'type',
        header: t('cred.cols.type'),
        width: '150px',
        render: (c) => {
          const Icon = KIND_ICONS[c.kind] ?? KeyIcon;
          return (
            <span className="yt-chip">
              <Icon />
              {kindLabel(c.kind, t)}
            </span>
          );
        },
      },
      {
        key: 'secret',
        header: t('cred.cols.secret'),
        width: '130px',
        render: () => <SealedSecret />,
      },
      {
        key: 'used_by',
        header: t('cred.cols.usedBy'),
        width: '110px',
        sortable: true,
        render: (c) => (
          <span className={c.used_by === 0 ? 'yt-usage zero' : 'yt-usage'}>
            {usageLabel(c.used_by, t)}
          </span>
        ),
      },
      { key: 'id', header: t('cred.cols.credentialId'), width: '1fr', render: (c) => <CopyableId id={c.id} /> },
      {
        key: 'actions',
        header: t('cred.cols.actions'),
        width: '92px',
        align: 'right',
        render: (c) =>
          canCredentials ? (
            <span className="ytable-actions">
              <OverflowMenu
                actions={[
                  {
                    label: t('common:actions.edit'),
                    icon: <EditIcon />,
                    onClick: () => setEditing(c),
                  },
                  {
                    label: t('common:actions.delete'),
                    icon: <TrashIcon />,
                    danger: true,
                    onClick: () => setDeleting(c),
                  },
                ]}
              />
            </span>
          ) : null,
      },
    ];
    for (const c of cols) c.filter = specs[c.key];
    return cols;
  }, [t, canCredentials, rows]);

  // URL-backed: one table on this route.
  const { filterCols, filters, setFilters, clear, shown: matched, counts, anyFiltered } =
    useClientFilters(columns, rows, { url: true });
  // Sorting stays with the caller — `DataTable` draws the arrow and reports the click but never
  // reorders `rows`, so a keyset-paged screen cannot accidentally sort a prefix (`lib/tableSort.ts`).
  const shown = useMemo(() => sortRows(matched, sort, credentialSortValues()), [matched, sort]);

  return (
    <div>
      <PageHeader
        title={t('nav:nodes.credentials')}
        trail={[{ label: t('nav:sections.nodes') }, { label: t('nav:nodes.credentials') }]}
        note={t('cred.note')}
      />

      {block ? (
        <LoadBlockNotice
          permission="manage_credentials"
          block={block}
          unavailable={t('cred.unavailable')}
        />
      ) : (
        <>
          <TableToolbar>
            <FilterButton
              columns={filterCols}
              filters={filters}
              onOpen={() => setSheet(true)}
            />
            <ClearFilters columns={filterCols} filters={filters} onClear={clear} />
            <TableSpacer />
            <ResultCount
              shown={shown.length}
              total={anyFiltered ? rows.length : undefined}
              noun={t('common:noun.credential', { count: rows.length })}
            />
            {canCredentials && (
              <Button variant="primary" onClick={() => setAdding(true)}>
                + {t('cred.add.title')}
              </Button>
            )}
          </TableToolbar>

          <DataTable
            rows={shown}
            columns={columns}
            rowKey={(c) => c.id}
            sort={sort}
            onSortChange={setSort}
            filters={filters}
            onFiltersChange={setFilters}
            filterCounts={counts}
            loading={loading}
            empty={anyFiltered ? t('cred.empty.filtered') : t('cred.empty.none')}
          />
          {sheet && (
            <MobileFilterSheet
              columns={filterCols}
              filters={filters}
              onChange={setFilters}
              counts={counts}
              labels={Object.fromEntries(columns.map((c) => [c.key, String(c.header)]))}
              onClose={() => setSheet(false)}
            />
          )}
        </>
      )}

      {adding && <AddCredentialModal onClose={() => setAdding(false)} onSaved={load} />}
      {editing && (
        <EditCredentialModal cred={editing} onClose={() => setEditing(null)} onSaved={load} />
      )}
      {deleting && (
        <DeleteCredentialModal
          cred={deleting}
          onClose={() => setDeleting(null)}
          onDone={() => {
            setDeleting(null);
            load();
          }}
        />
      )}
    </div>
  );
}
