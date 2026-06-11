// Device profiles (Nodes ▸ Device profiles). Profiles are device-class buckets that bundle
// collection sets by *attaching Collection templates* (profiles hold no raw OIDs themselves —
// edit a template once and every profile using it updates). CRUD against /profiles;
// template links via /profiles/:id/templates. ManageConfig-gated; 503 in skeleton surfaced.

import { useCallback, useEffect, useState } from 'react';
import { api, ApiError } from '../services/api';
import { useAuthStore } from '../store';
import type { CollectionTemplate, ProfileSummary } from '../types/api';
import { PageHeader } from '../components/ui/PageHeader';
import { Card } from '../components/ui/Card';
import { Button } from '../components/ui/Button';
import { TextInput } from '../components/ui/Field';
import './CrudList.css';

const errMsg = (e: unknown, fallback: string) => (e instanceof ApiError ? e.message : fallback);

export function ProfilesPage() {
  const authed = useAuthStore((s) => s.authed);
  const [rows, setRows] = useState<ProfileSummary[]>([]);
  const [templates, setTemplates] = useState<CollectionTemplate[]>([]);
  const [name, setName] = useState('');
  const [error, setError] = useState<string | null>(null);
  const [unavailable, setUnavailable] = useState(false);
  // Which profile's template attachments are expanded (one at a time).
  const [openTemplates, setOpenTemplates] = useState<string | null>(null);

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
    api.listCollectionTemplates().then(setTemplates).catch(() => setTemplates([]));
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
      .catch((e: unknown) => setError(errMsg(e, 'failed to add profile')));
  };

  const remove = (id: string) =>
    api
      .deleteProfile(id)
      .then(load)
      .catch((e: unknown) => setError(errMsg(e, 'failed to delete profile')));

  return (
    <div>
      <PageHeader
        title="Device profiles"
        trail={[{ label: 'Nodes' }, { label: 'Device profiles' }]}
        note="Device-class buckets. Attach Collection templates here; nodes inherit them via their profile."
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
                <div key={p.id}>
                  <div className="crud-row">
                    <span className="crud-name">{p.name}</span>
                    <span className="crud-id mono">{p.id}</span>
                    <Button
                      variant="ghost"
                      onClick={() => setOpenTemplates((cur) => (cur === p.id ? null : p.id))}
                    >
                      {openTemplates === p.id ? 'Hide templates' : 'Templates'}
                    </Button>
                    {authed && (
                      <Button variant="ghost" onClick={() => remove(p.id)}>
                        Delete
                      </Button>
                    )}
                  </div>
                  {openTemplates === p.id && (
                    <div className="crud-collection">
                      <ProfileTemplates profileId={p.id} templates={templates} canEdit={authed} />
                    </div>
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

/** The set of Collection templates attached to a profile, as a checklist. Toggling a row
 *  saves the new set immediately (replace-all via setProfileTemplates). */
function ProfileTemplates({
  profileId,
  templates,
  canEdit,
}: {
  profileId: string;
  templates: CollectionTemplate[];
  canEdit: boolean;
}) {
  const [attached, setAttached] = useState<Set<string>>(new Set());
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    api
      .listProfileTemplates(profileId)
      .then((list) => setAttached(new Set(list.map((t) => t.id))))
      .catch((e: unknown) => setError(errMsg(e, 'failed to load attached templates')));
  }, [profileId]);

  const toggle = (id: string) => {
    const next = new Set(attached);
    if (next.has(id)) next.delete(id);
    else next.add(id);
    setAttached(next);
    setBusy(true);
    setError(null);
    api
      .setProfileTemplates(profileId, [...next])
      .catch((e: unknown) => setError(errMsg(e, 'failed to save templates')))
      .finally(() => setBusy(false));
  };

  if (templates.length === 0) {
    return <p className="muted">No collection templates exist yet. Create some first.</p>;
  }
  return (
    <div className="profile-templates">
      {templates.map((t) => (
        <label key={t.id} className="profile-template-row">
          <input
            type="checkbox"
            checked={attached.has(t.id)}
            disabled={!canEdit || busy}
            onChange={() => toggle(t.id)}
          />
          <span className="profile-template-name">{t.name}</span>
          <span className="muted profile-template-meta">
            {t.description ? `${t.description} · ` : ''}
            {t.item_count} metrics
          </span>
        </label>
      ))}
      {error && <p className="form-error">{error}</p>}
    </div>
  );
}
