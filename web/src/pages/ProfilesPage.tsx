// Device profiles (Nodes ▸ Device profiles). Profiles are device-class buckets that bundle
// collection sets by *attaching Collection templates* (profiles hold no raw OIDs themselves —
// edit a template once and every profile using it updates). CRUD against /profiles;
// template links via /profiles/:id/templates. ManageConfig-gated; 503 in skeleton surfaced.
//
// Data-table standard v2: a toolbar (count + "+ Add profile") over the shared `.ytable`; add via
// modal, delete via confirm modal. Each row expands to a Collection-template attachment checklist.

import { Fragment, useCallback, useEffect, useMemo, useState } from 'react';
import { api, ApiError } from '../services/api';
import { useAuthStore } from '../store';
import type { CollectionTemplate, ProfileSummary } from '../types/api';
import { PageHeader } from '../components/ui/PageHeader';
import { Card } from '../components/ui/Card';
import { Button } from '../components/ui/Button';
import { Modal } from '../components/ui/Modal';
import { TextInput, RequiredMark } from '../components/ui/Field';
import { IconButton } from '../components/ui/IconButton';
import { TableToolbar, SearchInput, TableSpacer, ResultCount } from '../components/ui/TableToolbar';
import { CopyableId } from '../components/ui/tableCells';
import { TrashIcon } from '../components/ui/icons';
import './ProfilesPage.css';

const COLS = '1.6fr 1.4fr 150px 72px';

const errMsg = (e: unknown, fallback: string) => (e instanceof ApiError ? e.message : fallback);

export function ProfilesPage() {
  const authed = useAuthStore((s) => s.authed);
  const [rows, setRows] = useState<ProfileSummary[]>([]);
  const [templates, setTemplates] = useState<CollectionTemplate[]>([]);
  const [query, setQuery] = useState('');
  const [unavailable, setUnavailable] = useState(false);
  const [loading, setLoading] = useState(true);
  const [adding, setAdding] = useState(false);
  const [deleting, setDeleting] = useState<ProfileSummary | null>(null);
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
      })
      .finally(() => setLoading(false));
    api.listCollectionTemplates().then(setTemplates).catch(() => setTemplates([]));
  }, []);

  useEffect(() => {
    load();
  }, [load]);

  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    if (q === '') return rows;
    return rows.filter(
      (p) => p.name.toLowerCase().includes(q) || p.id.toLowerCase().includes(q),
    );
  }, [rows, query]);

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
        <>
          <TableToolbar>
            <SearchInput
              value={query}
              onChange={setQuery}
              placeholder="Search name or id…"
              ariaLabel="Search profiles"
            />
            <TableSpacer />
            <ResultCount shown={filtered.length} total={rows.length} noun="profiles" />
            {authed && (
              <Button variant="primary" onClick={() => setAdding(true)}>
                + Add profile
              </Button>
            )}
          </TableToolbar>

          <div className="ytable profiles-table">
            <div className="ytable-head" style={{ gridTemplateColumns: COLS }}>
              <div className="ytable-h">Name</div>
              <div className="ytable-h">Profile ID</div>
              <div className="ytable-h">Templates</div>
              <div className="ytable-h right">Actions</div>
            </div>

            {filtered.length === 0 ? (
              <div className="yt-empty">
                <p className="yt-empty-title">
                  {loading ? 'Loading…' : rows.length === 0 ? 'No profiles yet' : 'No profiles match'}
                </p>
                {!loading && (
                  <p className="yt-empty-sub">
                    {rows.length === 0
                      ? 'Add a device-class profile (e.g. “Cisco IOS switch”).'
                      : 'Try a different search.'}
                  </p>
                )}
              </div>
            ) : (
              filtered.map((p) => {
                const open = openTemplates === p.id;
                return (
                  <Fragment key={p.id}>
                    <div className="ytable-row" style={{ gridTemplateColumns: COLS }}>
                      <div className="ytable-cell">
                        <span className="yt-name-txt">{p.name}</span>
                      </div>
                      <div className="ytable-cell">
                        <CopyableId id={p.id} />
                      </div>
                      <div className="ytable-cell">
                        <Button
                          variant="ghost"
                          onClick={() => setOpenTemplates((cur) => (cur === p.id ? null : p.id))}
                        >
                          {open ? 'Hide templates' : 'Templates'}
                        </Button>
                      </div>
                      <div className="ytable-cell right">
                        {authed && (
                          <span className="ytable-actions">
                            <IconButton title="Delete profile" danger onClick={() => setDeleting(p)}>
                              <TrashIcon />
                            </IconButton>
                          </span>
                        )}
                      </div>
                    </div>
                    {open && (
                      <div className="crud-collection">
                        <ProfileTemplates profileId={p.id} templates={templates} canEdit={authed} />
                      </div>
                    )}
                  </Fragment>
                );
              })
            )}
          </div>
        </>
      )}

      {adding && (
        <AddProfileModal
          onClose={() => setAdding(false)}
          onDone={() => {
            setAdding(false);
            load();
          }}
        />
      )}
      {deleting && (
        <DeleteProfileModal
          profile={deleting}
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

/** Create a profile (focused-editing modal — just a name). */
function AddProfileModal({ onClose, onDone }: { onClose: () => void; onDone: () => void }) {
  const [name, setName] = useState('');
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const submit = () => {
    if (!name.trim()) return;
    setBusy(true);
    setError(null);
    api
      .createProfile(name.trim())
      .then(onDone)
      .catch((e: unknown) => {
        setError(errMsg(e, 'failed to add profile'));
        setBusy(false);
      });
  };

  return (
    <Modal
      title="Add device profile"
      onClose={onClose}
      footer={
        <>
          <Button variant="outline" onClick={onClose} disabled={busy}>
            Cancel
          </Button>
          <Button variant="primary" onClick={submit} disabled={!name.trim() || busy}>
            Add profile
          </Button>
        </>
      }
    >
      <div className="modal-field">
        <label className="modal-field-label">
          Name <RequiredMark />
        </label>
        <TextInput
          placeholder="e.g. Cisco IOS switch"
          value={name}
          onChange={(e) => setName(e.target.value)}
          autoFocus
        />
      </div>
      {error && <p className="form-error">{error}</p>}
    </Modal>
  );
}

/** Confirm + delete a profile (destructive-consent modal). */
function DeleteProfileModal({
  profile,
  onClose,
  onDone,
}: {
  profile: ProfileSummary;
  onClose: () => void;
  onDone: () => void;
}) {
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const submit = () => {
    setBusy(true);
    setError(null);
    api
      .deleteProfile(profile.id)
      .then(onDone)
      .catch((e: unknown) => {
        setError(errMsg(e, 'failed to delete profile'));
        setBusy(false);
      });
  };

  return (
    <Modal
      title="Delete profile"
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
        Delete profile <strong>{profile.name}</strong>? Nodes using it keep running but lose this
        class's collection defaults.
      </p>
      {error && <p className="form-error">{error}</p>}
    </Modal>
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
