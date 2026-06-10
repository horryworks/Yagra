// Device profiles (Nodes ▸ Device profiles, decision 2). Profiles are device-class buckets
// that bundle collection sets (§3.5); the create API today takes just a name (the OID/set
// wiring lands with Collection templates). CRUD against /profiles. ManageConfig-gated; 503 in
// skeleton mode is surfaced.

import { useCallback, useEffect, useState } from 'react';
import { api, ApiError } from '../services/api';
import { useAuthStore } from '../store';
import type { ProfileSummary } from '../types/api';
import { PageHeader } from '../components/ui/PageHeader';
import { Card } from '../components/ui/Card';
import { Button } from '../components/ui/Button';
import { TextInput } from '../components/ui/Field';
import './CrudList.css';

export function ProfilesPage() {
  const authed = useAuthStore((s) => s.authed);
  const [rows, setRows] = useState<ProfileSummary[]>([]);
  const [name, setName] = useState('');
  const [error, setError] = useState<string | null>(null);
  const [unavailable, setUnavailable] = useState(false);

  const load = useCallback(() => {
    api
      .listProfiles()
      .then((list) => {
        setRows(list);
        setUnavailable(false);
      })
      .catch((e: unknown) => {
        if (e instanceof ApiError && e.code === 'admin_unavailable') setUnavailable(true);
        else if (e instanceof ApiError && e.status === 401) setUnavailable(false);
      });
  }, []);

  useEffect(() => {
    load();
  }, [load]);

  const add = () => {
    setError(null);
    api
      .createProfile(name.trim())
      .then(() => {
        setName('');
        load();
      })
      .catch((e: unknown) => setError(e instanceof ApiError ? e.message : 'failed to add'));
  };

  const remove = (id: string) =>
    api
      .deleteProfile(id)
      .then(load)
      .catch((e: unknown) => setError(e instanceof ApiError ? e.message : 'failed to delete'));

  return (
    <div>
      <PageHeader
        title="Device profiles"
        trail={[{ label: 'Nodes' }, { label: 'Device profiles' }]}
        note="Device-class templates that bundle collection sets and bind credentials."
      />

      {unavailable ? (
        <Card>
          <p className="muted">
            Profile management is unavailable in skeleton mode (no metadata store).
          </p>
        </Card>
      ) : (
        <Card title="Profiles">
          {authed && (
            <div className="crud-add form-row">
              <TextInput
                placeholder="Profile name (e.g. Cisco IOS switch)"
                value={name}
                onChange={(e) => setName(e.target.value)}
              />
              <Button variant="primary" onClick={add} disabled={!name.trim()}>
                Add profile
              </Button>
            </div>
          )}
          {error && <p className="form-error">{error}</p>}
          {rows.length === 0 ? (
            <p className="muted">No profiles yet.</p>
          ) : (
            <div className="crud-list">
              {rows.map((p) => (
                <div className="crud-row" key={p.id}>
                  <span className="crud-name">{p.name}</span>
                  <span className="crud-id mono">{p.id}</span>
                  {authed && (
                    <Button variant="ghost" onClick={() => remove(p.id)}>
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
