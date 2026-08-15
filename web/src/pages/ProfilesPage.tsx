// SPDX-License-Identifier: AGPL-3.0-only
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
import { api, errMsg } from '../services/api';
import { useAuthStore } from '../store';
import type { CollectionTemplate, ProfileInput, ProfileSummary } from '../types/api';
import { PROFILE_CATEGORIES, categoryLabel } from '../lib/profileCategories';
import { PageHeader } from '../components/ui/PageHeader';
import { Button } from '../components/ui/Button';
import { ConfirmDeleteModal } from '../components/ui/ConfirmDeleteModal';
import { Modal } from '../components/ui/Modal';
import { TextInput, Select, RequiredMark } from '../components/ui/Field';
import { OverflowMenu } from '../components/ui/OverflowMenu';
import { TableToolbar, TableSpacer, ResultCount } from '../components/ui/TableToolbar';
import { ColumnFilterRow } from '../components/ui/ColumnFilterRow';
import { ClearFilters } from '../components/ui/ClearFilters';
import { FilterBar } from '../components/ui/FilterBar';
import { FilterButton, MobileFilterSheet } from '../components/ui/MobileFilterSheet';
import { defaultFilters, type FilterState } from '../lib/columnFilter';
import { facetCounts } from '../lib/filterCounts';
import { buildPredicate } from '../lib/filterPredicate';
import {
  profileCategoryColumns,
  profileColumns,
  profileFilterLabels,
} from './monitoringConfigFilters';
import { EditIcon, TrashIcon } from '../components/ui/icons';
import './ProfilesPage.css';
import { classifyLoadError, type LoadBlock } from '../lib/loadState';
import { LoadBlockNotice } from '../components/ui/LoadBlockNotice';

const COLS = '1.8fr 1fr 120px 130px 96px';

export function ProfilesPage() {
  const { t } = useTranslation('monitoring');
  const authed = useAuthStore((s) => s.authed);
  const [rows, setRows] = useState<ProfileSummary[]>([]);
  const [templates, setTemplates] = useState<CollectionTemplate[]>([]);
  const [block, setBlock] = useState<LoadBlock | null>(null);
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
        setBlock(null);
      })
      .catch((e: unknown) => setBlock(classifyLoadError(e)))
      .finally(() => setLoading(false));
    api.listCollectionTemplates().then(setTemplates).catch(() => setTemplates([]));
  }, []);

  useEffect(() => {
    load();
  }, [load]);

  // Three of the four controls sit under their own column headers (ADR-053 Inc.6 decision F). The
  // fourth — category — has no column: it *is* the group heading, so it goes in a `FilterBar`
  // beside the table. The one search box this replaces matched the category label too, and dropping
  // that silently would take a capability away.
  const colFilters = useMemo(() => profileColumns(t), [t]);
  const catCols = useMemo(
    () =>
      profileCategoryColumns(
        t,
        PROFILE_CATEGORIES.map((c) => ({ token: c.token, label: categoryLabel(c.token, t) })),
      ),
    [t],
  );
  const allFilterCols = useMemo(() => [...colFilters, ...catCols], [colFilters, catCols]);
  const [filters, setFilters] = useState<FilterState>({});
  const [sheet, setSheet] = useState(false);

  const filtered = useMemo(
    () => rows.filter(buildPredicate(allFilterCols, filters, Date.now())),
    [rows, allFilterCols, filters],
  );
  // Every enum column's facet counts, computed once per (rows, filters) change.
  // ⚠️ This used to be `category` here and a bare `facetCounts(...)` **inside the cell renderer** —
  // so the row's counts were recomputed for every cell on every render, each pass walking all rows,
  // and the two surfaces disagreed: the desktop row showed counts under Poll interval and the
  // mobile sheet, given only `category`, showed none. One memo, one answer, both surfaces.
  const filterCounts = useMemo(
    () =>
      Object.fromEntries(
        allFilterCols
          .filter((c) => c.filter.kind === 'enum')
          .map((c) => [c.key, facetCounts(rows, allFilterCols, filters, c.key, Date.now())]),
      ),
    [rows, allFilterCols, filters],
  );
  const filterLabels: Record<string, string> = useMemo(
    () => ({ ...profileFilterLabels(t), category: t('profiles.cols.category') }),
    [t],
  );

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

      {block ? (
        <LoadBlockNotice block={block} unavailable={t('profiles.unavailable')} />
      ) : (
        <>
          <TableToolbar>
            <FilterButton
              columns={allFilterCols}
              filters={filters}
              onOpen={() => setSheet(true)}
            />
            <ClearFilters
              columns={allFilterCols}
              filters={filters}
              onClear={() => setFilters(defaultFilters(allFilterCols))}
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
          {sheet && (
            <MobileFilterSheet
              columns={allFilterCols}
              labels={filterLabels}
              filters={filters}
              onChange={setFilters}
              counts={filterCounts}
              onClose={() => setSheet(false)}
            />
          )}
          {/* Category has no column of its own — it is the group heading below. */}
          <FilterBar
            columns={catCols}
            labels={filterLabels}
            filters={filters}
            onChange={setFilters}
            counts={filterCounts}
          />

          <div className="ytable profiles-table">
            <div className="ytable-head" style={{ gridTemplateColumns: COLS }}>
              <div className="ytable-h">{t('profiles.cols.name')}</div>
              <div className="ytable-h">{t('profiles.cols.vendor')}</div>
              <div className="ytable-h">{t('profiles.cols.pollInterval')}</div>
              <div className="ytable-h">{t('profiles.cols.metricSets')}</div>
              <div className="ytable-h right">{t('shared.colActions')}</div>
            </div>
            {/* ⚠️ The SAME `COLS` const as the header and every row — three grids, one binding, the
                discipline `DataTable` enforces for its own. This screen keeps its hand-rolled table
                because the rows are grouped by category and `DataTable` has no group heading; the
                trade is stated in `monitoringConfigFilters.ts`.
                The mobile gate used to be a `display: none` on `.ytable-filters` in
                `styles/table.css`. It moved here so that all seven filter surfaces read the one
                decision in `MobileFilterSheet.tsx` — the CSS copy was correct and still cost
                nothing to keep, but it meant "is the row visible" had two answers in two
                languages, and the four rows that had *neither* were invisible against that.
                ⚠️ `colFilters`, not `allFilterCols`: each surface answers for the columns it draws.
                A category filter is narrowing the list through the `FilterBar` above, which shows
                itself for exactly that reason — forcing *this* row open too would reveal a control
                that is not the one responsible. */}
            <ColumnFilterRow
              columns={colFilters}
              slots={['name', 'vendor', 'interval', null, null]}
              filters={filters}
              onChange={setFilters}
              counts={filterCounts}
              labels={filterLabels}
              className="ytable-filters"
              style={{ gridTemplateColumns: COLS }}
            />

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
                                <OverflowMenu
                                  actions={[
                                    {
                                      label: t('profiles.editProfile'),
                                      icon: <EditIcon />,
                                      onClick: () => setEditing(p),
                                    },
                                    {
                                      label: t('profiles.deleteProfile'),
                                      icon: <TrashIcon />,
                                      danger: true,
                                      onClick: () => setDeleting(p),
                                    },
                                  ]}
                                />
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
  return (
    <ConfirmDeleteModal
      title={t('profiles.deleteProfile')}
      onConfirm={() => api.deleteProfile(profile.id)}
      errorFallback={t('profiles.err.delete')}
      onClose={onClose}
      onDone={onDone}
    >
      <Trans
        t={t}
        i18nKey="profiles.delete.confirm"
        values={{ name: profile.name }}
        components={{ strong: <strong /> }}
      />
    </ConfirmDeleteModal>
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
