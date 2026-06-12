// Credentials & secrets (Settings ▸ Credentials & secrets). Stores monitoring secrets
// (SNMP communities, v3 creds, API tokens) — the crown jewels. The list NEVER includes secret
// values (the API returns metadata only); the secret is write-only here and sent over the
// encrypted-at-rest create endpoint. ManageCredentials-gated.
//
// snmp_v3 secrets are structured (USM): the form collects user / level / auth / privacy
// fields and serializes them into the JSON document the backend validates and seals.

import { useCallback, useEffect, useState } from 'react';
import { api, ApiError } from '../services/api';
import { useAuthStore } from '../store';
import type { CredentialSummary } from '../types/api';
import { PageHeader } from '../components/ui/PageHeader';
import { Card } from '../components/ui/Card';
import { Button } from '../components/ui/Button';
import { TextInput, Select } from '../components/ui/Field';
import './CrudList.css';
import './CredentialsPage.css';

const KINDS = ['snmp_v2c', 'snmp_v3', 'api_token'];
const V3_LEVELS = ['authpriv', 'auth', 'noauth'] as const;
const V3_AUTH_PROTOCOLS = ['sha', 'sha224', 'sha256', 'sha384', 'sha512', 'md5'];
const V3_PRIV_PROTOCOLS = ['aes', 'aes192', 'aes256', 'des'];

export function CredentialsPage() {
  const authed = useAuthStore((s) => s.authed);
  const [rows, setRows] = useState<CredentialSummary[]>([]);
  const [name, setName] = useState('');
  const [kind, setKind] = useState(KINDS[0]);
  const [secret, setSecret] = useState('');
  const [error, setError] = useState<string | null>(null);
  const [unavailable, setUnavailable] = useState(false);

  // SNMPv3 (USM) structured fields.
  const [v3User, setV3User] = useState('');
  const [v3Level, setV3Level] = useState<(typeof V3_LEVELS)[number]>('authpriv');
  const [v3AuthProto, setV3AuthProto] = useState(V3_AUTH_PROTOCOLS[0]);
  const [v3AuthKey, setV3AuthKey] = useState('');
  const [v3PrivProto, setV3PrivProto] = useState(V3_PRIV_PROTOCOLS[0]);
  const [v3PrivKey, setV3PrivKey] = useState('');

  const load = useCallback(() => {
    api
      .listCredentials()
      .then((list) => {
        setRows(list);
        setUnavailable(false);
      })
      .catch((e: unknown) => {
        if (e instanceof ApiError && e.code === 'admin_unavailable') setUnavailable(true);
      });
  }, []);

  useEffect(() => {
    load();
  }, [load]);

  const isV3 = kind === 'snmp_v3';
  const needsAuth = isV3 && v3Level !== 'noauth';
  const needsPriv = isV3 && v3Level === 'authpriv';

  const v3Ready =
    !isV3 ||
    (v3User.trim() !== '' && (!needsAuth || v3AuthKey !== '') && (!needsPriv || v3PrivKey !== ''));

  /** Serialize the v3 form into the USM JSON document the backend validates and seals. */
  const buildV3Secret = (): string =>
    JSON.stringify({
      user: v3User.trim(),
      security_level: v3Level,
      ...(needsAuth ? { auth_protocol: v3AuthProto, auth_key: v3AuthKey } : {}),
      ...(needsPriv ? { priv_protocol: v3PrivProto, priv_key: v3PrivKey } : {}),
    });

  const add = () => {
    setError(null);
    api
      .createCredential({
        name: name.trim(),
        kind,
        secret: isV3 ? buildV3Secret() : secret,
      })
      .then(() => {
        setName('');
        setSecret('');
        setV3User('');
        setV3AuthKey('');
        setV3PrivKey('');
        load();
      })
      .catch((e: unknown) => setError(e instanceof ApiError ? e.message : 'failed to add'));
  };

  const remove = (id: string) =>
    api
      .deleteCredential(id)
      .then(load)
      .catch((e: unknown) => setError(e instanceof ApiError ? e.message : 'failed to delete'));

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
                <Button
                  variant="primary"
                  onClick={add}
                  disabled={!name.trim() || (isV3 ? !v3Ready : !secret)}
                >
                  Add credential
                </Button>
              </div>
              {isV3 && (
                <div className="cred-v3 form-row">
                  <TextInput
                    className="mono"
                    placeholder="USM user"
                    value={v3User}
                    onChange={(e) => setV3User(e.target.value)}
                  />
                  <Select
                    value={v3Level}
                    onChange={(e) => setV3Level(e.target.value as (typeof V3_LEVELS)[number])}
                  >
                    {V3_LEVELS.map((l) => (
                      <option key={l} value={l}>
                        {l}
                      </option>
                    ))}
                  </Select>
                  {needsAuth && (
                    <>
                      <Select
                        value={v3AuthProto}
                        onChange={(e) => setV3AuthProto(e.target.value)}
                      >
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
                        value={v3AuthKey}
                        onChange={(e) => setV3AuthKey(e.target.value)}
                        autoComplete="new-password"
                      />
                    </>
                  )}
                  {needsPriv && (
                    <>
                      <Select
                        value={v3PrivProto}
                        onChange={(e) => setV3PrivProto(e.target.value)}
                      >
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
                        value={v3PrivKey}
                        onChange={(e) => setV3PrivKey(e.target.value)}
                        autoComplete="new-password"
                      />
                    </>
                  )}
                </div>
              )}
            </>
          )}
          {error && <p className="form-error">{error}</p>}
          {rows.length === 0 ? (
            <p className="muted">No credentials yet.</p>
          ) : (
            <div className="crud-list">
              {rows.map((c) => (
                <div className="crud-row" key={c.id}>
                  <span className="crud-name">{c.name}</span>
                  <span className="crud-kind mono">{c.kind}</span>
                  <span className="crud-id mono">{c.id}</span>
                  {authed && (
                    <Button variant="ghost" onClick={() => remove(c.id)}>
                      Delete
                    </Button>
                  )}
                </div>
              ))}
            </div>
          )}
        </Card>
      )}
    </div>
  );
}
