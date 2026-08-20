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
import { api } from '../services/api';
import { useCan } from '../store';
import type { StoredThreshold } from '../types/api';
import { LIVENESS_METRIC } from '../lib/format';
import { metricMeaningKey } from '../lib/metricMeaning';
import { boundText } from '../lib/portRuleForm';
import { ThresholdModal } from '../components/ThresholdModal/ThresholdModal';
import { splitInterfaceScopeId } from '../lib/interfaceScope';
import { PageHeader } from '../components/ui/PageHeader';
import { Card } from '../components/ui/Card';
import { Button } from '../components/ui/Button';
import { ConfirmDeleteModal } from '../components/ui/ConfirmDeleteModal';
import { Badge } from '../components/ui/Badge';
import { EntityName, useEntityNames } from '../components/ui/EntityName';
import { IconButton } from '../components/ui/IconButton';
import { EditIcon } from '../components/ui/icons';
import { ClearFilters } from '../components/ui/ClearFilters';
import { FilterButton, MobileFilterSheet } from '../components/ui/MobileFilterSheet';
import { decodeSet, defaultFilters, isAnyFiltered, specColumns } from '../lib/columnFilter';
import { useFilterParams } from '../lib/useFilterParams';
import { TableToolbar, TableSpacer, ResultCount } from '../components/ui/TableToolbar';
import { queryFor, thresholdFilters } from './thresholdQuery';
import { DataTable, type Column } from '../components/ui/DataTable';
import { TrashIcon } from '../components/ui/icons';
import './ThresholdsPage.css';
import { classifyLoadError, type LoadBlock } from '../lib/loadState';
import { LoadBlockNotice } from '../components/ui/LoadBlockNotice';

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
  // Its own resolver: this dialog is a separate component from the list, and the sentence it
  // shows names the same targets the row does. A raw UUID on a destructive confirmation is the
  // one place `no-raw-uuids-in-tables` matters most — the operator is being asked to be sure.
  const { scopeName } = useEntityNames();
  return (
    <ConfirmDeleteModal
      title={t('thresholds.deleteModal.title')}
      onConfirm={() => api.deleteThreshold(rule.id)}
      errorFallback={t('thresholds.err.delete')}
      onClose={onClose}
      onDone={onDone}
    >
      {/* A global rule has no target, and the shared sentence used to interpolate the empty
          string straight into "… for <mono></mono>?" — an empty pair of quotes where the target
          belongs, on a destructive confirmation. Two sentences rather than a placeholder that is
          sometimes blank. Since ADR-078 the non-global sentence may name several, comma-joined. */}
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
          scope: rule.scope_ids.map((id) => scopeName(rule.scope_level, id)).join(', '),
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
  /** How many port-scoped rules exist, whether or not this view is showing them.
   *
   *  🚨 Asked as its own request, not counted from `rows`. The rows are the operator's filter —
   *  which by default excludes exactly these — and capped at 500 besides, so counting them would
   *  answer "none" about a fleet with thousands. A default that hides rows without saying how many
   *  is a screen that quietly reports a shorter ruleset than the deployment has. */
  const [portRuleTotal, setPortRuleTotal] = useState(0);
  const { scopeName } = useEntityNames();
  // The whole target list, for the cell's `title`. Two names are drawn; this is what makes the
  // other two readable rather than merely counted.
  const scopeTitle = (row: StoredThreshold) =>
    row.scope_ids.map((id) => scopeName(row.scope_level, id)).join(', ');

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
          // A global rule has no target to resolve — the type column already says "every node",
          // and `EntityName` on an empty id renders a bare em dash with no explanation of why.
          row.scope_level === 'global' || row.scope_ids.length === 0 ? (
            <span className="muted" title={t('thresholds.scopeIdNone')}>
              —
            </span>
          ) : (
            // Since ADR-078 a rule may name several. Two are shown and the rest counted, because
            // the column is one line: a Huawei rule names four profiles, and wrapping four names
            // would make every other row on the screen taller to suit one of them. The `title`
            // carries the whole list, so nothing is only-countable.
            <span className="thresholds-scopes" title={scopeTitle(row)}>
              {row.scope_ids.slice(0, 2).map((id, i) => (
                <span key={id}>
                  {i > 0 && <span className="muted">, </span>}
                  {row.scope_level === 'interface' ? (
                    // The id is `<node-uuid>:<ifindex>`, so the hover title has to be the node's
                    // half — `EntityName` would offer the whole composed string as a copyable
                    // "id", which is not an id anything else accepts.
                    <EntityName
                      name={scopeName(row.scope_level, id)}
                      id={splitInterfaceScopeId(id)[0]}
                    />
                  ) : (
                    <EntityName name={scopeName(row.scope_level, id)} id={id} />
                  )}
                </span>
              ))}
              {row.scope_ids.length > 2 && (
                <span className="muted">
                  {' '}
                  {t('thresholds.scopeMore', { count: row.scope_ids.length - 2 })}
                </span>
              )}
            </span>
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
            {/* Read in the metric's own unit. An absolute interface rate is stored in bits per
                second (ADR-076 決定 9), so the raw value here was `800000000` — a number whose
                digits an operator has to count. */}
            {row.warning != null && (
              <Badge tone="warning">
                {t('thresholds.warnShort')} {boundText(row.metric, row.warning)}
              </Badge>
            )}
            {row.critical != null && (
              <Badge tone="critical">
                {t('thresholds.critShort')} {boundText(row.metric, row.critical)}
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
    // Likewise unfiltered and likewise riding `load`: the count has to stay true after an add or
    // a delete, and after the operator opens the level filter up.
    api
      .listThresholds({ scope_level: 'interface', limit: 1 })
      .then((p) => setPortRuleTotal(p.total))
      .catch(() => setPortRuleTotal(0));
  }, [filterCols, filters]);

  /** Whether this view is currently leaving port rules out. */
  const portRulesHidden = useMemo(() => {
    const selected = decodeSet(filters.scope_level ?? '');
    // An empty selection means every level, so nothing is being left out.
    return selected.length > 0 && !selected.includes('interface');
  }, [filters.scope_level]);

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

      {/* The default view leaves port rules out (ADR-076 決定 12), and this line is the whole of
          what makes that honest: the count comes from the server, and the way back in is one
          click. Without it a hidden rule is indistinguishable from a rule that does not exist —
          which is the wrong belief to hold about alerting configuration. */}
      {portRulesHidden && portRuleTotal > 0 && (
        <p className="thresholds-hidden">
          {t('thresholds.portRulesHidden', { count: portRuleTotal })}{' '}
          <button
            type="button"
            className="linklike"
            onClick={() => setFilters({ ...filters, scope_level: '' })}
          >
            {t('thresholds.showPortRules')}
          </button>
        </p>
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
