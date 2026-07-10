// Device profiles (Nodes ▸ Device profiles). Profiles are device-class buckets split by
// functional role (category) × vendor-NOS family; they bundle metrics by *attaching Metric sets*
// (profiles hold no raw OIDs themselves — edit a set once and every profile using it updates).
// CRUD against /profiles; set links via /profiles/:id/templates (the API path keeps the legacy
// "templates" wording). ManageConfig-gated; 503 in skeleton surfaced.
//
// Data-table standard v2: a toolbar (search + count + "+ Add profile") over the shared `.ytable`.
// Rows are grouped under role headers (the category); each row shows its vendor and expands to a
// Collection-template attachment checklist. Add/edit via modal, delete via confirm modal.

import { Fragment, useCallback, useEffect, useMemo, useState } from 'react';
import { Trans, useTranslation } from 'react-i18next';
import { api, ApiError } from '../services/api';
import { useAuthStore } from '../store';
import type { CollectionTemplate, ProfileInput, ProfileSummary } from '../types/api';
import { PROFILE_CATEGORIES, categoryLabel } from '../lib/profileCategories';
import { PageHeader } from '../components/ui/PageHeader';
import { Card } from '../components/ui/Card';
import { Button } from '../components/ui/Button';
import { Modal } from '../components/ui/Modal';
import { TextInput, Select, RequiredMark } from '../components/ui/Field';
import { IconButton } from '../components/ui/IconButton';
import { TableToolbar, SearchInput, TableSpacer, ResultCount } from '../components/ui/TableToolbar';
import { EditIcon, TrashIcon } from '../components/ui/icons';
import './ProfilesPage.css';

const COLS = '1.8fr 1fr 120px 130px 96px';

const errMsg = (e: unknown, fallback: string) => (e instanceof ApiError ? e.message : fallback);

export function ProfilesPage() {
  const { t } = useTranslation('monitoring');
  const authed = useAuthStore((s) => s.authed);
  const [rows, setRows] = useState<ProfileSummary[]>([]);
  const [templates, setTemplates] = useState<CollectionTemplate[]>([]);
  const [query, setQuery] = useState('');
  const [unavailable, setUnavailable] = useState(false);
  const [loading, setLoading] = useState(true);
  const [adding, setAdding] = useState(false);
  const [editing, setEditing] = useState<ProfileSummary | null>(null);
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
    return rows.filter((p) =>
      [p.name, p.vendor ?? '', categoryLabel(p.category, t)]
        .join(' ')
        .toLowerCase()
        .includes(q),
    );
  }, [rows, query, t]);

  // Group the filtered rows by category, in the canonical display order; trailing "Other"
  // bucket catches any unknown token so nothing is silently hidden.
  const groups = useMemo(() => {
    const order = PROFILE_CATEGORIES.map((c) => c.token);
    const seen = new Set(order);
    const byCat = new Map<string, ProfileSummary[]>();
    for (const p of filtered) {
      const key = seen.has(p.category) ? p.category : '__other';
      (byCat.get(key) ?? byCat.set(key, []).get(key)!).push(p);
    }
    const out: { token: string; label: string; items: ProfileSummary[] }[] = [];
    for (const token of order) {
      const items = byCat.get(token);
      if (items && items.length) out.push({ token, label: categoryLabel(token, t), items });
    }
    const other = byCat.get('__other');
    if (other && other.length) out.push({ token: '__other', label: t('profiles.otherGroup'), items: other });
    return out;
  }, [filtered, t]);

  return (
    <div>
      <PageHeader
        title={t('nav:nodes.profiles')}
        trail={[{ label: t('nav:sections.nodes') }, { label: t('nav:nodes.profiles') }]}
        note={t('profiles.note')}
      />

      {unavailable ? (
        <Card>
          <p className="muted">{t('profiles.unavailable')}</p>
        </Card>
      ) : (
        <>
          <TableToolbar>
            <SearchInput
              value={query}
              onChange={setQuery}
              placeholder={t('profiles.searchPlaceholder')}
              ariaLabel={t('profiles.searchAria')}
            />
            <TableSpacer />
            <ResultCount
              shown={filtered.length}
              total={rows.length}
              noun={t('common:noun.profile', { count: rows.length })}
            />
            {authed && (
              <Button variant="primary" onClick={() => setAdding(true)}>
                + {t('profiles.addProfile')}
              </Button>
            )}
          </TableToolbar>

          <div className="ytable profiles-table">
            <div className="ytable-head" style={{ gridTemplateColumns: COLS }}>
              <div className="ytable-h">{t('profiles.cols.name')}</div>
              <div className="ytable-h">{t('profiles.cols.vendor')}</div>
              <div className="ytable-h">{t('profiles.cols.pollInterval')}</div>
              <div className="ytable-h">{t('profiles.cols.metricSets')}</div>
              <div className="ytable-h right">{t('shared.colActions')}</div>
            </div>

            {filtered.length === 0 ? (
              <div className="yt-empty">
                <p className="yt-empty-title">
                  {loading
                    ? t('common:loading')
                    : rows.length === 0
                      ? t('profiles.empty.none')
                      : t('profiles.empty.noMatch')}
                </p>
                {!loading && (
                  <p className="yt-empty-sub">
                    {rows.length === 0 ? t('profiles.empty.noneSub') : t('shared.trySearch')}
                  </p>
                )}
              </div>
            ) : (
              groups.map((g) => (
                <Fragment key={g.token}>
                  <div className="profiles-group-head">
                    <span className="profiles-group-label">{g.label}</span>
                    <span className="profiles-group-count">{g.items.length}</span>
                  </div>
                  {g.items.map((p) => {
                    const open = openTemplates === p.id;
                    return (
                      <Fragment key={p.id}>
                        <div className="ytable-row" style={{ gridTemplateColumns: COLS }}>
                          <div className="ytable-cell">
                            <span className="yt-name-txt">{p.name}</span>
                          </div>
                          <div className="ytable-cell">
                            {p.vendor ? p.vendor : <span className="muted">—</span>}
                          </div>
                          <div className="ytable-cell">
                            {p.poll_interval_secs ? (
                              `${p.poll_interval_secs}s`
                            ) : (
                              <span className="muted">{t('profiles.defaultInterval')}</span>
                            )}
                          </div>
                          <div className="ytable-cell">
                            <Button
                              variant="ghost"
                              onClick={() =>
                                setOpenTemplates((cur) => (cur === p.id ? null : p.id))
                              }
                            >
                              {open ? t('profiles.hideSets') : t('profiles.cols.metricSets')}
                            </Button>
                          </div>
                          <div className="ytable-cell right">
                            {authed && (
                              <span className="ytable-actions">
                                <IconButton title={t('profiles.editProfile')} onClick={() => setEditing(p)}>
                                  <EditIcon />
                                </IconButton>
                                <IconButton
                                  title={t('profiles.deleteProfile')}
                                  danger
                                  onClick={() => setDeleting(p)}
                                >
                                  <TrashIcon />
                                </IconButton>
                              </span>
                            )}
                          </div>
                        </div>
                        {open && (
                          <div className="crud-collection">
                            <ProfileTemplates
                              profileId={p.id}
                              templates={templates}
                              canEdit={authed}
                            />
                          </div>
                        )}
                      </Fragment>
                    );
                  })}
                </Fragment>
              ))
            )}
          </div>
        </>
      )}

      {adding && (
        <ProfileModal
          mode="add"
          onClose={() => setAdding(false)}
          onDone={() => {
            setAdding(false);
            load();
          }}
        />
      )}
      {editing && (
        <ProfileModal
          mode="edit"
          profile={editing}
          onClose={() => setEditing(null)}
          onDone={() => {
            setEditing(null);
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

/** Add or edit a profile (focused-editing modal — name + role + vendor). */
function ProfileModal({
  mode,
  profile,
  onClose,
  onDone,
}: {
  mode: 'add' | 'edit';
  profile?: ProfileSummary;
  onClose: () => void;
  onDone: () => void;
}) {
  const { t } = useTranslation('monitoring');
  const [name, setName] = useState(profile?.name ?? '');
  const [category, setCategory] = useState(profile?.category ?? 'generic-snmp');
  const [vendor, setVendor] = useState(profile?.vendor ?? '');
  const [pollInterval, setPollInterval] = useState(
    profile?.poll_interval_secs != null ? String(profile.poll_interval_secs) : '',
  );
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const submit = () => {
    if (!name.trim()) return;
    const trimmedInterval = pollInterval.trim();
    if (trimmedInterval !== '') {
      const n = Number(trimmedInterval);
      if (!Number.isInteger(n) || n < 10 || n > 3600) {
        setError(t('profiles.err.pollInterval'));
        return;
      }
    }
    setBusy(true);
    setError(null);
    const body: ProfileInput = {
      name: name.trim(),
      category,
      vendor: vendor.trim() || null,
      poll_interval_secs: trimmedInterval === '' ? null : Number(trimmedInterval),
    };
    const call =
      mode === 'edit' && profile
        ? api.updateProfile(profile.id, body)
        : api.createProfile(body).then(() => undefined);
    call.then(onDone).catch((e: unknown) => {
      setError(errMsg(e, t('profiles.err.save')));
      setBusy(false);
    });
  };

  return (
    <Modal
      title={mode === 'edit' ? t('profiles.modal.editTitle') : t('profiles.modal.addTitle')}
      onClose={onClose}
      footer={
        <>
          <Button variant="outline" onClick={onClose} disabled={busy}>
            {t('common:actions.cancel')}
          </Button>
          <Button variant="primary" onClick={submit} disabled={!name.trim() || busy}>
            {mode === 'edit' ? t('common:actions.save') : t('profiles.addProfile')}
          </Button>
        </>
      }
    >
      <div className="modal-field">
        <label className="modal-field-label">
          {t('profiles.cols.name')} <RequiredMark />
        </label>
        <TextInput
          placeholder={t('profiles.modal.namePlaceholder')}
          value={name}
          onChange={(e) => setName(e.target.value)}
          autoFocus
        />
      </div>
      <div className="modal-field-row">
        <div className="modal-field">
          <label className="modal-field-label">{t('profiles.modal.role')}</label>
          <Select value={category} onChange={(e) => setCategory(e.target.value)}>
            {PROFILE_CATEGORIES.map((c) => (
              <option key={c.token} value={c.token}>
                {t(c.labelKey)}
              </option>
            ))}
          </Select>
        </div>
        <div className="modal-field">
          <label className="modal-field-label">{t('profiles.cols.vendor')}</label>
          <TextInput
            placeholder={t('shared.optional')}
            value={vendor}
            onChange={(e) => setVendor(e.target.value)}
          />
        </div>
      </div>
      <div className="modal-field">
        <label className="modal-field-label">{t('profiles.modal.pollInterval')}</label>
        <TextInput
          placeholder={t('profiles.modal.pollIntervalPlaceholder')}
          value={pollInterval}
          onChange={(e) => setPollInterval(e.target.value)}
          inputMode="numeric"
        />
        <span className="modal-hint">{t('profiles.modal.pollIntervalHint')}</span>
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
  const { t } = useTranslation('monitoring');
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const submit = () => {
    setBusy(true);
    setError(null);
    api
      .deleteProfile(profile.id)
      .then(onDone)
      .catch((e: unknown) => {
        setError(errMsg(e, t('profiles.err.delete')));
        setBusy(false);
      });
  };

  return (
    <Modal
      title={t('profiles.deleteProfile')}
      onClose={onClose}
      footer={
        <>
          <Button variant="outline" onClick={onClose} disabled={busy}>
            {t('common:actions.cancel')}
          </Button>
          <Button variant="danger" onClick={submit} disabled={busy}>
            {t('common:actions.delete')}
          </Button>
        </>
      }
    >
      <p className="modal-confirm-text">
        <Trans
          t={t}
          i18nKey="profiles.delete.confirm"
          values={{ name: profile.name }}
          components={{ strong: <strong /> }}
        />
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
  const { t } = useTranslation('monitoring');
  const [attached, setAttached] = useState<Set<string>>(new Set());
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    api
      .listProfileTemplates(profileId)
      .then((list) => setAttached(new Set(list.map((tmpl) => tmpl.id))))
      .catch((e: unknown) => setError(errMsg(e, t('profiles.err.loadTemplates'))));
  }, [profileId, t]);

  const toggle = (id: string) => {
    const next = new Set(attached);
    if (next.has(id)) next.delete(id);
    else next.add(id);
    setAttached(next);
    setBusy(true);
    setError(null);
    api
      .setProfileTemplates(profileId, [...next])
      .catch((e: unknown) => setError(errMsg(e, t('profiles.err.saveTemplates'))))
      .finally(() => setBusy(false));
  };

  if (templates.length === 0) {
    return <p className="muted">{t('profiles.templates.empty')}</p>;
  }
  return (
    <div className="profile-templates">
      {templates.map((tmpl) => (
        <label key={tmpl.id} className="profile-template-row">
          <input
            type="checkbox"
            checked={attached.has(tmpl.id)}
            disabled={!canEdit || busy}
            onChange={() => toggle(tmpl.id)}
          />
          <span className="profile-template-name">{tmpl.name}</span>
          <span className="muted profile-template-meta">
            {tmpl.description ? `${tmpl.description} · ` : ''}
            {t('shared.metricsCount', { count: tmpl.item_count })}
          </span>
        </label>
      ))}
      {error && <p className="form-error">{error}</p>}
    </div>
  );
}
