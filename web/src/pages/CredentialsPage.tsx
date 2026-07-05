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
import { api, ApiError } from '../services/api';
import { useAuthStore } from '../store';
import type { CredentialSummary } from '../types/api';
import { PageHeader } from '../components/ui/PageHeader';
import { Card } from '../components/ui/Card';
import { Button } from '../components/ui/Button';
import { Modal } from '../components/ui/Modal';
import { TextInput, Select } from '../components/ui/Field';
import { IconButton } from '../components/ui/IconButton';
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

const KIND_META: Record<string, { label: string; Icon: ComponentType }> = {
  snmp_v2c: { label: 'SNMP v2c (community)', Icon: HashIcon },
  snmp_v3: { label: 'SNMP v3 (user/password)', Icon: ShieldIcon },
  api_token: { label: 'API token', Icon: KeyIcon },
  // Meraki keys are created via Settings ▸ Integrations; shown here read-only.
  meraki_api: { label: 'Cisco Meraki API key', Icon: KeyIcon },
};

const COLS = '1.7fr 150px 130px 110px 1fr 92px';

const errMsg = (e: unknown, fallback: string) =>
  e instanceof ApiError ? e.message : fallback;

const kindLabel = (kind: string) => KIND_META[kind]?.label ?? kind;

const usageLabel = (n: number) => (n === 0 ? 'Unused' : `${n} ${n === 1 ? 'node' : 'nodes'}`);

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
  const needsAuth = value.level !== 'noauth';
  const needsPriv = value.level === 'authpriv';
  const set = (patch: Partial<V3State>) => onChange({ ...value, ...patch });
  return (
    <>
      <div className="modal-field">
        <label className="modal-field-label">USM user</label>
        <TextInput
          className="mono"
          placeholder="usm-user"
          value={value.user}
          onChange={(e) => set({ user: e.target.value })}
        />
      </div>
      <div className="modal-field">
        <label className="modal-field-label">Security level</label>
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
            <label className="modal-field-label">Auth protocol</label>
            <Select value={value.authProto} onChange={(e) => set({ authProto: e.target.value })}>
              {V3_AUTH_PROTOCOLS.map((p) => (
                <option key={p} value={p}>
                  {p}
                </option>
              ))}
            </Select>
          </div>
          <div className="modal-field">
            <label className="modal-field-label">Auth passphrase</label>
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
            <label className="modal-field-label">Privacy protocol</label>
            <Select value={value.privProto} onChange={(e) => set({ privProto: e.target.value })}>
              {V3_PRIV_PROTOCOLS.map((p) => (
                <option key={p} value={p}>
                  {p}
                </option>
              ))}
            </Select>
          </div>
          <div className="modal-field">
            <label className="modal-field-label">Privacy passphrase</label>
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
        setError(errMsg(e, 'failed to add credential'));
        setBusy(false);
      });
  };

  return (
    <Modal
      title="Add credential"
      onClose={onClose}
      footer={
        <>
          <Button variant="outline" onClick={onClose} disabled={busy}>
            Cancel
          </Button>
          <Button variant="primary" onClick={submit} disabled={!ready || busy}>
            Add credential
          </Button>
        </>
      }
    >
      <div className="modal-field">
        <label className="modal-field-label">Name</label>
        <TextInput
          placeholder="e.g. core-rtr-usm"
          value={name}
          onChange={(e) => setName(e.target.value)}
          autoFocus
        />
      </div>
      <div className="modal-field">
        <label className="modal-field-label">Type</label>
        <Select value={kind} onChange={(e) => setKind(e.target.value as Kind)}>
          {KINDS.map((k) => (
            <option key={k} value={k}>
              {kindLabel(k)}
            </option>
          ))}
        </Select>
      </div>
      {isV3 ? (
        <V3Fields value={v3} onChange={setV3} />
      ) : (
        <div className="modal-field">
          <label className="modal-field-label">Secret</label>
          <TextInput
            className="mono"
            type="password"
            placeholder={kind === 'api_token' ? 'Bearer token' : 'Community string'}
            value={secret}
            onChange={(e) => setSecret(e.target.value)}
            autoComplete="new-password"
          />
          <span className="modal-hint">
            Sent over the encrypted-at-rest endpoint and sealed server-side. It is never shown again.
          </span>
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
        setError(errMsg(e, 'failed to update'));
        setBusy(false);
      });
  };

  return (
    <Modal
      title="Edit credential"
      onClose={onClose}
      footer={
        <>
          <Button variant="outline" onClick={onClose} disabled={busy}>
            Cancel
          </Button>
          <Button variant="primary" onClick={save} disabled={!ready || busy}>
            Save
          </Button>
        </>
      }
    >
      <div className="modal-field">
        <label className="modal-field-label">Name</label>
        <TextInput value={name} onChange={(e) => setName(e.target.value)} autoFocus />
      </div>
      <label className="cred-replace">
        <input type="checkbox" checked={replace} onChange={(e) => setReplace(e.target.checked)} />
        <span>Replace secret</span>
        <span className="muted">— the stored secret stays unless you re-enter it.</span>
      </label>
      {replace && (
        <>
          <div className="modal-field">
            <label className="modal-field-label">Type</label>
            <Select value={kind} onChange={(e) => setKind(e.target.value as Kind)}>
              {KINDS.map((k) => (
                <option key={k} value={k}>
                  {kindLabel(k)}
                </option>
              ))}
            </Select>
          </div>
          {isV3 ? (
            <V3Fields value={v3} onChange={setV3} />
          ) : (
            <div className="modal-field">
              <label className="modal-field-label">New secret</label>
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
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const submit = () => {
    setBusy(true);
    setError(null);
    api
      .deleteCredential(cred.id)
      .then(onDone)
      .catch((e: unknown) => {
        setError(errMsg(e, 'failed to delete'));
        setBusy(false);
      });
  };

  return (
    <Modal
      title="Delete credential"
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
        Delete credential <strong>{cred.name}</strong>?
        {cred.used_by > 0
          ? ` ${usageLabel(cred.used_by)} reference it — they will lose this binding.`
          : ' It is unused.'}{' '}
        This cannot be undone.
      </p>
      {error && <p className="form-error">{error}</p>}
    </Modal>
  );
}

type SortKey = 'name' | 'used_by';

export function CredentialsPage() {
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
        title="Credentials & secrets"
        trail={[{ label: 'Settings' }, { label: 'Credentials & secrets' }]}
        note="Encrypted at rest. Secret values are never displayed or returned by the API."
      />

      {unavailable ? (
        <Card>
          <p className="muted">
            Credential management is unavailable in skeleton mode (no secret store).
          </p>
        </Card>
      ) : (
        <>
          <TableToolbar>
            <SearchInput
              value={query}
              onChange={setQuery}
              placeholder="Search name or id…"
              ariaLabel="Search credentials"
            />
            <Select
              value={kindFilter}
              onChange={(e) => setKindFilter(e.target.value)}
              aria-label="Filter by type"
            >
              <option value="all">All types</option>
              <option value="snmp_v2c">SNMP v2c</option>
              <option value="snmp_v3">SNMP v3</option>
              <option value="api_token">API token</option>
            </Select>
            <TableSpacer />
            <ResultCount shown={list.length} total={rows.length} noun="credentials" />
            {authed && (
              <Button variant="primary" onClick={() => setAdding(true)}>
                + Add credential
              </Button>
            )}
          </TableToolbar>

          <div className="ytable cred-table">
            <div className="ytable-scroll">
              <div className="ytable-head" style={{ gridTemplateColumns: COLS }}>
                <div className="ytable-h sortable" onClick={() => toggleSort('name')}>
                  Name {arrow('name')}
                </div>
                <div className="ytable-h">Type</div>
                <div className="ytable-h">Secret</div>
                <div className="ytable-h sortable" onClick={() => toggleSort('used_by')}>
                  Used by {arrow('used_by')}
                </div>
                <div className="ytable-h">Credential ID</div>
                <div className="ytable-h right">Actions</div>
              </div>

              {list.length === 0 ? (
                <div className="yt-empty">
                  <p className="yt-empty-title">
                    {loading ? 'Loading…' : rows.length === 0 ? 'No credentials yet' : 'No credentials match'}
                  </p>
                  {!loading && (
                    <p className="yt-empty-sub">
                      {rows.length === 0
                        ? 'Add an SNMP community, SNMPv3 USM, or API token.'
                        : 'Try a different search or filter.'}
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
                          <span className="yt-typeicon" title={kindLabel(c.kind)}>
                            <Icon />
                          </span>
                          <span className="yt-name-txt">{c.name}</span>
                        </span>
                      </div>
                      <div className="ytable-cell">
                        <span className="yt-chip">
                          <Icon />
                          {kindLabel(c.kind)}
                        </span>
                      </div>
                      <div className="ytable-cell">
                        <SealedSecret />
                      </div>
                      <div className="ytable-cell">
                        <span className={c.used_by === 0 ? 'yt-usage zero' : 'yt-usage'}>
                          {usageLabel(c.used_by)}
                        </span>
                      </div>
                      <div className="ytable-cell">
                        <CopyableId id={c.id} />
                      </div>
                      <div className="ytable-cell right">
                        {authed && (
                          <span className="ytable-actions">
                            <IconButton title="Edit" onClick={() => setEditing(c)}>
                              <EditIcon />
                            </IconButton>
                            <IconButton title="Delete" danger onClick={() => setDeleting(c)}>
                              <TrashIcon />
                            </IconButton>
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
