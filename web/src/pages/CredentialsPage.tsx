// Credentials & secrets (Settings ▸ Credentials & secrets). Stores monitoring secrets
// (SNMP communities, v3 creds, API tokens) — the crown jewels. The list NEVER includes secret
// values (the API returns metadata only); the secret is write-only here and sent over the
// encrypted-at-rest create endpoint. ManageCredentials-gated.

import { useCallback, useEffect, useState } from 'react';
import { api, ApiError } from '../services/api';
import { useAuthStore } from '../store';
import type { CredentialSummary } from '../types/api';
import { PageHeader } from '../components/ui/PageHeader';
import { Card } from '../components/ui/Card';
import { Button } from '../components/ui/Button';
import { TextInput, Select } from '../components/ui/Field';
import './CrudList.css';

const KINDS = ['snmp_v2c', 'snmp_v3', 'api_token'];

export function CredentialsPage() {
  const authed = useAuthStore((s) => s.authed);
  const [rows, setRows] = useState<CredentialSummary[]>([]);
  const [name, setName] = useState('');
  const [kind, setKind] = useState(KINDS[0]);
  const [secret, setSecret] = useState('');
  const [error, setError] = useState<string | null>(null);
  const [unavailable, setUnavailable] = useState(false);

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

  const add = () => {
    setError(null);
    api
      .createCredential({ name: name.trim(), kind, secret })
      .then(() => {
        setName('');
        setSecret('');
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
              <TextInput
                className="mono"
                type="password"
                placeholder="Secret"
                value={secret}
                onChange={(e) => setSecret(e.target.value)}
                autoComplete="new-password"
              />
              <Button
                variant="primary"
                onClick={add}
                disabled={!name.trim() || !secret}
              >
                Add credential
              </Button>
            </div>
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
