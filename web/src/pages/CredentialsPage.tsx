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
import type { TFunction } from 'i18next';
import { api, errMsg, ApiError } from '../services/api';
import { useAuthStore } from '../store';
import type { CredentialSummary } from '../types/api';
import { PageHeader } from '../components/ui/PageHeader';
import { Card } from '../components/ui/Card';
import { Button } from '../components/ui/Button';
import { ConfirmDeleteModal } from '../components/ui/ConfirmDeleteModal';
import { Modal } from '../components/ui/Modal';
import { TextInput, Select } from '../components/ui/Field';
import { OverflowMenu } from '../components/ui/OverflowMenu';
import { TableToolbar, SearchInput, TableSpacer, ResultCount } from '../components/ui/TableToolbar';
import { SealedSecret, CopyableId } from '../components/ui/tableCells';
import { HashIcon, ShieldIcon, KeyIcon, EditIcon, TrashIcon } from '../components/ui/icons';
import type { ComponentType } from 'react';
import './CredentialsPage.css';

const KINDS = ['snmp_v2c', 'snmp_v3', 'api_token'] as const;
type Kind = (typeof KINDS)[number];
const V3_LEVELS = ['authpriv', 'auth', 'noauth'] as const;
const V3_AUTH_PROTOCOLS = ['sha', 'sha224', 'sha256', 'sha384', 'sha512', 'md5'];
const V3_PRIV_PROTOCOLS = ['aes', 'aes192', 'aes256', 'des'];

// Kind → { i18n label key, icon }. The label is resolved with `t` at the call site (never at
// module load) so it follows the active language.
const KIND_META: Record<string, { labelKey: string; Icon: ComponentType }> = {
  snmp_v2c: { labelKey: 'cred.kind.snmp_v2c', Icon: HashIcon },
  snmp_v3: { labelKey: 'cred.kind.snmp_v3', Icon: ShieldIcon },
  api_token: { labelKey: 'cred.kind.api_token', Icon: KeyIcon },
  // Meraki keys are created via Settings ▸ Integrations; shown here read-only.
  meraki_api: { labelKey: 'cred.kind.meraki_api', Icon: KeyIcon },
};

const COLS = '1.7fr 150px 130px 110px 1fr 92px';

const kindLabel = (kind: string, t: TFunction) => {
  const meta = KIND_META[kind];
  return meta ? t(meta.labelKey) : kind;
};

const usageLabel = (n: number, t: TFunction) =>
  n === 0 ? t('cred.usage.unused') : t('cred.usage.count', { count: n });

/** SNMPv3 (USM) structured form state — shared by the add and edit modals. */
interface V3State {
  user: string;
  level: (typeof V3_LEVELS)[number];
  authProto: string;
  authKey: string;
  privProto: string;
  privKey: string;
}

const emptyV3 = (): V3State => ({
  user: '',
  level: 'authpriv',
  authProto: V3_AUTH_PROTOCOLS[0],
  authKey: '',
  privProto: V3_PRIV_PROTOCOLS[0],
  privKey: '',
});

/** Whether the v3 form has the keys its declared security level requires. */
const v3Ready = (v: V3State): boolean => {
  const needsAuth = v.level !== 'noauth';
  const needsPriv = v.level === 'authpriv';
  return v.user.trim() !== '' && (!needsAuth || v.authKey !== '') && (!needsPriv || v.privKey !== '');
};

/** Serialize the v3 form into the USM JSON document the backend validates and seals. */
const buildV3Secret = (v: V3State): string => {
  const needsAuth = v.level !== 'noauth';
  const needsPriv = v.level === 'authpriv';
  return JSON.stringify({
    user: v.user.trim(),
    security_level: v.level,
    ...(needsAuth ? { auth_protocol: v.authProto, auth_key: v.authKey } : {}),
    ...(needsPriv ? { priv_protocol: v.privProto, priv_key: v.privKey } : {}),
  });
};

/** The SNMPv3 (USM) sub-form. Controlled — the same fields back the add and edit modals. */
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
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const isV3 = kind === 'snmp_v3';
  const ready = name.trim() !== '' && (isV3 ? v3Ready(v3) : secret !== '');

  const submit = () => {
    if (!ready) return;
    setBusy(true);
    setError(null);
    api
      .createCredential({ name: name.trim(), kind, secret: isV3 ? buildV3Secret(v3) : secret })
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

type SortKey = 'name' | 'used_by';

export function CredentialsPage() {
  const { t } = useTranslation('access');
  const authed = useAuthStore((s) => s.authed);
  const [rows, setRows] = useState<CredentialSummary[]>([]);
  const [query, setQuery] = useState('');
  const [kindFilter, setKindFilter] = useState('all');
  const [sort, setSort] = useState<{ key: SortKey; dir: 1 | -1 }>({ key: 'name', dir: 1 });
  const [unavailable, setUnavailable] = useState(false);
  const [loading, setLoading] = useState(true);
  const [adding, setAdding] = useState(false);
  const [editing, setEditing] = useState<CredentialSummary | null>(null);
  const [deleting, setDeleting] = useState<CredentialSummary | null>(null);

  const load = useCallback(() => {
    api
      .listCredentials()
      .then((list) => {
        setRows(list);
        setUnavailable(false);
      })
      .catch((e: unknown) => {
        if (e instanceof ApiError && e.code === 'admin_unavailable') setUnavailable(true);
      })
      .finally(() => setLoading(false));
  }, []);

  useEffect(() => {
    load();
  }, [load]);

  const list = useMemo(() => {
    const q = query.trim().toLowerCase();
    const filtered = rows.filter(
      (c) =>
        (q === '' || c.name.toLowerCase().includes(q) || c.id.toLowerCase().includes(q)) &&
        (kindFilter === 'all' || c.kind === kindFilter),
    );
    const sorted = [...filtered].sort((a, b) => {
      const av = sort.key === 'name' ? a.name : a.used_by;
      const bv = sort.key === 'name' ? b.name : b.used_by;
      return (av < bv ? -1 : av > bv ? 1 : 0) * sort.dir;
    });
    return sorted;
  }, [rows, query, kindFilter, sort]);

  const toggleSort = (key: SortKey) =>
    setSort((s) => (s.key === key ? { key, dir: (s.dir * -1) as 1 | -1 } : { key, dir: 1 }));
  const arrow = (key: SortKey) =>
    sort.key === key ? <span className="ytable-arrow">{sort.dir === 1 ? '▲' : '▼'}</span> : null;

  return (
    <div>
      <PageHeader
        title={t('nav:settings.credentials')}
        trail={[{ label: t('nav:sections.settings') }, { label: t('nav:settings.credentials') }]}
        note={t('cred.note')}
      />

      {unavailable ? (
        <Card>
          <p className="muted">{t('cred.unavailable')}</p>
        </Card>
      ) : (
        <>
          <TableToolbar>
            <SearchInput
              value={query}
              onChange={setQuery}
              placeholder={t('cred.searchPlaceholder')}
              ariaLabel={t('cred.searchAria')}
            />
            <Select
              value={kindFilter}
              onChange={(e) => setKindFilter(e.target.value)}
              aria-label={t('cred.filterAria')}
            >
              <option value="all">{t('cred.filter.allTypes')}</option>
              <option value="snmp_v2c">{t('cred.filter.snmp_v2c')}</option>
              <option value="snmp_v3">{t('cred.filter.snmp_v3')}</option>
              <option value="api_token">{t('cred.filter.api_token')}</option>
            </Select>
            <TableSpacer />
            <ResultCount
              shown={list.length}
              total={rows.length}
              noun={t('common:noun.credential', { count: rows.length })}
            />
            {authed && (
              <Button variant="primary" onClick={() => setAdding(true)}>
                + {t('cred.add.title')}
              </Button>
            )}
          </TableToolbar>

          <div className="ytable cred-table">
            <div className="ytable-scroll">
              <div className="ytable-head" style={{ gridTemplateColumns: COLS }}>
                <div className="ytable-h sortable" onClick={() => toggleSort('name')}>
                  {t('cred.cols.name')} {arrow('name')}
                </div>
                <div className="ytable-h">{t('cred.cols.type')}</div>
                <div className="ytable-h">{t('cred.cols.secret')}</div>
                <div className="ytable-h sortable" onClick={() => toggleSort('used_by')}>
                  {t('cred.cols.usedBy')} {arrow('used_by')}
                </div>
                <div className="ytable-h">{t('cred.cols.credentialId')}</div>
                <div className="ytable-h right">{t('cred.cols.actions')}</div>
              </div>

              {list.length === 0 ? (
                <div className="yt-empty">
                  <p className="yt-empty-title">
                    {loading
                      ? t('common:loading')
                      : rows.length === 0
                        ? t('cred.empty.none')
                        : t('cred.empty.filtered')}
                  </p>
                  {!loading && (
                    <p className="yt-empty-sub">
                      {rows.length === 0 ? t('cred.empty.noneSub') : t('cred.empty.filteredSub')}
                    </p>
                  )}
                </div>
              ) : (
                list.map((c) => {
                  const Icon = KIND_META[c.kind]?.Icon ?? KeyIcon;
                  return (
                    <div className="ytable-row" style={{ gridTemplateColumns: COLS }} key={c.id}>
                      <div className="ytable-cell">
                        <span className="yt-name">
                          <span className="yt-typeicon" title={kindLabel(c.kind, t)}>
                            <Icon />
                          </span>
                          <span className="yt-name-txt">{c.name}</span>
                        </span>
                      </div>
                      <div className="ytable-cell">
                        <span className="yt-chip">
                          <Icon />
                          {kindLabel(c.kind, t)}
                        </span>
                      </div>
                      <div className="ytable-cell">
                        <SealedSecret />
                      </div>
                      <div className="ytable-cell">
                        <span className={c.used_by === 0 ? 'yt-usage zero' : 'yt-usage'}>
                          {usageLabel(c.used_by, t)}
                        </span>
                      </div>
                      <div className="ytable-cell">
                        <CopyableId id={c.id} />
                      </div>
                      <div className="ytable-cell right">
                        {authed && (
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
                        )}
                      </div>
                    </div>
                  );
                })
              )}
            </div>
          </div>
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
