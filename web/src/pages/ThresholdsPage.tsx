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
  DIRECTIONS,
  SCOPE_LEVELS,
  type Direction,
  type ScopeLevel,
  type StoredThreshold,
} from '../types/api';
import { METRIC_PRESETS } from '../lib/suppression';
import { LIVENESS_METRIC } from '../lib/format';
import { PageHeader } from '../components/ui/PageHeader';
import { Card } from '../components/ui/Card';
import { Button } from '../components/ui/Button';
import { ConfirmDeleteModal } from '../components/ui/ConfirmDeleteModal';
import { Modal } from '../components/ui/Modal';
import { TextInput, Select } from '../components/ui/Field';
import { Badge } from '../components/ui/Badge';
import { EntityName, useEntityNames } from '../components/ui/EntityName';
import { IconButton } from '../components/ui/IconButton';
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

/** Create a threshold rule (focused-editing modal). */
function AddThresholdModal({ onClose, onSaved }: { onClose: () => void; onSaved: () => void }) {
  const { t } = useTranslation('alertsConfig');
  const [level, setLevel] = useState<ScopeLevel>('profile');
  const [scopeId, setScopeId] = useState('');
  const [metric, setMetric] = useState('');
  const [direction, setDirection] = useState<Direction>('above');
  const [warning, setWarning] = useState('');
  const [critical, setCritical] = useState('');
  const [dwell, setDwell] = useState('3');
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  // A `global` rule targets every node, so it has nothing to point at and the server pins its
  // `scope_id` to the empty string. Requiring one here would make the level unusable.
  const global = level === 'global';
  const ready = metric.trim() !== '' && (global || scopeId.trim() !== '');

  const submit = () => {
    if (!ready) return;
    setBusy(true);
    setError(null);
    const num = (s: string) => (s.trim() === '' ? undefined : Number(s));
    api
      .createThreshold({
        scope_level: level,
        scope_id: global ? '' : scopeId.trim(),
        metric: metric.trim(),
        direction,
        warning: num(warning),
        critical: num(critical),
        dwell_samples: num(dwell),
      })
      .then(() => {
        onSaved();
        onClose();
      })
      .catch((e: unknown) => {
        setError(errMsg(e, t('thresholds.err.add')));
        setBusy(false);
      });
  };

  return (
    <Modal
      title={t('thresholds.addModal.title')}
      onClose={onClose}
      footer={
        <>
          <Button variant="outline" onClick={onClose} disabled={busy}>
            {t('common:actions.cancel')}
          </Button>
          <Button variant="primary" onClick={submit} disabled={!ready || busy}>
            {t('thresholds.addModal.add')}
          </Button>
        </>
      }
    >
      <div className="modal-field">
        <label className="modal-field-label">{t('thresholds.addModal.scopeLevel')}</label>
        <Select value={level} onChange={(e) => setLevel(e.target.value as ScopeLevel)}>
          {SCOPE_LEVELS.map((l) => (
            <option key={l} value={l}>
              {t(`thresholds.scopeLevel.${l}`)}
            </option>
          ))}
        </Select>
      </div>
      {global ? (
        <div className="modal-field">
          <span className="modal-hint">{t(`thresholds.addModal.scopeIdNoun.global`)}</span>
        </div>
      ) : (
        <div className="modal-field">
          <label className="modal-field-label">{t('thresholds.addModal.scopeId')}</label>
          <TextInput
            className="mono"
            placeholder={t(`thresholds.addModal.scopeIdPlaceholder.${level}`)}
            value={scopeId}
            onChange={(e) => setScopeId(e.target.value)}
            autoFocus
          />
          <span className="modal-hint">
            {t('thresholds.addModal.scopeIdHint', {
              noun: t(`thresholds.addModal.scopeIdNoun.${level}`),
            })}
          </span>
        </div>
      )}
      <div className="modal-field">
        <label className="modal-field-label">{t('thresholds.addModal.metric')}</label>
        <TextInput
          className="mono"
          placeholder={t('thresholds.addModal.metricPlaceholder')}
          list="metric-presets"
          value={metric}
          onChange={(e) => setMetric(e.target.value)}
        />
        <datalist id="metric-presets">
          {METRIC_PRESETS.map((m) => (
            <option key={m} value={m} />
          ))}
        </datalist>
      </div>
      <div className="modal-field">
        <label className="modal-field-label">{t('thresholds.addModal.direction')}</label>
        <Select value={direction} onChange={(e) => setDirection(e.target.value as Direction)}>
          {DIRECTIONS.map((d) => (
            <option key={d} value={d}>
              {t(`thresholds.direction.${d}`)}
            </option>
          ))}
        </Select>
      </div>
      <div className="modal-field">
        <label className="modal-field-label">{t('thresholds.addModal.boundsDwell')}</label>
        <div className="thresholds-bounds">
          <TextInput
            className="thresholds-num"
            placeholder={t('thresholds.addModal.warnPlaceholder')}
            value={warning}
            onChange={(e) => setWarning(e.target.value)}
          />
          <TextInput
            className="thresholds-num"
            placeholder={t('thresholds.addModal.critPlaceholder')}
            value={critical}
            onChange={(e) => setCritical(e.target.value)}
          />
          <TextInput
            className="thresholds-num"
            placeholder={t('thresholds.addModal.dwellPlaceholder')}
            value={dwell}
            onChange={(e) => setDwell(e.target.value)}
            title={t('thresholds.addModal.dwellTitle')}
          />
        </div>
        <span className="modal-hint">{t('thresholds.addModal.boundsHint')}</span>
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
      <Trans
        t={t}
        i18nKey="thresholds.deleteModal.body"
        values={{
          level: t(`thresholds.scopeLevel.${rule.scope_level}`),
          metric: rule.metric,
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
        key: 'scope_level',
        header: t('thresholds.cols.scope'),
        width: '1.6fr',
        render: (row) => (
          <>
            <Badge tone="neutral">{t(`thresholds.scopeLevel.${row.scope_level}`)}</Badge>
            {/* A global rule has no id to resolve; the badge already says "every node", and an
                `EntityName` on an empty id would render a bare em dash beside it. */}
            {row.scope_level !== 'global' && (
              <EntityName name={scopeName(row.scope_level, row.scope_id)} id={row.scope_id} />
            )}
          </>
        ),
      },
      {
        key: 'q',
        header: t('thresholds.cols.metric'),
        width: '1.4fr',
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
        key: 'direction',
        header: t('thresholds.cols.direction'),
        width: '110px',
        render: (row) => (
          <span className="muted">{t(`thresholds.direction.${row.direction}`)}</span>
        ),
      },
      {
        key: 'bounds',
        header: t('thresholds.cols.bounds'),
        width: '170px',
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
        width: '92px',
        align: 'right',
        render: (row) =>
          canConfig && (
            <span className="ytable-actions">
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

      {adding && <AddThresholdModal onClose={() => setAdding(false)} onSaved={load} />}
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
