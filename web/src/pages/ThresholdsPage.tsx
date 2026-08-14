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
import { useSearchParams } from 'react-router-dom';
import { Trans, useTranslation } from 'react-i18next';
import { api, errMsg, ApiError } from '../services/api';
import { useAuthStore } from '../store';
import {
  DIRECTIONS,
  SCOPE_LEVELS,
  type Direction,
  type ScopeLevel,
  type StoredThreshold,
} from '../types/api';
import { METRIC_PRESETS } from '../lib/suppression';
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
import { filterableColumns, type FilterState } from '../lib/columnFilter';
import { TableToolbar, TableSpacer, ResultCount } from '../components/ui/TableToolbar';
import {
  DEFAULT_THRESHOLD_FILTERS,
  filtersFromState,
  isFiltered,
  queryFor,
  readFilters,
  stateFromFilters,
  thresholdFilters,
  writeFilters,
} from './thresholdQuery';
import { DataTable, type Column } from '../components/ui/DataTable';
import { TrashIcon } from '../components/ui/icons';
import './ThresholdsPage.css';

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

  const ready = metric.trim() !== '' && scopeId.trim() !== '';

  const submit = () => {
    if (!ready) return;
    setBusy(true);
    setError(null);
    const num = (s: string) => (s.trim() === '' ? undefined : Number(s));
    api
      .createThreshold({
        scope_level: level,
        scope_id: scopeId.trim(),
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
  const authed = useAuthStore((s) => s.authed);
  const [rows, setRows] = useState<StoredThreshold[]>([]);
  /** Whole-ruleset size and whether `rows` is only a prefix of it — both come from the server. */
  const [page, setPage] = useState<{ total: number; truncated: boolean }>({
    total: 0,
    truncated: false,
  });
  const [unavailable, setUnavailable] = useState(false);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [adding, setAdding] = useState(false);
  const [deleting, setDeleting] = useState<StoredThreshold | null>(null);
  const { scopeName } = useEntityNames();

  // The filters live in the URL — nothing else holds them, so a narrowed ruleset survives a
  // reload and can be shared. `replace: true` because a settled filter is not a place you
  // navigated to: pushing one per keystroke would make Back walk through every intermediate
  // state instead of leaving the screen.
  const [sheet, setSheet] = useState(false);
  const [params, setParams] = useSearchParams();
  const filters = useMemo(
    () => readFilters(params, SCOPE_LEVELS, DIRECTIONS),
    [params],
  );

  // Refetch whenever the filter changes: the predicate runs in the database, so a browser-side
  // narrowing would only ever examine the 500 rules already on screen — which is the whole
  // reason this screen filters server-side (see `thresholdQuery.ts`).
  const load = useCallback(() => {
    api
      .listThresholds(queryFor(filters))
      .then((p) => {
        setRows(p.items);
        setPage({ total: p.total, truncated: p.truncated });
        setUnavailable(false);
      })
      .catch((e: unknown) => {
        if (e instanceof ApiError && e.code === 'admin_unavailable') setUnavailable(true);
      })
      .finally(() => setLoading(false));
  }, [filters]);

  useEffect(() => {
    load();
  }, [load]);

  const specs = thresholdFilters(t);
  const columns: Column<StoredThreshold>[] = [
    {
      key: 'scope_level',
      header: t('thresholds.cols.scope'),
      width: '1.6fr',
      render: (row) => (
        <>
          <Badge tone="neutral">{t(`thresholds.scopeLevel.${row.scope_level}`)}</Badge>
          <EntityName name={scopeName(row.scope_level, row.scope_id)} id={row.scope_id} />
        </>
      ),
    },
    {
      key: 'q',
      header: t('thresholds.cols.metric'),
      width: '1.4fr',
      render: (row) => <span className="mono">{row.metric}</span>,
    },
    {
      key: 'direction',
      header: t('thresholds.cols.direction'),
      width: '110px',
      render: (row) => <span className="muted">{t(`thresholds.direction.${row.direction}`)}</span>,
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
        authed && (
          <span className="ytable-actions">
            <IconButton title={t('common:actions.delete')} danger onClick={() => setDeleting(row)}>
              <TrashIcon />
            </IconButton>
          </span>
        ),
    },
  ];
  for (const c of columns) c.filter = specs[c.key];
  const filterCols = filterableColumns(columns);
  const rowFilters = stateFromFilters(filters);
  const onRowFilters = (next: FilterState) => {
    const p = new URLSearchParams(params);
    writeFilters(p, filtersFromState(next));
    setParams(p, { replace: true });
  };

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

      {unavailable ? (
        <Card>
          <p className="muted">{t('thresholds.unavailable')}</p>
        </Card>
      ) : (
        <>
          <TableToolbar>
            <FilterButton
              columns={filterCols}
              filters={rowFilters}
              onOpen={() => setSheet(true)}
            />
            <ClearFilters
              columns={filterCols}
              filters={rowFilters}
              onClear={() => onRowFilters(stateFromFilters(DEFAULT_THRESHOLD_FILTERS))}
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
              total={isFiltered(filters) ? page.total : undefined}
              noun={t('common:noun.rule', { count: rows.length })}
            />
            {authed && (
              <Button variant="primary" onClick={() => setAdding(true)}>
                {t('thresholds.add')}
              </Button>
            )}
          </TableToolbar>

          {error && <p className="form-error">{error}</p>}

          <DataTable
            rows={rows}
            columns={columns}
            filters={rowFilters}
            onFiltersChange={onRowFilters}
            rowKey={(r) => r.id}
            loading={loading}
            // Keyed off the filter, never off `rows.length`: with the predicate in SQL, a
            // filtered query that legitimately returns zero is indistinguishable from a
            // ruleset that has no rules at all — and on this table those read very differently.
            empty={
              isFiltered(filters) ? (
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
              filters={rowFilters}
              onChange={onRowFilters}
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
