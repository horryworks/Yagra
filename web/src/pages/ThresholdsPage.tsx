// SPDX-License-Identifier: AGPL-3.0-only
// Alert rules (Alerts ▸ Alert rules). Thresholds resolve by hierarchical
// override — profile → group → node, most-specific wins (§3.3) — so each rule carries a
// scope level + id. CRUD against /thresholds. Rules are evaluated live: the alert engine
// snapshots them (refreshed every ~30s) and checks each matching poll sample through the
// same hysteresis/flapping machinery as liveness, so a breach fires a real alert.
//
// Data-table standard v2: a toolbar (count + "+ Add rule") over the shared `DataTable`; the
// add form and delete confirmation both go through modals. The blue-left-border note card
// above the toolbar keeps the "what a rule is" explainer in view.
//
// This is the one configuration list that grows with the fleet — a node-level override is per
// (node × metric) — so it uses the virtualized `DataTable` rather than a hand-rolled grid, and the
// server caps the response. When the cap bites, the toolbar says so: a silently short ruleset reads
// as "these are all the rules", which is exactly the wrong belief to hold about alerting config.

import { useCallback, useEffect, useMemo, useState } from 'react';
import { Trans, useTranslation } from 'react-i18next';
import { api, errMsg } from '../services/api';
import { useCan } from '../store';
import {
  CREATABLE_SCOPE_LEVELS,
  DIRECTIONS,
  type Direction,
  type NodeGroup,
  type ProfileSummary,
  type ScopeLevel,
  type StoredThreshold,
} from '../types/api';
import { METRIC_PRESETS } from '../lib/suppression';
import { LIVENESS_METRIC } from '../lib/format';
import { metricMeaningKey } from './thresholdMeaning';
import {
  isThresholdReady,
  scopeIdKind,
  thresholdBody,
  thresholdFormFrom,
  type ThresholdForm,
} from './thresholdRequest';
import { PageHeader } from '../components/ui/PageHeader';
import { Card } from '../components/ui/Card';
import { Button } from '../components/ui/Button';
import { ConfirmDeleteModal } from '../components/ui/ConfirmDeleteModal';
import { Modal } from '../components/ui/Modal';
import { TextInput, Select } from '../components/ui/Field';
import { Badge } from '../components/ui/Badge';
import { EntityName, useEntityNames } from '../components/ui/EntityName';
import { NodePicker } from '../components/NodePicker/NodePicker';
import { groupOptions } from '../lib/nodeTree';
import { IconButton } from '../components/ui/IconButton';
import { EditIcon } from '../components/ui/icons';
import { ClearFilters } from '../components/ui/ClearFilters';
import { FilterButton, MobileFilterSheet } from '../components/ui/MobileFilterSheet';
import { defaultFilters, isAnyFiltered, specColumns } from '../lib/columnFilter';
import { useFilterParams } from '../lib/useFilterParams';
import { TableToolbar, TableSpacer, ResultCount } from '../components/ui/TableToolbar';
import { queryFor, thresholdFilters } from './thresholdQuery';
import { DataTable, type Column } from '../components/ui/DataTable';
import { TrashIcon } from '../components/ui/icons';
import './ThresholdsPage.css';
import { classifyLoadError, type LoadBlock } from '../lib/loadState';
import { LoadBlockNotice } from '../components/ui/LoadBlockNotice';

/** The scope-id control for the level the operator has chosen.
 *
 *  Before ADR-075 増分 3 this was one free-text box for every level, and the id it wanted — a
 *  device profile's UUID — is printed nowhere in the WebUI, so creating a profile-scoped rule was
 *  not actually possible. A mistyped id is not an error either: the engine compares it and simply
 *  never matches, so the rule is created, listed, and silently evaluates for no node.
 *
 *  Which control belongs to which level is `scopeIdKind`'s answer, not a second `switch` here —
 *  the same answer decides whether Save may be pressed, and two copies would let the dialog show a
 *  picker while readiness still tested a text box. */
function ScopeIdField({
  form,
  onChange,
}: {
  form: ThresholdForm;
  onChange: (scopeId: string) => void;
}) {
  const { t } = useTranslation('alertsConfig');
  const kind = scopeIdKind(form.level);
  const [profiles, setProfiles] = useState<ProfileSummary[]>([]);
  const [groups, setGroups] = useState<NodeGroup[]>([]);
  const { nodeName } = useEntityNames();

  // Both lists are small, bounded config tables — the same two the maintenance-window form loads.
  // Failing quietly leaves an empty picker rather than blocking the dialog: the operator can still
  // switch to a level whose list did load.
  useEffect(() => {
    if (kind === 'profile') api.listProfiles().then(setProfiles).catch(() => setProfiles([]));
    if (kind === 'folderGroup') api.listNodeGroups().then(setGroups).catch(() => setGroups([]));
  }, [kind]);

  const groupItems = useMemo(() => groupOptions(groups), [groups]);

  if (kind === 'none') {
    return (
      <div className="modal-field">
        <span className="modal-hint">{t(`thresholds.addModal.scopeIdNoun.global`)}</span>
      </div>
    );
  }
  return (
    <div className="modal-field">
      <label className="modal-field-label">{t('thresholds.addModal.scopeId')}</label>
      {kind === 'node' ? (
        <NodePicker
          value={form.scopeId || null}
          valueLabel={form.scopeId ? nodeName(form.scopeId) : undefined}
          onChange={(n) => onChange(n?.id ?? '')}
          placeholder={t('thresholds.addModal.scopeIdPlaceholder.node')}
        />
      ) : kind === 'profile' ? (
        <Select value={form.scopeId} onChange={(e) => onChange(e.target.value)}>
          <option value="">{t('thresholds.addModal.scopeIdPlaceholder.profile')}</option>
          {profiles.map((p) => (
            <option key={p.id} value={p.id}>
              {p.name}
            </option>
          ))}
        </Select>
      ) : kind === 'folderGroup' ? (
        <Select value={form.scopeId} onChange={(e) => onChange(e.target.value)}>
          <option value="">{t('thresholds.addModal.scopeIdPlaceholder.group_id')}</option>
          {groupItems.map((g) => (
            <option key={g.id} value={g.id}>
              {g.label}
            </option>
          ))}
        </Select>
      ) : (
        // The legacy tag scope. Free text because a tag value *is* free text, and no list of the
        // ones in use exists — nothing in the product writes `nodes.tags` but a bundle import.
        <TextInput
          className="mono"
          placeholder={t('thresholds.addModal.scopeIdPlaceholder.group')}
          value={form.scopeId}
          onChange={(e) => onChange(e.target.value)}
        />
      )}
      <span className="modal-hint">
        {kind === 'folderGroup'
          ? t('thresholds.addModal.folderGroupHint')
          : kind === 'tag'
            ? t('thresholds.addModal.legacyTagHint')
            : t('thresholds.addModal.scopeIdHint', {
                noun: t(`thresholds.addModal.scopeIdNoun.${form.level}`),
              })}
      </span>
    </div>
  );
}

/** Create or edit a threshold rule (focused-editing modal).
 *
 *  One dialog for both, the shape `EventRulesPage`'s `RuleModal` uses. Two would be two answers to
 *  "what does this form send", and the add path and the edit path would drift. The judgement —
 *  what the fields become, and whether they may be submitted — lives in `thresholdRequest.ts`,
 *  because Vitest does not execute a `.tsx`.
 *
 *  ⚠️ The form state is held here, inside a conditionally-mounted component, so closing the dialog
 *  *is* the reset (ui-conventions "Modals"). A `resetForm()` enumerating the fields would be a
 *  second copy of the field list. */
function ThresholdModal({
  mode,
  rule,
  onClose,
  onSaved,
}: {
  mode: 'add' | 'edit';
  rule?: StoredThreshold;
  onClose: () => void;
  onSaved: () => void;
}) {
  const { t } = useTranslation('alertsConfig');
  const [form, setForm] = useState<ThresholdForm>(() => thresholdFormFrom(rule));
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const set = <K extends keyof ThresholdForm>(key: K, value: ThresholdForm[K]) =>
    setForm((f) => ({ ...f, [key]: value }));

  const ready = isThresholdReady(form);

  // The legacy tag-based `group` level is not offered for a *new* rule — nothing in the product
  // writes `nodes.tags`, so a rule created at it cannot match anything (ADR-075 増分 3, the same
  // move the maintenance-window form already made). ⚠️ It must still appear while editing a rule
  // that already sits at it: a `<select>` whose value is absent from its options renders blank,
  // and the next save would silently move the rule to whichever level rendered first.
  const levels = useMemo(
    () =>
      CREATABLE_SCOPE_LEVELS.includes(form.level)
        ? CREATABLE_SCOPE_LEVELS
        : [...CREATABLE_SCOPE_LEVELS, form.level],
    [form.level],
  );

  // Two derivations of "this is the reachability rule", and they are deliberately different.
  //
  //  - `lockedMetric` reads the **stored** rule, so editing that row never shows or lets anyone
  //    retype the engine's internal sentinel. Deriving it from the typed value instead would make
  //    the input disappear the moment someone typed `__liveness__` into it, with no way back.
  //  - `noBounds` reads the **current** value, so the bounds also disappear in add mode — the
  //    sentinel is offered in the metric datalist, and bounds on it are read by nothing
  //    (`repo.rs`'s seed comment): the engine takes the severity from the committed `NodeState`.
  const lockedMetric = mode === 'edit' && rule?.metric === LIVENESS_METRIC;
  const noBounds = form.metric.trim() === LIVENESS_METRIC;

  const submit = () => {
    if (!ready) return;
    setBusy(true);
    setError(null);
    const body = thresholdBody(form);
    const call =
      mode === 'edit' && rule
        ? api.updateThreshold(rule.id, body)
        : api.createThreshold(body).then(() => undefined);
    call
      .then(() => {
        onSaved();
        onClose();
      })
      .catch((e: unknown) => {
        setError(errMsg(e, t('thresholds.err.save')));
        setBusy(false);
      });
  };

  return (
    <Modal
      title={mode === 'edit' ? t('thresholds.editModal.title') : t('thresholds.addModal.title')}
      onClose={onClose}
      footer={
        <>
          <Button variant="outline" onClick={onClose} disabled={busy}>
            {t('common:actions.cancel')}
          </Button>
          <Button variant="primary" onClick={submit} disabled={!ready || busy}>
            {mode === 'edit' ? t('common:actions.save') : t('thresholds.addModal.add')}
          </Button>
        </>
      }
    >
      <div className="modal-field">
        <label className="modal-field-label">{t('thresholds.addModal.scopeLevel')}</label>
        <Select
          value={form.level}
          onChange={(e) =>
            // Clear the id with the level. Ids are not interchangeable across levels — a profile
            // UUID left behind on a folder-group rule is a rule that matches nothing — and every
            // control below is now a picker, so there is nothing an operator would want carried.
            setForm((f) => ({ ...f, level: e.target.value as ScopeLevel, scopeId: '' }))
          }
        >
          {levels.map((l) => (
            <option key={l} value={l}>
              {t(`thresholds.scopeLevel.${l}`)}
            </option>
          ))}
        </Select>
      </div>
      <ScopeIdField form={form} onChange={(scopeId) => set('scopeId', scopeId)} />
      <div className="modal-field">
        <label className="modal-field-label">{t('thresholds.addModal.metric')}</label>
        {lockedMetric ? (
          <>
            <p className="thresholds-fixed">{t('format:liveness')}</p>
            <span className="modal-hint">{t('thresholds.livenessMetric')}</span>
          </>
        ) : (
          <>
            <TextInput
              className="mono"
              placeholder={t('thresholds.addModal.metricPlaceholder')}
              list="metric-presets"
              value={form.metric}
              onChange={(e) => set('metric', e.target.value)}
            />
            <datalist id="metric-presets">
              {METRIC_PRESETS.map((m) => (
                <option key={m} value={m} />
              ))}
            </datalist>
          </>
        )}
      </div>
      <div className="modal-field">
        <label className="modal-field-label">{t('thresholds.addModal.direction')}</label>
        <Select
          value={form.direction}
          onChange={(e) => set('direction', e.target.value as Direction)}
        >
          {DIRECTIONS.map((d) => (
            <option key={d} value={d}>
              {t(`thresholds.direction.${d}`)}
            </option>
          ))}
        </Select>
      </div>
      <div className="modal-field">
        <label className="modal-field-label">
          {noBounds ? t('thresholds.addModal.dwellOnly') : t('thresholds.addModal.boundsDwell')}
        </label>
        <div className="thresholds-bounds">
          {!noBounds && (
            <>
              <TextInput
                className="thresholds-num"
                placeholder={t('thresholds.addModal.warnPlaceholder')}
                value={form.warning}
                onChange={(e) => set('warning', e.target.value)}
              />
              <TextInput
                className="thresholds-num"
                placeholder={t('thresholds.addModal.critPlaceholder')}
                value={form.critical}
                onChange={(e) => set('critical', e.target.value)}
              />
            </>
          )}
          <TextInput
            className="thresholds-num"
            placeholder={t('thresholds.addModal.dwellPlaceholder')}
            value={form.dwell}
            onChange={(e) => set('dwell', e.target.value)}
            title={t('thresholds.addModal.dwellTitle')}
          />
        </div>
        <span className="modal-hint">
          {noBounds ? t('thresholds.livenessMetric') : t('thresholds.addModal.boundsHint')}
        </span>
      </div>
      {error && <p className="form-error">{error}</p>}
    </Modal>
  );
}

/** Confirm + delete a threshold rule (destructive-consent modal). */
function DeleteThresholdModal({
  rule,
  onClose,
  onDone,
}: {
  rule: StoredThreshold;
  onClose: () => void;
  onDone: () => void;
}) {
  const { t } = useTranslation('alertsConfig');
  return (
    <ConfirmDeleteModal
      title={t('thresholds.deleteModal.title')}
      onConfirm={() => api.deleteThreshold(rule.id)}
      errorFallback={t('thresholds.err.delete')}
      onClose={onClose}
      onDone={onDone}
    >
      {/* A global rule has no scope id, and the shared sentence used to interpolate the empty
          string straight into "… for <mono></mono>?" — an empty pair of quotes where the target
          belongs, on a destructive confirmation. Two sentences rather than a placeholder that is
          sometimes blank. */}
      <Trans
        t={t}
        i18nKey={
          rule.scope_level === 'global'
            ? 'thresholds.deleteModal.bodyGlobal'
            : 'thresholds.deleteModal.body'
        }
        values={{
          level: t(`thresholds.scopeLevel.${rule.scope_level}`),
          metric: rule.metric === LIVENESS_METRIC ? t('format:liveness') : rule.metric,
          scope: rule.scope_id,
        }}
        components={{ strong: <strong />, mono: <strong className="mono" /> }}
      />
    </ConfirmDeleteModal>
  );
}

export function ThresholdsPage() {
  const { t } = useTranslation('alertsConfig');
  const canConfig = useCan('manage_config');
  const [rows, setRows] = useState<StoredThreshold[]>([]);
  /** Whole-ruleset size and whether `rows` is only a prefix of it — both come from the server. */
  const [page, setPage] = useState<{ total: number; truncated: boolean }>({
    total: 0,
    truncated: false,
  });
  const [block, setBlock] = useState<LoadBlock | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [adding, setAdding] = useState(false);
  const [editing, setEditing] = useState<StoredThreshold | null>(null);
  const [deleting, setDeleting] = useState<StoredThreshold | null>(null);
  /** Whether any reachability rule exists **anywhere** — `null` until the question is answered.
   *
   *  It cannot be read off `rows`: that list is the operator's current filter, capped by the
   *  server, so an absent rule and a narrowed view look identical. And the tri-state matters —
   *  rendering the warning while the answer is still `undefined` would flash "nothing is watching
   *  your fleet" on every page load. */
  const [hasLiveness, setHasLiveness] = useState<boolean | null>(null);
  const { scopeName } = useEntityNames();

  const [sheet, setSheet] = useState(false);

  // ⚠️ **`filterCols` comes from the specs, not from `filterableColumns(columns)`.**
  // `useFilterParams` derives the filter state from whatever list it is given, `load` depends on
  // that state, and an effect depends on `load` — so the list has to be stable. The display columns
  // are not: they close over `scopeName`, which changes identity every time a name batch resolves,
  // and the ruleset would be refetched at that moment. The specs depend on `t` alone.
  const specs = useMemo(() => thresholdFilters(t), [t]);
  const filterCols = useMemo(() => specColumns(specs), [specs]);
  const columns = useMemo<Column<StoredThreshold>[]>(() => {
    const cols: Column<StoredThreshold>[] = [
      {
        // Two values, two columns (ADR-075 増分 2). They shared a cell until an operator asked
        // which of the two things in it was the scope id — every other column here is one value
        // under a heading that names it, and this one was not. The headings are the add dialog's
        // field names verbatim, so "where do I set this?" is answered by the words matching.
        key: 'scope_level',
        header: t('thresholds.cols.scope'),
        width: '110px',
        render: (row) => <Badge tone="neutral">{t(`thresholds.scopeLevel.${row.scope_level}`)}</Badge>,
      },
      {
        // Not filterable, and the absence is the declaration: this list is capped at 500 rules
        // server-side, so a browser-side predicate would examine that prefix and report on it —
        // see `thresholdQuery.ts`. The API has no `scope_id` parameter to push it into.
        key: 'scope_id',
        header: t('thresholds.cols.scopeId'),
        width: '1.2fr',
        render: (row) =>
          // A global rule has no id to resolve — the level column already says "every node", and
          // `EntityName` on an empty id renders a bare em dash with no explanation of why.
          row.scope_level === 'global' ? (
            <span className="muted" title={t('thresholds.scopeIdNone')}>
              —
            </span>
          ) : (
            <EntityName name={scopeName(row.scope_level, row.scope_id)} id={row.scope_id} />
          ),
      },
      {
        key: 'q',
        header: t('thresholds.cols.metric'),
        width: '1.1fr',
        // The reachability rule carries the engine's internal check name. Every other surface
        // already shows it as "Reachability" (`AlertWhatText`), and this is the one screen where
        // an operator would otherwise meet the raw sentinel.
        render: (row) =>
          row.metric === LIVENESS_METRIC ? (
            <span>{t('format:liveness')}</span>
          ) : (
            <span className="mono">{row.metric}</span>
          ),
      },
      {
        // What the metric measures. Every other column here is a property of the *rule*; this is
        // the only one that is a property of the metric, and without it a row like
        // `Reachability | below | (no bounds)` says how the rule behaves and nothing about what
        // it watches. Not filterable — it is derived from the metric, which has its own filter.
        key: 'meaning',
        header: t('thresholds.cols.meaning'),
        width: '2fr',
        render: (row) => {
          const key = metricMeaningKey(row.metric);
          // Rows are a fixed 44px, so the text is one line with an ellipsis. `title` carries the
          // tail for a narrow window — it is an overflow fallback, not the only place the
          // explanation lives (ADR-055 R4), which is why the column is not hover-only.
          return key ? (
            <span className="thresholds-meaning" title={t(key)}>
              {t(key)}
            </span>
          ) : (
            <span className="muted" title={t('thresholds.meaningUnknown')}>
              —
            </span>
          );
        },
      },
      {
        key: 'direction',
        header: t('thresholds.cols.direction'),
        width: '92px',
        render: (row) => (
          <span className="muted">{t(`thresholds.direction.${row.direction}`)}</span>
        ),
      },
      {
        key: 'bounds',
        header: t('thresholds.cols.bounds'),
        width: '150px',
        render: (row) => (
          <span className="thresholds-bounds">
            {row.warning != null && (
              <Badge tone="warning">
                {t('thresholds.warnShort')} {row.warning}
              </Badge>
            )}
            {row.critical != null && (
              <Badge tone="critical">
                {t('thresholds.critShort')} {row.critical}
              </Badge>
            )}
          </span>
        ),
      },
      {
        key: 'dwell',
        header: t('thresholds.cols.dwell'),
        width: '100px',
        render: (row) => (
          <span className="muted">{t('thresholds.dwellValue', { n: row.dwell_samples })}</span>
        ),
      },
      {
        key: 'actions',
        header: t('thresholds.cols.actions'),
        width: '124px',
        align: 'right',
        // Drawn only for a caller who may use them (ADR-056) — not `disabled`, which explains
        // itself on hover only and so explains itself to nobody on a touch device.
        render: (row) =>
          canConfig && (
            <span className="ytable-actions">
              <IconButton title={t('common:actions.edit')} onClick={() => setEditing(row)}>
                <EditIcon />
              </IconButton>
              <IconButton
                title={t('common:actions.delete')}
                danger
                onClick={() => setDeleting(row)}
              >
                <TrashIcon />
              </IconButton>
            </span>
          ),
      },
    ];
    for (const c of cols) c.filter = specs[c.key];
    return cols;
  }, [canConfig, scopeName, specs, t]);

  // The filters live in the URL — nothing else holds them, so a narrowed ruleset survives a
  // reload and can be shared. Since Inc.10 that is the shared codec (`useFilterParams`) rather
  // than a per-field `readFilters`/`writeFilters` pair: the column key **is** the query key, so
  // the bookmarks taken before this change still resolve. `replace: true` lives in the hook,
  // because a settled filter is not a place you navigated to.
  const { filters, setFilters } = useFilterParams(filterCols);
  const filtered = isAnyFiltered(filterCols, filters);

  // Refetch whenever the filter changes: the predicate runs in the database, so a browser-side
  // narrowing would only ever examine the 500 rules already on screen — which is the whole
  // reason this screen filters server-side (see `thresholdQuery.ts`).
  const load = useCallback(() => {
    api
      .listThresholds(queryFor(filterCols, filters))
      .then((p) => {
        setRows(p.items);
        setPage({ total: p.total, truncated: p.truncated });
        setBlock(null);
      })
      .catch((e: unknown) => setBlock(classifyLoadError(e)))
      .finally(() => setLoading(false));
    // Asked separately and unfiltered, because the question is about the whole ruleset. Rides
    // `load` so it is re-asked after an add or a delete — the two moments the answer changes.
    api
      .listThresholds({ q: LIVENESS_METRIC })
      .then((p) => setHasLiveness(p.total > 0))
      .catch(() => setHasLiveness(null));
  }, [filterCols, filters]);

  useEffect(() => {
    load();
  }, [load]);

  return (
    <div>
      <PageHeader
        title={t('nav:alerts.rules')}
        trail={[{ label: t('nav:sections.alerts') }, { label: t('nav:alerts.rules') }]}
        note={t('thresholds.note')}
      />

      <Card className="thresholds-note-card">
        <p className="thresholds-note">{t('thresholds.explainer')}</p>
      </Card>

      {/* Deleting the reachability rule is allowed — it is an ordinary row — and it switches off
          node-down paging for the whole fleet. Saying so where it happened is the alternative to
          making that one row undeletable, which would make it a different kind of row. */}
      {hasLiveness === false && (
        <Card className="thresholds-warn-card">
          <p className="thresholds-note">{t('thresholds.noLiveness')}</p>
        </Card>
      )}

      {block ? (
        <LoadBlockNotice
          permission="manage_config"
          block={block}
          unavailable={t('thresholds.unavailable')}
        />
      ) : (
        <>
          <TableToolbar>
            <FilterButton columns={filterCols} filters={filters} onOpen={() => setSheet(true)} />
            <ClearFilters
              columns={filterCols}
              filters={filters}
              onClear={() => setFilters(defaultFilters(filterCols))}
            />
            <TableSpacer />
            {/* Says how many of how many when the server capped the response — never a bare count
                that would read as the whole ruleset. */}
            {page.truncated && (
              <span className="muted thresholds-truncated">
                {t('thresholds.truncated', { shown: rows.length, total: page.total })}
              </span>
            )}
            <ResultCount
              shown={rows.length}
              total={filtered ? page.total : undefined}
              noun={t('common:noun.rule', { count: rows.length })}
            />
            {canConfig && (
              <Button variant="primary" onClick={() => setAdding(true)}>
                {t('thresholds.add')}
              </Button>
            )}
          </TableToolbar>

          {error && <p className="form-error">{error}</p>}

          <DataTable
            rows={rows}
            columns={columns}
            filters={filters}
            onFiltersChange={setFilters}
            rowKey={(r) => r.id}
            loading={loading}
            // Keyed off the filter, never off `rows.length`: with the predicate in SQL, a
            // filtered query that legitimately returns zero is indistinguishable from a
            // ruleset that has no rules at all — and on this table those read very differently.
            empty={
              filtered ? (
                <div className="yt-empty">
                  <p className="yt-empty-title">{t('thresholds.emptyFiltered')}</p>
                </div>
              ) : (
                <div className="yt-empty">
                  <p className="yt-empty-title">{t('thresholds.empty')}</p>
                  <p className="yt-empty-sub">{t('thresholds.emptySub')}</p>
                </div>
              )
            }
          />
          {sheet && (
            <MobileFilterSheet
              columns={filterCols}
              filters={filters}
              onChange={setFilters}
              labels={{
                q: t('thresholds.cols.metric'),
                scope_level: t('thresholds.cols.scope'),
                direction: t('thresholds.cols.direction'),
              }}
              onClose={() => setSheet(false)}
            />
          )}
        </>
      )}

      {adding && <ThresholdModal mode="add" onClose={() => setAdding(false)} onSaved={load} />}
      {editing && (
        // Keyed by the row's id so opening a *different* rule remounts the dialog with that rule's
        // values. Without it React would keep the mounted form state and show the previous rule.
        <ThresholdModal
          key={editing.id}
          mode="edit"
          rule={editing}
          onClose={() => setEditing(null)}
          onSaved={load}
        />
      )}
      {deleting && (
        <DeleteThresholdModal
          rule={deleting}
          onClose={() => setDeleting(null)}
          onDone={() => {
            setDeleting(null);
            setError(null);
            load();
          }}
        />
      )}
    </div>
  );
}
