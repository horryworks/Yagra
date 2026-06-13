// Credentials & secrets (Settings ▸ Credentials & secrets). Stores monitoring secrets
// (SNMP communities, v3 creds, API tokens) — the crown jewels. The list NEVER includes secret
// values (the API returns metadata only); the secret is write-only here and sent over the
// encrypted-at-rest create endpoint. ManageCredentials-gated.
//
// snmp_v3 secrets are structured (USM): the form collects user / level / auth / privacy
// fields and serializes them into the JSON document the backend validates and seals.
//
// Edit: name is always editable; the secret is never returned, so it is left intact unless the
// operator opts to replace it (then kind + secret are re-entered and re-sealed server-side).

import { useCallback, useEffect, useState } from 'react';
import { api, ApiError } from '../services/api';
import { useAuthStore } from '../store';
import type { CredentialSummary } from '../types/api';
import { PageHeader } from '../components/ui/PageHeader';
import { Card } from '../components/ui/Card';
import { Button } from '../components/ui/Button';
import { Modal } from '../components/ui/Modal';
import { TextInput, Select } from '../components/ui/Field';
import './CrudList.css';
import './CredentialsPage.css';

const KINDS = ['snmp_v2c', 'snmp_v3', 'api_token'];
const V3_LEVELS = ['authpriv', 'auth', 'noauth'] as const;
const V3_AUTH_PROTOCOLS = ['sha', 'sha224', 'sha256', 'sha384', 'sha512', 'md5'];
const V3_PRIV_PROTOCOLS = ['aes', 'aes192', 'aes256', 'des'];

const errMsg = (e: unknown, fallback: string) =>
  e instanceof ApiError ? e.message : fallback;

/** SNMPv3 (USM) structured form state — shared by the add row and the edit modal. */
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

/** The SNMPv3 (USM) sub-form. Controlled — the same fields back the add row and the edit modal. */
function V3Form({ value, onChange }: { value: V3State; onChange: (v: V3State) => void }) {
  const needsAuth = value.level !== 'noauth';
  const needsPriv = value.level === 'authpriv';
  const set = (patch: Partial<V3State>) => onChange({ ...value, ...patch });
  return (
    <div className="cred-v3 form-row">
      <TextInput
        className="mono"
        placeholder="USM user"
        value={value.user}
        onChange={(e) => set({ user: e.target.value })}
      />
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
      {needsAuth && (
        <>
          <Select value={value.authProto} onChange={(e) => set({ authProto: e.target.value })}>
            {V3_AUTH_PROTOCOLS.map((p) => (
              <option key={p} value={p}>
                auth {p}
              </option>
            ))}
          </Select>
          <TextInput
            className="mono"
            type="password"
            placeholder="Auth passphrase"
            value={value.authKey}
            onChange={(e) => set({ authKey: e.target.value })}
            autoComplete="new-password"
          />
        </>
      )}
      {needsPriv && (
        <>
          <Select value={value.privProto} onChange={(e) => set({ privProto: e.target.value })}>
            {V3_PRIV_PROTOCOLS.map((p) => (
              <option key={p} value={p}>
                priv {p}
              </option>
            ))}
          </Select>
          <TextInput
            className="mono"
            type="password"
            placeholder="Privacy passphrase"
            value={value.privKey}
            onChange={(e) => set({ privKey: e.target.value })}
            autoComplete="new-password"
          />
        </>
      )}
    </div>
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
  const [kind, setKind] = useState(cred.kind);
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
      <div className="form-col">
        <label className="cred-edit-label">
          Name
          <TextInput value={name} onChange={(e) => setName(e.target.value)} />
        </label>
        <label className="cred-edit-check">
          <input
            type="checkbox"
            checked={replace}
            onChange={(e) => setReplace(e.target.checked)}
          />
          <span>Replace secret</span>
          <span className="muted">— the stored secret stays unless you re-enter it.</span>
        </label>
        {replace && (
          <>
            <Select value={kind} onChange={(e) => setKind(e.target.value)}>
              {KINDS.map((k) => (
                <option key={k} value={k}>
                  {k}
                </option>
              ))}
            </Select>
            {isV3 ? (
              <V3Form value={v3} onChange={setV3} />
            ) : (
              <TextInput
                className="mono"
                type="password"
                placeholder="New secret"
                value={secret}
                onChange={(e) => setSecret(e.target.value)}
                autoComplete="new-password"
              />
            )}
          </>
        )}
        {error && <p className="form-error">{error}</p>}
      </div>
    </Modal>
  );
}

export function CredentialsPage() {
  const authed = useAuthStore((s) => s.authed);
  const [rows, setRows] = useState<CredentialSummary[]>([]);
  const [name, setName] = useState('');
  const [kind, setKind] = useState(KINDS[0]);
  const [secret, setSecret] = useState('');
  const [v3, setV3] = useState<V3State>(emptyV3);
  const [error, setError] = useState<string | null>(null);
  const [unavailable, setUnavailable] = useState(false);
  const [loading, setLoading] = useState(true);
  const [editing, setEditing] = useState<CredentialSummary | null>(null);

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

  const isV3 = kind === 'snmp_v3';
  const addReady = name.trim() !== '' && (isV3 ? v3Ready(v3) : secret !== '');

  const add = () => {
    setError(null);
    api
      .createCredential({
        name: name.trim(),
        kind,
        secret: isV3 ? buildV3Secret(v3) : secret,
      })
      .then(() => {
        setName('');
        setSecret('');
        setV3(emptyV3());
        load();
      })
      .catch((e: unknown) => setError(errMsg(e, 'failed to add')));
  };

  const remove = (id: string) =>
    api
      .deleteCredential(id)
      .then(load)
      .catch((e: unknown) => setError(errMsg(e, 'failed to delete')));

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
        <Card title="Credentials">
          {authed && (
            <>
              <div className="crud-add form-row">
                <TextInput
                  placeholder="Name"
                  value={name}
                  onChange={(e) => setName(e.target.value)}
                />
                <Select value={kind} onChange={(e) => setKind(e.target.value)}>
                  {KINDS.map((k) => (
                    <option key={k} value={k}>
                      {k}
                    </option>
                  ))}
                </Select>
                {!isV3 && (
                  <TextInput
                    className="mono"
                    type="password"
                    placeholder="Secret"
                    value={secret}
                    onChange={(e) => setSecret(e.target.value)}
                    autoComplete="new-password"
                  />
                )}
                <Button variant="primary" onClick={add} disabled={!addReady}>
                  Add credential
                </Button>
              </div>
              {isV3 && <V3Form value={v3} onChange={setV3} />}
            </>
          )}
          {error && <p className="form-error">{error}</p>}
          {rows.length === 0 ? (
            <p className="muted">{loading ? 'Loading…' : 'No credentials yet.'}</p>
          ) : (
            <div className="crud-list">
              {rows.map((c) => (
                <div className="crud-row" key={c.id}>
                  <span className="crud-name">{c.name}</span>
                  <span className="crud-kind mono">{c.kind}</span>
                  <span className="crud-id mono">{c.id}</span>
                  {authed && (
                    <span className="crud-actions">
                      <Button variant="outline" onClick={() => setEditing(c)}>
                        Edit
                      </Button>
                      <Button variant="ghost" onClick={() => remove(c.id)}>
                        Delete
                      </Button>
                    </span>
                  )}
                </div>
              ))}
            </div>
          )}
        </Card>
      )}

      {editing && (
        <EditCredentialModal
          cred={editing}
          onClose={() => setEditing(null)}
          onSaved={load}
        />
      )}
    </div>
  );
}
